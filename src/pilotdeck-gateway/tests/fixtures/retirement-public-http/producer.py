# anchor-selfcheck: unsupported isolated SOP process capture producer; no product source mutations
import os, subprocess, socket, time, json, pathlib, signal, hashlib
import argparse
parser=argparse.ArgumentParser()
parser.add_argument("--engine",choices=("ts","native"),required=True)
parser.add_argument("--head",required=True)
for field in ("baseline","out","candidate-manifest","source-manifest"):
 parser.add_argument("--"+field,type=pathlib.Path,required=True)
parser.add_argument("--expected-source-sha",required=True)
parser.add_argument("--node",type=pathlib.Path,required=True)
parser.add_argument("--gateway",type=pathlib.Path)
parser.add_argument("--sidecar",type=pathlib.Path)
parser.add_argument('--skills-disabled',action='store_true')
parser.add_argument('--approval-unconfigured',action='store_true')
args=parser.parse_args()
d=args.out.resolve();d.mkdir(mode=0o700,parents=True,exist_ok=False)
root=args.baseline.resolve();node=str(args.node.resolve())
versions=json.loads(subprocess.check_output([node,"-p","JSON.stringify(process.versions)"],text=True))
assert versions["node"]=="22.22.0" and versions["icu"]=="78.2"
assert hashlib.sha256(args.source_manifest.read_bytes()).hexdigest()==args.expected_source_sha and json.loads(args.source_manifest.read_text())['head']==args.head
candidate_rows=json.loads(args.candidate_manifest.read_text())
candidate_sha={r["path"]:r["sha256"] for r in candidate_rows}
if args.engine=="native":
 assert args.gateway and args.sidecar
 assert hashlib.sha256(args.gateway.read_bytes()).hexdigest()==candidate_sha["pilotdeck-gateway"]
 assert hashlib.sha256(args.sidecar.read_bytes()).hexdigest()==candidate_sha["pilotdeck-agent-loop-sidecar"]

head=subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip();assert head=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',head
(d/'driver').mkdir(exist_ok=False)
h=pathlib.Path('/app/home');p=pathlib.Path('/app/project');(p/'fixture.txt').write_text('BUSINESS_FILE_CONTENT_42\n')
plugin=p/'.pilotdeck/plugins/compact-observer';plugin.mkdir(parents=True);command='cat >> '+str(d/'hook-input.jsonl')+'; printf \"\\n\" >> '+str(d/'hook-input.jsonl')+'; printf \"{\\\"continue\\\":true}\"';(plugin/'plugin.json').write_text(json.dumps({'name':'compact-observer','hooks':{event:[{'matcher':'*','hooks':[{'type':'command','command':command}]}] for event in ['PreCompact','PostCompact']}}))
import http.server,threading
MAX_REQUESTS=64; MAX_BODY=2*1024*1024
rows=[];formal_rows=[];provider_wire=[];wire_lock=threading.Lock()
(d/'provider-wire.jsonl').touch(exist_ok=False)
class WireSink:
 def __init__(self,handler,target):self.handler=handler;self.target=target
 def write(self,data):
  self.handler.response_length+=len(data)
  if len(self.handler.response_bytes)<MAX_BODY:self.handler.response_bytes.extend(data[:MAX_BODY-len(self.handler.response_bytes)])
  return self.target.write(data)
 def __getattr__(self,name):return getattr(self.target,name)

