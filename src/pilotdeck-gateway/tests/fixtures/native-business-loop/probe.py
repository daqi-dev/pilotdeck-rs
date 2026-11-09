#!/usr/bin/env python3
# anchor-selfcheck: unsupported Native business integration probe; no source mutation table.
"""Unfrozen product diagnostic. TS official client to both real WS hosts; no Cargo."""
import argparse,hashlib,shutil,http.server,importlib.util,json,os,signal,socket,subprocess,sys,threading,time
from pathlib import Path
p=argparse.ArgumentParser()
for k in ('baseline','gateway','sidecar','out'):p.add_argument('--'+k,type=Path,required=True)
p.add_argument('--provenance-label',required=True);a=p.parse_args();sys.dont_write_bytecode=True
for k in ('baseline','gateway','sidecar','out'):setattr(a,k,getattr(a,k).resolve())
assert a.out!=Path.home();a.out.mkdir(mode=0o700,parents=True,exist_ok=False)
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[4]
spec=importlib.util.spec_from_file_location('business_common',ROOT/'scripts/gateway-parity/common.py');common=importlib.util.module_from_spec(spec);spec.loader.exec_module(common)
MAX_REQUESTS=24;MAX_BODY=1024*1024;MAX_OUTPUT_TOKENS=256;RUN_SECONDS=240
start=time.monotonic();wire=[];children=[];identities={};stop=threading.Event();errors=[];provider=None;success=False;host=None;monitor_thread=None
sha=lambda p:hashlib.sha256(p.read_bytes()).hexdigest()
def save(p,v):p.write_text(json.dumps(v,indent=2,ensure_ascii=False)+'\n')
def ps():
 r=subprocess.run(['/bin/ps','-axo','pid=,ppid=,lstart='],capture_output=True,text=True,timeout=3,check=True)
 return {int(v[0]):(int(v[1]),v[2]) for row in r.stdout.splitlines() if len(v:=row.strip().split(None,2))==3}
def scan():
 table=ps();owned={p.pid for p in children if p.poll() is None}
 while True:
  more={pid for pid,(parent,born) in table.items() if parent in owned}
  if more<=owned:break
  owned|=more
 for pid in owned:
  if pid in table:identities.setdefault((pid,table[pid][1]),table[pid][0])
def monitor():
 while not stop.wait(.15):
  try:scan()
  except Exception as e:errors.append('monitor '+str(e));return
def spawn(argv,env,out):
 stdout=(out/'.stdout.log').open('ab');stderr=(out/'.stderr.log').open('ab')
 child=subprocess.Popen([str(x) for x in argv],env=env,cwd=project,stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr,start_new_session=True)
 stdout.close();stderr.close();children.append(child);scan();return child
def wait_ready(child,port):
 end=time.monotonic()+25
 while time.monotonic()<end:
  assert child.poll() is None,('host exited',child.returncode)
  try:
   with socket.create_connection(('127.0.0.1',port),timeout=.2):return
  except OSError:time.sleep(.05)
 raise TimeoutError('ready')
def halt(child):
 scan()
 if child.poll() is None:child.send_signal(signal.SIGTERM)
 assert child.wait(timeout=20)==0,('host shutdown',child.returncode)
 # Descendant observations use (pid,birth); a reused number is not ours.
 end=time.monotonic()+5
 while time.monotonic()<end:
  table=ps();left=[pid for (pid,born) in identities if pid in table and table[pid][1]==born and pid!=os.getpid()]
  if not left:return
  time.sleep(.1)
 raise AssertionError(('owned identities survive halt',left))
def interrupted(signum,frame):
 for n in (signal.SIGINT,signal.SIGTERM,signal.SIGHUP):signal.signal(n,signal.SIG_IGN)
 raise InterruptedError('signal '+str(signum))
