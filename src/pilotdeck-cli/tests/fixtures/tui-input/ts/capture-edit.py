import argparse,fcntl,hashlib,json,os,pty,select,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--node',required=True,type=Path);p.add_argument('--ts-root',required=True,type=Path);p.add_argument('--out',required=True,type=Path);a=p.parse_args();base=a.ts_root.resolve();node=a.node.resolve();out=a.out.resolve();out.mkdir(exist_ok=False)
assert subprocess.check_output([str(node),'-p','process.version+" "+process.versions.icu'],text=True).strip()=='v22.22.0 78.2';commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';assert subprocess.check_output(['git','-C',str(base),'rev-parse','HEAD'],text=True).strip()==commit
L=b'\x1b[D';R=b'\x1b[C';BS=b'\x7f';DEL=b'\x1b[3~'
cases={'ascii-insert':[b'abcd',L,L,b'X'], 'ascii-backspace':[b'abcd',L,L,BS], 'ascii-delete':[b'abcd',L,L,DEL], 'ascii-left-right':[b'abcd',L,L,R,b'X'], 'chinese-backspace':['甲乙丙'.encode(),L,BS], 'emoji-backspace':['A😀B'.encode(),L,BS], 'emoji-left-insert':['A😀B'.encode(),L,L,b'X'], 'combining-backspace':['Ae\u0301B'.encode(),L,BS], 'paste-crlf':[b'alpha\r\nbeta']}
cases.pop('paste-crlf')
source=base/'src/adapters/channel/tui/app/TuiApp.tsx';dep=base/'node_modules/ink-text-input/build/index.js';meta={'commit':commit,'node':'v22.22.0','icu':'78.2','source_sha256':hashlib.sha256(source.read_bytes()).hexdigest(),'textinput_sha256':hashlib.sha256(dep.read_bytes()).hexdigest(),'cases':[]}
for name,ops in cases.items():
 root=out/name;root.mkdir();home=root/'home';project=root/'project';home.mkdir();project.mkdir();wire=root/'wire.jsonl';wire.touch();env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1','NODE_NO_WARNINGS':'1'};children=[];raw=bytearray();failure=None;before=after=None;exited=None;master=slave=None
 server=subprocess.Popen([str(node),str(Path(__file__).with_name('server.mjs')),str(base),str(wire)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children.append(server)
 try:
  ready,_,_=select.select([server.stdout],[],[],8);assert ready;port=int(server.stdout.readline());assert 0<port<65536 and port!=18789
  (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('input-test-token\n')
  master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0));before=termios.tcgetattr(slave);child=subprocess.Popen([str(node),'--import',str(base/'node_modules/tsx/dist/loader.mjs'),str(base/'src/cli/pilotdeck.ts'),'tui'],cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);children.append(child);deadline=time.monotonic()+30;queries=0
  def drain():
   global queries
   readable,_,_=select.select([master],[],[],.03)
   if readable:
    try:raw.extend(os.read(master,65536))
    except OSError:pass
   count=bytes(raw).count(b'\x1b[6n')
   while queries<count:os.write(master,b'\x1b[1;1R');queries+=1
  def pause(s):
   end=time.monotonic()+s
   while time.monotonic()<end:drain()
  def rows():return [json.loads(x) for x in wire.read_text().splitlines()]
  def wait(pred,label):
   while time.monotonic()<deadline:
    drain()
    if pred():return
    if child.poll() is not None:raise RuntimeError(f'{label} exit={child.returncode}')
   raise TimeoutError(label)
  wait(lambda:any(x.get('in',{}).get('method')=='list_sessions' for x in rows()),'startup')
  for op in ops:os.write(master,op);pause(.35)
  pause(.4)
  if name!='paste-crlf':os.write(master,b'\r')
  wait(lambda:any(x.get('in',{}).get('method')=='submit_turn' for x in rows()),'submit');pause(.8)
  messages=[x['in']['params']['message'] for x in rows() if x.get('in',{}).get('method')=='submit_turn'];assert messages
  os.write(master,b'\x03');pause(.5);after=termios.tcgetattr(slave);exited=child.poll()
 except BaseException as e:failure=repr(e);messages=[]
 finally:
  for c in reversed(children):
   if c.poll() is None:c.terminate()
   try:c.wait(timeout=3)
   except subprocess.TimeoutExpired:c.kill();c.wait(timeout=3)
  if master is not None:os.close(master);os.close(slave)
  (root/'terminal.bin').write_bytes(raw);(root/'server.stderr').write_bytes(server.stderr.read());entry={'case':name,'failure':failure,'messages':messages,'ops_hex':[x.hex() for x in ops],'ctrlc_exit':exited,'termios_restored':before==after if after is not None else None,'children':[(c.pid,c.returncode) for c in children]};(root/'result.json').write_text(json.dumps(entry,ensure_ascii=True,indent=2)+'\n');meta['cases'].append(entry);print(name,json.dumps(entry,ensure_ascii=True),flush=True)
(out/'summary.json').write_text(json.dumps(meta,ensure_ascii=True,indent=2)+'\n');raise SystemExit(0 if all(x['failure'] is None for x in meta['cases']) else 1)
