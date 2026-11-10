#!/usr/bin/env python3
# anchor-selfcheck: unsupported transparent wire capture proxy; no product source mutations
"""Unfrozen loopback transparent TCP capture: record original WS frame bytes."""
import argparse,asyncio,json,time
from pathlib import Path
ap=argparse.ArgumentParser();ap.add_argument("--listen",type=int,required=True);ap.add_argument("--upstream",type=int,required=True);ap.add_argument("--out",type=Path,required=True);a=ap.parse_args()
assert all(28000<=x<=28999 for x in (a.listen,a.upstream)) and a.listen!=a.upstream
output=a.out.open("x",buffering=1);seq=0;counter=0
class Decoder:
 def __init__(self,emit,direction):self.buf=b"";self.http=True;self.emit=emit;self.direction=direction
 def feed(self,data):
  self.buf+=data
  if self.http:
   pos=self.buf.find(b"\r\n\r\n")
   if pos<0:return
   head,self.buf=self.buf[:pos+4],self.buf[pos+4:];self.http=False
   self.emit({"direction":self.direction,"httpHeadHex":head.hex()})
  while len(self.buf)>=2:
   b0,b1=self.buf[:2];code=b1&127;off=2
   if code in (126,127):
    width=2 if code==126 else 8
    if len(self.buf)<off+width:return
    size=int.from_bytes(self.buf[off:off+width],"big");off+=width
   else:size=code
   if size>16*1024*1024:raise ValueError("frame budget")
   mask=bool(b1&128);maskbytes=self.buf[off:off+4] if mask else b"";off+=4 if mask else 0
   if len(self.buf)<off+size:return
   wire=self.buf[:off+size];payload=self.buf[off:off+size];self.buf=self.buf[off+size:]
   if mask:payload=bytes(v^maskbytes[i%4] for i,v in enumerate(payload))
   row={"direction":self.direction,"wireHex":wire.hex(),"wire":{"opcode":b0&15,"fin":bool(b0&128),"rsv":b0&112,"masked":mask,"lengthCode":code,"payloadLength":size}}
   decoded=payload.decode("utf8",errors="replace")
   row["frame"]=decoded
   if b0&15==1:
    row["raw"]=decoded
    if self.direction=="receive":
     try:row["frame"]=json.loads(decoded)
     except ValueError:pass
   elif b0&15==8 and self.direction=="receive":
    row["frame"]={"code":int.from_bytes(payload[:2],"big") if len(payload)>=2 else 1005,"reason":payload[2:].decode("utf8",errors="replace")}
   self.emit(row)
async def main():
 global seq,counter
 async def connection(reader,writer):
  global counter,seq
  counter+=1;ident=counter
  def emit(row):
   global seq
   seq+=1;output.write(json.dumps({"seq":seq,"connection":ident,"monotonicNs":time.monotonic_ns(),**row})+"\n")
  upstream=None
  try:
   upstream_reader,upstream=await asyncio.open_connection("127.0.0.1",a.upstream)
   async def pipe(src,dst,direction):
    decoder=Decoder(emit,direction)
    while data:=await src.read(65536):
     decoder.feed(data);dst.write(data);await dst.drain()
    try:dst.write_eof()
    except (OSError,AttributeError):pass
   await asyncio.gather(pipe(reader,upstream,"send"),pipe(upstream_reader,writer,"receive"))
  finally:
   writer.close()
   if upstream:upstream.close()
 server=await asyncio.start_server(connection,"127.0.0.1",a.listen)
 async with server:await server.serve_forever()
try:asyncio.run(main())
finally:output.close()
