#!/usr/bin/env python3
"""Observe Darwin PTY F_GETFL changes under isolated individual operations."""
import fcntl,json,os,pty,sys,termios,tty
from pathlib import Path
out=Path(sys.argv[1]);out.mkdir();result=[]
for case in ('write-slave','read-master','read-slave','tcgetattr','tcsetattr','nonblock-roundtrip'):
 master,slave=pty.openpty();record={'case':case,'nonblock_mask':os.O_NONBLOCK,'before':fcntl.fcntl(slave,fcntl.F_GETFL)}
 try:
  if case=='write-slave':os.write(slave,b'x')
  elif case=='read-master':os.write(slave,b'x');record['after_setup']=fcntl.fcntl(slave,fcntl.F_GETFL);os.read(master,1)
  elif case=='read-slave':os.write(master,b'x\n');record['after_setup']=fcntl.fcntl(slave,fcntl.F_GETFL);os.read(slave,2)
  elif case=='tcgetattr':termios.tcgetattr(slave)
  elif case=='tcsetattr':attrs=termios.tcgetattr(slave);tty.setraw(slave);record['after_raw']=fcntl.fcntl(slave,fcntl.F_GETFL);termios.tcsetattr(slave,termios.TCSANOW,attrs)
  else:fcntl.fcntl(slave,fcntl.F_SETFL,record['before']|os.O_NONBLOCK);record['during']=fcntl.fcntl(slave,fcntl.F_GETFL);fcntl.fcntl(slave,fcntl.F_SETFL,record['before'])
  record['after']=fcntl.fcntl(slave,fcntl.F_GETFL);record['xor']=record['before']^record['after']
 finally:os.close(master);os.close(slave)
 result.append(record)
(out/'result.json').write_text(json.dumps(result,indent=2)+'\n');print(json.dumps(result,indent=2))
