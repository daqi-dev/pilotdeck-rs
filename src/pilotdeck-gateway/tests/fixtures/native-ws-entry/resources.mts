import assert from 'node:assert/strict';
import {EventEmitter, once} from 'node:events';
import {connect} from 'node:net';
import {Writable} from 'node:stream';
import {createHash} from 'node:crypto';
import {mock} from 'node:test';

import {RFC6455_SAMPLE_NONCE} from './sample.mts';

// Real TS transport with deterministic data/write boundaries. The real TCP probe below separately checks half-close behavior.
export async function resourceObservations(websocket:any, startGatewayServer:any, sockets:any[], connection:any) {
  class SocketProbe extends EventEmitter {
    writes: Buffer[] = [];
    ends = 0;
    write(bytes:Buffer) {this.writes.push(bytes); return false;}
    end() {this.ends++; return this;}
  }
  console.error("#749 resource start");
  const observations:any = {};
  for (const size of [100 * 1024 * 1024, 100 * 1024 * 1024 + 1, 2 ** 40]) {
    const socket = new SocketProbe();
    const text = new websocket.TextWebSocketConnection(socket);
    let messages = 0;
    text.onMessage(() => messages++);
    const header = Buffer.alloc(14);
    header[0] = 0x81; header[1] = 0xff; header.writeBigUInt64BE(BigInt(size), 2);
    socket.emit('data', header);
    observations[String(size)] = {messages, buffered: text.buffer.length, writes: socket.writes.length, ends: socket.ends};
  }
  observations.largeOutput = [];
  for (const size of [2 * 1024 * 1024, 100 * 1024 * 1024]) {
    const socket = new SocketProbe();
    const transport = new websocket.TextWebSocketConnection(socket);
    transport.sendText('x'.repeat(size));
    assert.equal(socket.writes.length, 1, '#749 TS large text emits one frame');
    const wire = socket.writes[0];
    observations.largeOutput.push({size, opcode:wire[0] & 15,
      declared:Number(wire.readBigUInt64BE(2)), wireBytes:wire.length,
      sha256:createHash('sha256').update(wire.subarray(10)).digest('hex'), ends:socket.ends});
  }
  // A real Writable with a held first _write callback gives an exact backlog.
  // Node's end/finish is the same stream lifecycle used by net.Socket.
  let release!:()=>void;
  let completed = 0;
  const queued = new Writable({write(_chunk, _encoding, callback) {
    if (completed === 0) {completed++; release=()=>callback();}
    else {completed++; callback();}
  }});
  const queuedText = new websocket.TextWebSocketConnection(queued);
  for (let i=0;i<64;i++) queuedText.sendText('queued');
  const finished = once(queued, 'finish');
  queued.end();
  const before = {completed, finished:queued.writableFinished};
  release(); await finished;
  observations.fullQueueEnd = {before, completed, finished:queued.writableFinished,
    destroyed:queued.destroyed};
  const slow = new SocketProbe();
  const text = new websocket.TextWebSocketConnection(slow);
  for (let i = 0; i < 64; i++) text.sendText('slow');
  text.close();
  observations.writeFalse = {writes: slow.writes.length, ends: slow.ends,
    closeCode: slow.writes.at(-1)!.readUInt16BE(2)};

  const heldSocket = new SocketProbe();
  const heldText = new websocket.TextWebSocketConnection(heldSocket);
  const releases:Array<()=>void> = [];
  let calls = 0;
  let allEntered!:()=>void;
  const entered = new Promise<void>(resolve => allEntered=resolve);
  new connection.GatewayWsConnection(heldText, {token:'fixture-token',serverVersion:'probe',
    gateway:{async describeServer(){
      if (++calls > 1) {await new Promise<void>(resolve => {releases.push(resolve);if(releases.length===65)allEntered();});}
      return {};
    }}});
  const emit = (value:any) => {
    const body=Buffer.from(JSON.stringify(value)); assert.ok(body.length<126,'#749 pending probe short frame');
    heldSocket.emit('data',Buffer.concat([Buffer.from([0x81,0x80|body.length,1,2,3,4]),
      Buffer.from(body.map((byte,index)=>byte^([1,2,3,4][index%4])))]));
  };
  emit({type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'probe',token:'fixture-token'});
  await new Promise(resolve=>setImmediate(resolve));
  for(let i=0;i<65;i++)emit({type:'request',id:String(i),method:'describe_server'});
  await entered;
  observations.heldRequests=releases.length;
  for(const release of releases)release();
  await new Promise(resolve=>setImmediate(resolve));
  heldSocket.emit('close');

  const server = await startGatewayServer({host:'127.0.0.1',port:0,token:'fixture-token',
    gateway:{async describeServer(){return {};}}});
  const peer = connect({port:Number(new URL(server.url).port),host:'127.0.0.1',allowHalfOpen:true});
  await once(peer,'connect');
  let bytes = '';
  const upgraded = new Promise<void>(resolve => peer.on('data', data => {
    bytes += data.toString(); if (bytes.includes('\r\n\r\n')) resolve();
  }));
  peer.write('GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: '+RFC6455_SAMPLE_NONCE+'\r\n\r\n');
  await upgraded; console.error("#749 resource upgraded");
  const socket = sockets.find(socket => socket.localPort === Number(new URL(server.url).port)
    && socket.remotePort === peer.localPort);
  assert.ok(socket, '#749 capture accepted server socket after upgrade barrier');
  peer.pause();
  // A deterministic Writable predecessor: cork prevents any write from
  // completing until explicitly released, independent of TCP buffer sizes.
  socket.cork();
  const transport = new websocket.TextWebSocketConnection(socket);
  transport.sendText('x'.repeat(65536));
  const ended = once(socket,'end');
  peer.end(); await ended; console.error("#749 resource EOF");
  let closed = false;
  const closing = server.close().then(() => {closed = true;});
  await new Promise(resolve => setImmediate(resolve));
  observations.halfCloseBlocked = {readEnded:socket.readableEnded,
    pendingBytes:socket.writableLength > 0, destroyed:socket.destroyed,
    serverClosed:closed};
  // Cancellation of the permanently blocked predecessor is deterministic.
  const socketClosed = once(socket,'close');
  socket.destroy(); peer.destroy(); await socketClosed; console.error("#749 resource destroyed"); await closing;
  observations.halfCloseCancelled = {destroyed:socket.destroyed,serverClosed:closed};
  observations.closeFlush = [];
  for (const event of ['close', 'eof']) for (const outcome of ['release', 'cancel']) {
    console.error('#749 closeFlush begin',event,outcome);
    const server = await startGatewayServer({host:'127.0.0.1',port:0,token:'fixture-token',
      gateway:{async describeServer(){return {};}}});
    const peer = connect({port:Number(new URL(server.url).port),host:'127.0.0.1',allowHalfOpen:true});
    await once(peer,'connect');
    const chunks:Buffer[]=[];
    let upgraded!:()=>void;
    const upgrade=new Promise<void>(resolve=>upgraded=resolve);
    peer.on('data',data=>{chunks.push(data);if(Buffer.concat(chunks).includes('\r\n\r\n'))upgraded();});
    peer.write('GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: '+RFC6455_SAMPLE_NONCE+'\r\n\r\n');
    await upgrade;
    chunks.length=0;
    const socket=sockets.find(s=>s.localPort===Number(new URL(server.url).port)&&s.remotePort===peer.localPort);
    assert.ok(socket?.fixtureTransport,'#749 actual GatewayServer transport captured');
    const originalWrite=socket._write;
    let release!:()=>void;
    // Hold the first actual net.Socket write. end() automatically uncorks a
    // socket, so cork alone cannot hold the peer-close path deterministically.
    socket._write=function(chunk:any,encoding:any,callback:any){
      this._write=originalWrite;
      release=()=>originalWrite.call(this,chunk,encoding,callback);
    };
    socket.fixtureTransport.sendText('x'.repeat(2*1024*1024));
    assert.equal(typeof release,'function','#749 accepted output reached write gate');
    mock.timers.enable({apis:['setTimeout','setInterval']});
    try {
      // TCP may split the six-byte close frame. Observe the real close()
      // calling socket.end(), not merely the first data callback.
      const seen=event==='eof'?once(socket,'end'):new Promise<void>(resolve=>{
        const end=socket.end;
        socket.end=function(...args:any[]){const result=end.apply(this,args);resolve();return result;};
      });
      if(event==='close')peer.write(Buffer.from([0x88,0x80,1,2,3,4]));else peer.end();
      await seen;
      console.error('#749 closeFlush event',event,outcome);
      let serverClosed=false;
      const closing=server.close().then(()=>{serverClosed=true;});
      mock.timers.tick(3_600_000);
      await new Promise(resolve=>setImmediate(resolve));
      const before={pendingBytes:socket.writableLength>0,finished:socket.writableFinished,
        destroyed:socket.destroyed,serverClosed,socketTimeout:socket.timeout??0};
      assert.deepEqual(before,{pendingBytes:true,finished:false,destroyed:false,serverClosed:false,socketTimeout:0},
        '#749 TS retains accepted output without close timer');
      const socketClosed=once(socket,'close');
      let after:any;
      if(outcome==='release'){
        const delivered=event==='close'?once(peer,'end'):new Promise<void>(resolve=>{
          peer.on('data',()=>{if(Buffer.concat(chunks).length>=2*1024*1024+10)resolve();});
        });
        release();await delivered;
        console.error('#749 closeFlush flushed',event,outcome);
        const wire=Buffer.concat(chunks);
        const size=Number(wire.readBigUInt64BE(2));
        after={size,opcode:wire[0]&15,wireBytes:wire.length,
          sha256:createHash('sha256').update(wire.subarray(10,10+size)).digest('hex'),
          tail:wire.subarray(10+size).toString('hex'),finished:socket.writableFinished};
        // HTTP upgrade sockets allow half-open reads: EOF alone does not call
        // end(). Observe full delivery, then explicitly cancel the idle half.
        if(event==='eof'){socket.destroy();peer.destroy();}else peer.end();
        await socketClosed;await closing;
      }else{
        socket.destroy();peer.destroy();await socketClosed;await closing;
        after={destroyed:socket.destroyed,serverClosed};
      }
      observations.closeFlush.push({event,outcome,advancedMs:3_600_000,before,after});
      console.error('#749 closeFlush done',event,outcome);
    }finally{mock.timers.reset();socket.destroy();peer.destroy();await server.close().catch(()=>{});}
  }
  console.error('#749 resource probe', JSON.stringify(observations));
  return observations;
}
