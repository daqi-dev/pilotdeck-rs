# anchor-selfcheck: unsupported isolated SOP process capture producer; no product source mutations
import os, subprocess, socket, time, json, pathlib, signal, hashlib
import argparse
parser=argparse.ArgumentParser()
parser.add_argument("--engine",choices=("ts","native"),required=True)
for field in ("baseline","out","candidate-manifest","source-manifest"):
 parser.add_argument("--"+field,type=pathlib.Path,required=True)
parser.add_argument("--expected-source-sha",required=True)
parser.add_argument("--node",type=pathlib.Path,required=True)
parser.add_argument("--gateway",type=pathlib.Path)
parser.add_argument("--sidecar",type=pathlib.Path)
args=parser.parse_args()
d=args.out.resolve();d.mkdir(mode=0o700,parents=True,exist_ok=False)
root=args.baseline.resolve();node=str(args.node.resolve())
versions=json.loads(subprocess.check_output([node,"-p","JSON.stringify(process.versions)"],text=True))
assert versions["node"]=="22.22.0" and versions["icu"]=="78.2"
assert hashlib.sha256(args.source_manifest.read_bytes()).hexdigest()==args.expected_source_sha
candidate_rows=json.loads(args.candidate_manifest.read_text())
candidate_sha={r["path"]:r["sha256"] for r in candidate_rows}
if args.engine=="native":
 assert args.gateway and args.sidecar
 assert hashlib.sha256(args.gateway.read_bytes()).hexdigest()==candidate_sha["pilotdeck-gateway"]
 assert hashlib.sha256(args.sidecar.read_bytes()).hexdigest()==candidate_sha["pilotdeck-agent-loop-sidecar"]

head=subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip();assert head=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',head
for n in ['home','project','tmp','workspaces','driver']:(d/n).mkdir(exist_ok=False)
h=d/'home';p=d/'project';(p/'fixture.txt').write_text('BUSINESS_FILE_CONTENT_42\n')
plugin=p/'.pilotdeck/plugins/compact-observer';plugin.mkdir(parents=True);command='cat >> '+str(d/'hook-input.jsonl')+'; printf \"\\n\" >> '+str(d/'hook-input.jsonl')+'; printf \"{\\\"continue\\\":true}\"';(plugin/'plugin.json').write_text(json.dumps({'name':'compact-observer','hooks':{event:[{'matcher':'*','hooks':[{'type':'command','command':command}]}] for event in ['PreCompact','PostCompact']}}))
import http.server,threading
rows=[]
class Provider(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_GET(self):
  value={'status':'ok','protocolVersion':'2.0','moduleId':'sop.runtime','contract':'sop.lifecycle/v2','operations':['prepare','submit'],'descriptorVersion':'1.0','implementationId':'fixture.owner','transport':'sop-http-v2'}
  raw=json.dumps(value).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
 def do_POST(self):
  body=json.loads(self.rfile.read(int(self.headers['Content-Length']))); rows.append(body)
  if self.path.startswith('/v1/sop/'):
   operation=self.path.rsplit('/',1)[-1];state={'status':'active' if operation=='prepare' else 'completed','active_skill_id':'fixture','active_step_id':'node'}
   step={'skillId':'fixture','skillName':'Fixture','version':'v1','nodeId':'node','node':{'node_id':'node'},'instruction':'Complete the fixture.','expectedUserInfo':[],'knownSlots':{},'allowedNextStepIds':[],'requiredToolNames':[],'allowedActions':['completed'],'isTerminal':True,'declaresHandoff':False,'contextMode':'new_session'}
   result={'status':'completed','replyFragment':'SOP_FIXTURE_COMPLETE','slotUpdates':{},'events':[]}
   payload={'state':state,**({'step':step} if operation=='prepare' else {'result':result})}
   raw=json.dumps({'protocolVersion':'2.0','requestId':body['requestId'],'ok':True,'outcome':'completed','payload':payload}).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw);return

  messages=body.get('messages',[]); content=json.dumps(messages)
  tool=None
  if body.get('stream') and 'conversation summarizer' not in json.dumps(body).lower() and not any(m.get('role')=='tool' for m in messages):
   if 'SOP_TRIGGER' in content and any(t.get('function',{}).get('name')=='submit_step_result' for t in body.get('tools',[])):tool=('submit_step_result',{'status':'completed','replyFragment':'SOP_FIXTURE_COMPLETE'})
   elif 'BUSINESS_READ' in content:tool=('read_file',{'file_path':str(p/'fixture.txt')})
   elif 'BUSINESS_PERMISSION' in content:tool=('bash',{'command':'touch '+str(p/'DENIED_SIDE_EFFECT')})
  message={'role':'assistant','content': 'BUSINESS_FILE_CONTENT_42' if 'BUSINESS_READ' in content else 'BUSINESS_DENY_DONE'}
  if tool:message={'role':'assistant','content':None,'tool_calls':[{'id':'business-call','type':'function','function':{'name':tool[0],'arguments':json.dumps(tool[1])}}]}
  self.send_response(200)
  if body.get('stream'):
   self.send_header('Content-Type','text/event-stream');self.send_header('Connection','close');self.end_headers()
   delta={'role':'assistant','tool_calls':[{'index':0,**message['tool_calls'][0]}]} if tool else {'role':'assistant','content':message['content']}
   for item in [{'choices':[{'index':0,'delta':delta,'finish_reason':None}]},{'choices':[{'index':0,'delta':{},'finish_reason':'tool_calls' if tool else 'stop'}],'usage':{'prompt_tokens':11000,'completion_tokens':5,'total_tokens':11005}}]:self.wfile.write(('data: '+json.dumps(item)+'\n\n').encode())
   self.wfile.write(b'data: [DONE]\n\n');self.close_connection=True
  else:
   data=json.dumps({'choices':[{'index':0,'message':message,'finish_reason':'stop'}]}).encode();self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
  self.wfile.flush()
provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);threading.Thread(target=provider.serve_forever,daemon=True).start()
(h/'token').write_text('test-token\n');(h/'token').chmod(0o600)
(h/'pilotdeck.yaml').write_text('schemaVersion: 1\nagent: {model: fixture/mock, maxOutputTokens: 256, maxContextTokens: 7000}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:MOCK_PORT/v1\n      apiKey: mock-only\n      models: {mock: {}}\ntelemetry: {enabled: false}\n'.replace('MOCK_PORT',str(provider.server_port)))
(p/'definitions.json').write_text(json.dumps({'sops':[{'id':'fixture','name':'Fixture','content':{'start_node_id':'node','nodes':[{'node_id':'node','instruction':'Complete the fixture.','allowed_actions':['completed']}]}}]}))
with (h/'pilotdeck.yaml').open('a') as f:f.write('modules: '+json.dumps({'sop':{'enabled':True,'implementationId':'fixture.owner','contract':'sop.lifecycle/v2','transport':'sop-http-v2','endpoint':'http://127.0.0.1:'+str(provider.server_port),'manifestPath':'/healthz','definitionsPath':str(p/'definitions.json'),'defaultSopId':'fixture','timeoutMs':30000}})+'\n')
(h/'projects/fixture').mkdir(parents=True);(h/'projects/fixture/.cwd').write_text(str(p)+'\n')
port=None
for candidate in range(28610,28650):
 try:
  with socket.socket() as sock:sock.bind(('127.0.0.1',candidate));port=candidate;break
 except OSError:continue
