# anchor-selfcheck: unsupported isolated native process capture producer; no product source mutations
import os, subprocess, socket, time, json, pathlib, signal, hashlib
d=pathlib.Path(__file__).parent/'cur';d.mkdir(exist_ok=False); root=pathlib.Path(os.environ['PDRS_BASELINE']);node=os.environ['PDRS_NODE']
head=subprocess.check_output(['git','-C',str(root),'rev-parse','HEAD'],text=True).strip();assert head=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',head
for n in ['home','project','tmp','workspaces','driver']:(d/n).mkdir(exist_ok=False)
h=d/'home';p=d/'project';(p/'fixture.txt').write_text('BUSINESS_FILE_CONTENT_42\n')
import http.server,threading
rows=[]
class Provider(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_POST(self):
  body=json.loads(self.rfile.read(int(self.headers['Content-Length']))); rows.append(body)
  messages=body.get('messages',[]); content=json.dumps(messages)
  tool=None
  if body.get('stream') and not any(m.get('role')=='tool' for m in messages):
   if 'BUSINESS_READ' in content:tool=('read_file',{'file_path':str(p/'fixture.txt')})
   elif 'BUSINESS_PERMISSION' in content:tool=('bash',{'command':'touch '+str(p/'DENIED_SIDE_EFFECT')})
  message={'role':'assistant','content': 'BUSINESS_FILE_CONTENT_42' if 'BUSINESS_READ' in content else 'BUSINESS_DENY_DONE'}
  if tool:message={'role':'assistant','content':None,'tool_calls':[{'id':'business-call','type':'function','function':{'name':tool[0],'arguments':json.dumps(tool[1])}}]}
  self.send_response(200)
  if body.get('stream'):
   self.send_header('Content-Type','text/event-stream');self.send_header('Connection','close');self.end_headers()
   delta={'role':'assistant','tool_calls':[{'index':0,**message['tool_calls'][0]}]} if tool else {'role':'assistant','content':message['content']}
   for item in [{'choices':[{'index':0,'delta':delta,'finish_reason':None}]},{'choices':[{'index':0,'delta':{},'finish_reason':'tool_calls' if tool else 'stop'}],'usage':{'prompt_tokens':10,'completion_tokens':5,'total_tokens':15}}]:self.wfile.write(('data: '+json.dumps(item)+'\n\n').encode())
   self.wfile.write(b'data: [DONE]\n\n');self.close_connection=True
  else:
   data=json.dumps({'choices':[{'index':0,'message':message,'finish_reason':'stop'}]}).encode();self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(data)));self.end_headers();self.wfile.write(data)
  self.wfile.flush()
provider=http.server.ThreadingHTTPServer(('127.0.0.1',28680),Provider);threading.Thread(target=provider.serve_forever,daemon=True).start()
(h/'token').write_text('test-token\n');(h/'token').chmod(0o600)
(h/'pilotdeck.yaml').write_text('schemaVersion: 1\nagent: {model: fixture/mock, maxOutputTokens: 256}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:MOCK_PORT/v1\n      apiKey: mock-only\n      models: {mock: {}}\ntelemetry: {enabled: false}\n'.replace('MOCK_PORT',str(provider.server_port)))
with (h/'pilotdeck.yaml').open('a') as f:f.write('extension: '+json.dumps({'builtinPluginsEnabled':{'funasr':False,'windows-skills':False,'browser-use':False}})+'\n')
with (h/'pilotdeck.yaml').open('a') as f:f.write('alwaysOn: '+json.dumps({'trigger':{'tickIntervalMinutes':0.02},'projects':{str(p):{'enabled':True}}})+'\n')
(h/'projects/fixture').mkdir(parents=True);(h/'projects/fixture/.cwd').write_text(str(p)+'\n')
port=None
for candidate in range(28610,28650):
 try:
  with socket.socket() as sock:sock.bind(('127.0.0.1',candidate));port=candidate;break
 except OSError:continue
