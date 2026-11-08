#!/usr/bin/env python3
"""Rust TUI sidebar PTY against a loopback fake Gateway."""
import argparse,fcntl,hashlib,json,os,pty,select,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--bin',required=True,type=Path);p.add_argument('--narrow',action='store_true');p.add_argument('--slow',action='store_true');p.add_argument('--large',action='store_true');p.add_argument('--node',required=True,type=Path);p.add_argument('--ts-root',required=True,type=Path);p.add_argument('--out',required=True,type=Path);a=p.parse_args()
base=a.ts_root.resolve();node=a.node.resolve();out=a.out.resolve();out.mkdir();home=out/'home';project=out/'project';home.mkdir();project.mkdir();wire=out/'wire.jsonl';wire.touch();raw=bytearray();children=[];failure=None;queries=0;before=after=None;exited=None
assert subprocess.check_output([str(node),'-p','process.version+" "+process.versions.icu'],text=True).strip()=='v22.22.0 78.2'
assert subprocess.check_output(['git','-C',str(base),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1'}
server=subprocess.Popen([str(node),str(Path(__file__).with_name('server.mjs')),str(base),str(wire),'large' if a.large else 'slow' if a.slow else 'normal'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children.append(server)
try:
 ready,_,_=select.select([server.stdout],[],[],8);assert ready;port=int(server.stdout.readline());assert 0<port<65536 and port!=18789
 (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('pty-fixture-token\n')
 master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,50 if a.narrow else 100,0,0));before=termios.tcgetattr(slave)
 child=subprocess.Popen([str(a.bin.resolve()),'tui'],cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);children.append(child);deadline=time.monotonic()+35
 def drain():
  global queries
  ready,_,_=select.select([master],[],[],.03)
  if ready:
   try:raw.extend(os.read(master,65536))
   except OSError:pass
   count=bytes(raw).count(b'\x1b[6n')
   while queries<count:os.write(master,b'\x1b[1;1R');queries+=1
 def pause(s):
  until=time.monotonic()+s
  while time.monotonic()<until:drain()
 def rows():return [json.loads(x) for x in wire.read_text().splitlines()]
 def wait(pred,label):
  while time.monotonic()<deadline:
   drain()
   if pred():return
   if child.poll() is not None:raise RuntimeError(f'{label}: exit {child.returncode}')
  raise TimeoutError(label)
 def type_line(text):os.write(master,text);pause(.5);os.write(master,b'\r');pause(.5)
 wait(lambda:any(r.get('in',{}).get('method')=='list_sessions' for r in rows()),'startup')
 def snap(label):pause(.4);(out/(label+'.bin')).write_bytes(raw)
 def key(data):os.write(master,data);pause(.5)
 initial=len([r for r in rows() if r.get('in',{}).get('method')=='list_sessions'])
 if a.large:
  start=time.monotonic();key(b'\x05');wait(lambda:b'item-000' in raw,'large session list visible');shown=time.monotonic()-start
  assert shown<10, f'large list display took {shown:.3f}s'
  key(b'\x03');assert child.poll() is None,'first Ctrl-C must close sidebar'
  type_line(b'large-response-check');wait(lambda:any(r.get('in',{}).get('method')=='submit_turn' for r in rows()),'input after large list')
  wait(lambda:any(r.get('out',{}).get('final') for r in rows()),'turn completed');pause(.4);os.write(master,b'\x03');wait(lambda:child.poll() is not None,'exit')
  after=termios.tcgetattr(slave);exited=child.poll();assert before==after,'terminal not restored';assert exited==0
  (out/'large-duration.json').write_text(json.dumps({'display_seconds':shown})+'\n')
 else:
  key(b'\x05');
  if a.slow:
   key(b'\x05');assert len([r for r in rows() if r.get('in',{}).get('method')=='list_sessions'])==initial+1,'duplicate pending list'
  wait(lambda:len([r for r in rows() if r.get('in',{}).get('method')=='list_sessions'])>initial,'sidebar list');wait(lambda:b'alpha' in raw and b'beta' in raw,'visible project groups');snap('opened')
  key(b'\x13');wait(lambda:b'running' in raw and b'idle' in raw,'visible status groups');snap('status')
  key(b'\x1b[C');wait(lambda:b'ALPHA_SESSION' in raw,'custom title visible');snap('expand')
  key(b'\x1b[D');snap('collapse')
  key(b'\x1b[C');key(b'\x1b[B');snap('select')
  key(b'\r');snap('enter')
  wait(lambda:any(r.get('in',{}).get('method')=='read_session_messages' for r in rows()),'history request')
  calls=[r['in'] for r in rows() if r.get('in',{}).get('method') in ('resume_session','read_session_messages')]
  assert {c['method'] for c in calls}=={'resume_session','read_session_messages'} and all(c['params']['sessionKey']=='session-a' for c in calls)
  assert not any(r.get('in',{}).get('method')=='submit_turn' for r in rows()),'control key leaked into prompt'
  (out/'selection.json').write_text(json.dumps(calls,indent=2)+'\n')
  key(b'\x05');
  if a.slow:pause(1.4)
  snap('toggle');key(b'\x03');assert child.poll() is None,'sidebar Ctrl-C exited instead of closing'
  os.write(master,b'nonempty');pause(.3);key(b'\x05');wait(lambda:len([r for r in rows() if r.get('in',{}).get('method')=='list_sessions'])>=initial+3,'sidebar reopened')
  if a.slow:pause(1.4)
  key(b'-batch');key(b'\x13');key(b'\x1b');key(b'\r');wait(lambda:any(r.get('in',{}).get('method')=='submit_turn' for r in rows()),'submit preserved input')
  submit=next(r['in'] for r in rows() if r.get('in',{}).get('method')=='submit_turn');assert submit['params']['message']=='nonempty-batch',submit
  wait(lambda:any(r.get('out',{}).get('final') for r in rows()),'turn completed');pause(.4);os.write(master,b'\x03');wait(lambda:child.poll() is not None,'exit');after=termios.tcgetattr(slave);exited=child.poll();assert before==after,'terminal not restored';assert exited==0

except BaseException as e:failure=repr(e)
finally:
 for process in reversed(children):
  if process.poll() is None:process.terminate()
  try:process.wait(timeout=4)
  except subprocess.TimeoutExpired:process.kill();process.wait(timeout=4)
 if 'master' in globals():
  for _ in range(5):drain()
  os.close(master);os.close(slave)
 (out/'terminal.bin').write_bytes(raw);(out/'server.stderr').write_bytes(server.stderr.read());result={'observed':failure is None,'failure':failure,'exited_after_ctrl_c':exited,'termios_restored':before==after if after is not None else None,'children':[(c.pid,c.returncode) for c in children],'binary_sha256':hashlib.sha256(a.bin.read_bytes()).hexdigest(),'source_commit':'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85','source_sha256':hashlib.sha256((base/'src/adapters/channel/tui/app/TuiApp.tsx').read_bytes()).hexdigest()};(out/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result));raise SystemExit(0 if failure is None else 1)