assert port is not None
env={k:v for k,v in os.environ.items() if k in ('PATH','LANG','LC_ALL')};env.update(PDRS_BASELINE=str(root),HOME=str(h),PILOT_HOME=str(h),TMPDIR=str(d/'tmp'),PILOTDECK_GATEWAY_HOST='127.0.0.1',PILOTDECK_GATEWAY_PORT=str(port),PILOTDECK_GATEWAY_TOKEN='test-token',PILOTDECK_GATEWAY_TOKEN_PATH=str(h/'token'),PILOTDECK_SKIP_BROWSER_OPEN='1',DATABASE_PATH=str(h/'auth.db'),WORKSPACES_ROOT=str(d/'workspaces'),GP_PROJECT=str(p))
env['GIT_CEILING_DIRECTORIES']=str(d)
env['PATH']=str(args.node.resolve().parent)+':'+env.get('PATH','')
loader=str(root/'node_modules/tsx/dist/loader.mjs')
with (d/'server.stdout').open('wb') as out, (d/'server.stderr').open('wb') as err:
 binary=args.gateway;sidecar=args.sidecar
 if args.engine=="native":
  (d/'binary-sha256.txt').write_text(hashlib.sha256(binary.read_bytes()).hexdigest()+'  '+str(binary)+'\n'+hashlib.sha256(sidecar.read_bytes()).hexdigest()+'  '+str(sidecar)+'\n')
 command=([str(binary),'--native','--home',str(h),'--project',str(p),'--listen',f'127.0.0.1:{port}','--token-file',str(h/'token'),'--sidecar',str(sidecar),'--drain-ms','2000'] if args.engine=="native" else [node,'--import',loader,str(root/'src/cli/pilotdeck.ts'),'server'])
 proc=subprocess.Popen(command,env=env,cwd=p,stdout=out,stderr=err,start_new_session=True)
 try:
  end=time.monotonic()+30
  while time.monotonic()<end:
   assert proc.poll() is None,proc.returncode
   try:
    with socket.create_connection(('127.0.0.1',port),timeout=.2):break
   except OSError:time.sleep(.05)
  else:raise TimeoutError('server ready')
  result=subprocess.run([node,'--import',loader,str(pathlib.Path(__file__).parent/'driver.mts'),f'ws://127.0.0.1:{port}/ws',str(p),str(d/'driver')],env=env,cwd=p,timeout=70)
  (d/'provenance.json').write_text(json.dumps({'baseline':str(root),'head':head,'node':subprocess.check_output([node,'-p','JSON.stringify(process.versions)'],text=True),'clientExit':result.returncode,'serverCommand':command,'engine':args.engine,'sourceManifestSha256':args.expected_source_sha,'candidateManifestSha256':hashlib.sha256(args.candidate_manifest.read_bytes()).hexdigest(),'sourceSha256':hashlib.sha256((root/'src/cli/pilotdeck.ts').read_bytes()).hexdigest()},indent=2))
  assert result.returncode==0, 'business interaction driver failed'
  assert any(row.get('requestId') for row in rows), 'SOP service was not called'
  assert any('conversation summarizer' in json.dumps(row).lower() for row in rows), 'SOP summary not called'
  assert (d/'hook-input.jsonl').exists(), 'SOP hooks missing'
  wire=(d/'driver/wire.jsonl').read_text();assert 'compact_started' in wire and 'compact_completed' in wire, 'SOP compact notifications missing'
  hooks=[json.loads(line) for line in (d/'hook-input.jsonl').read_text().splitlines()];assert any(x.get('hookEventName')=='PreCompact' for x in hooks) and any(x.get('hookEventName')=='PostCompact' for x in hooks);assert all(x.get('sessionId')=='web:sop-fixture' for x in hooks), 'wrong lifecycle session'
 finally:
  (d/'model-requests.json').write_text(json.dumps(rows,indent=2));provider.shutdown();provider.server_close()
  if proc.poll() is None:proc.send_signal(signal.SIGTERM)
  try:proc.wait(timeout=15)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()

assert proc.returncode==0, ('server shutdown',proc.returncode)
with socket.socket() as check:check.settimeout(.5);assert check.connect_ex(('127.0.0.1',port))!=0, 'listener still open'