assert port is not None
env={k:v for k,v in os.environ.items() if k in ('PATH','LANG','LC_ALL')};env.update(PDRS_BASELINE=str(root),HOME=str(h),PILOT_HOME=str(h),TMPDIR=str(d/'tmp'),PILOTDECK_GATEWAY_HOST='127.0.0.1',PILOTDECK_GATEWAY_PORT=str(port),PILOTDECK_GATEWAY_TOKEN='test-token',PILOTDECK_GATEWAY_TOKEN_PATH=str(h/'token'),PILOTDECK_SKIP_BROWSER_OPEN='1',DATABASE_PATH=str(h/'auth.db'),WORKSPACES_ROOT=str(d/'workspaces'),GP_PROJECT=str(p))
env['GIT_CEILING_DIRECTORIES']=str(d)
env['PILOTDECK_BUNDLED_SKILLS_DIR']=str(root/'skills')
gitcheck=subprocess.run(['git','-C',str(p),'rev-parse','--show-toplevel'],env=env,capture_output=True,text=True)
(d/'git-preflight.json').write_text(json.dumps({'exit':gitcheck.returncode,'stdout':gitcheck.stdout,'stderr':gitcheck.stderr,'ceiling':str(d)}))
assert gitcheck.returncode!=0,'isolated project must not discover parent git'
subprocess.run(['git','init',str(p)],env=env,check=True,capture_output=True)
subprocess.run(['git','-C',str(p),'config','user.name','Fixture'],env=env,check=True)
subprocess.run(['git','-C',str(p),'config','user.email','fixture@example.invalid'],env=env,check=True)
assert subprocess.check_output(['git','-C',str(p),'rev-parse','--show-toplevel'],env=env,text=True).strip()==str(p)
subprocess.run(['git','-C',str(p),'-c','user.name=Fixture','-c','user.email=fixture@example.invalid','commit','--allow-empty','-m','fixture'],env=env,check=True,capture_output=True)
loader=str(root/'node_modules/tsx/dist/loader.mjs')
seed=subprocess.run([node,'--import',loader,str(d.parent/'ao-seed-tool.mts'),str(root),str(h),str(p)],env=env,cwd=p,capture_output=True,text=True,timeout=20)
(d/'seed.stdout').write_text(seed.stdout);(d/'seed.stderr').write_text(seed.stderr);assert seed.returncode==0
with (d/'server.stdout').open('wb') as out, (d/'server.stderr').open('wb') as err:
 binary=os.environ['PDRS_GATEWAY_BINARY']
 sidecar=os.environ['PDRS_SIDECAR_BINARY']
 assert hashlib.sha256(pathlib.Path(sidecar).read_bytes()).hexdigest()==os.environ['PDRS_SIDECAR_SHA256']
 assert hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest()==os.environ['PDRS_GATEWAY_SHA256']
 proc=subprocess.Popen([binary,'--native','--home',str(h),'--project',str(p),'--listen',f'127.0.0.1:{port}','--token-file',str(h/'token'),'--sidecar',sidecar,'--drain-ms','2000'],env=env,cwd=p,stdout=out,stderr=err,start_new_session=True)
 try:
  end=time.monotonic()+30
  while time.monotonic()<end:
   assert proc.poll() is None,proc.returncode
   try:
    with socket.create_connection(('127.0.0.1',port),timeout=.2):break
   except OSError:time.sleep(.05)
  else:raise TimeoutError('server ready')
  proxy=subprocess.Popen(['/opt/homebrew/bin/python3.14',str(d.parent/'wire-proxy-formal-shape.py'),'--listen','28681','--upstream',str(port),'--out',str(d/'wire-raw.jsonl')],env=env,cwd=p,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
  try:
   deadline=time.monotonic()+5
   while time.monotonic()<deadline:
    assert proxy.poll() is None
    try:
     with socket.create_connection(('127.0.0.1',28681),timeout=.1):break
    except OSError:time.sleep(.02)
   else:raise TimeoutError('capture proxy')
   result=subprocess.run([node,'--import',str(d.parent/'ws-fixed-mask.mjs'),'--import',loader,str(d.parent/'ao-driver.mts'),f'ws://127.0.0.1:{28681}/ws',str(p),str(d/'driver')],env=env,cwd=p,timeout=90)
  finally:
   if proxy.poll() is None:proxy.send_signal(signal.SIGTERM)
   proxy.wait(timeout=10)

  (d/'provenance.json').write_text(json.dumps({'baseline':str(root),'head':head,'node':subprocess.check_output([node,'-p','JSON.stringify(process.versions)'],text=True),'clientExit':result.returncode,'serverCommand':binary,'rustGatewaySha256':hashlib.sha256(pathlib.Path(binary).read_bytes()).hexdigest(),'sourceSha256':hashlib.sha256((root/'src/cli/pilotdeck.ts').read_bytes()).hexdigest()},indent=2))
  assert result.returncode==0, 'business interaction driver failed'
  stages = {
      "discovery": "autonomous Always-On discovery",
      "workspace": "preparing an isolated workspace",
      "execution": "executing an Always-On discovery plan",
      "report": "writing a work report",
  }
  missing=[name for name, marker in stages.items() if not any(marker in json.dumps(row) for row in rows)]
  assert not missing, f"missing actual model phases: {missing}"
  assert any('BUSINESS_READ' in json.dumps(row) for row in rows), 'Always-On did not reach model'
  assert any(any(m.get('role')=='tool' for m in row.get('messages',[])) for row in rows), 'tool result never reached model'
 finally:
  (d/'model-requests.json').write_text(json.dumps(rows,indent=2));provider.shutdown();provider.server_close()
  if proc.poll() is None:proc.send_signal(signal.SIGTERM)
  try:proc.wait(timeout=15)
  except subprocess.TimeoutExpired:proc.kill();proc.wait()
