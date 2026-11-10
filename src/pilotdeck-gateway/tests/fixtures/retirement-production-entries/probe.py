"""Real public CLI Server HTTP / WS / periodic Always-On / Cron collector, mock only.
Run only inside the validated network-none namespace. No behavioral scoring replacement.
"""
import argparse,hashlib,http.client,http.server,io,json,os,re,signal,socket,subprocess,sys,threading,time
from pathlib import Path
from http.client import HTTPResponse
sys.dont_write_bytecode=True
p=argparse.ArgumentParser()
p.add_argument('--engine',choices=('ts','native'),required=True);p.add_argument('--mode',choices=('http','cron','alwayson'),required=True)
for key in ('baseline','node','cli','sidecar','source-manifest','candidate-manifest','out'):p.add_argument('--'+key,type=Path,required=True)
a=p.parse_args();a.out=a.out.resolve();a.out.mkdir(parents=True,exist_ok=False);HERE=Path(__file__).resolve().parent
MAX_REQUESTS=32;MAX_BODY=2*1024*1024;MAX_OUTPUT_TOKENS=256;SECONDS=90
sha=lambda x:hashlib.sha256(Path(x).read_bytes()).hexdigest()
manifest=json.loads(a.candidate_manifest.read_text());binaries={r['path']:r['sha256']for r in manifest}
assert sha(a.cli)==binaries['pilotdeck'];assert sha(a.sidecar)==binaries['pilotdeck-agent-loop-sidecar']
assert subprocess.check_output(['git','-C',str(a.baseline),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
versions=json.loads(subprocess.check_output([str(a.node),'-p','JSON.stringify(process.versions)'],text=True));assert versions['node']=='22.22.0' and versions['icu']=='78.2'
start=time.monotonic();requests=[];errors=[];child=None;pidfd=None;success=False;server=None;http_rows=[];background=None
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
   body=json.loads(self.rfile.read(length));requests.append({'path':self.path,'body':body});text='ENTRY_RUNTIME_REPLY_'+a.mode;self.send_response(200)
   if body.get('stream'):
    self.send_header('content-type','text/event-stream');self.send_header('connection','close');self.end_headers()
    rows=[{'choices':[{'index':0,'delta':{'role':'assistant','content':text},'finish_reason':None}]},{'choices':[{'index':0,'delta':{},'finish_reason':'stop'}],'usage':{'prompt_tokens':10,'completion_tokens':5,'total_tokens':15}}]
    for row in rows:self.wfile.write(('data: '+json.dumps(row)+'\n\n').encode())
    self.wfile.write(b'data: [DONE]\n\n');self.close_connection=True
   else:
    raw=json.dumps({'choices':[{'index':0,'message':{'role':'assistant','content':'{"title":"Entry fixture"}'},'finish_reason':'stop'}]}).encode();self.send_header('content-type','application/json');self.send_header('content-length',str(len(raw)));self.end_headers();self.wfile.write(raw)
   self.wfile.flush()
  except Exception as error:errors.append('provider '+repr(error));self.close_connection=True
server=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);server.daemon_threads=True;threading.Thread(target=server.serve_forever,daemon=True).start()
config={'schemaVersion':1,'agent':{'model':'fixture/mock','maxOutputTokens':MAX_OUTPUT_TOKENS,'maxContextTokens':64000},'model':{'providers':{'fixture':{'protocol':'openai','url':f'http://127.0.0.1:{server.server_port}/v1','apiKey':'mock-only','models':{'mock':{}}}}},'extension':{'builtinPluginsEnabled':{'browser-use':False,'funasr':False,'windows-skills':False}},'telemetry':{'enabled':False},'cron':{'enabled':a.mode=='cron','timezone':'UTC'}}
if a.mode=='alwayson':config['alwaysOn']={'trigger':{'tickIntervalMinutes':0.01,'cooldownMinutes':1,'dailyBudget':1,'recentUserMsgMinutes':0},'projects':{str(project):{'enabled':True}}}
(home/'pilotdeck.yaml').write_text(json.dumps(config,indent=2))
port=28478 if a.engine=='ts' else 28479
env={k:v for k,v in os.environ.items()if k in ('PATH','LANG','LC_ALL','LD_LIBRARY_PATH')};env.update(HOME=str(home),PILOT_HOME=str(home),TMPDIR=str(tmp),DATABASE_PATH=str(home/'auth.db'),WORKSPACES_ROOT=str(a.out/'workspaces'),PILOTDECK_GATEWAY_HOST='127.0.0.1',PILOTDECK_GATEWAY_PORT=str(port),PILOTDECK_GATEWAY_TOKEN=token,PILOTDECK_GATEWAY_TOKEN_PATH=str(home/'token'),PILOTDECK_SKIP_BROWSER_OPEN='1',PILOTDECK_RS_SIDECAR=str(a.sidecar),PDRS_BASELINE=str(a.baseline))
command=([str(a.node),str(a.baseline/'dist/src/cli/pilotdeck.js')]if a.engine=='ts'else[str(a.cli)])+['server','--host','127.0.0.1','--port',str(port)]
provenance={'engine':a.engine,'mode':a.mode,'sourceManifest':{'path':str(a.source_manifest),'sha256':sha(a.source_manifest)},'candidateManifest':{'path':str(a.candidate_manifest),'sha256':sha(a.candidate_manifest)},'binaries':{'pilotdeck':sha(a.cli),'sidecar':sha(a.sidecar)},'producerSha256':sha(__file__),'cronDriverSha256':sha(HERE/'cron.mts'),'baselineEntrySha256':sha(a.baseline/'dist/src/cli/pilotdeck.js'),'node':versions,'command':command,'networkNamespace':os.readlink('/proc/self/ns/net'),'formalAcceptance':False}
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
try:
 with (a.out/'server.stdout').open('xb')as stdout,(a.out/'server.stderr').open('xb')as stderr:child=subprocess.Popen(command,cwd=project,env=env,stdout=stdout,stderr=stderr,start_new_session=True)
 pidfd=os.pidfd_open(child.pid,0)
 end=time.monotonic()+20
 while True:
  assert child.poll()is None,('server exited',child.returncode)
  try:
   if http('readiness','GET','/health')['status']==200:break
  except(ConnectionRefusedError,TimeoutError,OSError):pass
  assert time.monotonic()<end,'ready deadline';time.sleep(.05)
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
  with (a.out/'driver.stdout').open('xb')as stdout,(a.out/'driver.stderr').open('xb')as stderr:r=subprocess.run([str(a.node),'--import',str(a.baseline/'node_modules/tsx/dist/loader.mjs'),str(HERE/'cron.mts'),f'ws://127.0.0.1:{port}/ws',token,str(project),str(a.out)],env=env,cwd=project,stdout=stdout,stderr=stderr,timeout=100)
  assert r.returncode==0,('cron driver',r.returncode);assert any('ENTRY_BACKGROUND_CRON'in json.dumps(r['body'])for r in requests);background=json.loads((a.out/'cron.json').read_text())
 else:
  end=time.monotonic()+35
  while True:
   paths=list((home/'always-on/projects').glob('*/state.json'));state=json.loads(paths[0].read_text())if paths else{}
   histories=list((home/'always-on/projects').glob('*/run-history.jsonl'));history=histories[0].read_text()if histories else''
   events=list((home/'always-on/projects').glob('*/events.jsonl'));event_text=events[0].read_text()if events else''
   if requests and 'no_plan'in history and 'discovery_started'in event_text:break
   assert time.monotonic()<end,('alwayson trigger deadline',state,history,requests);time.sleep(.05)
  background={'id':'gateway.background:always-on','state':state,'runHistory':history,'events':event_text,'modelRequests':len(requests)}
 assert not errors,errors;success=True
except BaseException as error:errors.append(repr(error));raise
finally:
 if child is not None and child.poll()is None:
  signal.pidfd_send_signal(pidfd,signal.SIGTERM)
  try:child.wait(timeout=20)
  except subprocess.TimeoutExpired:signal.pidfd_send_signal(pidfd,signal.SIGKILL);child.wait(timeout=5);errors.append('server required SIGKILL')
 if pidfd is not None:os.close(pidfd)
 if server:server.shutdown();server.server_close()
 unchanged=sha(a.cli)==binaries['pilotdeck']and sha(a.sidecar)==binaries['pilotdeck-agent-loop-sidecar']
 result={'pass':success and not errors and child is not None and child.returncode==0 and unchanged,'engine':a.engine,'mode':a.mode,'http':http_rows,'background':background,'providerRequests':requests,'errors':errors,'serverExitCode':None if child is None else child.returncode,'binariesUnchanged':unchanged,'formalAcceptance':False,'elapsedSeconds':time.monotonic()-start}
 (a.out/'result.json').write_text(json.dumps(result,indent=2))
