#!/usr/bin/env python3
import argparse,fcntl,hashlib,json,os,pty,select,signal,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--bin',required=True,type=Path);p.add_argument('--node',required=True,type=Path);p.add_argument('--ts-root',required=True,type=Path);p.add_argument('--out',required=True,type=Path);p.add_argument('--case',choices=['unterminated','oversize','help-overflow','active-overflow'],required=True);a=p.parse_args()
out=a.out.resolve();out.mkdir();home=out/'home';project=out/'project';home.mkdir();project.mkdir();wire=out/'wire.jsonl';wire.touch();env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1'}
server=subprocess.Popen([str(a.node),str(Path(__file__).with_name('stress-server.mjs')),str(a.ts_root),str(wire)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children=[server];raw=bytearray();before=after=None;failure=None;notes={};master=slave=None
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
 if a.case=='help-overflow':
  os.write(master,b'?');wait(lambda:b'Shortcuts' in raw,'help visible')
 else:
  os.write(master,b'probe');time.sleep(.3);os.write(master,b'\r');wait(lambda:any(x.get('in',{}).get('method')=='submit_turn' for x in rows()),'submit')
 payload=b'\x1b[200~'+b'x'*(128*1024)+(b'\x1b[201~' if a.case!='unterminated' else b'');sent=0;started=time.monotonic();until=started+2
 while sent<len(payload) and time.monotonic()<until:
  drain()
  if select.select([],[master],[],.01)[1]:
   try:sent+=os.write(master,payload[sent:sent+4096])
   except BlockingIOError:pass
 notes.update(sent=sent,total=len(payload),send_seconds=time.monotonic()-started)
 assert sent==len(payload),f"only sent {sent} of {len(payload)} bytes"
 if a.case in ('help-overflow','active-overflow'):
  pause_until=time.monotonic()+1
  while time.monotonic()<pause_until:drain();time.sleep(.01)
  if a.case=='help-overflow':os.write(master,b'q');time.sleep(.3)
  os.write(master,b'after');time.sleep(.3);os.write(master,b'\r')
  wait(lambda:sum(r.get('in',{}).get('method')=='submit_turn' for r in rows())==(2 if a.case=='active-overflow' else 1),'submit after modal overflow')
  calls=[r['in'] for r in rows() if r.get('in',{}).get('method')=='submit_turn'];assert calls[-1]['params']['message']=='after' and len(calls)==(2 if a.case=='active-overflow' else 1),calls
  wait(lambda:sum(r.get('out',{}).get('final') is True for r in rows())==(2 if a.case=='active-overflow' else 1),'turn completed');os.write(master,b'\x03');wait(lambda:child.poll() is not None,'exit');notes.update(exited=child.returncode,termios_restored=before==termios.tcgetattr(slave));assert notes['exited']==0 and notes['termios_restored']
 else:
  try:os.write(master,b'\x03');notes['ctrl_c_written']=True
  except BlockingIOError:notes['ctrl_c_written']=False
  end=time.monotonic()+2;second=False
  while time.monotonic()<end and child.poll() is None:
   drain()
   if not second and time.monotonic()-started>1.1:
    try:os.write(master,b'\x03');second=True
    except BlockingIOError:pass
   time.sleep(.01)
  notes['second_ctrl_c_written']=second
  notes.update(abort_count=sum(r.get('in',{}).get('method')=='abort_turn' for r in rows()),exited=child.poll(),pong=any(r.get('pong') for r in rows()),flow_sent=any(r.get('out',{}).get('event',{}).get('text')=='FLOW_MARKER' for r in rows()),flow_visible=b'FLOW_MARKER' in raw,termios_restored=before==termios.tcgetattr(slave))
  if a.case=='unterminated':assert notes['abort_count']==0, 'paste cancellation sent abort'
  assert notes['exited']==0 and notes['termios_restored'] and notes['pong'] and notes['flow_visible'],notes
except BaseException as e:failure=repr(e)
finally:
 for process in reversed(children):
  if process.poll() is None:process.terminate()
  try:process.wait(timeout=2)
  except subprocess.TimeoutExpired:process.kill();process.wait(timeout=2)
 if master is not None:
  drain();after=termios.tcgetattr(slave);os.close(master);os.close(slave)
 (out/'terminal.bin').write_bytes(raw);(out/'server.stderr').write_bytes(server.stderr.read());(out/'result.json').write_text(json.dumps({'failure':failure,'case':a.case,'notes':notes,'after_cleanup_termios_restored':before==after,'children':[(c.pid,c.returncode) for c in children],'binary_sha256':hashlib.sha256(a.bin.read_bytes()).hexdigest()},indent=2)+'\n');print((out/'result.json').read_text());raise SystemExit(0 if failure is None else 1)
