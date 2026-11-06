// #1351 golden fixture: the 16384-byte TrackHeader span budget applied to
// the TS GatewayServer's WS *upgrade* lane, cell-recorded byte-exactly at the
// boundary (16383 served / 16384 rejected) on both the upgrade and the
// non-upgrade lane, with the Date value masked. Same loading contract as
// native-http-basic: real GatewayServer on the pinned baseline, raw sockets,
// 250 ms quiet-window capture.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdir, writeFile} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {connect} from 'node:net';
import {once} from 'node:events';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#1351 pinned Node');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, ['src/gateway/server/GatewayServer.ts', 'src/gateway/server/websocket.ts']);
const deadline = setTimeout(() => { console.error('#1351 probe deadline'); process.exit(1); }, 120000);
const req = createRequire(join(baseline, 'package.json'));
const {transformSync} = createRequire(req.resolve('tsx'))('esbuild');
async function load(source:string, imports:Record<string,any>={}) {
  const code=transformSync(await (await import('node:fs/promises')).readFile(join(baseline,source),'utf8'),{loader:'ts',format:'cjs',target:'es2022',sourcefile:source}).code;
  const module={exports:{} as any};
  new Function('require','module','exports',code)((name:string)=>{
    if(name in imports)return imports[name];
    if(name.startsWith('node:'))return req(name);
    return new Proxy({}, {get(_t,p){if(p==='__esModule')return true;throw new Error(`unexercised import ${source} -> ${name}.${String(p)}`);}});
  },module,module.exports);return module.exports;
}
const version=await load('src/gateway/protocol/version.ts');
const errors=await load('src/gateway/dialog/errors.ts');
const skills=await load('src/extension/skills/SkillManager.ts');
const connection=await load('src/gateway/server/GatewayWsConnection.ts',{'../protocol/version.js':version,'../dialog/errors.js':errors,'../../extension/skills/index.js':skills});
const websocket=await load('src/gateway/server/websocket.ts');
const staticAssets=await load('src/gateway/server/staticAssets.ts');
const approval=await load('src/composition/publicApprovalHttpTransport.ts');
const hostTransport=await load('src/composition/publicHostHttpTransport.ts');
const webRouter=await load('src/adapters/web/httpRouter.ts');
const {startGatewayServer}=await load('src/gateway/server/GatewayServer.ts',{
  './websocket.js':websocket,'./GatewayWsConnection.js':connection,'./staticAssets.js':staticAssets,
  '../../composition/publicApprovalHttpTransport.js':approval,
  '../../composition/publicHostHttpTransport.js':hostTransport,
  '../../adapters/web/httpRouter.js':webRouter,
});

// One loopback listener, no static root: the budget fires in the header
// parser before any routing, upgrade or not.
const gateway = { async describeServer(){ return {name:'probe',capabilities:[]}; } };
const server = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token: 'fixture-token', serverVersion: 'probe'});

const maskDate = (bytes: string) => bytes.replace(/^date:.*$/gim, 'date: <masked>');
async function raw(request: string, quietMs = 250, wsClose = false): Promise<{response: string, closed: boolean}> {
  const address = new URL(server.url);
  const socket = connect(Number(address.port), address.hostname);
  await once(socket, 'connect');
  let bytes = '';
  let last = Date.now();
  let closed = false;
  socket.on('data', (b: Buffer) => { bytes += b.toString('latin1'); last = Date.now(); });
  socket.on('close', () => { closed = true; last = 0; });
  socket.on('error', () => {});
  socket.write(Buffer.from(request, 'latin1'));
  const start = Date.now();
  while (Date.now() - start < 5000) {
    await new Promise(r => setTimeout(r, 25));
    if (closed || socket.destroyed) break;
    if (bytes && Date.now() - last > quietMs) break;
  }
  // The recorded state is the capture-window verdict; the teardown below is
  // harness shutdown, not part of the observation, so the bytes are frozen
  // before it (the still-attached data listener would otherwise race the
  // close reply into the record).
  const observed = closed;
  const captured = bytes;
  if (wsClose && !closed && !socket.destroyed) {
    // A real ws client's close handshake: the TS connection only ends its
    // socket on a masked close frame (a bare client FIN is never resumed),
    // so without this the listener keeps the connection and close() hangs.
    // The capture window is already over; the reply frame is not recorded.
    socket.write(Buffer.from([0x88, 0x80, 1, 2, 3, 4]));
    await Promise.race([once(socket, 'close'), new Promise(r => setTimeout(r, 2000))]);
  }
  if (!closed && !socket.destroyed) socket.destroy();
  return {response: maskDate(captured), closed: observed};
}

