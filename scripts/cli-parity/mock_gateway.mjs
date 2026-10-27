#!/usr/bin/env node
// #933 D0-pr2 mock gateway: the deterministic WebSocket server cli-parity starts
// OUTSIDE the sandbox for scenarios marked "mockGateway". Hand-rolled RFC 6455
// (handshake + masked client frames + unmasked server frames) so it depends on
// no baseline package. Wire contract mirrors the real gateway server
// (src/gateway/server/GatewayWsConnection.ts, verified read on 2026-10-28):
//   hello  -> token mismatch closes 4003 "auth_failed"
//           -> ok sends {type:"hello_ok", protocolVersion, serverVersion, serverInfo}
//   request-> list_sessions answers {type:"response", id, ok:true, result:{sessions}}
//             anything else answers ok:false method_not_found
// Determinism rule: no timestamps, no randomness, no runtime paths in payloads.
// argv: [port]; env: PILOTDECK_MOCK_TOKEN (required).
import { createHash } from 'node:crypto';
import net from 'node:net';

const port = Number(process.argv[2]);
const token = process.env.PILOTDECK_MOCK_TOKEN;
if (!Number.isInteger(port) || port <= 0 || !token) {
  console.error('usage: mock_gateway.mjs <port> (PILOTDECK_MOCK_TOKEN required)');
  process.exit(1);
}
const PROTOCOL_VERSION = '1.2';
const SERVER_VERSION = '0.1.0-mock';
const SERVER_INFO = { mode: 'remote', protocolVersion: PROTOCOL_VERSION, projectKey: 'd0-mock-project', sessionCount: 1, capabilities: [] };
// Modeled on the real SessionInfo shape (src/session/catalog/SessionCatalogPort.ts).
// Non-empty on purpose: an empty [] would be indistinguishable from the SDK's
// `.sessions ?? []` fallback on a malformed response.
const SESSIONS = [{
  sessionId: 'd0-mock-session-1',
  sessionKey: 'd0-mock-session-1',
  summary: 'cli-parity mock session',
  lastModified: 1760000000000,
  createdAt: 1759000000000,
  fileSize: 1234,
  cwd: '/d0-mock/project',
}];

function frame(opcode, payload) {
  const len = payload.length;
  let header;
  if (len < 126) {
    header = Buffer.from([0x80 | opcode, len]);
  } else if (len < 65536) {
    header = Buffer.allocUnsafe(4);
    header[0] = 0x80 | opcode;
    header[1] = 126;
    header.writeUInt16BE(len, 2);
  } else {
    header = Buffer.allocUnsafe(10);
    header[0] = 0x80 | opcode;
    header[1] = 127;
    header.writeBigUInt64BE(BigInt(len), 2);
  }
  return Buffer.concat([header, payload]);
}

function sendText(socket, value) {
  socket.write(frame(0x1, Buffer.from(value, 'utf8')));
}

function sendClose(socket, code, reason) {
  const payload = Buffer.allocUnsafe(2 + Buffer.byteLength(reason));
  payload.writeUInt16BE(code, 0);
  payload.write(reason, 2);
  socket.write(frame(0x8, payload));
}

function handshake(buffer) {
  // Returns { accept } on a valid upgrade request, null when more bytes are
  // needed, and throws on anything the real server would reject.
  const end = buffer.indexOf('\r\n\r\n');
  if (end < 0) return null;
  const head = buffer.subarray(0, end).toString('utf8');
  const lines = head.split('\r\n');
  const [method, path] = lines[0].split(' ');
  const headers = new Map();
  for (const line of lines.slice(1)) {
    const at = line.indexOf(':');
    if (at > 0) headers.set(line.slice(0, at).trim().toLowerCase(), line.slice(at + 1).trim());
  }
  if (method !== 'GET' || path !== '/ws') throw new Error('not the gateway path');
  if ((headers.get('upgrade') ?? '').toLowerCase() !== 'websocket') throw new Error('not a websocket upgrade');
  const key = headers.get('sec-websocket-key');
  if (!key || headers.get('sec-websocket-version') !== '13') throw new Error('unsupported websocket version');
  const accept = createHash('sha1').update(key + '258EAFA5-E914-47DA-95CA-C5AB0DC85B11').digest('base64');
  return { accept, consumed: end + 4 };
}

class Connection {
  constructor(socket) {
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.authed = false;
    this.closed = false;
    socket.on('data', (chunk) => this.onData(chunk));
    socket.on('error', () => this.destroy());
    socket.on('close', () => { this.closed = true; });
  }

