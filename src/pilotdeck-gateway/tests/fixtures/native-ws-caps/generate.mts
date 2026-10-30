import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {readFile, mkdir, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {connect} from 'node:net';
import {once} from 'node:events';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
import {RFC6455_SAMPLE_NONCE} from '../native-ws-entry/sample.mts';

assert.equal(process.version, 'v22.22.0', '#1079 pinned Node');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/gateway/server/GatewayServer.ts', 'src/gateway/server/GatewayWsConnection.ts',
  'src/gateway/server/websocket.ts',
]);
const req = createRequire(join(baseline, 'package.json'));
const {transformSync} = createRequire(req.resolve('tsx'))('esbuild');
async function load(source: string, imports: Record<string, any> = {}) {
  const code = transformSync(await readFile(join(baseline, source), 'utf8'),
    {loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source}).code;
  const module = {exports: {} as any};
  new Function('require', 'module', 'exports', code)((name: string) => {
    if (name in imports) return imports[name];
    if (name.startsWith('node:')) return req(name);
    return new Proxy({}, {get(_target, key) {
      if (key === '__esModule') return true;
      throw new Error(`#1079 unexpected dependency ${source} -> ${name}.${String(key)}`);
    }});
  }, module, module.exports);
  return module.exports;
}
const version = await load('src/gateway/protocol/version.ts');
const errors = await load('src/gateway/dialog/errors.ts');
const skills = await load('src/extension/skills/SkillManager.ts');
const connection = await load('src/gateway/server/GatewayWsConnection.ts', {
  '../protocol/version.js': version, '../dialog/errors.js': errors,
  '../../extension/skills/index.js': skills,
});
const websocket = await load('src/gateway/server/websocket.ts');
const sockets: any[] = [];
const transports: any[] = [];
const http = req('node:http');
const observedHttp = {...http, createServer(...args: any[]) {
  const server = http.createServer(...args);
  server.on('connection', (socket: any) => sockets.push(socket));
  return server;
}};
let closedTransports = 0;
const observedWebsocket = {...websocket, TextWebSocketConnection: class extends websocket.TextWebSocketConnection {
  constructor(socket: any) {
    super(socket);
    transports.push(this);
    socket.fixtureTransport = this;
    this.onClose(() => closedTransports++);
  }
}};
const {startGatewayServer} = await load('src/gateway/server/GatewayServer.ts', {
  'node:http': observedHttp, './websocket.js': observedWebsocket,
  './GatewayWsConnection.js': connection,
});
const deadline = setTimeout(() => {console.error('#1079 TS probe deadline'); process.exit(1);}, 30000);
let holdBusiness = false;
let completedBusiness = 0;
const held: Array<{release: () => void, done: Promise<void>}> = [];
const server = await startGatewayServer({host: '127.0.0.1', port: 0,
  token: 'fixture-token', gateway: {async describeServer() {
    if (holdBusiness) {
      let release!: () => void;
      let complete!: () => void;
      const gate = new Promise<void>(resolve => {release = resolve;});
      const done = new Promise<void>(resolve => {complete = resolve;});
      held.push({release, done});
      await gate;
      completedBusiness++;
      complete();
    }
    return {};
  }}});