// The RFC6455 §1.3 sample nonce (tests/common/native_ws_sample.rs peer);
// value is public spec text, not a secret. gitleaks:allow
const nonce = 'dGhlIHNhbXBsZSBub25jZQ==';
// Node's TrackHeader span over the head: the request-line target plus every
// header name and value (separators excluded, single leading value OWS
// skipped) — computed from the request bytes so each cell's arithmetic is
// checked at record time instead of trusted from a comment.
function trackedSpan(request: string): number {
  const head = request.slice(0, request.indexOf('\r\n\r\n'));
  const lines = head.split('\r\n');
  const target = lines[0]!.split(' ')[1]!;
  let span = target.length;
  for (const line of lines.slice(1)) {
    const colon = line.indexOf(':');
    span += line.slice(0, colon).length + line.slice(colon + 1).replace(/^ +/, '').length;
  }
  return span;
}
// upgrade head = 3 ("/ws") + 13 (Host: localhost) + 17 (Connection: Upgrade)
// + 16 (Upgrade: websocket) + 41 (Sec-WebSocket-Key: nonce) + 3 + n (x-p)
// = 93 + n; plain head = 7 ("/health") + 13 + 3 + n = 23 + n.
const upgradeHead = (n: number) => `GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: ${nonce}\r\nx-p: ${'a'.repeat(n)}\r\n\r\n`;
const plainHead = (n: number) => `GET /health HTTP/1.1\r\nHost: localhost\r\nx-p: ${'a'.repeat(n)}\r\n\r\n`;
const TOO_LARGE = 'HTTP/1.1 431 Request Header Fields Too Large\r\nConnection: close\r\n\r\n';
// Each cell: the exact request bytes and the recorded boundary arithmetic.
const cells: Array<{id: string, request: string, tracked: number}> = [
  {id: 'upgrade-16383', request: upgradeHead(16290), tracked: 16383},
  {id: 'upgrade-16384', request: upgradeHead(16291), tracked: 16384},
  // One past the inclusive boundary and a 20 KiB far-over cell (#1351
  // 2026-11-06 round probe `probe.out`: 16385 and 20480 are the same canned
  // 431 + close as 16384 on the upgrade lane, exactly like the plain lane).
  {id: 'upgrade-16385', request: upgradeHead(16292), tracked: 16385},
  {id: 'upgrade-20480', request: upgradeHead(20387), tracked: 20480},
  // b4-U1.out's far-over shape (17 020 tracked) on the same head template.
  {id: 'upgrade-17020', request: upgradeHead(16927), tracked: 17020},
  {id: 'plain-16383', request: plainHead(16360), tracked: 16383},
  {id: 'plain-16384', request: plainHead(16361), tracked: 16384},
  {id: 'plain-16385', request: plainHead(16362), tracked: 16385},
  {id: 'plain-20480', request: plainHead(20457), tracked: 20480},
];

const observations: any[] = [];
try {
  for (const cell of cells) {
    assert.equal(trackedSpan(cell.request), cell.tracked, `#1351 ${cell.id} span arithmetic`);
    const {response, closed} = await raw(cell.request, 250, cell.id === 'upgrade-16383');
    if (!response && !closed) { console.error(`#1351 cell ${cell.id} produced no bytes and stayed open`); process.exit(1); }
    // The boundary itself must land where the arithmetic says it does.
    if (cell.id === 'upgrade-16383') {
      assert.ok(response.startsWith('HTTP/1.1 101 Switching Protocols\r\n'), `#1351 ${cell.id} upgrade accepted`);
      assert.ok(response.includes('sec-websocket-accept: ') || response.includes('Sec-WebSocket-Accept: '), `#1351 ${cell.id} accept header`);
      assert.equal(closed, false, `#1351 ${cell.id} stays open`);
    } else if (cell.id === 'plain-16383') {
      assert.ok(response.startsWith('HTTP/1.1 200 OK\r\n'), `#1351 ${cell.id} health served`);
      assert.ok(response.includes('{"ok":true}'), `#1351 ${cell.id} health body`);
      assert.equal(closed, false, `#1351 ${cell.id} stays open`);
    } else {
      assert.equal(response, TOO_LARGE, `#1351 ${cell.id} canned 431`);
      assert.equal(closed, true, `#1351 ${cell.id} closes`);
    }
    observations.push({id: cell.id, tracked: cell.tracked, request: cell.request, response, closed});
  }
  const output = {sourceCommit, sourceSha256, node: process.version, cells: observations};
  await mkdir(process.argv[2], {recursive: true});
  await writeFile(join(process.argv[2], 'oracle.json'), JSON.stringify(output, null, 2) + '\n', {flag: 'wx'});
  console.log(JSON.stringify(output, null, 2));
} finally {
  await server.close().catch(() => {});
  clearTimeout(deadline);
}
