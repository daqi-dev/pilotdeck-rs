#!/usr/bin/env python3
"""Independent real PTY regression for session race, permission retry, final-only."""
import argparse,fcntl,json,os,pty,select,signal,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--node',type=Path,required=True);p.add_argument('--ts-root',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--case',choices=['new','permission','permission-abort','final','budget','switch','permission-disconnect','pending-budget','abort-late','abort-timeout','throughput','grapheme'],required=True);a=p.parse_args()
root=a.output.resolve();root.mkdir();home=root/'home';project=root/'project';home.mkdir();project.mkdir();wire=root/'wire.jsonl';wire.touch();raw=bytearray();children=[];failure=None;queries=0;rc=None;before=after=None
env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1'}
server=subprocess.Popen([str(a.node.resolve()),str(Path(__file__).with_name('scenarios-server.mjs')),str(a.ts_root.resolve()),str(wire),a.case],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children.append(server)
try:
 ready,_,_=select.select([server.stdout],[],[],8);assert ready;port=int(server.stdout.readline());assert 0<port<65536 and port!=18789
 (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('pty-fixture-token\n')
 master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0));before=termios.tcgetattr(slave)
 child=subprocess.Popen([str(a.binary.resolve()),'tui'],cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);children.append(child);deadline=time.monotonic()+50
 def drain():
  global queries
  ready,_,_=select.select([master],[],[],.02)
  if ready:
   try:raw.extend(os.read(master,65536))
   except OSError:pass
   count=bytes(raw).count(b'\x1b[6n')
   while queries<count:os.write(master,b'\x1b[1;1R');queries+=1
 def rows():return [json.loads(x) for x in wire.read_text().splitlines()]
 def calls(method):return [r['in'] for r in rows() if r.get('in',{}).get('method')==method]
 def wait(pred,label):
  while time.monotonic()<deadline:
   drain()
   if pred():return
   if child.poll() is not None:raise RuntimeError(f'{label}: exit {child.returncode}')
  raise TimeoutError(label)
 def pause(n):
  end=time.monotonic()+n
  while time.monotonic()<end:drain()
 def type_line(data):os.write(master,data);pause(.5);os.write(master,b'\r')
 wait(lambda:bool(calls('list_sessions')),'list')
 if a.case=='new':
  type_line(b'/new');wait(lambda:bool(calls('new_session')),'new');type_line(b'old prompt');pause(.1);assert not calls('submit_turn'),'sent while new pending';wait(lambda:any(r.get('out',{}).get('result',{}).get('sessionKey')=='new-session' for r in rows()),'new reply');pause(.3);os.write(master,b'\r');wait(lambda:bool(calls('submit_turn')),'new submit');assert calls('submit_turn')[0]['params']['sessionKey']=='new-session'
 elif a.case in ('abort-late','abort-timeout'):
  type_line(b'first');wait(lambda:len(calls('submit_turn'))==1,'first submit');pause(.3);os.write(master,b'\x1b');wait(lambda:len(calls('abort_turn'))==1,'abort sent');wait(lambda:any(r.get('out',{}).get('event',{}).get('type')=='turn_completed' for r in rows()),'old terminal');type_line(b'second');pause(.3);assert len(calls('submit_turn'))==1,'new turn ran before abort confirmation'
  if a.case=='abort-late':
   wait(lambda:any(r.get('out',{}).get('type')=='response' and r.get('out',{}).get('id')==calls('abort_turn')[0]['id'] for r in rows()),'abort confirmation');pause(.3);os.write(master,b'\r');wait(lambda:len(calls('submit_turn'))==2,'submit after confirmed abort')
  else:
   pause(11);wait(lambda:b'reconnect' in raw and b'required' in raw,'abort timeout requires reconnect');os.write(master,b'\r');pause(.5);assert len(calls('submit_turn'))==1,'new turn ran after unknown abort outcome'
 elif a.case=='grapheme':
  type_line(b'grapheme');wait(lambda:any(r.get('out',{}).get('final') is True for r in rows()),'grapheme final');wait(lambda:'👩‍💻'.encode() in raw and 'e\u0301'.encode() in raw,'grapheme displayed');fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',25,72,0,0));os.kill(child.pid,signal.SIGWINCH);pause(.3)
  for _ in range(8):os.write(master,b'\x1b[5~');pause(.15)
  wait(lambda:b'BEGIN_MARK' in raw,'scroll to beginning after resize')
 elif a.case=='throughput':
  type_line(b'throughput');wait(lambda:any(r.get('out',{}).get('final') is True for r in rows()),'300-delta final');wait(lambda:b'THROUGHPUT' in raw and b'DONE' in raw,'render final');assert len([r for r in rows() if r.get('out',{}).get('event',{}).get('type')=='assistant_text_delta'])==301
 elif a.case=='switch':
  type_line(b'/switch 1');wait(lambda:bool(calls('resume_session')),'resume sent');wait(lambda:bool(calls('read_session_messages')),'history does not wait for resume');pause(.5);type_line(b'after switch');wait(lambda:bool(calls('submit_turn')),'submit after switch');assert calls('submit_turn')[0]['params']['sessionKey']=='saved-session'
 elif a.case=='permission':
  type_line(b'permission turn');wait(lambda:any(r.get('out',{}).get('event',{}).get('type')=='permission_request' for r in rows()),'permission event');wait(lambda:b'git' in raw and b'status' in raw,'permission detail');pause(.2);os.write(master,b'y');wait(lambda:len(calls('permission_decide'))==1,'first decision');pause(.4);os.write(master,b'y');wait(lambda:len(calls('permission_decide'))==2,'retry');assert all(r['params']['sessionKey']==f'tui:project={project}:default' for r in calls('permission_decide'))
 elif a.case=='permission-abort':
  type_line(b'permission turn');wait(lambda:any(r.get('out',{}).get('event',{}).get('type')=='permission_request' for r in rows()),'permission event');pause(.4);os.write(master,b'\x03');wait(lambda:len(calls('abort_turn'))==1,'permission ctrl-c abort');assert not calls('permission_decide')
 elif a.case=='permission-disconnect':
  type_line(b'permission turn');wait(lambda:any(r.get('out',{}).get('event',{}).get('type')=='permission_request' for r in rows()),'permission event');wait(lambda:any(r.get('ws_closed') for r in rows()),'socket close');pause(.5)
 elif a.case=='pending-budget':
  type_line(b'permission turn');wait(lambda:any(r.get('out',{}).get('event',{}).get('type')=='permission_request' for r in rows()),'permission event');pause(.3);os.write(master,b'y');wait(lambda:len(calls('permission_decide'))==1,'decision pending');wait(lambda:len(calls('abort_turn'))==1,'state budget abort')
 elif a.case=='budget':
  type_line(b'budget');wait(lambda:len(calls('abort_turn'))==1,'state budget abort')
  # Wire delivery and the next terminal repaint are independent. Ratatui may
  # also place cursor escapes between words; require the notice after a bounded
  # display interval instead of assuming the wire observer saw a painted frame.
  notice_deadline=time.monotonic()+3
  def notice_visible():return all(word in raw for word in (b'TUI',b'limit',b'removed'))
  while time.monotonic()<notice_deadline and not notice_visible():drain()
  assert notice_visible(),'state-limit notice missing after 3 seconds'
 else:
  type_line(b'first');wait(lambda:any(r.get('out',{}).get('final') is True for r in rows()),'first final');pause(.5);type_line(b'second');wait(lambda:len(calls('submit_turn'))==2,'second submit')
 pause(.5);os.write(master,b'\x03');wait(lambda:child.poll() is not None,'exit');rc=child.returncode;after=termios.tcgetattr(slave);assert rc==0 and before==after
except BaseException as e:failure=repr(e)
finally:
 for process in reversed(children):
  if process.poll() is None:
   process.terminate()
   try:process.wait(timeout=4)
   except subprocess.TimeoutExpired:process.kill();process.wait(timeout=4)
 if 'master' in globals():os.close(master);os.close(slave)
 (root/'terminal.bin').write_bytes(raw);(root/'server.stderr').write_bytes(server.stderr.read());(root/'result.json').write_text(json.dumps({'case':a.case,'pass':failure is None,'failure':failure,'rc':rc,'termios_restored':before==after if after is not None else None,'children':[(c.pid,c.returncode) for c in children]},indent=2)+'\n');print(json.dumps({'case':a.case,'pass':failure is None,'failure':failure}));raise SystemExit(0 if failure is None else 1)
