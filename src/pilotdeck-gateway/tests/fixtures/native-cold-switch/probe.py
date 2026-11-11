#!/usr/bin/env python3
# anchor-selfcheck: unsupported Native business integration probe; no source mutation table.
"""Isolated cold-switch diagnostic; existing business-loop process/identity helpers."""
import argparse,hashlib,json,sys
from pathlib import Path
COLD_HERE=Path(__file__).resolve().parent
p=argparse.ArgumentParser(add_help=False);p.add_argument('--source-receipt',required=True,type=Path);p.add_argument('--inject-missing-drain-receipt',action='store_true');p.add_argument('--force-real-drain-failure',action='store_true');cold,args=p.parse_known_args();sys.argv=[sys.argv[0],*args]
shared=COLD_HERE.parent/'native-business-loop/probe.py';shared_bytes=shared.read_bytes();assert hashlib.sha256(shared_bytes).hexdigest()=='ee43dbb95e8be6dce2b21663e4564ced000e75aca18eeeb64b68ab56a32c8bde','shared process helper drift'
source=shared_bytes.decode();prefix=source[:source.index('lease=None;lease_acquired=False;alignment=None')]
original_file=__file__;__file__=str(shared)
try:exec(compile(prefix,str(shared),'exec'),globals())
finally:__file__=original_file
receipt=json.loads(cold.source_receipt.read_text());receipt_sha=hashlib.sha256(cold.source_receipt.read_bytes()).hexdigest()
for key,canonical in [('gateway','pilotdeck-gateway'),('sidecar','pilotdeck-agent-loop-sidecar')]:assert receipt['binaries'][canonical]==sha(getattr(a,key)),('receipt mismatch',key)
provenance=json.loads((a.out/'provenance.json').read_text());provenance.update(sharedProbeSha256=sha(shared),probeSha256=sha(COLD_HERE/'probe.py'),driverSha256=sha(COLD_HERE/'driver.mts'),sourceReceipt={'path':str(cold.source_receipt.resolve()),'sha256':receipt_sha},topology='TS + Rust proxy -> native Rust -> TS + Rust proxy -> native Rust -> native Rust restart',formalAcceptance=False)
runtime_rows=[]
if 'runtimeLibraries' in receipt:
 source_ref=receipt['sourceManifest'];manifest_ref=receipt['binaryManifest']
 source_file=Path(source_ref['path']);manifest_file=Path(manifest_ref['path'])
 if sha(source_file)!=source_ref['sha256'] or sha(manifest_file)!=manifest_ref['sha256']:
  raise ValueError('cold switch source/binary manifest bytes changed')
 source_doc=json.loads(source_file.read_text())
 if source_doc.get('runtimeLibraries')!=receipt['runtimeLibraries']:
  raise ValueError('cold switch runtime library receipt differs from source manifest')
 sources=common.verify_native_runtime_libraries(source_doc,manifest_file)
 for row in sources:
  source=manifest_file.parent/row['path'];target=a.out/'binaries'/row['path'];target.parent.mkdir(exist_ok=True)
  with source.open('rb') as stream,target.open('xb') as output:shutil.copyfileobj(stream,output)
  target.chmod(0o500)
  if target.stat().st_size!=row['bytes'] or sha(target)!=row['sha256'] or sha(source)!=row['sha256']:
   raise ValueError('cold switch runtime snapshot bytes changed')
  runtime_rows.append({**row,'source':str(source),'snapshot':str(target)})
provenance['runtimeLibraries']=runtime_rows
save(a.out/'provenance.json',provenance)
(a.out/'fixture-source').mkdir()
for file in COLD_HERE.iterdir():
 if file.is_file():(a.out/'fixture-source'/file.name).write_bytes(file.read_bytes())
lease=None;acquired=False;success=False;handoff_blocked=False;last_drain=True;phases=[];proxy=None;active_port=None
real_drain_failed=False;survivor=None
state=a.out/'state.json'
def durable_snapshot():
 result={}
 for label,base in [('home',home),('project',project)]:
  for file in base.rglob('*'):
   if file.is_file() and(file.suffix=='.jsonl' or 'cron' in file.parts or any(part.startswith('chat-upload')for part in file.parts)):
    assert file.stat().st_size<=50*1024*1024
    record={'sha256':sha(file),'bytes':file.stat().st_size}
    if file.suffix=='.json':record['jsonValue']=json.loads(file.read_text())
    result[label+'/'+str(file.relative_to(base))]=record
 return result
def verify_readback_preserves_data(before,after,rounddir):
 changes=[]
 assert before.keys()<=after.keys(),('durable paths deleted',before.keys()-after.keys())
 for path,old in before.items():
  new=after[path]
  if old==new:continue
  assert path.endswith('/tasks.json') and old.get('jsonValue')==new.get('jsonValue'),('durable value changed',path,old,new)
  changes.append({'path':path,'kind':'TS cron migration serialization rewrite','before':old,'after':new,'fullJsonValueEqual':True})
 for path in after.keys()-before.keys():
  value=after[path].get('jsonValue')
  assert path=='home/cron/store-migration-v1.json' and value.get('version')==1 and isinstance(value.get('completedAt'),str),('unexpected new durable path',path,after[path])
  changes.append({'path':path,'kind':'TS cron migration marker created','after':after[path]})
 save(rounddir/'readback-effects.json',{'noPathsDeleted':True,'allPriorValuesPreserved':True,'changes':changes,'notByteReadOnly':bool(changes),'formalDifferenceApprovalClaimed':False})
