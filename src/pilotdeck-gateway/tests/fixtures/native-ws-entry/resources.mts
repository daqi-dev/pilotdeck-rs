import assert from 'node:assert/strict';
import {EventEmitter, once} from 'node:events';
import {connect} from 'node:net';
import {Writable} from 'node:stream';
import {createHash} from 'node:crypto';

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
  console.error('#749 resource probe', JSON.stringify(observations));
  return observations;
}
