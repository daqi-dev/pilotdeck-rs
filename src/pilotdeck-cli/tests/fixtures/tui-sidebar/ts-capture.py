#!/usr/bin/env python3
"""Fixed TS TUI: actual tool focus/short output/viewer PTY recording."""
import argparse,fcntl,hashlib,json,os,pty,select,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--narrow',action='store_true');p.add_argument('--node',required=True,type=Path);p.add_argument('--ts-root',required=True,type=Path);p.add_argument('--out',required=True,type=Path);a=p.parse_args()
base=a.ts_root.resolve();node=a.node.resolve();out=a.out.resolve();out.mkdir();home=out/'home';project=out/'project';home.mkdir();project.mkdir();wire=out/'wire.jsonl';wire.touch();raw=bytearray();children=[];failure=None;queries=0;before=after=None;exited=None
assert subprocess.check_output([str(node),'-p','process.version+" "+process.versions.icu'],text=True).strip()=='v22.22.0 78.2'
assert subprocess.check_output(['git','-C',str(base),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1'}
server=subprocess.Popen([str(node),str(Path(__file__).with_name('ts-server.mjs')),str(base),str(wire)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children.append(server)
try:
 ready,_,_=select.select([server.stdout],[],[],8);assert ready;port=int(server.stdout.readline());assert 0<port<65536 and port!=18789
 (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('pty-fixture-token\n')
 master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,50 if a.narrow else 100,0,0));before=termios.tcgetattr(slave)
 child=subprocess.Popen([str(node),'--import',str(base/'node_modules/tsx/dist/loader.mjs'),str(base/'src/cli/pilotdeck.ts'),'tui'],cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);children.append(child);deadline=time.monotonic()+35
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
 key(b'\x05');wait(lambda:len([r for r in rows() if r.get('in',{}).get('method')=='list_sessions'])>initial,'sidebar list');snap('opened')
 key(b'\x13');snap('status')
 key(b'\x1b[C');snap('expand')
 key(b'\x1b[D');snap('collapse')
 key(b'\x1b[C');key(b'\x1b[B');snap('select')
 key(b'\r');snap('enter')
 calls=[r['in'] for r in rows() if r.get('in',{}).get('method') in ('resume_session','read_session_messages')]
 (out/'selection.json').write_text(json.dumps(calls,indent=2)+'\n')
 key(b'\x05');snap('toggle')
 os.write(master,b'\x03');pause(1);after=termios.tcgetattr(slave);exited=child.poll();assert before==after,'terminal not restored'

except BaseException as e:failure=repr(e)
finally:
 for process in reversed(children):
  if process.poll() is None:process.terminate()
  try:process.wait(timeout=4)
  except subprocess.TimeoutExpired:process.kill();process.wait(timeout=4)
 if 'master' in globals():
  for _ in range(5):drain()
  os.close(master);os.close(slave)
 (out/'terminal.bin').write_bytes(raw);(out/'server.stderr').write_bytes(server.stderr.read());result={'observed':failure is None,'failure':failure,'exited_after_ctrl_c':exited,'termios_restored':before==after if after is not None else None,'children':[(c.pid,c.returncode) for c in children],'source_commit':'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85','source_sha256':hashlib.sha256((base/'src/adapters/channel/tui/app/TuiApp.tsx').read_bytes()).hexdigest()};(out/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result));raise SystemExit(0 if failure is None else 1)
