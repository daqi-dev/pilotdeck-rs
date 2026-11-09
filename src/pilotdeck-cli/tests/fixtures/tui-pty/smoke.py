#!/usr/bin/env python3
"""Run the Rust TUI in a real PTY against an isolated loopback fake Gateway."""
import argparse,fcntl,json,os,pty,select,signal,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--node',type=Path,required=True);p.add_argument('--ts-root',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--small',action='store_true');a=p.parse_args()
root=a.output.resolve();root.mkdir();home=root/'home';project=root/'project';home.mkdir();project.mkdir();wire=root/'wire.jsonl';wire.touch()
env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1'}
server=subprocess.Popen([str(a.node.resolve()),str(Path(__file__).with_name('server.mjs')),str(a.ts_root.resolve()),str(wire)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env)
children=[server]; raw=bytearray(); failure=None; before=after=None; rc=None; cursor_queries=0
try:
 ready,_,_=select.select([server.stdout],[],[],8);assert ready,'server did not start';port=int(server.stdout.readline());assert 0<port<65536 and port!=18789
 (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n')
 (home/'server-token').write_text('pty-fixture-token\n')
 master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',1 if a.small else 30,2 if a.small else 100,0,0));before=termios.tcgetattr(slave)
 child=subprocess.Popen([str(a.binary.resolve()),'tui'],cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);children.append(child)
 deadline=time.monotonic()+25
 def drain():
  global cursor_queries
  ready,_,_=select.select([master],[],[],.05)
  if ready:
   try:raw.extend(os.read(master,65536))
   except OSError:pass
   count=bytes(raw).count(b'\x1b[6n')
   while cursor_queries<count:
    os.write(master,b'\x1b[1;1R');cursor_queries+=1
 def rows():return [json.loads(x) for x in wire.read_text().splitlines()]
 def wait_for(pred,label):
  while time.monotonic()<deadline:
   drain()
   if pred():return
   if child.poll() is not None:raise RuntimeError(f'{label}: exit {child.returncode}')
  raise TimeoutError(label)
 wait_for(lambda:any(r.get('in',{}).get('method')=='list_sessions' for r in rows()),'list sessions')
 os.write(master,b'hello pty')
 until=time.monotonic()+.5
 while time.monotonic()<until:drain()
 os.write(master,b'\r')
 wait_for(lambda:any(r.get('out',{}).get('final') is True for r in rows()),'final event')
 if not a.small:wait_for(lambda:b'PTY_REPLY' in raw,'render response')
 until=time.monotonic()+.5
 while time.monotonic()<until:drain()
 os.write(master,b'\x03');wait_for(lambda:child.poll() is not None,'idle ctrl-c exit');rc=child.returncode;after=termios.tcgetattr(slave)
 assert rc==0,rc;assert before==after,'terminal mode not restored'
 calls=[r['in'] for r in rows() if 'in' in r];submit=[r for r in calls if r.get('method')=='submit_turn'];assert len(submit)==1,submit
 expected={'sessionKey':f'tui:project={project}:default','channelKey':'tui','projectKey':str(project),'message':'hello pty','mode':'default'}
 assert submit[0]['params']==expected,submit
 wait_for(lambda:any(r.get('ws_closed') for r in rows()),'websocket close')
except BaseException as e:failure=repr(e)
finally:
 for process in reversed(children):
  if process.poll() is None:
   process.terminate()
   try:process.wait(timeout=4)
   except subprocess.TimeoutExpired:process.kill();process.wait(timeout=4)
 if 'master' in globals():
  for _ in range(5):drain()
  os.close(master);os.close(slave)
 (root/'terminal.bin').write_bytes(raw);(root/'server.stderr').write_bytes(server.stderr.read())
 result={'pass':failure is None,'failure':failure,'rc':rc,'termios_restored':before==after if after is not None else None,'small':a.small,'children':[(x.pid,x.returncode) for x in children]}
 (root/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result));raise SystemExit(0 if failure is None else 1)
