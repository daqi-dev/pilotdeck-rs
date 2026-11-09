#!/usr/bin/env python3
import argparse,fcntl,hashlib,json,os,pty,select,signal,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--bin',required=True,type=Path);p.add_argument('--node',required=True,type=Path);p.add_argument('--ts-root',required=True,type=Path);p.add_argument('--out',required=True,type=Path);p.add_argument('--case',choices=['empty','exact','over','permission-overflow','raw-crlf','late-marker','long-csi','permission-raw','active-long-csi'],required=True);a=p.parse_args()
out=a.out.resolve();out.mkdir();home=out/'home';project=out/'project';home.mkdir();project.mkdir();wire=out/'wire.jsonl';wire.touch();env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1'}
server=subprocess.Popen([str(a.node),str(Path(__file__).with_name('boundary-server.mjs')),str(a.ts_root),str(wire)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children=[server];raw=bytearray();before=after=None;failure=None;notes={};master=slave=None
try:
 assert select.select([server.stdout],[],[],8)[0];port=int(server.stdout.readline());assert port!=18789
 (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('pty-fixture-token\n')
 master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0));before=termios.tcgetattr(slave);os.set_blocking(master,False)
 child=subprocess.Popen([str(a.bin),'tui'],cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);children.append(child)
 queries=0
 def drain():
  global queries
  try:
   while True:raw.extend(os.read(master,65536))
  except (BlockingIOError,OSError):pass
  count=bytes(raw).count(b'\x1b[6n')
  while queries<count:
   try:os.write(master,b'\x1b[1;1R');queries+=1
   except BlockingIOError:break
 def rows():return [json.loads(line) for line in wire.read_text().splitlines()]
 def wait(fn,label,seconds=8):
  until=time.monotonic()+seconds
  while time.monotonic()<until:
   drain()
   if fn():return
   if child.poll() is not None:raise RuntimeError(f'{label}: child {child.returncode}')
   time.sleep(.01)
  raise TimeoutError(label)
 wait(lambda:any(x.get('in',{}).get('method')=='list_sessions' for x in rows()),'startup')
 if a.case in ('permission-overflow','permission-raw'):
  os.write(master,b'permission-test');time.sleep(.3);os.write(master,b'\r');wait(lambda:any(r.get('out',{}).get('event',{}).get('type')=='permission_request' for r in rows()),'permission request')
 if a.case=='active-long-csi':
  os.write(master,b'active-test');time.sleep(.3);os.write(master,b'\r');wait(lambda:any(r.get('in',{}).get('method')=='submit_turn' for r in rows()),'active turn')
 if a.case=='empty':os.write(master,b'prefix');time.sleep(.2)
 n={'empty':0,'exact':65536,'over':65537,'permission-overflow':131072,'raw-crlf':0,'late-marker':0,'long-csi':0,'permission-raw':0,'active-long-csi':0}[a.case]
 if a.case=='late-marker':os.write(master,b'\x1b');time.sleep(.4)
 payload=(b'yn' if a.case=='permission-raw' else b'\x1b['+b'1'*40 if a.case=='active-long-csi' else b'alpha\r\nbeta' if a.case=='raw-crlf' else b'[200~alpha\r\nbeta\x1b[201~' if a.case=='late-marker' else b'\x1b['+b'1'*40+b'~' if a.case=='long-csi' else b'\x1b[200~'+b'x'*n+b'\x1b[201~')
 sent=0;end=time.monotonic()+2
 while sent<len(payload) and time.monotonic()<end:
  drain()
  if select.select([],[master],[],.01)[1]:
   try:sent+=os.write(master,payload[sent:sent+4096])
   except BlockingIOError:pass
 notes.update(sent=sent,total=len(payload));assert sent==len(payload),'incomplete paste write'
 settle=time.monotonic()+1
 while time.monotonic()<settle:drain();time.sleep(.01)
 calls=lambda:[r['in'] for r in rows() if r.get('in',{}).get('method')=='submit_turn']
 initial=1 if a.case in ('permission-overflow','permission-raw','active-long-csi') else 0
 if a.case=='raw-crlf':
  notes['before_explicit_enter']=[x['params']['message'] for x in calls()]
  assert not calls(), 'single-write CRLF submitted before explicit Enter'
  os.write(master,b'\r');wait(lambda:len(calls())==1,'raw CRLF submit');assert calls()[0]['params']['message']=='alpha\r\nbeta'
  wait(lambda:any(r.get('out',{}).get('final') is True for r in rows()),'final');time.sleep(.3);os.write(master,b'\x03');wait(lambda:child.poll() is not None,'exit');notes.update(exited=child.returncode,termios_restored=before==termios.tcgetattr(slave));assert notes['exited']==0 and notes['termios_restored']
 else:
  assert len(calls())==initial,'unexpected pre-Enter submit'
  if a.case in ('permission-overflow','permission-raw'):
   assert not any(r.get('in',{}).get('method')=='permission_decide' for r in rows()),'bulk y/n triggered permission'
   os.write(master,b'y');wait(lambda:any(r.get('in',{}).get('method')=='permission_decide' for r in rows()),'decision');wait(lambda:any(r.get('out',{}).get('final') for r in rows()),'permission turn ended');time.sleep(.3)
   os.write(master,b'after');time.sleep(.3);os.write(master,b'\r');wait(lambda:len(calls())==2,'submit after permission overflow');assert calls()[-1]['params']['message']=='after'
  elif a.case=='active-long-csi':
   os.write(master,b'\x03');time.sleep(.4);drain();assert not any(r.get('in',{}).get('method')=='abort_turn' for r in rows()),'sequence cancel aborted turn'
   os.write(master,b'\x03');wait(lambda:any(r.get('in',{}).get('method')=='abort_turn' for r in rows()),'turn abort');wait(lambda:any(r.get('out',{}).get('final') is True for r in rows()),'abort terminal');time.sleep(.3)
   os.write(master,b'after');time.sleep(.3);os.write(master,b'\r');wait(lambda:len(calls())==2,'submit after abort');assert calls()[-1]['params']['message']=='after'
  elif a.case in ('late-marker','long-csi'):
   os.write(master,b'\r');time.sleep(.4);drain();assert not calls(),'partial sequence submitted';os.write(master,b'after');time.sleep(.3);os.write(master,b'\r');wait(lambda:len(calls())==1,'submit after recovery');assert calls()[0]['params']['message']=='after'
  elif a.case=='over':
   os.write(master,b'\r');time.sleep(.5);drain();assert not calls(),'over-limit paste submitted';os.write(master,b'after');time.sleep(.3);os.write(master,b'\r');wait(lambda:len(calls())==1,'submit after rejection');assert calls()[0]['params']['message']=='after'
  else:
   os.write(master,b'\r');wait(lambda:len(calls())==1,'explicit submit');assert calls()[0]['params']['message']==('prefix' if a.case=='empty' else 'x'*65536)
  expected_finals=2 if a.case in ('permission-overflow','permission-raw','active-long-csi') else 1
  wait(lambda:sum(r.get('out',{}).get('final') is True for r in rows())>=expected_finals,'final');time.sleep(.3);os.write(master,b'\x03');wait(lambda:child.poll() is not None,'exit');notes.update(exited=child.returncode,termios_restored=before==termios.tcgetattr(slave));assert notes['exited']==0 and notes['termios_restored']

except BaseException as e:failure=repr(e)
finally:
 for process in reversed(children):
  if process.poll() is None:process.terminate()
  try:process.wait(timeout=2)
  except subprocess.TimeoutExpired:process.kill();process.wait(timeout=2)
 if master is not None:
  drain();after=termios.tcgetattr(slave);os.close(master);os.close(slave)
 (out/'terminal.bin').write_bytes(raw);(out/'server.stderr').write_bytes(server.stderr.read());(out/'result.json').write_text(json.dumps({'failure':failure,'case':a.case,'notes':notes,'after_cleanup_termios_restored':before==after,'children':[(c.pid,c.returncode) for c in children],'binary_sha256':hashlib.sha256(a.bin.read_bytes()).hexdigest()},indent=2)+'\n');print((out/'result.json').read_text());raise SystemExit(0 if failure is None else 1)
