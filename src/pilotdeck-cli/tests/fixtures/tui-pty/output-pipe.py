#!/usr/bin/env python3
"""Reject PTY input plus undrained pipe output without hanging or modifying termios."""
import argparse, fcntl, json, os, pty, subprocess, termios, threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--binary',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
root=a.output.resolve();root.mkdir();home=root/'home';project=root/'project';home.mkdir();project.mkdir()
requests=[]
class Handler(BaseHTTPRequestHandler):
 def do_GET(self):
  requests.append({'path':self.path,'upgrade':self.headers.get('Upgrade')});self.send_response(200);self.end_headers();self.wfile.write(b'{}')
 def log_message(self,*_):pass
server=HTTPServer(('127.0.0.1',0),Handler);port=server.server_port;assert port!=18789
thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
env={'PATH':'/usr/bin:/bin','HOME':str(home),'PILOT_HOME':str(home),'TERM':'xterm-256color','LANG':'en_US.UTF-8'}
(home/'pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent: {{model: fixture/mock}}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:{port}/v1\n      apiKey: mock-only\n      models:\n        mock: {{}}\ngateway:\n  port: {port}\n');(home/'server-token').write_text('pty-fixture-token\n')
master,slave=pty.openpty();before=termios.tcgetattr(slave);flags=fcntl.fcntl(slave,fcntl.F_GETFL);failure=None;rc=None;child=None
try:
 child=subprocess.Popen([str(a.binary.resolve()),'tui'],cwd=project,env=env,stdin=slave,stdout=subprocess.PIPE,stderr=subprocess.PIPE,start_new_session=True)
 rc=child.wait(timeout=5);assert rc==1,rc;assert before==termios.tcgetattr(slave);assert (flags & os.O_NONBLOCK)==(fcntl.fcntl(slave,fcntl.F_GETFL)&os.O_NONBLOCK)
except BaseException as e:failure=repr(e)
finally:
 if child and child.poll() is None:child.kill();child.wait(timeout=3)
 stdout,stderr=child.communicate() if child else (b'',b'');server.shutdown();thread.join();server.server_close();os.close(master);os.close(slave)
 if failure is None and (stdout or b'interactive output terminal' not in stderr or any(r['upgrade'] for r in requests)):failure='missing output-terminal rejection or unexpected WebSocket upgrade'
 (root/'stdout.bin').write_bytes(stdout);(root/'stderr.bin').write_bytes(stderr);(root/'result.json').write_text(json.dumps({'pass':failure is None,'failure':failure,'rc':rc,'requests':requests},indent=2)+'\n');print(json.dumps({'pass':failure is None,'failure':failure}));raise SystemExit(0 if failure is None else 1)
