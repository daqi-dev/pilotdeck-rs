"""Run archived source-oracle cases against the real Rust proxy + observer.
Only localhost, isolated fixture files, and one owned lock-holder are used.
"""
import argparse,base64,hashlib,json,os,signal,socket,struct,subprocess,threading,time
from pathlib import Path
parser=argparse.ArgumentParser(description=__doc__);parser.add_argument('--output',required=True,type=Path);args=parser.parse_args()
root=Path(__file__).resolve().parents[3];out=args.output.resolve()
if os.geteuid()==0:raise ValueError('non-root user required for real EACCES cases')
if not out.is_relative_to(root/'_tmp'):raise ValueError('output must be inside this worktree _tmp')
out.mkdir(parents=True,exist_ok=False)
for d in ['isolation/home','isolation/xdg','isolation/tmp']:(out/d).mkdir(parents=True)
env={k:v for k,v in os.environ.items() if not k.startswith(('PILOT_','PILOTDECK_','STAFFDECK_')) and k!='NODE_OPTIONS'};env.update(HOME=str(out/'isolation/home'),XDG_CONFIG_HOME=str(out/'isolation/xdg'),XDG_CACHE_HOME=str(out/'isolation/xdg'),XDG_DATA_HOME=str(out/'isolation/xdg'),TMPDIR=str(out/'isolation/tmp'),PILOTDECK_SKIP_BROWSER_OPEN='1',SKIP_BROWSER_OPEN='1')
for port in [28860,28861]:
 with socket.socket() as probe:probe.bind(('127.0.0.1',port))
lock=subprocess.Popen(['/bin/sh','-c','echo ready; read owned_input; exit 0'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,env=env,text=True)
assert lock.stdout.readline()=='ready\n'
fixture_path=root/'src/pilotdeck-gateway/tests/fixtures/session-model/cases.json'
fixture_bytes=fixture_path.read_bytes();fixture=json.loads(fixture_bytes)
io_fixture_path=fixture_path.with_name('io-errors.json')
io_fixture_bytes=io_fixture_path.read_bytes();io_fixture=json.loads(io_fixture_bytes)
fixture['cases'].extend(io_fixture['cases'])
io_round5_path=fixture_path.with_name('io-errors-round5.json')
io_round5_bytes=io_round5_path.read_bytes();io_round5=json.loads(io_round5_bytes)
fixture['cases'].extend(io_round5['cases'])
def native_case(row):
 # Forwarding and injected errno are mandatory unit controls. A NUL-bearing
 # env object is also a loader-only control: OS process environments cannot
 # contain NUL, so it cannot be supplied to a real Rust proxy process.
 # Controlled mutations between check and read are replayed by mandatory unit
 # fixtures; a real proxy has no test hook at that filesystem boundary.
 return not row['forward'] and not row['process']['eperm'] and not row.get('io',{}).get('race',False) and not any('\0' in v for v in row['env'].values())
rows=[];observations=[];errors=[];processes=[]
listener=socket.socket();listener.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);listener.bind(('127.0.0.1',28860));listener.listen();listener.settimeout(.2)
stop=threading.Event()
def exactly(sock,n):
 b=b''
 while len(b)<n:
  chunk=sock.recv(n-len(b))
  if not chunk:raise EOFError
  b+=chunk
 return b

def frame(sock):
 h=exactly(sock,2);n=h[1]&127
 if n==126:n=struct.unpack('!H',exactly(sock,2))[0]
 elif n==127:n=struct.unpack('!Q',exactly(sock,8))[0]
 mask=exactly(sock,4) if h[1]&128 else None
 b=exactly(sock,n)
 if mask:b=bytes(v^mask[i%4] for i,v in enumerate(b))
 return h[0]&15,b

def send_frame(sock,payload,client=False):
 h=bytes([129]);maskbit=128 if client else 0;n=len(payload)
 h+=bytes([maskbit|n]) if n<126 else bytes([maskbit|126])+struct.pack('!H',n) if n<65536 else bytes([maskbit|127])+struct.pack('!Q',n)
 if client:
  mask=b'\x11\x22\x33\x44';h+=mask;payload=bytes(v^mask[i%4] for i,v in enumerate(payload))
 sock.sendall(h+payload)

