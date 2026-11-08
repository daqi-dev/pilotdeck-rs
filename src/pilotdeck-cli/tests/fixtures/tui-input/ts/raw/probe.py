import argparse,fcntl,hashlib,json,os,pty,select,struct,subprocess,termios,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--ts-root',required=True,type=Path);p.add_argument('--node',required=True,type=Path);p.add_argument('--out',required=True,type=Path);a=p.parse_args();base=a.ts_root.resolve();node=a.node.resolve();out=a.out.resolve();out.mkdir(exist_ok=False)
assert subprocess.check_output([str(node),'-p','process.version+" "+process.versions.icu'],text=True).strip()=='v22.22.0 78.2';assert subprocess.check_output(['git','-C',str(base),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
results=[]
for case in ('text','help','permission'):
 root=out/case;root.mkdir();home=root/'home';project=root/'project';home.mkdir();project.mkdir();wire=root/'wire.jsonl';wire.touch();env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8','NO_COLOR':'1','NODE_NO_WARNINGS':'1'};children=[];raw=bytearray();failure=None;marks={};master=slave=None
 server=subprocess.Popen([str(node),str(Path(__file__).with_name('server.mjs')),str(base),str(wire)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env);children.append(server)
 try:
  assert select.select([server.stdout],[],[],8)[0];port=int(server.stdout.readline());assert port!=18789
  (home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('input-test-token\n')
  master,slave=pty.openpty();fcntl.ioctl(slave,termios.TIOCSWINSZ,struct.pack('HHHH',30,100,0,0));before=termios.tcgetattr(slave);child=subprocess.Popen([str(node),'--import',str(base/'node_modules/tsx/dist/loader.mjs'),str(base/'src/cli/pilotdeck.ts'),'tui'],cwd=project,env=env,stdin=slave,stdout=slave,stderr=slave,start_new_session=True);children.append(child);queries=0
  def drain():
   global queries
   if select.select([master],[],[],.03)[0]:
    try:raw.extend(os.read(master,65536))
    except OSError:pass
   count=bytes(raw).count(b'\x1b[6n')
   while queries<count:os.write(master,b'\x1b[1;1R');queries+=1
  def pause(s):
   end=time.monotonic()+s
   while time.monotonic()<end:drain()
  def rows():return [json.loads(x) for x in wire.read_text().splitlines()]
  def mark(name):marks[name]={'wire_rows':len(rows()),'screen_bytes':len(raw)}
  def wait(pred,label):
   until=time.monotonic()+18
   while time.monotonic()<until:
    drain()
    if pred():return
    if child.poll() is not None:raise RuntimeError(f'{label} exit={child.returncode}')
   raise TimeoutError(label)
  wait(lambda:any(x.get('in',{}).get('method')=='list_sessions' for x in rows()),'startup');pause(1)
  if case=='permission':
   os.write(master,b'permission-test');pause(.6);os.write(master,b'\r');wait(lambda:any(x.get('out',{}).get('event',{}).get('type')=='permission_request' for x in rows()),'permission');pause(1)
  if case=='help':
   os.write(master,b'?');pause(1)
  mark('before_batch');os.write(master,{'text':b'hello\r','help':b'q\r','permission':b'y\r'}[case]);pause(1.5);mark('after_batch')
  if case=='text':os.write(master,b'\r');pause(1.5)
  elif case=='help':os.write(master,b'q');pause(1.5)
  else:os.write(master,b'y');pause(1.5)
  mark('after_single');os.write(master,b'\x03');pause(.5);after=termios.tcgetattr(slave);exit_after_ctrlc=child.poll()
 except BaseException as e:failure=repr(e);after=None;exit_after_ctrlc=None
 finally:
  for c in reversed(children):
   if c.poll() is None:c.terminate()
   try:c.wait(timeout=3)
   except subprocess.TimeoutExpired:c.kill();c.wait(timeout=3)
  if master is not None:os.close(master);os.close(slave)
  (root/'terminal.bin').write_bytes(raw);(root/'server.stderr').write_bytes(server.stderr.read());entry={'case':case,'failure':failure,'marks':marks,'ctrlc_exit':exit_after_ctrlc,'termios_restored':before==after if after is not None else None,'children':[(c.pid,c.returncode) for c in children]};(root/'result.json').write_text(json.dumps(entry,indent=2)+'\n');results.append(entry);print(json.dumps(entry),flush=True)
(out/'summary.json').write_text(json.dumps(results,indent=2)+'\n');raise SystemExit(any(x['failure'] for x in results))