for n in (signal.SIGINT,signal.SIGTERM,signal.SIGHUP,signal.SIGALRM):signal.signal(n,interrupted)
signal.alarm(RUN_SECONDS)
class Provider(http.server.BaseHTTPRequestHandler):
 protocol_version='HTTP/1.1'
 def log_message(self,*a):pass
 def do_POST(self):
  try:
   self.connection.settimeout(5);n=int(self.headers.get('Content-Length','0'));assert 0<n<=MAX_BODY
   body=json.loads(self.rfile.read(n));assert len(wire)<MAX_REQUESTS
   messages=body.get('messages',[]);text=json.dumps(messages);done=any(m.get('role')=='tool' for m in messages)
   tool=None
   if body.get('stream') and not done:
    if 'BUSINESS_READ' in text:tool=('read_file',{'file_path':str(project/'fixture.txt')})
    elif 'BUSINESS_PERMISSION' in text:tool=('bash',{'command':'touch '+str(project/'DENIED_SIDE_EFFECT')})
   if done and 'BUSINESS_READ' in text:assert 'BUSINESS_FILE_CONTENT_42' in json.dumps([m for m in messages if m.get('role')=='tool']),'read data did not reach provider'
   content='BUSINESS_FILE_CONTENT_42' if 'BUSINESS_READ' in text else 'BUSINESS_DENY_DONE'
   if not body.get('stream'):content='{"title":"Business fixture"}'
   message={'role':'assistant','content':content}
   if tool:message={'role':'assistant','content':None,'tool_calls':[{'id':'business-call','type':'function','function':{'name':tool[0],'arguments':json.dumps(tool[1])}}]}
   wire.append({'side':side,'request':body,'response':message});self.send_response(200)
   if body.get('stream'):
    self.send_header('Content-Type','text/event-stream');self.send_header('Connection','close');self.end_headers()
    delta={'role':'assistant','tool_calls':[{'index':0,**message['tool_calls'][0]}]} if tool else {'role':'assistant','content':content}
    for row in [{'choices':[{'index':0,'delta':delta,'finish_reason':None}]},{'choices':[{'index':0,'delta':{},'finish_reason':'tool_calls' if tool else 'stop'}],'usage':{'prompt_tokens':10,'completion_tokens':5,'total_tokens':15}}]:self.wfile.write(('data: '+json.dumps(row)+'\n\n').encode())
    self.wfile.write(b'data: [DONE]\n\n');self.close_connection=True
   else:
    raw=json.dumps({'choices':[{'index':0,'message':message,'finish_reason':'stop'}]}).encode();self.send_header('Content-Type','application/json');self.send_header('Content-Length',str(len(raw)));self.end_headers();self.wfile.write(raw)
   self.wfile.flush()
  except Exception as e:errors.append('provider '+str(e));self.close_connection=True
home=a.out/'home';project=a.out/'project';tmp=a.out/'tmp'
for d in (home,project,tmp):d.mkdir()
(project/'fixture.txt').write_text('BUSINESS_FILE_CONTENT_42\n')
# Same absolute home/project and fixture across sequential TS and Rust rounds.
def env_for(port):
 env={k:v for k,v in os.environ.items() if k in ('PATH','LANG','LC_ALL')};env.update(HOME=str(home),PILOT_HOME=str(home),TMPDIR=str(tmp),PDRS_BASELINE=str(a.baseline),GP_PROJECT=str(project),PILOTDECK_GATEWAY_HOST='127.0.0.1',PILOTDECK_GATEWAY_PORT=str(port),PILOTDECK_GATEWAY_TOKEN='business-owned-token',PILOTDECK_GATEWAY_TOKEN_PATH=str(home/'token'),PILOTDECK_SKIP_BROWSER_OPEN='1',DATABASE_PATH=str(home/'auth.db'),WORKSPACES_ROOT=str(a.out/'workspaces'));return env
def config(model_port):
 (home/'token').write_text('business-owned-token\n');(home/'token').chmod(0o600)
 (home/'permissions.json').write_text(json.dumps({'version':1,'allowedTools':[],'askTools':['bash'],'disallowedTools':[],'skipPermissions':False}))
 (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock, maxOutputTokens: {MAX_OUTPUT_TOKENS}}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{model_port}/v1\n      apiKey: mock-only\n      models: {{mock: {{}}}}\ntelemetry: {{enabled: false}}\n')
 reg=home/'projects/business-fixture';reg.mkdir(parents=True);(reg/'.cwd').write_text(str(project)+'\n')