  destroy() {
    this.closed = true;
    this.socket.destroy();
  }

  onData(chunk) {
    if (this.closed) return;
    this.buffer = this.buffer.length ? Buffer.concat([this.buffer, chunk]) : chunk;
    try {
      if (!this.upgraded) {
        const done = handshake(this.buffer);
        if (!done) {
          if (this.buffer.length > 8192) this.destroy();
          return;
        }
        this.upgraded = true;
        this.buffer = this.buffer.subarray(done.consumed);
        this.socket.write('HTTP/1.1 101 Switching Protocols\r\n' +
          'Upgrade: websocket\r\nConnection: Upgrade\r\n' +
          `Sec-WebSocket-Accept: ${done.accept}\r\n\r\n`);
      }
      this.pump();
    } catch (error) {
      sendClose(this.socket, 4002, 'invalid_frame');
      this.destroy();
    }
  }

  pump() {
    while (this.buffer.length >= 2 && !this.closed) {
      const b0 = this.buffer[0], b1 = this.buffer[1];
      const fin = b0 & 0x80, opcode = b0 & 0x0f;
      const masked = b1 & 0x80;
      let length = b1 & 0x7f, offset = 2;
      if (length === 126) {
        if (this.buffer.length < 4) return;
        length = this.buffer.readUInt16BE(2);
        offset = 4;
      } else if (length === 127) {
        if (this.buffer.length < 10) return;
        const big = this.buffer.readBigUInt64BE(2);
        if (big > 4n * 1024n * 1024n) throw new Error('frame too large');
        length = Number(big);
        offset = 10;
      }
      if (!masked || !fin) throw new Error('client frames must be masked and unfragmented');
      if (this.buffer.length < offset + 4 + length) return;
      const mask = this.buffer.subarray(offset, offset + 4);
      const payload = Buffer.from(this.buffer.subarray(offset + 4, offset + 4 + length));
      for (let i = 0; i < payload.length; i++) payload[i] ^= mask[i & 3];
      this.buffer = this.buffer.subarray(offset + 4 + length);
      if (!this.dispatch(opcode, payload)) return;
    }
  }

  dispatch(opcode, payload) {
    if (opcode === 0x8) { // client close: echo the code, then hang up
      const code = payload.length >= 2 ? payload.readUInt16BE(0) : 1005;
      sendClose(this.socket, code, '');
      this.destroy();
      return false;
    }
    if (opcode === 0x9) { // ping -> pong
      this.socket.write(frame(0xa, payload));
      return true;
    }
    if (opcode === 0xa) return true; // unsolicited pong: ignore
    if (opcode !== 0x1) throw new Error('binary frames are not the gateway contract');
    let frame;
    try {
      frame = JSON.parse(payload.toString('utf8'));
    } catch {
      sendClose(this.socket, 4002, 'invalid_frame');
      this.destroy();
      return false;
    }
    if (!this.authed) {
      if (frame?.type !== 'hello') {
        sendClose(this.socket, 4001, 'hello_required');
        this.destroy();
        return false;
      }
      if (frame.protocolVersion !== PROTOCOL_VERSION) {
        sendClose(this.socket, 4001, 'protocol_mismatch');
        this.destroy();
        return false;
      }
      if (frame.token !== token) {
        sendClose(this.socket, 4003, 'auth_failed');
        this.destroy();
        return false;
      }
      this.authed = true;
      sendText(this.socket, JSON.stringify({
        type: 'hello_ok', protocolVersion: PROTOCOL_VERSION, serverVersion: SERVER_VERSION, serverInfo: SERVER_INFO,
      }));
      return true;
    }
    if (frame?.type !== 'request' || typeof frame.id !== 'string') {
      sendClose(this.socket, 4002, 'invalid_frame');
      this.destroy();
      return false;
    }
    if (frame.method === 'list_sessions') {
      sendText(this.socket, JSON.stringify({
        type: 'response', id: frame.id, ok: true, result: { sessions: SESSIONS },
      }));
    } else {
      sendText(this.socket, JSON.stringify({
        type: 'response', id: frame.id, ok: false, error: { code: 'method_not_found', message: `Unknown method: ${frame.method}` },
      }));
    }
    return true;
  }
}

const server = net.createServer((socket) => new Connection(socket));
server.on('error', (error) => {
  console.error(`mock-gateway server error: ${error.message}`);
  process.exit(1);
});
server.listen(port, '127.0.0.1', () => {
  console.log(`mock-gateway listening on 127.0.0.1:${port}`);
});
