"""Verify the actual isolated network before executing the unchanged collector supervisor."""
import errno,json,os,socket,subprocess,sys
from pathlib import Path
out=Path(sys.argv[1]);out.mkdir(parents=True,exist_ok=False)
rows=[]
for family,address in [(socket.AF_INET,('198.51.100.1',9)),(socket.AF_INET6,('2001:db8::1',9))]:
    for kind in (socket.SOCK_STREAM,socket.SOCK_DGRAM):
        try:
            with socket.socket(family,kind) as s:
                s.settimeout(1)
                code=s.connect_ex(address)
                if code==0 and kind==socket.SOCK_DGRAM:
                    try:s.send(b'network-none-negative-control');code=0
                    except OSError as error:code=error.errno
        except OSError as error:code=error.errno
        assert code in (errno.ENETUNREACH,errno.ENETDOWN,errno.EAFNOSUPPORT,errno.EADDRNOTAVAIL),(family,kind,code)
        rows.append({'family':family,'kind':kind,'address':address,'errno':code,'blocked':True})
with socket.socket() as server,socket.socket() as client:
    server.bind(('127.0.0.1',0));server.listen();assert client.connect_ex(server.getsockname())==0
    peer,_=server.accept();peer.close()
assert socket.if_nameindex()==[(1,'lo')]
receipt={'networkNamespace':os.readlink('/proc/self/ns/net'),'pidNamespace':os.readlink('/proc/self/ns/pid'),'hostname':socket.gethostname(),'interfaces':socket.if_nameindex(),
         'negativeControls':rows,'loopbackConnect':'PASS','uid':os.getuid(),'sourceOfBlocking':'Docker --network none; only loopback interface'}
(out/'network-isolation.json').write_text(json.dumps(receipt,indent=2))
os.execv(sys.executable,[sys.executable,'-I','-B',*sys.argv[2:]])
