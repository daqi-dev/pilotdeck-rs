"""Real public CLI Server HTTP / WS / periodic Always-On / Cron collector, mock only.
Run only inside the validated network-none namespace. No behavioral scoring replacement.
"""
import argparse,hashlib,http.server,importlib.util,io,json,os,signal,socket,subprocess,sys,threading,time
from pathlib import Path
from http.client import HTTPResponse
sys.dont_write_bytecode=True
p=argparse.ArgumentParser()
p.add_argument('--engine',choices=('ts','native'),required=True);p.add_argument('--mode',choices=('http','cron','alwayson','history'),required=True)
for key in ('baseline','node','cli','sidecar','source-manifest','candidate-manifest','out'):p.add_argument('--'+key,type=Path,required=True)
a=p.parse_args();a.out=a.out.resolve();a.out.mkdir(parents=True,exist_ok=False);HERE=Path(__file__).resolve().parent
MAX_REQUESTS=32;MAX_BODY=2*1024*1024;MAX_OUTPUT_TOKENS=256
root=HERE.parents[4];runner_path=root/'scripts/gateway-parity/run_round.py'
spec=importlib.util.spec_from_file_location('retirement_product_owned_runner',runner_path)
runner=importlib.util.module_from_spec(spec);sys.modules[spec.name]=runner
runner_source=runner_path.read_bytes();exec(compile(runner_source,str(runner_path),'exec'),runner.__dict__)
runner._apparatus_source=runner_source
runner._verify_cli_imports()
common=runner.common;owned=[];lease=common.parity_lock(timeout=1);leased=False;listener=None
assert socket.if_nameindex()==[(1,'lo')], 'production entry collector requires isolated loopback'
sha=lambda x:hashlib.sha256(Path(x).read_bytes()).hexdigest()
manifest=json.loads(a.candidate_manifest.read_text());binaries={r['path']:r['sha256']for r in manifest}
assert sha(a.cli)==binaries['pilotdeck'];assert sha(a.sidecar)==binaries['pilotdeck-agent-loop-sidecar']
assert subprocess.check_output(['git','-C',str(a.baseline),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
versions=json.loads(subprocess.check_output([str(a.node),'-p','JSON.stringify(process.versions)'],text=True));assert versions['node']=='22.22.0' and versions['icu']=='78.2'
start=time.monotonic();requests=[];errors=[];child=None;success=False;server=None;http_rows=[];background=None
home=a.out/'home';project=a.out/'project';tmp=a.out/'tmp'
for d in (home,project,tmp):d.mkdir()
(project/'note.txt').write_text('ENTRY_NOTE_42\n');(project/'ui/dist').mkdir(parents=True);(project/'ui/dist/acceptance.js').write_text('ENTRY_STATIC_42\n');(project/'ui/dist/index.html').write_text('<html>ENTRY_INDEX_42</html>\n')
registry=home/'projects/entry-project';registry.mkdir(parents=True);(registry/'.cwd').write_text(str(project)+'\n')
token='entry-owned-token';(home/'token').write_text(token+'\n');(home/'permissions.json').write_text(json.dumps({'version':1,'allowedTools':[],'askTools':[],'disallowedTools':[],'skipPermissions':True}))
class Provider(http.server.BaseHTTPRequestHandler):
 def log_message(self,*args):pass
 def do_POST(self):
  try:
   self.connection.settimeout(5);length=int(self.headers.get('content-length','0'));assert 0<length<=MAX_BODY and len(requests)<MAX_REQUESTS
   body=json.loads(self.rfile.read(length));requests.append({'path':self.path,'body':body});text='GP_MOCK_OK' if a.mode=='history' else 'ENTRY_RUNTIME_REPLY_'+a.mode;self.send_response(200)
   if body.get('stream'):
    self.send_header('content-type','text/event-stream');self.send_header('connection','close');self.end_headers()
    rows=[{'choices':[{'index':0,'delta':{'role':'assistant','content':text},'finish_reason':None}]},{'choices':[{'index':0,'delta':{},'finish_reason':'stop'}],'usage':{'prompt_tokens':10,'completion_tokens':5,'total_tokens':15}}]
    for row in rows:self.wfile.write(('data: '+json.dumps(row)+'\n\n').encode())
    self.wfile.write(b'data: [DONE]\n\n');self.close_connection=True
   else:
    raw=json.dumps({'choices':[{'index':0,'message':{'role':'assistant','content':'{"title":"Entry fixture"}'},'finish_reason':'stop'}]}).encode();self.send_header('content-type','application/json');self.send_header('content-length',str(len(raw)));self.end_headers();self.wfile.write(raw)
   self.wfile.flush()
  except Exception as error:errors.append('provider '+repr(error));self.close_connection=True
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);server.daemon_threads=False;threading.Thread(target=server.serve_forever,daemon=True).start()
config={'schemaVersion':1,'agent':{'model':'fixture/mock','maxOutputTokens':MAX_OUTPUT_TOKENS,'maxContextTokens':64000},'model':{'providers':{'fixture':{'protocol':'openai','url':f'http://127.0.0.1:{server.server_port}/v1','apiKey':'mock-only','models':{'mock':{}}}}},'extension':{'builtinPluginsEnabled':{'browser-use':False,'funasr':False,'windows-skills':False}},'telemetry':{'enabled':False},'cron':{'enabled':a.mode=='cron','timezone':'UTC'}}
if a.mode=='alwayson':config['alwaysOn']={'trigger':{'tickIntervalMinutes':0.01,'cooldownMinutes':1,'dailyBudget':1,'recentUserMsgMinutes':0},'projects':{str(project):{'enabled':True}}}
(home/'pilotdeck.yaml').write_text(json.dumps(config,indent=2))
port=28478 if a.engine=='ts' else 28479
env={k:v for k,v in os.environ.items()if k in ('PATH','LANG','LC_ALL','LD_LIBRARY_PATH')};env.update(HOME=str(home),PILOT_HOME=str(home),TMPDIR=str(tmp),DATABASE_PATH=str(home/'auth.db'),WORKSPACES_ROOT=str(a.out/'workspaces'),PILOTDECK_GATEWAY_HOST='127.0.0.1',PILOTDECK_GATEWAY_PORT=str(port),PILOTDECK_GATEWAY_TOKEN=token,PILOTDECK_GATEWAY_TOKEN_PATH=str(home/'token'),PILOTDECK_SKIP_BROWSER_OPEN='1',PILOTDECK_RS_SIDECAR=str(a.sidecar),PDRS_BASELINE=str(a.baseline))
command=([str(a.node),str(a.baseline/'dist/src/cli/pilotdeck.js')]if a.engine=='ts'else[str(a.cli)])+['server','--host','127.0.0.1','--port',str(port)]
provenance={'engine':a.engine,'mode':a.mode,'sourceManifest':{'path':str(a.source_manifest),'sha256':sha(a.source_manifest)},'candidateManifest':{'path':str(a.candidate_manifest),'sha256':sha(a.candidate_manifest)},'binaries':{'pilotdeck':sha(a.cli),'sidecar':sha(a.sidecar)},'producerSha256':sha(__file__),'cronDriverSha256':sha(HERE/'cron.mts'),'baselineEntrySha256':sha(a.baseline/'dist/src/cli/pilotdeck.js'),'node':versions,'command':command,'networkNamespace':os.readlink('/proc/self/ns/net'),'formalAcceptance':False}
if a.mode == 'history':
 provenance['diagnosticOnly'] = True
 provenance['formalAcceptance'] = 'NOT_RUN'
 provenance['historyDriverSha256'] = sha(HERE/'history.mts')
 provenance['registeredHistoryDriverSha256'] = sha(root/'scripts/gateway-parity/session_history_driver.mts')
 provenance['subagentBundleSha256'] = sha(HERE.parent/'native-history/subagent-bundle.json')
(a.out/'provenance.json').write_text(json.dumps(provenance,indent=2))
class ResponseSocket:
 def __init__(self,raw):self.raw=raw
 def makefile(self,*args):return io.BytesIO(self.raw)
def http(id,method,path,body=None):
 raw_body=b''if body is None else json.dumps(body).encode();raw=(f'{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {len(raw_body)}\r\n\r\n').encode()+raw_body
 with socket.create_connection(('127.0.0.1',port),timeout=3)as s:
  s.settimeout(5);s.sendall(raw);parts=[];size=0
  while True:
   part=s.recv(65536)
   if not part:break
   parts.append(part);size+=len(part);assert size<=4*1024*1024
 response=b''.join(parts);parsed=HTTPResponse(ResponseSocket(response));parsed.begin();decoded=parsed.read();row={'id':id,'method':method,'path':path,'status':parsed.status,'body':decoded.decode('utf-8',errors='strict'),'requestHex':raw.hex(),'responseHex':response.hex()};http_rows.append(row);return row
def start_owned(name,argv):
 log=(a.out/f'{name}.stdout').open('xb');argv=[str(item) for item in argv]
 return runner.start_owned(owned,log,{'name':name,'argv':argv},argv,cwd=project,env=env,
                           stdout=log,stderr=subprocess.STDOUT)

def always_on_files():
 root=home/'always-on/projects'
 histories=sorted(root.glob('*/run-history.jsonl'));events=sorted(root.glob('*/events.jsonl'))
 if len(histories)!=1 or len(events)!=1 or histories[0].parent!=events[0].parent:return None
 return histories[0],events[0]

def read_after_reap():
 files=always_on_files()
 if files is None:raise ValueError('missing unique post-reap Always On event/history files')
 history,events=files;history_bytes=history.read_bytes();events_bytes=events.read_bytes()
 return {'id':'gateway.background:always-on','snapshotAfterReap':True,
         'runHistory':history_bytes.decode(),'events':events_bytes.decode(),'modelRequests':len(requests),
         'sha256':{'runHistory':hashlib.sha256(history_bytes).hexdigest(),
                   'events':hashlib.sha256(events_bytes).hexdigest()}}

common.install_shutdown()
try:
 lease.__enter__();leased=True
 child=start_owned('server',command)
 end=time.monotonic()+20
 while time.monotonic()<end:
  try:runner.wait_owned(child,0)
  except subprocess.TimeoutExpired:pass
  else:raise RuntimeError('owned server exited before listener readiness')
  try:
   with socket.create_connection(('127.0.0.1',port),timeout=.25):pass
  except OSError:
   time.sleep(.05);continue
  witness=subprocess.run(['lsof','-nP','-a','-p',str(child.pid),'-iTCP','-sTCP:LISTEN','-Fn'],
                         capture_output=True,text=True,timeout=5)
  addresses=[row[1:] for row in witness.stdout.splitlines() if row.startswith('n')]
  if witness.returncode!=0 or addresses!=[f'127.0.0.1:{port}']:
   raise RuntimeError(f'fixed port listener is not exclusively owned by this fixture pid: {addresses!r}')
  if http('readiness','GET','/health')['status']==200:
   listener={'ownerPid':child.pid,'addresses':addresses};break
 else:raise TimeoutError('owned server readiness deadline')
 if a.mode=='http':
  from urllib.parse import quote
  key=quote(str(project),safe='');base='/api/web/projects/'+key
  probes=[('gateway.http:ANY /health','GET','/health',None),('gateway.http:ANY /auth/local-token','GET','/auth/local-token',None),('gateway.http:ANY /feishu/webhook','POST','/feishu/webhook',{}),('gateway.http:GET /api/web/projects','GET','/api/web/projects',None),('gateway.http:GET /api/web/projects/:projectKey','GET',base,None),('gateway.http:GET /api/web/projects/:projectKey/files/tree','GET',base+'/files/tree',None),('gateway.http:GET /api/web/projects/:projectKey/files/read','GET',base+'/files/read?path=note.txt',None),('gateway.http:POST /api/web/projects/:projectKey/files/write','POST',base+'/files/write',{'path':'acceptance-http-write.txt','content':'ENTRY_HTTP_WRITE_42\n'}),('gateway.http:POST /api/module-host/describe','POST','/api/module-host/describe',{}),('gateway.http:POST /api/module-host/call','POST','/api/module-host/call',{'principal':{'pilotDeckUserId':'entry','tenantId':'entry','actorUserId':'entry','agentId':'entry'},'operation':'entry-unknown-operation','input':{}}),('gateway.http:POST /api/module-host/approvals/status','POST','/api/module-host/approvals/status',{}),('gateway.http:POST /api/module-host/approvals/resume','POST','/api/module-host/approvals/resume',{}),('gateway.http:static-fallback','GET','/acceptance.js',None),('gateway.http:<unmatched-404>','GET','/entry-missing-42.txt',None)]
  for args in probes:
   if args[0]=='gateway.http:<unmatched-404>':(project/'ui/dist/index.html').unlink()
   http(*args)
  assert next(r for r in http_rows if r['id']=='gateway.http:static-fallback')['body']=='ENTRY_STATIC_42\n'
  assert(project/'acceptance-http-write.txt').read_text()=='ENTRY_HTTP_WRITE_42\n'
  assert next(r for r in http_rows if r['id']=='gateway.http:<unmatched-404>')['status']==404
  assert len({r['id']for r in http_rows if r['id']!='readiness'})==14
 elif a.mode=='cron':
  driver=start_owned('driver',[a.node,'--import',a.baseline/'node_modules/tsx/dist/loader.mjs',HERE/'cron.mts',
                               f'ws://127.0.0.1:{port}/ws',token,project,a.out])
  code=runner.wait_owned(driver,100)
  assert code==0,('cron driver',code);assert any('ENTRY_BACKGROUND_CRON' in json.dumps(r['body']) for r in requests)
  background=json.loads((a.out/'cron.json').read_text())
 elif a.mode=='history':
  driver=start_owned('driver',[a.node,'--import',a.baseline/'node_modules/tsx/dist/loader.mjs',HERE/'history.mts',
                               a.baseline,HERE.parent/'native-history/subagent-bundle.json',project,home,
                               f'ws://127.0.0.1:{port}/ws',token,a.out/'history'])
  code=runner.wait_owned(driver,100)
  assert code==0,('ordinary history driver',code)
  ordinary=json.loads((a.out/'history/result.json').read_text())
  restoration=json.loads((a.out/'history/restoration.json').read_text())
  assert ordinary['closed'] is True and restoration['pass'] is True
  assert any(row['body'].get('stream') is True for row in requests)
  background={'id':'normal-history','u9u10':ordinary,'restoration':restoration}
 else:
  end=time.monotonic()+35
  while time.monotonic()<end:
   files=always_on_files()
   if (requests and files is not None and 'no_plan' in files[0].read_text()
       and 'discovery_started' in files[1].read_text()):break
   time.sleep(.05)
  else:raise TimeoutError('Always On trigger deadline')
 assert not errors,errors;success=True
except BaseException as error:errors.append(repr(error));raise
finally:
 try:
  with common.deferred_signals():
   if child is not None:
    try:runner.wait_owned(child,0)
    except subprocess.TimeoutExpired:
     # The direct child is still held, even if it exits before this signal.
     # Keep it unreaped; cleanup_owned owns the group escalation/absence proof.
     try:os.kill(child.pid,signal.SIGTERM)
     except ProcessLookupError:pass
     try:runner.wait_owned(child,20)
     except subprocess.TimeoutExpired:errors.append('server needed group cleanup escalation')
    except OSError as error:errors.append(f'cannot verify held server identity: {error}')
   audit=runner.cleanup_owned(owned)
   if server:server.shutdown();server.server_close()
   if a.mode=='alwayson' and success and audit['allStopped']:
    try:background=read_after_reap()
    except (OSError,ValueError) as error:errors.append(f'Always On post-reap capture: {error}')
   unchanged=sha(a.cli)==binaries['pilotdeck'] and sha(a.sidecar)==binaries['pilotdeck-agent-loop-sidecar']
   passed=(success and not errors and child is not None and child.returncode==0 and unchanged and audit['allStopped'])
   result={'pass':passed,'engine':a.engine,'mode':a.mode,'http':http_rows,'background':background,
           'providerRequests':requests,'errors':errors,'serverExitCode':None if child is None else child.returncode,
           'serverPid':None if child is None else child.pid,'listener':listener,'ownedCleanup':audit,
           'binariesUnchanged':unchanged,'formalAcceptance':False,'elapsedSeconds':time.monotonic()-start}
   if a.mode=='history':result.update(diagnosticOnly=True,formalAcceptance='NOT_RUN')
   (a.out/'result.json').write_text(json.dumps(result,indent=2))
 finally:
  if leased:lease.__exit__(None,None,None)
 if not passed and sys.exc_info()[0] is None:raise SystemExit(1)
