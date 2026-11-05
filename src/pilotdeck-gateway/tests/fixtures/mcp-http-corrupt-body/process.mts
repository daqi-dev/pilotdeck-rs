// #1553: legacy SSE start() determinism matrix on the real baseline SDK.
// A corrupt gzip member before the first endpoint event must reject start()
// immediately with the decompression cause; every other failure state is
// pinned here as the reference for the Rust matrix test. Raw TCP sockets,
// loopback only, baseline tree read-only.
import assert from 'node:assert/strict';
import net from 'node:net';
import zlib from 'node:zlib';
import { mkdirSync, writeFileSync, readFileSync, realpathSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindBaseline } from '../_shared/generator_guard.mjs';
assert.equal(process.version, 'v22.22.0', '#1553 Node version');
assert.ok(process.env.PDRS_BASELINE, '#1553 PDRS_BASELINE required');
assert.ok(process.argv[2], '#1553 output directory required');
const baseline = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]);
const git = realpathSync(execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const { ref: sourceCommit } = bindBaseline({ baseline, commit: 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', gitExecutable: git }).provenance;
const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const sdkRoot = 'node_modules/@modelcontextprotocol/sdk';
const sdk = JSON.parse(readFileSync(join(baseline, sdkRoot, 'package.json'), 'utf8')).version;
assert.equal(sdk, '1.29.0', '#1553 SDK version');
const { SSEClientTransport } = await load(`${sdkRoot}/dist/esm/client/sse.js`);

const ENDPOINT_SSE = 'event: endpoint\ndata: /post\n\n';
const corrupt = Buffer.from('invalid compressed body');
const validEndpointGzip = zlib.gzipSync(Buffer.from(ENDPOINT_SSE));
// A valid gzip prefix cut mid-stream: the wire promises more bytes than sent.
const wireInterrupt = validEndpointGzip.subarray(0, 18);
// Valid gzip member + a corrupt trailing member: the endpoint decodes, then the
// second member fails the header check (the staged post-endpoint row).
const tailCorrupt = Buffer.concat([validEndpointGzip, corrupt]);

function errorShape(error: any) {
  return { name: error?.name, code: error?.code ?? null, message: error?.message };
}
function outcomeOf(started: Promise<void>, ms: number) {
  return Promise.race([
    started.then(() => ({ ok: true }), (error) => ({ ok: false, error: errorShape(error) })),
    new Promise(resolve => setTimeout(() => resolve({ ok: `pending-${ms}ms` }), ms)),
  ]);
}
function head(extra: Record<string, string | number>) {
  const lines = Object.entries(extra).map(([key, value]) => `${key}: ${value}`);
  return `HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n${lines.join('\r\n')}\r\nConnection: close\r\n\r\n`;
}
// One scripted raw-socket SSE server per row. `script` writes the response
// once the request arrives; every accepted socket is owned here and destroyed
// when the row finishes, so no server outlives its case.
async function row(
  name: string,
  script: (socket: net.Socket) => void,
  probe: (transport: any, started: Promise<void>) => Promise<any>,
) {
  const sockets = new Set<net.Socket>();
  const server = net.createServer(socket => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    socket.once('data', () => script(socket));
  });
  server.listen(0, '127.0.0.1');
  await new Promise<void>(done => server.once('listening', done));
  const watchdog = setTimeout(() => { throw Error(`#1553 ${name}: row watchdog`); }, 10000);
  const url = new URL(`http://127.0.0.1:${(server.address() as net.AddressInfo).port}/sse`);
  const transport = new SSEClientTransport(url);
  const lateErrors: any[] = [];
  transport.onerror = error => lateErrors.push(errorShape(error));
  const started = transport.start();
  let result: any;
  try {
    result = await probe(transport, started);
  } finally {
    clearTimeout(watchdog);
    transport.close();
    for (const socket of sockets) socket.destroy();
    server.close();
  }
  const entry = { case: name, result, lateErrors };
  console.log(JSON.stringify(entry));
  return entry;
}

const entries: any[] = [];

// Corrupt gzip before the endpoint: start rejects with the codec cause — the
// golden the Rust replay asserts cell-for-cell (mcp-http-encoding oracle).
entries.push(await row('corrupt-before-endpoint',
  socket => socket.end(Buffer.concat([
    Buffer.from(head({ 'Content-Encoding': 'gzip', 'Content-Length': corrupt.length })), corrupt])),
  async (_transport, started) => ({ outcome: await outcomeOf(started, 3000) })));

// Empty SSE body that ends cleanly before any event: the EventSource itself
// would reconnect, but the SDK start() promise settles first.
entries.push(await row('clean-eof-empty-before-endpoint',
  socket => socket.end(Buffer.from(head({ 'Content-Length': 0 }))),
  async (_transport, started) => ({ outcome: await outcomeOf(started, 3000) })));

// A decodable SSE body that ends cleanly before the endpoint event.
entries.push(await row('clean-eof-data-before-endpoint',
  socket => {
    const body = zlib.gzipSync(Buffer.from('data: hello\n\n'));
    socket.end(Buffer.concat([Buffer.from(head({ 'Content-Encoding': 'gzip', 'Content-Length': body.length })), body]));
  },
  async (_transport, started) => ({ outcome: await outcomeOf(started, 3000) })));

// The wire promises a full gzip member but resets mid-body: undici reports the
// content-length mismatch as the cause of the termination.
entries.push(await row('wire-interrupt-before-endpoint',
  socket => {
    socket.write(head({ 'Content-Encoding': 'gzip', 'Content-Length': validEndpointGzip.length }));
    socket.write(wireInterrupt);
    socket.destroy();
  },
  async (_transport, started) => ({ outcome: await outcomeOf(started, 3000) })));

// A stalled body: start() stays pending while the read blocks; close() is the
// only way out. Both the pending marker and the post-close settlement are
// recorded — the Rust side settles close() with `Connection closed`.
entries.push(await row('abort-before-endpoint',
  socket => {
    socket.write(head({ 'Content-Encoding': 'gzip', 'Content-Length': validEndpointGzip.length }));
    socket.write(validEndpointGzip.subarray(0, 10));
    // never send the rest
  },
  async (transport, started) => {
    setTimeout(() => transport.close(), 200);
    const pending = await outcomeOf(started, 3000);
    return { outcome: pending, afterClose: await outcomeOf(started, 1000) };
  }));

// The endpoint event settles start(), then the wire itself fails: the plain
// body declares more bytes than it sends before the connection ends. The
// connection counter records whether the EventSource reconnects afterwards.
let cutConnections = 0;
entries.push(await row('endpoint-then-cut',
  socket => {
    cutConnections += 1;
    socket.write(head({ 'Content-Length': 50 }));
    socket.write(Buffer.from(ENDPOINT_SSE));
    socket.end();
  },
  async (_transport, started) => {
    const outcome = await outcomeOf(started, 3000);
    await new Promise(resolve => setTimeout(resolve, 4000));
    return { outcome, connections: cutConnections };
  }));

// A staged corruption after the endpoint: the first member decodes, start()
// resolves, and the trailing garbage never surfaces on the SSE stream.
let stagedConnections = 0;
entries.push(await row('corrupt-after-endpoint-staged',
  socket => {
    stagedConnections += 1;
    socket.write(head({ 'Content-Encoding': 'gzip', 'Content-Length': tailCorrupt.length }));
    socket.write(validEndpointGzip);
    setTimeout(() => { socket.write(corrupt); socket.end(); }, 500);
  },
  async (_transport, started) => {
    const outcome = await outcomeOf(started, 3000);
    await new Promise(resolve => setTimeout(resolve, 4000));
    return { outcome, connections: stagedConnections };
  }));

mkdirSync(output, { recursive: true });
writeFileSync(join(output, 'matrix.jsonl'), entries.map(entry => JSON.stringify(entry)).join('\n') + '\n',
  { flag: 'wx' });