const peers: any[] = [];
const port = Number(new URL(server.url).port);
async function peer(upgrade: boolean) {
  const socket = connect(port, '127.0.0.1');
  peers.push(socket);
  await once(socket, 'connect');
  if (upgrade) {
    let bytes = '';
    const upgraded = new Promise<void>(resolve => socket.on('data', chunk => {
      bytes += chunk.toString();
      if (bytes.includes('\r\n\r\n')) resolve();
    }));
    socket.write(`GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: ${RFC6455_SAMPLE_NONCE}\r\n\r\n`);
    await upgraded;
    assert.ok(bytes.startsWith('HTTP/1.1 101'), '#1079 actual GatewayServer upgrade');
  } else socket.write('GET /ws HTTP/1.1\r\n');
  return socket;
}
const observations: any = {};
try {
  // A never-completing HTTP predecessor remains connected during all 65 upgrades.
  const incomplete = await peer(false);
  for (let i = 0; i < 65; i++) await peer(true);
  observations.connections = {upgraded: transports.length, accepted: sockets.length,
    incompleteOpen: !incomplete.destroyed, closedTransports};
  assert.deepEqual(observations.connections,
    {upgraded: 65, accepted: 66, incompleteOpen: true, closedTransports: 0}, '#1079 connection sample');

  // Exact callback boundaries on transports constructed by the real GatewayServer.
  // No copied decoder or surrogate transport; this bypasses only TCP chunk splitting.
  const payload = 100 * 1024 * 1024;
  const partial = 90 * 1024 * 1024;
  const active = sockets.filter(socket => socket.fixtureTransport).slice(0, 3);
  for (const socket of active) {
    const chunk = Buffer.alloc(partial);
    chunk[0] = 0x82; chunk[1] = 0xff;
    chunk.writeBigUInt64BE(BigInt(payload), 2);
    socket.emit('data', chunk);
  }
  const buffered = () => active.reduce((sum, socket) => sum + socket.fixtureTransport.buffer.length, 0);
  observations.aggregate = {connections: active.length, declaredPayload: payload,
    buffered: buffered(), closed: active.map(socket => socket.fixtureTransport.closed),
    timeouts: active.map(socket => socket.timeout ?? 0)};
  assert.equal(buffered(), 270 * 1024 * 1024, '#1079 aggregate exceeds Rust 256 MiB budget');
  active[0].emit('data', Buffer.alloc(payload + 14 - partial));
  observations.consumed = {firstBuffered: active[0].fixtureTransport.buffer.length,
    remainingBuffered: buffered(), closed: active[0].fixtureTransport.closed};
  const closed = once(active[1], 'close');
  active[1].destroy(); await closed;
  observations.cancelled = {destroyed: active[1].destroyed,
    closeNotified: active[1].fixtureTransport.closeNotified, closedTransports};
  await peer(true);
  observations.replacement = {upgraded: transports.length, closedTransports};

  // Real transport + real GatewayWsConnection; exact callback and service gates.
  // Four complete 1 MiB frames are consumed while every describeServer is held.
  const pendingSocket = sockets.filter(socket => socket.fixtureTransport)[3];
  const pendingTransport = pendingSocket.fixtureTransport;
  let deliveredBytes = 0;
  pendingTransport.onMessage((text: string) => {deliveredBytes += Buffer.byteLength(text);});
  function largeText(value: any) {
    value.padding = '';
    value.padding = 'x'.repeat(1024 * 1024 - Buffer.byteLength(JSON.stringify(value)));
    const text = Buffer.from(JSON.stringify(value));
    const wire = Buffer.alloc(14 + text.length);
    wire[0] = 0x81; wire[1] = 0xff;
    wire.writeBigUInt64BE(BigInt(text.length), 2); // zero mask; real decoder still runs
    text.copy(wire, 14);
    return wire;
  }
  holdBusiness = true;
  pendingSocket.emit('data', largeText({type: 'hello', protocolVersion: '1.2',
    clientName: 'ui', clientVersion: 'p', token: 'fixture-token'}));
  for (let i = 0; i < 3; i++) pendingSocket.emit('data', largeText({type: 'request',
    id: String(i), method: 'describe_server', params: {}}));
  observations.pendingRequests = {entered: held.length, completed: completedBusiness,
    deliveredBytes, buffered: pendingTransport.buffer.length, closed: pendingTransport.closed};
  assert.deepEqual(observations.pendingRequests,
    {entered: 4, completed: 0, deliveredBytes: 4194304, buffered: 0, closed: false},
    '#1079 complete frames remain pending beyond decoder');
  held[0].release(); await held[0].done;
  observations.completedRequest = {completed: completedBusiness, pending: held.length - completedBusiness};
  const pendingClosed = once(pendingSocket, 'close');
  pendingSocket.destroy(); await pendingClosed;
  observations.closedPendingRequests = {closed: pendingTransport.closeNotified,
    completed: completedBusiness, pending: held.length - completedBusiness};
  assert.equal(completedBusiness, 1, '#1079 socket close does not cancel business');
  for (const item of held) item.release();
  await Promise.all(held.map(item => item.done));
  observations.completedAfterClose = {completed: completedBusiness};
  holdBusiness = false;
  observations.lossyText = [[0x61, 0xff, 0xff, 0x62], [0xf0, 0x9f, 0x92],
    [0xe2, 0x82, 0xac], [0xed, 0xa0, 0x80]].map(bytes => {
      const text = Buffer.from(bytes).toString('utf8');
      return {bytes, text, utf8Bytes: Buffer.byteLength(text)};
    });

  let done = false;
  const closing = server.close().then(() => {done = true;});
  await new Promise(resolve => setImmediate(resolve));
  observations.pendingStop = {closed: done, pendingBuffered: active[2].fixtureTransport.buffer.length};
  for (const socket of sockets) socket.destroy();
  for (const socket of peers) socket.destroy();
  await closing;
  observations.cancelledStop = {closed: done};
  const output = {sourceCommit, sourceSha256, node: process.version, observations};
  await mkdir(process.argv[2], {recursive: true});
  await writeFile(join(process.argv[2], 'oracle.json'), JSON.stringify(output, null, 2) + '\n', {flag: 'wx'});
  console.log(JSON.stringify(output, null, 2));
} finally {
  for (const item of held) item.release();
  for (const socket of peers) socket.destroy();
  for (const socket of sockets) socket.destroy();
  await server.close().catch(() => {});
  clearTimeout(deadline);
}