class Provider(http.server.BaseHTTPRequestHandler):
 def setup(self):
  super().setup();self.request_bytes=b'';self.response_bytes=bytearray();self.response_length=0;self.wfile=WireSink(self,self.wfile)
 def finish(self):
  try:super().finish()
  finally:
   if getattr(self,'command',None):
    row={'method':self.command,'path':self.path,'headers':list(self.headers.raw_items()),'requestBodyHex':self.request_bytes.hex(),'responseWireHex':bytes(self.response_bytes).hex(),'responseTruncated':self.response_length>MAX_BODY}
    with wire_lock:
     provider_wire.append(row)
     with (d/'provider-wire.jsonl').open('a') as stream:stream.write(json.dumps(row)+'\n')
 def log_message(self,*args): pass
 def do_GET(self):
  auth=self.headers.get('Authorization','');formal_rows.append({'path':self.path,'bearerKind':'approver' if auth=='Bearer approve-only' else 'actor' if auth=='Bearer actor-only' else 'other'})
  if self.path=='/api/auth/me':
   value={'id':'approver' if auth=='Bearer approve-only' else 'actor','tenant_id':'tenant','source':'web','disabled':False,'role':'member'}
   if auth not in ('Bearer approve-only','Bearer actor-only'):value={'id':'foreign','tenant_id':'foreign','source':'web','disabled':False,'role':'member'}
  elif self.path=='/api/auth/me/api-credentials':
   value=[{'id':'credential','user_id':'actor','status':'active','access':'user_full_access','key_prefix':'fixture-discovery-ke'+'…','revoked_at':None,'expires_at':None}]
  elif self.path=='/api/v1/agents/target':value={'id':'target','status':'active','is_overall':False}
  else:value={'status':'ok','protocolVersion':'2.0','moduleId':'sop.runtime','contract':'sop.lifecycle/v2','operations':['prepare','submit'],'descriptorVersion':'1.0','implementationId':'fixture.owner','transport':'sop-http-v2'}
  raw=json.dumps(value).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
 def do_POST(self):
  length=int(self.headers['Content-Length']);assert 0<length<=MAX_BODY and len(rows)<MAX_REQUESTS
  self.request_bytes=self.rfile.read(length);body=json.loads(self.request_bytes); rows.append(body)
  if self.path=='/api/v1/agents/target/sops:route':
   raw=json.dumps({'decision':'selected','selected_sop_id':'fixture','candidate_sop_ids':['fixture']}).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw);return
  if self.path.startswith('/v1/sop/'):
   operation=self.path.rsplit('/',1)[-1];old=body['payload'].get('state',{});proposal=body['payload'].get('proposal',{})
   status='active' if operation=='prepare' else proposal['status']
   state={**old,'status':status,'active_skill_id':'fixture','active_step_id':'node'}
   if status=='handoff':state['awaiting_input_json']={'wait_id':'human-wait-42'}
   else:state.pop('awaiting_input_json',None)
   step={'skillId':'fixture','skillName':'Fixture','version':'v1','nodeId':'node','node':{'node_id':'node','assignee_user_id':'approver'},'instruction':'Request human approval via handoff. After authorized SOP_APPROVAL_DECISION_42, persist decision then complete.','expectedUserInfo':[],'knownSlots':state.get('slots_json',{}),'allowedNextStepIds':[],'requiredToolNames':[],'allowedActions':['handoff','completed'],'isTerminal':True,'declaresHandoff':True,'contextMode':'new_session'}
   result={'status':status,'replyFragment':'SOP_APPROVAL_WAIT' if status=='handoff' else 'SOP_APPROVAL_FINISHED','slotUpdates':proposal.get('slotUpdates',{}),'events':[]}
   state['slots_json']={**state.get('slots_json',{}),**proposal.get('slotUpdates',{})}
   payload={'state':state,**({'step':step} if operation=='prepare' else {'result':result})}
   raw=json.dumps({'protocolVersion':'2.0','requestId':body['requestId'],'ok':True,'outcome':'completed','payload':payload}).encode();self.send_response(200);self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw);return

  messages=body.get('messages',[]); content=json.dumps(messages)
  tool=None
  if body.get('stream') and 'conversation summarizer' not in json.dumps(body).lower() and not any(m.get('role')=='tool' for m in messages[1+max((i for i,m in enumerate(messages) if m.get('role')=='user'),default=-1):]):
   if 'SOP_TRIGGER' in content and any(t.get('function',{}).get('name')=='submit_step_result' for t in body.get('tools',[])):tool=('submit_step_result',{'status':'completed' if 'SOP_APPROVAL_DECISION_42' in json.dumps([m for m in messages if m.get('role')=='user']) else 'handoff','replyFragment':'SOP_APPROVAL_FINISHED' if 'SOP_APPROVAL_DECISION_42' in json.dumps([m for m in messages if m.get('role')=='user']) else 'SOP_APPROVAL_WAIT','slotUpdates':{'decision':'SOP_APPROVAL_DECISION_42'} if 'SOP_APPROVAL_DECISION_42' in json.dumps([m for m in messages if m.get('role')=='user']) else {}})
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
provider=http.server.ThreadingHTTPServer(('127.0.0.1',28690),Provider);threading.Thread(target=provider.serve_forever,daemon=True).start()
(h/'token').write_text('test-token\n');(h/'token').chmod(0o600)
context_tokens=64000
(h/'pilotdeck.yaml').write_text('schemaVersion: 1\nagent: {model: fixture/mock, maxOutputTokens: 256, maxContextTokens: 7000}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:MOCK_PORT/v1\n      apiKey: mock-only\n      models: {mock: {}}\ntelemetry: {enabled: false}\n'.replace('MOCK_PORT',str(provider.server_port)).replace('maxContextTokens: 7000','maxContextTokens: '+str(context_tokens)))
(p/'definitions.json').write_text(json.dumps({'sops':[{'id':'fixture','name':'Fixture','version':'v1','content':{'start_node_id':'node','nodes':[{'node_id':'node','instruction':'Complete the fixture.','allowed_actions':['handoff','completed'],'assignee_user_id':'approver'}]}}]}))
with (h/'pilotdeck.yaml').open('a') as f:f.write('modules: '+json.dumps({'sop':{'enabled':True,'implementationId':'fixture.owner','contract':'sop.lifecycle/v2','transport':'sop-http-v2','endpoint':'http://127.0.0.1:'+str(provider.server_port),'manifestPath':'/healthz','definitionsPath':str(p/'definitions.json'),'defaultSopId':'fixture','timeoutMs':30000,'stateRoot':str(h/'sop-state'),'discoveryEndpoint':'http://127.0.0.1:'+str(provider.server_port)+'/api/v1','discoveryAgentId':'target','discoveryApiKey':'fixture-discovery-key-0123456789'}})+'\n')
(h/'projects/fixture').mkdir(parents=True);(h/'projects/fixture/.cwd').write_text(str(p)+'\n')
import sqlite3
db=sqlite3.connect(h/'auth.db');db.execute('CREATE TABLE users(id TEXT PRIMARY KEY,is_active INTEGER)');db.execute('INSERT INTO users VALUES(?,1)',('pilot-owner',));db.commit();db.close()
(h/'staffdeck/binding').mkdir(parents=True)
(h/'staffdeck/binding/sops.json').write_bytes((p/'definitions.json').read_bytes())
(h/'staffdeck/project-bindings.json').write_text(json.dumps({'version':1,'installationId':'fixture-install','projects':[{'projectRoot':str(p),'tenantId':'tenant','origin':'http://127.0.0.1:'+str(provider.server_port),'agentId':'target','status':'bound','bindingId':'binding'}]}))
if not args.approval_unconfigured:
 seed=json.loads((pathlib.Path(__file__).parent/'approval-seed.json').read_text())
 seed['bundle']=json.loads((p/'definitions.json').read_text())
 state=h/'staffdeck/binding/state/sessions';state.mkdir(parents=True,exist_ok=True)
 (state/'d2ViOnNvcC1maXh0dXJl.json').write_text(json.dumps(seed))