def raw_survivor(port,rounddir):
 sock=socket.create_connection(('127.0.0.1',port),timeout=3);sock.settimeout(3)
 request=b'GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n'
 sock.sendall(request);response=b''
 while not response.endswith(b'\r\n\r\n'):
  byte=sock.recv(1);assert byte,'survivor upgrade EOF';response+=byte;assert len(response)<=8192
 assert response.startswith(b'HTTP/1.1 101'),response
 save(rounddir/'survivor-upgrade.json',{'request':request.decode(),'response':response.decode(),'reference':'#1353 an_expired_drain_halts_and_the_death_march_closes_the_survivor'})
 return sock
def free_port():
 for port in range(28610,28690):
  try:
   with socket.socket()as sock:sock.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);sock.bind(('127.0.0.1',port))
   return port
  except OSError:continue
 raise RuntimeError('no private high port')
def assert_port_closed(port):
 with socket.socket()as sock:sock.settimeout(.5);assert sock.connect_ex(('127.0.0.1',port))!=0,('port remains accepting',port)
def stop_stage(backend,front,backend_port,front_port,rounddir):
 # No new writer may be launched unless all of these receipts succeed.
 global last_drain
 last_drain=False;scan()
 if front:
  front.send_signal(signal.SIGINT);assert front.wait(timeout=10)==0,'proxy did not exit cleanly';assert_port_closed(front_port)
 halt(backend);assert_port_closed(backend_port)
 table=ps();left=[(pid,birth)for(pid,birth)in identities if pid in table and table[pid][1]==birth]
 assert not left,('identities survived writer drain',left)
 last_drain=True;save(rounddir/'drain.json',{'exit':backend.returncode,'proxyExit':front.returncode if front else None,'portsClosed':True,'ownedIdentitiesGone':True,'nextWriterAllowed':True,'snapshot':durable_snapshot()})
