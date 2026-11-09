#!/usr/bin/env python3
"""Rust help and settings PTY against loopback Gateway."""
import argparse,fcntl,hashlib,json,os,pty,select,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--bin',required=True,type=Path);p.add_argument('--fail',action='store_true');p.add_argument('--case',choices=['base','restart','small','permission','permission-abort','long-session'],default='base');p.add_argument('--node',required=True,type=Path);p.add_argument('--ts-root',required=True,type=Path);p.add_argument('--out',required=True,type=Path);a=p.parse_args()
base=a.ts_root.resolve();node=a.node.resolve();out=a.out.resolve();out.mkdir();home=out/'home';project=out/'project';home.mkdir();project.mkdir();wire=out/'wire.jsonl';wire.touch();raw=bytearray();children=[];failure=None;queries=0;before=after=None;exited=None
assert subprocess.check_output([str(node),'-p','process.version+" "+process.versions.icu'],text=True).strip()=='v22.22.0 78.2'
assert subprocess.check_output(['git','-C',str(base),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1'}
server=subprocess.Popen([str(node),str(Path(__file__).with_name('help-server.mjs')),str(base),str(wire),a.case],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children.append(server)
try:
 ready,_,_=select.select([server.stdout],[],[],8);assert ready;port=int(server.stdout.readline());assert 0<port<65536 and port!=18789
 (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('pty-fixture-token\n')
 master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',12 if a.case in ('small','long-session') else 30,34 if a.case in ('small','long-session') else 100,0,0));before=termios.tcgetattr(slave)
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
 def screen():
  # Decode cursor-addressed ASCII cells from the recorded PTY output, retaining
  # only the current screen rather than accepting a stale earlier frame.
  import re
  width,height=(34,12) if a.case in ('small','long-session') else (100,30)
  cells=[[' ']*width for _ in range(height)];x=y=0
  data=bytes(raw).decode('utf-8','replace');i=0
  while i<len(data):
   if data[i]=='\x1b' and i+1<len(data) and data[i+1]=='[':
    m=re.match(r'\x1b\[([0-9;?]*)([ -/]*)([@-~])',data[i:])
    if m:
     arg=m.group(1);op=m.group(3);nums=[int(v) if v else 0 for v in arg.split(';')] if not arg.startswith('?') else []
     if op in 'Hf':y=max(0,(nums[0] if nums else 1)-1);x=max(0,(nums[1] if len(nums)>1 else 1)-1)
     elif op=='A':y=max(0,y-(nums[0] if nums and nums[0] else 1))
     elif op=='B':y=min(height-1,y+(nums[0] if nums and nums[0] else 1))
     elif op=='C':x=min(width-1,x+(nums[0] if nums and nums[0] else 1))
     elif op=='D':x=max(0,x-(nums[0] if nums and nums[0] else 1))
     elif op=='J' and nums and nums[0]==2:cells=[[' ']*width for _ in range(height)]
     elif op=='K' and y<height:
      for n in range(x,width):cells[y][n]=' '
     i+=len(m.group(0));continue
   c=data[i];i+=1
   if c=='\r':x=0
   elif c=='\n':y=min(height-1,y+1)
   elif c.isprintable():
    if y<height and x<width:cells[y][x]=c
    x=min(width,x+1)
  return '\n'.join(''.join(row) for row in cells)

 wait(lambda:any(r.get('in',{}).get('method')=='list_sessions' for r in rows()),'startup')
 def key(data):os.write(master,data);pause(.5)
 if a.case=='long-session':
  type_line(b'/new');wait(lambda:any(r.get('out',{}).get('result',{}).get('sessionKey','').startswith('SSSS') for r in rows()),'large session');pause(.8)
  key(b'?');key(b'2');wait(lambda:'Settings' in screen(),'long-session settings');key(b'q');os.write(master,b'\x03');wait(lambda:child.poll() is not None,'long-session exit');assert child.returncode==0
 elif a.case in ('permission','permission-abort'):
  type_line(b'permission test');wait(lambda:any(r.get('in',{}).get('method')=='submit_turn' for r in rows()),'turn submitted');key(b'?')
  wait(lambda:any(r.get('out',{}).get('event',{}).get('type')=='permission_request' for r in rows()),'permission requested')
  pause(.5);key(b'y' if a.case=='permission' else b'\x03')
  method='permission_decide' if a.case=='permission' else 'abort_turn'
  wait(lambda:any(r.get('in',{}).get('method')==method for r in rows()),method)
  assert not any(r.get('in',{}).get('method')==('abort_turn' if a.case=='permission' else 'permission_decide') for r in rows())
  wait(lambda:any(r.get('out',{}).get('event',{}).get('type')=='turn_completed' for r in rows()),'turn completion');pause(.5);key(b'q')
 elif a.case=='small':
  key(b'?');wait(lambda:b'Shortcuts' in raw,'shortcuts')
  for _ in range(10):key(b'\x1b[6~')
  wait(lambda:b'viewer' in raw.lower(),'bottom of shortcuts')
  key(b'3');wait(lambda:b'About' in raw,'about')
  for _ in range(8):key(b'\x1b[6~')
  wait(lambda:b'Permission' in raw,'bottom of about');key(b'q')
 else:
  if a.fail: (home/'permissions.json').mkdir()
  key(b'?');wait(lambda:b'Shortcuts' in raw,'shortcuts')
  key(b'2');wait(lambda:b'Settings' in raw,'settings')
  key(b'\x1b[C');pause(.3);key(b'\r')
  if a.fail:
   wait(lambda:'Save failed; mode unchanged' in screen(),'visible save failure');assert (home/'permissions.json').is_dir()
   key(b'q');type_line(b'after failed save');wait(lambda:any(r.get('out',{}).get('final') for r in rows()),'turn after failed save');pause(.4)
   submit=next(r['in'] for r in rows() if r.get('in',{}).get('method')=='submit_turn');assert submit['params']['mode']=='default';assert (home/'permissions.json').is_dir()
   key(b'?')
  else:
   assert json.loads((home/'permissions.json').read_text())['skipPermissions'] is False
   key(b'\r');assert json.loads((home/'permissions.json').read_text())['skipPermissions'] is True
   if a.case=='restart':
    key(b'q');os.write(master,b'\x03');wait(lambda:child.poll() is not None,'first exit');assert child.returncode==0;assert before==termios.tcgetattr(slave)
    old_startups=sum(r.get('in',{}).get('method')=='list_sessions' for r in rows())
    child=subprocess.Popen([str(a.bin.resolve()),'tui'],cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);children.append(child)
    wait(lambda:sum(r.get('in',{}).get('method')=='list_sessions' for r in rows())>old_startups,'restart')
    type_line(b'restarted');wait(lambda:any(r.get('in',{}).get('method')=='submit_turn' for r in rows()),'restart submit')
    submit=next(r['in'] for r in rows() if r.get('in',{}).get('method')=='submit_turn');assert submit['params']['mode']=='bypassPermissions'
   else:
    key(b'q');type_line(b'hello');wait(lambda:any(r.get('out',{}).get('final') for r in rows()),'bypass turn');pause(.4)
    submit=next(r['in'] for r in rows() if r.get('in',{}).get('method')=='submit_turn');assert submit['params']['mode']=='bypassPermissions'
    key(b'?');key(b'2');key(b'\r');assert json.loads((home/'permissions.json').read_text())['skipPermissions'] is False
    key(b'\x1b[B');key(b'\r');assert json.loads((home/'permissions.json').read_text())['skipPermissions'] is False
  if a.case!='restart':key(b'3');wait(lambda:b'Gateway' in raw,'about');key(b'q')
 if child.poll() is None:os.write(master,b'\x03');wait(lambda:child.poll() is not None,'exit')
 after=termios.tcgetattr(slave);exited=child.poll();assert before==after,'terminal not restored';assert exited==0

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
