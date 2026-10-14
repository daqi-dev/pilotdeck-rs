// Node v22 + the read-only TS baseline's ws. No listen(), network or baseline writes.
// Usage: node tests/node-ws-semantics.cjs /absolute/path/to/PilotDeck
const assert = require('node:assert/strict');
const http = require('node:http');
const { Duplex } = require('node:stream');
const { once } = require('node:events');
const { createRequire } = require('node:module');
const path = require('node:path');
assert.match(process.version, /^v22\./);
const baselineRequire = createRequire(path.resolve(process.argv[2], 'ui/package.json'));
const { WebSocketServer } = baselineRequire('ws');

class MemorySocket extends Duplex {
  constructor() { super(); this.output = []; }
  _read() {}
  _write(chunk, encoding, callback) { this.output.push(Buffer.from(chunk)); callback(); }
  _final(callback) { this.push(null); callback(); }
  setTimeout() { return this; }
  setNoDelay() { return this; }
}
function closeFrame(payload, masked) {
  const mask = Buffer.from([1, 2, 3, 4]);
  return Buffer.concat([
    Buffer.from([0x88, payload.length | (masked ? 0x80 : 0)]),
    ...(masked ? [mask, Buffer.from(payload.map((b, i) => b ^ mask[i % 4]))] : [payload]),
  ]);
}
function decodeFrames(wire) {
  const frames = [];
  for (let offset = 0; offset < wire.length;) {
    const opcode = wire[offset++] & 15;
    const lengthByte = wire[offset++];
    const masked = !!(lengthByte & 128);
    const length = lengthByte & 127;
    assert.ok(length < 126, 'small control frame fixture');
    const mask = masked ? wire.subarray(offset, offset += 4) : null;
    const payload = Buffer.from(wire.subarray(offset, offset += length));
    assert.equal(payload.length, length);
    if (mask) payload.forEach((b, i) => payload[i] = b ^ mask[i % 4]);
    frames.push({ opcode, masked, payload_hex: payload.toString('hex'),
      code: length ? payload.readUInt16BE() : 1005,
      reason: length ? payload.subarray(2).toString() : '' });
  }
  return frames;
}
async function upgradeAndClose(initiator, code, reason) {
  const server = http.createServer();
  const wss = new WebSocketServer({ noServer: true });
  const socket = new MemorySocket();
  const connected = new Promise(resolve => server.on('upgrade', (req, sock, head) =>
    wss.handleUpgrade(req, sock, head, resolve)));
  server.emit('connection', socket);
  socket.push('GET /ws HTTP/1.1\r\nHost: memory\r\nConnection: Upgrade\r\n' +
    'Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n' +
    `Sec-WebSocket-Key: ${Buffer.from('the sample nonce').toString('base64')}\r\n\r\n`);
  const ws = await connected;
  assert.equal(ws._closeTimeout, 30000, "baseline ws default close receipt budget");
  const handshake = Buffer.concat(socket.output).toString();
  assert.ok(!/^date:/im.test(handshake));
  assert.ok(handshake.startsWith('HTTP/1.1 101 Switching Protocols\r\n'));
  socket.output.length = 0;
  const payload = code === null ? Buffer.alloc(0) : Buffer.concat([
    Buffer.from([code >> 8, code & 255]), Buffer.from(reason),
  ]);
  const closed = once(ws, 'close');
  if (initiator === 'server') {
    code === null ? ws.close() : ws.close(code, reason);
    // The peer's actual masked receipt follows the server's close.
    await new Promise(resolve => setImmediate(resolve));
  }
  const clientWire = closeFrame(payload, true);
  socket.push(Buffer.from(clientWire));
  const [observedCode, observedReason] = await closed;
  const down = decodeFrames(Buffer.concat(socket.output));
  const up = decodeFrames(clientWire);
  assert.equal(down.length, 1);
  assert.equal(up.length, 1);
  assert.equal(up[0].payload_hex, payload.toString('hex'));
  assert.equal(down[0].opcode, 8);
  assert.equal(down[0].payload_hex, payload.toString('hex'));
  assert.equal(observedCode, code ?? 1005);
  assert.equal(observedReason.toString(), reason);
  wss.close();
  return { initiator, handshake, c2u: up, u2c: down, observedCode };
}
async function serverResponse101() {
  const socket = new MemorySocket();
  const request = new http.IncomingMessage(socket);
  request.httpVersionMajor = 1; request.httpVersionMinor = 1;
  const response = new http.ServerResponse(request);
  response.assignSocket(socket);
  response.writeHead(101, { Connection: 'Upgrade', Upgrade: 'websocket' });
  response.end();
  await new Promise(resolve => setImmediate(resolve));
  const wire = Buffer.concat(socket.output).toString();
  assert.match(wire, /^Date:/im);
  socket.destroy();
  return wire;
}
async function pingAndDuplicateKey() {
  const request = 'GET /ws HTTP/1.1\r\nHost: memory\r\nConnection: Upgrade\r\n' +
    'Upgrade: websocket\r\nSec-WebSocket-Version: 13\r\n' +
    `Sec-WebSocket-Key: ${Buffer.from('the sample nonce').toString('base64')}\r\n`;
  const server = http.createServer();
  const wss = new WebSocketServer({ noServer: true });
  const socket = new MemorySocket();
  const connected = new Promise(resolve => server.on('upgrade', (req, sock, head) =>
    wss.handleUpgrade(req, sock, head, resolve)));
  server.emit('connection', socket);
  socket.push(request + '\r\n');
  const ws = await connected;
  assert.equal(ws._closeTimeout, 30000);
  socket.output.length = 0;
  const ping = closeFrame(Buffer.from('ping payload'), true);
  ping[0] = 0x89;
  socket.push(ping);
  await new Promise(resolve => setImmediate(resolve));
  const pong = decodeFrames(Buffer.concat(socket.output));
  assert.equal(pong.length, 1);
  assert.equal(pong[0].opcode, 10);
  assert.equal(pong[0].payload_hex, Buffer.from('ping payload').toString('hex'));
  ws.terminate(); wss.close(); socket.destroy();
  const badServer = http.createServer();
  const badWss = new WebSocketServer({ noServer: true });
  const badSocket = new MemorySocket();
  badServer.on('upgrade', (req, sock, head) => badWss.handleUpgrade(req, sock, head, () =>
    assert.fail('duplicate key cannot upgrade')));
  badServer.emit('connection', badSocket);
  badSocket.push(request + `Sec-WebSocket-Key: ${Buffer.from('another nonce123').toString('base64')}\r\n\r\n`);
  await new Promise(resolve => setImmediate(resolve));
  const rejection = Buffer.concat(badSocket.output).toString();
  assert.match(rejection, /^HTTP\/1.1 400/);
  assert.ok(!/^date:/im.test(rejection), 'ws abortHandshake has no Date');
  badSocket.destroy(); badWss.close();
  return { closeTimeout: 30000, onePong: true, duplicateKeyStatus: 400, duplicateKeyHasDate: false, rejection };
}

(async () => {
  const results = [];
  for (const initiator of ['client', 'server']) {
    for (const [code, reason] of [[1000, ''], [1001, '离开'], [4001, 'custom'], [null, '']]) {
      results.push(await upgradeAndClose(initiator, code, reason));
    }
  }
  console.log(JSON.stringify({ node: process.version, ws: baselineRequire('ws/package.json').version,
    serverResponse101: await serverResponse101(), controls: await pingAndDuplicateKey(), upgrade: results }, null, 2));
})().catch(error => { console.error(error); process.exitCode = 1; });