port=28610
env={k:v for k,v in os.environ.items() if k in ('PATH','LANG','LC_ALL','LD_LIBRARY_PATH')};env.update(PDRS_BASELINE=str(root),HOME=str(h),PILOT_HOME=str(h),TMPDIR='/app/tmp',PILOTDECK_GATEWAY_HOST='127.0.0.1',PILOTDECK_GATEWAY_PORT=str(port),PILOTDECK_GATEWAY_TOKEN='test-token',PILOTDECK_GATEWAY_TOKEN_PATH=str(h/'token'),PILOTDECK_SKIP_BROWSER_OPEN='1',DATABASE_PATH=str(h/'auth.db'),WORKSPACES_ROOT='/app/workspaces',GP_PROJECT=str(p))
env.update(STAFFDECK_SOP_RUNTIME_ORIGIN='http://127.0.0.1:'+str(provider.server_port),STAFFDECK_PROJECT_BINDING_ENABLED='true',STAFFDECK_FORMAL_API_ORIGIN='http://127.0.0.1:'+str(provider.server_port),STAFFDECK_APPROVAL_USER_ID='approver',STAFFDECK_COPY_TENANT_ID='tenant',STAFFDECK_COPY_TARGET_AGENT_ID='target',STAFFDECK_COPY_PILOTDECK_USER_ID='pilot-owner',STAFFDECK_COPY_ACTOR_USER_ID='actor',STAFFDECK_SOP_MANAGEMENT_CREDENTIAL_ID='credential',STAFFDECK_COPY_USER_TOKEN='actor-only',STAFFDECK_SOP_MANAGEMENT_API_KEY='fixture-discovery-key-0123456789')
env['GIT_CEILING_DIRECTORIES']=str(d)
# Explicit controlled profile: no third-party MCP tools.
with (h/'pilotdeck.yaml').open('a') as f:f.write('extension: {builtinPluginsEnabled: {browser-use: false, funasr: false, windows-skills: false}}\n')
env['PATH']=str(args.node.resolve().parent)+':'+env.get('PATH','')
if args.skills_disabled:
 config=(h/'pilotdeck.yaml').read_text();lines=[]
 for line in config.splitlines():
  if line.startswith('modules: '):
   binding=json.loads(line[9:]);binding['skills']={'enabled':False};line='modules: '+json.dumps(binding)
  lines.append(line)
 (h/'pilotdeck.yaml').write_text('\n'.join(lines)+'\n')