def observe(sock):
 try:
  data=b''
  while b'\r\n\r\n' not in data:data+=exactly(sock,1)
  headers={p.split(':',1)[0].lower():p.split(':',1)[1].strip() for p in data.decode().split('\r\n')[1:] if ':' in p}
  accept=base64.b64encode(hashlib.sha1((headers['sec-websocket-key']+'258EAFA5-E914-47DA-95CA-C5AB0DC85B11').encode()).digest())
  sock.sendall(b'HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: '+accept+b'\r\n\r\n')
  while True:
   opcode,b=frame(sock)
   if opcode==8:break
   if opcode!=1:continue
   v=json.loads(b);observations.append({'direction':'upstream','raw':b.decode()})
   if v['type']=='hello':send_frame(sock,b'{"type":"hello_ok","protocolVersion":"1.2","serverVersion":"observer","serverInfo":{},"interactionBinding":{"connectionId":"observer","generation":1}}')
   elif v.get('id')=='observer-fence':send_frame(sock,b'{"type":"response","id":"observer-fence","ok":true,"result":{}}')
   else:errors.append('Unexpected forwarded request '+b.decode())
 except EOFError:pass
 except Exception as e:errors.append(repr(e))
 finally:sock.close()
def serve():
 while not stop.is_set():
  try:s,_=listener.accept()
  except socket.timeout:continue
  threading.Thread(target=observe,args=(s,),daemon=True).start()
