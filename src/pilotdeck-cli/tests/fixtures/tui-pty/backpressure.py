#!/usr/bin/env python3
"""Verify undrained PTY output cannot hang the runner or leave terminal flags changed."""
import argparse,fcntl,json,os,pty,select,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--node',type=Path,required=True);p.add_argument('--ts-root',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
root=a.output.resolve();root.mkdir();home=root/'home';project=root/'project';home.mkdir();project.mkdir();wire=root/'wire.jsonl';wire.touch();raw=bytearray();children=[];failure=None;before=after=None;original_flags=after_flags=None;rc=None;queries=0
env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1'}
server=subprocess.Popen([str(a.node.resolve()),str(Path(__file__).with_name('scenarios-server.mjs')),str(a.ts_root.resolve()),str(wire),'flood'],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children.append(server)
try:
 ready,_,_=select.select([server.stdout],[],[],8);assert ready;port=int(server.stdout.readline());assert port!=18789
 (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('pty-fixture-token\n')
 master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0));before=termios.tcgetattr(slave);original_flags=fcntl.fcntl(slave,fcntl.F_GETFL)
 child=subprocess.Popen([str(a.binary.resolve()),'tui'],cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);children.append(child);deadline=time.monotonic()+25
 def drain():
  global queries
  ready,_,_=select.select([master],[],[],.02)
  if ready:
   try:raw.extend(os.read(master,65536))
   except OSError:pass
   count=bytes(raw).count(b'\x1b[6n')
   while queries<count:os.write(master,b'\x1b[1;1R');queries+=1
 def calls(method):return [r['in'] for r in (json.loads(x) for x in wire.read_text().splitlines()) if r.get('in',{}).get('method')==method]
 def wait(pred,label):
  while time.monotonic()<deadline:
   drain()
   if pred():return
   if child.poll() is not None:raise RuntimeError(f'{label}: exit {child.returncode}')
  raise TimeoutError(label)
 wait(lambda:bool(calls('list_sessions')),'startup');os.write(master,b'flood');end=time.monotonic()+.4
 while time.monotonic()<end:drain()
 os.write(master,b'\r');wait(lambda:bool(calls('submit_turn')),'submit')
 # Intentionally do not read from the PTY after this point.
 end=time.monotonic()+12
 while child.poll() is None and time.monotonic()<end:time.sleep(.05)
 if child.poll() is None:
  os.write(master,b'\x03');end=time.monotonic()+3
  while child.poll() is None and time.monotonic()<end:time.sleep(.05)
 assert child.poll() is not None,'undrained output blocked runner'
 rc=child.returncode;after=termios.tcgetattr(slave);after_flags=fcntl.fcntl(slave,fcntl.F_GETFL)
 for _ in range(10):drain()
 assert rc==1,f'expected bounded output failure exit 1, got {rc}'
 assert before==after,'termios not restored';assert (original_flags & os.O_NONBLOCK)==(after_flags & os.O_NONBLOCK),'NONBLOCK not restored'
except BaseException as e:failure=repr(e)
finally:
 for process in reversed(children):
  if process.poll() is None:
   process.terminate()
   try:process.wait(timeout=4)
   except subprocess.TimeoutExpired:process.kill();process.wait(timeout=4)
 if 'master' in globals():os.close(master);os.close(slave)
 (root/'terminal.bin').write_bytes(raw);(root/'server.stderr').write_bytes(server.stderr.read());(root/'result.json').write_text(json.dumps({'pass':failure is None,'failure':failure,'rc':rc,'termios_restored':before==after if after is not None else None,'original_flags':original_flags,'after_flags':after_flags,'children':[(c.pid,c.returncode) for c in children]},indent=2)+'\n');print(json.dumps({'pass':failure is None,'failure':failure,'rc':rc}));raise SystemExit(0 if failure is None else 1)