if args.approval_unconfigured:
 for key in list(env):
  if key.startswith('STAFFDECK_'):del env[key]
loader=str(root/'node_modules/tsx/dist/loader.mjs')
selected_driver=pathlib.Path(__file__).parent/('driver-unconfigured.mts' if args.approval_unconfigured else 'driver-disabled.mts' if args.skills_disabled else 'driver-gold.mts')
binary=args.gateway;sidecar=args.sidecar
if args.engine=='native':
 (d/'binary-sha256.txt').write_text(hashlib.sha256(binary.read_bytes()).hexdigest()+'  '+str(binary)+'\n'+hashlib.sha256(sidecar.read_bytes()).hexdigest()+'  '+str(sidecar)+'\n')
command=([str(binary),'--native','--home',str(h),'--project',str(p),'--listen',f'127.0.0.1:{port}','--token-file',str(h/'token'),'--sidecar',str(sidecar),'--drain-ms','2000'] if args.engine=='native' else [node,'--import',loader,str(root/'src/cli/pilotdeck.ts'),'server'])
phases=[]
observer=pathlib.Path(__file__).parent.parent/'native-channel-production/owned-observer.mjs'
launch_helper=pathlib.Path(__file__).parent/'launch-owned.mjs'
observations=d/'observation';observations.mkdir()
env.update(PDRS_OWNED_PROCESS_DIR=str(observations),PDRS_EXPECTED_EXECUTABLE=command[0])
try:
 for phase in ['pause']:
  driver=d/('driver-'+phase);driver.mkdir()
  with (d/(phase+'-server.stdout')).open('xb') as out,(d/(phase+'-server.stderr')).open('xb') as err:
   start_file=d/'driver-start.json';done_file=d/'driver-exit.json'
   launch=d/'launch-owned.json';launch.write_text(json.dumps({'command':command,'driver':str(selected_driver),'driverArgv':[f'ws://127.0.0.1:{port}/ws',str(p),str(driver),phase,str(d/'driver-pause/result.json')],'startFile':str(start_file),'doneFile':str(done_file)}))
   proc=subprocess.Popen([node,'--import',loader,'--import',str(observer),str(launch_helper),str(launch)],env=env,cwd=p,stdout=out,stderr=err,start_new_session=True)
   phases.append({'phase':phase,'pid':proc.pid,'driverExit':None})
   try:
    end=time.monotonic()+30
    while time.monotonic()<end:
     assert proc.poll() is None,proc.returncode
     try:
      with socket.create_connection(('127.0.0.1',port),timeout=.2):break
     except OSError:time.sleep(.05)
    else:raise TimeoutError('server ready '+phase)
    start_file.write_text('{}')
    deadline=time.monotonic()+70
    while not done_file.exists():
     assert proc.poll() is None,proc.returncode
     if time.monotonic()>deadline:raise TimeoutError('public HTTP driver deadline')
     time.sleep(.025)
    result=json.loads(done_file.read_text());phases[-1]['driverExit']=result['code']
    assert result['code']==0,('public HTTP driver failed',phase,result)
   finally:
    if proc.poll() is None:proc.send_signal(signal.SIGTERM)
    try:proc.wait(timeout=15)
    except subprocess.TimeoutExpired:proc.kill();proc.wait()
    phases[-1]['serverExit']=proc.returncode
   assert proc.returncode==0,('server shutdown',phase,proc.returncode)
   with socket.socket() as check:check.settimeout(.5);assert check.connect_ex(('127.0.0.1',port))!=0,('listener still open',phase)
finally:
 (d/'model-requests.json').write_text(json.dumps(rows,indent=2))
 (d/'formal-requests.json').write_text(json.dumps(formal_rows,indent=2))
 (d/'provenance.json').write_text(json.dumps({'baseline':str(root),'head':head,'nativeHead':args.head,'node':versions['node'],'icu':versions['icu'],'serverCommand':command,'engine':args.engine,'sourceManifestSha256':args.expected_source_sha,'candidateManifestSha256':hashlib.sha256(args.candidate_manifest.read_bytes()).hexdigest(),'phases':phases,'producerSha256':hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),'driverSha256':hashlib.sha256(selected_driver.read_bytes()).hexdigest(),'maxRequests':MAX_REQUESTS,'maxOutputTokens':256},indent=2))
 provider.shutdown();provider.server_close()
assert len(phases)==1 and phases[0]['driverExit']==0