thread=threading.Thread(target=serve);thread.start()
try:
 for row in fixture['cases']:
  if not native_case(row):continue
  if not row['process']['alive'] and lock.poll() is None:lock.stdin.close();lock.wait(timeout=5)
  case=out/('case-'+str(len(rows)));case.mkdir();replace=lambda s:s.replace('/fixture',str(case))
  for path,fact in row['files'].items():
   if fact.get('absent'):continue
   target=Path(replace(path))
   if fact['directory']:target.mkdir(parents=True,exist_ok=True)
   else:
    target.parent.mkdir(parents=True,exist_ok=True)
    if 'bytes' not in fact:
     with target.open('wb')as f:f.truncate(fact['size'])
    else:
     b=base64.b64decode(fact['bytes']).replace(b'/fixture',str(case).encode())
     if path.endswith('project-bindings.json.lock'):
      old=row['process']['pid'];s=b.decode('utf8',errors='replace')
      for prefix,fmt in [('0x','x'),('0o','o'),('0b','b')]:
       if s==prefix+format(old,fmt):s=prefix+format(lock.pid,fmt);break
      else:s=s.replace(str(old),str(lock.pid))
      b=s.encode()
     target.write_bytes(b)
  permission_path=None
  if 'io' in row:
   target=Path(replace(row['io']['target']));scenario=row['io']['scenario']
   if scenario.endswith('denied'):
    permission_path=target.parent if scenario.startswith('stat') else target
    permission_path.chmod(0)
   elif scenario.endswith('missing'):target.unlink()
   elif scenario.endswith('notdir'):
    target.parent.rename(target.parent.with_name(target.parent.name+'.held'))
    target.parent.write_text('parent replaced')
   elif scenario.endswith('directory'):target.unlink();target.mkdir()
   elif scenario.endswith('longname'):target.unlink();target.symlink_to('x'*300)
   elif scenario.endswith('loop'):target.unlink();target.symlink_to(target)
   elif scenario.endswith('deep'):
    target.unlink()
    for i in range(80):
     link=Path(str(target)+'.chain.'+str(i));link.symlink_to(str(target)+('.end' if i==79 else '.chain.'+str(i+1)))
    Path(str(target)+'.end').write_text('');target.symlink_to(str(target)+'.chain.0')
   elif scenario=='stat-long-session':pass
  runenv=env.copy();runenv.update({k:replace(v) for k,v in row['env'].items()});runenv.update(PILOTDECK_PROXY_LISTEN='127.0.0.1:28861',PILOTDECK_PROXY_UPSTREAM='http://127.0.0.1:28860')
  log=(case/'proxy.log').open('w');proc=subprocess.Popen([str(root/'src/target/debug/pilotdeck-gateway')],env=runenv,cwd=case,stdout=log,stderr=subprocess.STDOUT)
  processes.append({'id':row['id'],'pid':proc.pid,'argv':[str(root/'src/target/debug/pilotdeck-gateway')],'cwd':str(case),'env':{k:runenv[k] for k in ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR']}})
  sock=None
  try:
   end=time.monotonic()+5
   while True:
    if proc.poll() is not None:raise RuntimeError('Owned Rust proxy exited before handshake')
    try:sock=socket.create_connection(('127.0.0.1',28861),timeout=.1);break
    except OSError:
     if time.monotonic()>end:raise
     time.sleep(.01)
   sock.settimeout(5)
   sock.sendall(b'GET /ws HTTP/1.1\r\nHost: 127.0.0.1:28861\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: '+base64.b64encode(os.urandom(16))+b'\r\nSec-WebSocket-Version: 13\r\n\r\n')
   headers=b''
   while b'\r\n\r\n' not in headers:headers+=exactly(sock,1)
   assert b'101' in headers
   send_frame(sock,b'{"type":"hello","protocolVersion":"1.2","clientName":"web","clientVersion":"probe","token":"fixture"}',True);frame(sock)
   before=len(observations);request=replace(row['request']).encode();send_frame(sock,request,True);_,actual=frame(sock)
   send_frame(sock,b'{"type":"request","id":"observer-fence","method":"describe_server"}',True);_,fence=frame(sock)
   assert json.loads(fence)['id']=='observer-fence'
   visible=observations[before:];assert len(visible)==1 and json.loads(visible[0]['raw'])['id']=='observer-fence',visible
   normalized=actual.decode().replace(str(case),'/fixture');expected=replace(row['response'])
   if 'invalidPath' in row:
    # Node's ERR_INVALID_ARG_VALUE truncates inspected paths at 128 UTF-16
    # units. Replacing roots after truncation is not a valid normalization.
    # Get the real Node fs error at this run's path; NUL fails before a syscall.
    source="const fs=require('node:fs');if(process.version!=='v22.22.0')throw Error('Node v22.22.0 required');const v=JSON.parse(fs.readFileSync(0,'utf8'));if(!v.path.includes('\\0'))throw Error('NUL required');try{fs.statSync(v.path);throw Error('NUL accepted');}catch(e){if(e.code!=='ERR_INVALID_ARG_VALUE')throw e;v.frame.error={code:e.code,message:e.message};process.stdout.write(JSON.stringify(v.frame));}"
    expected=subprocess.check_output(['node','-e',source],input=json.dumps({'path':replace(row['invalidPath']),'frame':json.loads(expected)}).encode(),env=runenv).decode()
    byte_equal=actual.decode()==expected
   else:byte_equal=normalized==row['response']
   result={'id':row['id'],'request':request.decode(),'actual':actual.decode(),'expected':expected,'byteEqual':byte_equal,'upstreamObserved':visible}
   rows.append(result)
   assert result['byteEqual'],result
  finally:
   if sock:sock.close()
   proc.send_signal(signal.SIGINT);proc.wait(timeout=5);log.close();processes[-1]['exitCode']=proc.returncode
   if permission_path:permission_path.chmod(0o700)
  print(row['id'],'PASS',flush=True)
except Exception as e:
 errors.append(repr(e));raise
finally:
 if lock.poll() is None:lock.stdin.close();lock.wait(timeout=5)
 stop.set();thread.join(timeout=2);listener.close()
 (out/'observations.json').write_text(json.dumps({'passed':not errors and len(rows)==sum(native_case(r) for r in fixture['cases']) and all(r['byteEqual'] for r in rows),'fixtureSha256':hashlib.sha256(fixture_bytes).hexdigest(),'ioFixtureSha256':hashlib.sha256(io_fixture_bytes).hexdigest(),'ioRound5FixtureSha256':hashlib.sha256(io_round5_bytes).hexdigest(),'cases':rows,'excluded':[r['id'] for r in fixture['cases'] if not native_case(r)],'errors':errors,'processes':processes,'ownedLockHolder':{'pid':lock.pid,'exitCode':lock.returncode}},indent=2)+'\n')
 assert not errors,errors
