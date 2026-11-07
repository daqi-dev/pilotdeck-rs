#!/usr/bin/env python3
"""Rust TUI tool interactions against a loopback fake gateway."""
import argparse,fcntl,hashlib,json,os,pty,select,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--bin',required=True,type=Path);p.add_argument('--node',required=True,type=Path);p.add_argument('--ts-root',required=True,type=Path);p.add_argument('--out',required=True,type=Path);a=p.parse_args()
base=a.ts_root.resolve();node=a.node.resolve();out=a.out.resolve();out.mkdir();home=out/'home';project=out/'project';home.mkdir();project.mkdir();wire=out/'wire.jsonl';wire.touch();raw=bytearray();children=[];failure=None;queries=0;before=after=None;exited=None
assert subprocess.check_output([str(node),'-p','process.version+" "+process.versions.icu'],text=True).strip()=='v22.22.0 78.2'
assert subprocess.check_output(['git','-C',str(base),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1'}
with (out/'oversized.txt').open('xb') as output:output.truncate(1024*1024+1)
os.mkfifo(out/'output.fifo',0o600)
server=subprocess.Popen([str(node),str(Path(__file__).with_name('tools-server-rust.mjs')),str(base),str(wire)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children.append(server)
try:
 ready,_,_=select.select([server.stdout],[],[],8);assert ready;port=int(server.stdout.readline());assert 0<port<65536 and port!=18789
 (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('pty-fixture-token\n')
 master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0));before=termios.tcgetattr(slave)
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
 type_line(b'tool fixture');wait(lambda:any(r.get('out',{}).get('final') for r in rows()),'turn final');pause(.5)
 os.write(master,b'\t');pause(.25);os.write(master,b'\x1b[Z');pause(.25);os.write(master,b'\t');pause(.25);os.write(master,b'\r');wait(lambda:b'SHORT_TOOL_MARK' in raw,'tab/expand');pause(.3)
 os.write(master,b'\x1b');pause(.4);os.write(master,b'\r');wait(lambda:b'LONG_START' in raw,'escape clears focus and empty enter opens latest');os.write(master,b'q');pause(.4);os.write(master,b'\t');pause(.3)
 prior=bytes(raw).count(b'SHORT_TOOL_MARK');type_line(b'/view 1');wait(lambda:bytes(raw).count(b'SHORT_TOOL_MARK')>prior,'short output scrollback')
 prior_long=bytes(raw).count(b'LONG_START');type_line(b'/view 2');wait(lambda:bytes(raw).count(b'LONG_START')>prior_long,'long viewer start');os.write(master,b'G');wait(lambda:b'LONG_END' in raw,'long viewer end');os.write(master,b'g');pause(.4);os.write(master,b'j');wait(lambda:b'WIDE_END' in raw,'wrapped long line')
 fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',20,45,0,0));pause(.5);os.write(master,b'G');pause(.5);os.write(master,b'g');pause(.5);os.write(master,b'\x1b[6~');pause(.3);os.write(master,b'\x1b[5~');pause(.3);os.write(master,b'k');pause(.3);os.write(master,b'q');pause(.4)
 fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0));pause(.4)
 type_line(b'resource fixture');wait(lambda:len([r for r in rows() if r.get('out',{}).get('final')])==2,'resource turn final')
 # Ratatui may reuse existing space cells and emit cursor motion between words.
 # Check the new error draw, not an impossible contiguous terminal byte phrase.
 error_start=len(raw);type_line(b'/view 3');wait(lambda:all(word in raw[error_start:] for word in (b'exceeds',b'MiB',b'viewing',b'limit')),'oversized output refused')
 prior_fifo=bytes(raw).count(b'FIFO_FALLBACK_MARK');type_line(b'/view 4');wait(lambda:bytes(raw).count(b'FIFO_FALLBACK_MARK')>prior_fifo,'FIFO read returns preview without blocking')
 os.write(master,b'\x03');pause(1);after=termios.tcgetattr(slave);exited=child.poll();assert before==after,'terminal not restored';assert exited == 0, f'child exit {exited}';assert len([r for r in rows() if r.get('in',{}).get('method')=='submit_turn'])==2;wait(lambda:any(r.get('ws_closed') for r in rows()),'socket close')
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