checks=[];binary_before={str(x):sha(x) for x in (a.gateway,a.sidecar)}
assert subprocess.check_output(['git','-C',str(a.baseline),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85','wrong baseline'
(a.out/'binaries').mkdir();binary_sources={}
for key in ('gateway','sidecar'):
 source=getattr(a,key);target=a.out/'binaries'/key
 with source.open('rb') as src,target.open('xb') as dst:shutil.copyfileobj(src,dst)
 assert sha(target)==binary_before[str(source)]==sha(source),'source changed during snapshot'
 target.chmod(0o700);binary_sources[key]={'source':str(source),'snapshot':str(target),'sha256':sha(target)};setattr(a,key,target)
save(a.out/'provenance.json',{'binaries':binary_sources,'label':a.provenance_label,'nodeEnvironment':json.loads(subprocess.check_output(['node','-p','JSON.stringify({execPath:process.execPath,versions:process.versions})'],text=True)),'formalEnvironmentMatched':False,'probeSha256':sha(Path(__file__)),'driverSha256':sha(HERE/'driver.mts'),'commonSha256':sha(ROOT/'scripts/gateway-parity/common.py'),'tsDistSha256':sha(a.baseline/'dist/src/cli/pilotdeck.js'),'tsGatewaySourceSha256':sha(a.baseline/'src/gateway/server/GatewayServer.ts')})
lease=None;lease_acquired=False;alignment=None
try:
 lease=common.parity_lock(timeout=1);lease.__enter__();lease_acquired=True
 if True:
  provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);provider.daemon_threads=True;threading.Thread(target=provider.serve_forever,daemon=True).start();monitor_thread=threading.Thread(target=monitor,daemon=True);monitor_thread.start()
  for side in ('ts','rust'):
   config(provider.server_port);rounddir=a.out/side;rounddir.mkdir();port=None
   for candidate in range(28610,28650):
    try:
     with socket.socket() as test:test.bind(('127.0.0.1',candidate));port=candidate;break
    except OSError:continue
   assert port;env=env_for(port)
   argv=['node',str(a.baseline/'dist/src/cli/pilotdeck.js'),'server'] if side=='ts' else [a.gateway,'--native','--home',home,'--project',project,'--listen',f'127.0.0.1:{port}','--token-file',home/'token','--sidecar',a.sidecar,'--drain-ms','2000']
   host=spawn(argv,env,rounddir);wait_ready(host,port)
   for phase in ('warm','cold'):
    out=rounddir/phase;out.mkdir()
    cmd=['node','--import',str(a.baseline/'node_modules/tsx/dist/loader.mjs'),str(HERE/'driver.mts'),'--url',f'ws://127.0.0.1:{port}/ws','--token','business-owned-token','--project',str(project),'--file',str(project/'fixture.txt'),'--out',str(out),'--phase',phase]
    driver=spawn(cmd,env,out);assert driver.wait(timeout=70)==0,('driver failed',side,phase)
    assert not (project/'DENIED_SIDE_EFFECT').exists(),'denied command executed'
    if phase=='warm':
     halt(host);before={str(p):sha(p) for p in home.rglob('*.jsonl')};assert before,'no transcript';host=spawn(argv,env,rounddir);wait_ready(host,port)
    else:
     after={str(p):sha(p) for p in home.rglob('*.jsonl')};assert before==after,'cold read changed transcript'
     warm=json.loads((rounddir/'warm/result.json').read_text());cold=json.loads((out/'result.json').read_text());assert warm['history']['messages']==cold['history']['messages'],'cold history differs'
   halt(host);host=None;checks.append({'side':side,'pass':True,'coldHistoryReadOnly':True})
   home.rename(a.out/(side+'-home'));home.mkdir()
   assert time.monotonic()-start<RUN_SECONDS
  assert not errors,errors;assert all(sha(Path(p))==h for p,h in binary_before.items());success=True
  # Compare factual protocol projections without replacing the frozen ruler.
  projected={}
  for label in ('ts','rust'):
   result=json.loads((a.out/label/'warm/result.json').read_text())
   projected[label]={}
   for case,events in [('read',result['read']),('permission',result['permission']['events'])]:
    projected[label][case]={'eventTypes':[e.get('type') for e in events],'tools':[{'toolName':e.get('toolName'),'ok':e.get('ok'),'errorCode':e.get('errorCode')} for e in events if e.get('type')=='tool_call_finished'],'terminal':[{'type':e.get('type'),'finishReason':e.get('finishReason')} for e in events if e.get('type')=='turn_completed'],'deniedEvents':[{k:v for k,v in e.items() if k!='runId'} for e in events if e.get('type')=='permission_denied']}
  differences=[{'case':case,'field':field,'ts':projected['ts'][case][field],'rust':projected['rust'][case][field]} for case in ('read','permission') for field in ('eventTypes','tools','terminal','deniedEvents') if projected['ts'][case][field]!=projected['rust'][case][field]]
  alignment={'pass':not differences,'scope':'Exact event type sequence, tool result code and terminal projection only; raw wire retained. Not full frame/disk parity.','differences':differences,'projection':projected};save(a.out/'comparison.json',alignment)
finally:
 signal.alarm(0);stop.set()
 if monitor_thread:monitor_thread.join(timeout=4)
 # Only identities we actually observed and can still match are signalled.
 for child in children:
  if child.poll() is None:
   child.send_signal(signal.SIGTERM)
   try:child.wait(timeout=5)
   except subprocess.TimeoutExpired:child.kill();child.wait(timeout=5)
 table=ps()
 for (pid,born) in identities:
  if pid in table and table[pid][1]==born:
   try:os.kill(pid,signal.SIGTERM)
   except ProcessLookupError:pass
 if provider:provider.shutdown();provider.server_close()
 table=ps();left=[{'pid':pid,'birth':born} for pid,born in identities if pid in table and table[pid][1]==born]
 save(a.out/'provider.json',wire);save(a.out/'result.json',{'pass':success and not left and not errors and bool(alignment and alignment['pass']),'executionPass':success and not left and not errors,'alignmentPass':alignment['pass'] if alignment else None,'checks':checks,'errors':errors,'remainingOwnedIdentities':left,'provenanceLabel':a.provenance_label,'binarySha256':binary_before,'baseline':str(a.baseline),'baselineHead':subprocess.check_output(['git','-C',str(a.baseline),'rev-parse','HEAD'],text=True).strip(),'requests':len(wire),'requestLimit':MAX_REQUESTS,'maxOutputTokens':MAX_OUTPUT_TOKENS,'elapsedSeconds':time.monotonic()-start,'notFormalRetirement':True})

 if lease_acquired:
  common.LIVE_GROUPS.update(row['pid'] for row in left)
  lease.__exit__(None,None,None)
if not success or left or errors or not alignment or not alignment['pass']:raise SystemExit(1)