try:
 lease=common.parity_lock(timeout=1);lease.__enter__();acquired=True;common.LIVE_GROUPS.add(os.getpid())
 provider=http.server.ThreadingHTTPServer(('127.0.0.1',0),Provider);provider.daemon_threads=True;threading.Thread(target=provider.serve_forever,daemon=True).start();monitor_thread=threading.Thread(target=monitor,daemon=True);monitor_thread.start()
 config(provider.server_port)
 with(home/'pilotdeck.yaml').open('a')as file:file.write('cron: {enabled: true, timezone: UTC}\n')
 rollback_before=None
 for phase,engine in [('ts-seed','ts'),('rust-write','rust'),('ts-readback','ts'),('rust-again','rust'),('rust-final-read','rust')]:
  assert last_drain,'next writer blocked: prior drain receipt missing or failed'
  assert host is None and proxy is None,'overlapping writer ownership'
  side=engine;rounddir=a.out/phase;rounddir.mkdir();backend_port=free_port();env=env_for(backend_port)
  if engine=='ts':
   host=spawn(['node',str(a.baseline/'dist/src/cli/pilotdeck.js'),'server'],env,rounddir);wait_ready(host,backend_port)
   front_port=free_port();assert front_port!=backend_port;proxyenv=dict(env,PILOTDECK_GATEWAY_MODE='proxy',PILOTDECK_PROXY_LISTEN=f'127.0.0.1:{front_port}',PILOTDECK_PROXY_UPSTREAM=f'http://127.0.0.1:{backend_port}')
   proxy=spawn([a.gateway],proxyenv,rounddir);wait_ready(proxy,front_port);port=front_port
  else:
   front_port=None;host=spawn([a.gateway,'--native','--home',home,'--project',project,'--listen',f'127.0.0.1:{backend_port}','--token-file',home/'token','--sidecar',a.sidecar,'--drain-ms','2000'],env,rounddir);wait_ready(host,backend_port);port=backend_port
  save(rounddir/'start.json',{'phase':phase,'engine':engine,'writerPid':host.pid,'proxyPid':proxy.pid if proxy else None,'backendPort':backend_port,'clientPort':port,'previousDrainVerified':last_drain,'birthTable':{str(pid):record for pid,record in ps().items()if pid in [host.pid,proxy.pid if proxy else None]}})
  cmd=['node','--import',str(a.baseline/'node_modules/tsx/dist/loader.mjs'),str(COLD_HERE/'driver.mts'),f'ws://127.0.0.1:{port}/ws',home/'token',project,rounddir,state,phase]
  driver=spawn(cmd,env,rounddir);assert driver.wait(timeout=50)==0,('phase driver failed',phase)
  result=json.loads((rounddir/'result.json').read_text());assert result['pass'];phases.append({'phase':phase,'engine':engine,'reads':list(result['reads']),'writes':result['writes']})
  if phase=='rust-write' and cold.force_real_drain_failure:
   survivor=raw_survivor(backend_port,rounddir)
   before_failed_drain=durable_snapshot();save(rounddir/'before-failed-drain.json',before_failed_drain)
   try:stop_stage(host,proxy,backend_port,front_port,rounddir)
   except AssertionError as error:
    assert host.poll() is not None and host.returncode!=0,('not a real nonzero shutdown',host.poll(),str(error))
    stderr=(rounddir/'.stderr.log').read_text();assert 'drain deadline' in stderr,stderr
    tail=b''
    while True:
     chunk=survivor.recv(8192)
     if not chunk:break
     tail+=chunk;assert len(tail)<=64*1024
    survivor.close();survivor=None;assert_port_closed(backend_port)
    real_drain_failed=True;handoff_blocked=True;assert not last_drain
    after_failed_drain=durable_snapshot();assert before_failed_drain==after_failed_drain,'failed switch altered new durable data'
    save(a.out/'blocked-next-writer.json',{'reason':'actual native WS drain deadline failure','runtimeExit':host.returncode,'runtimeStderrSha256':sha(rounddir/'.stderr.log'),'survivorTailHex':tail.hex(),'nextWriterStarted':False,'lastDrainCertificate':last_drain,'newRustDataPreserved':True,'completedPhases':phases,'switchProofReference':'native_process::switch_wiring_tests::an_expired_drain_halts_and_the_death_march_closes_the_survivor'})
    host=None;proxy=None;break
   else:raise AssertionError('real survivor unexpectedly drained successfully')
  stop_stage(host,proxy,backend_port,front_port,rounddir);host=None;proxy=None
  snapshot=durable_snapshot();save(rounddir/'durable-snapshot.json',snapshot)
  (rounddir/'durable-files').mkdir()
  for relative,record in snapshot.items():
   label,name=relative.split('/',1);source_file=(home if label=='home' else project)/name;target=rounddir/'durable-files'/relative;target.parent.mkdir(parents=True,exist_ok=True);target.write_bytes(source_file.read_bytes());assert sha(target)==record['sha256']
  if phase=='rust-write':
   rollback_before=snapshot
   if cold.inject_missing_drain_receipt:
    last_drain=False;handoff_blocked=True;save(a.out/'blocked-next-writer.json',{'reason':'injected missing drain certificate, not an actual forced runtime drain failure','nextWriterStarted':False,'newRustDataPreserved':snapshot==durable_snapshot(),'completedPhases':phases});break
  if phase=='ts-readback':verify_readback_preserves_data(rollback_before,snapshot,rounddir)
  if phase=='rust-again':final_before=snapshot
  if phase=='rust-final-read':assert snapshot==final_before,'native read-only restart changed durable data'
  assert time.monotonic()-start<RUN_SECONDS
 if not handoff_blocked:assert len(phases)==5;assert not errors,errors;success=True
finally:
 signal.alarm(0);stop.set()
 if survivor:survivor.close()
 if monitor_thread:monitor_thread.join(timeout=4)
 for child in children:
  if child.poll() is None:
   child.send_signal(signal.SIGTERM)
   try:child.wait(timeout=5)
   except subprocess.TimeoutExpired:child.kill();child.wait(timeout=5)
 table=ps()
 for(pid,birth)in identities:
  if pid in table and table[pid][1]==birth:
   try:os.kill(pid,signal.SIGTERM)
   except ProcessLookupError:pass
 if provider:provider.shutdown();provider.server_close()
 table=ps();left=[{'pid':pid,'birth':birth}for(pid,birth)in identities if pid in table and table[pid][1]==birth]
 binaries_unchanged=all(sha(Path(path))==before for path,before in binary_before.items())and all(sha(Path(row['snapshot']))==row['sha256']for row in provenance['binaries'].values())
 binaries_unchanged=binaries_unchanged and all(sha(Path(row['source']))==sha(Path(row['snapshot']))==row['sha256'] for row in runtime_rows)
 save(a.out/'provider.json',wire);save(a.out/'result.json',{'pass':success and not left and not errors and binaries_unchanged,'handoffBlocked':handoff_blocked,'realDrainFailureObserved':real_drain_failed,'injectedMissingReceipt':cold.inject_missing_drain_receipt,'phases':phases,'errors':errors,'remainingOwnedIdentities':left,'cleanupVerified':not left and not errors,'binariesUnchanged':binaries_unchanged,'noSnapshotRestorePerformed':True,'sourceReceiptSha256':receipt_sha,'notFormalRetirement':True,'requests':len(wire),'elapsedSeconds':time.monotonic()-start})
 if acquired:
  if not left and not errors:common.LIVE_GROUPS.discard(os.getpid())
  common.LIVE_GROUPS.update(row['pid']for row in left);lease.__exit__(None,None,None)
if handoff_blocked:raise SystemExit(2)
if not success or left or errors or not binaries_unchanged:raise SystemExit(1)
