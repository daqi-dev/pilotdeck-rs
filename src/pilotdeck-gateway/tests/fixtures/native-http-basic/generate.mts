// #1209 golden fixture: raw-socket behavior of the TS GatewayServer's
// non-upgrade HTTP routing (health / local-token / static fallback / 404),
// recorded byte-exactly per cell with the Date value masked.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdtemp, mkdir, writeFile} from 'node:fs/promises';
import {realpathSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {connect} from 'node:net';
import {once} from 'node:events';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#1209 pinned Node');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, ['src/gateway/server/GatewayServer.ts', 'src/gateway/server/staticAssets.ts']);
const deadline = setTimeout(() => { console.error('#1209 probe deadline'); process.exit(1); }, 120000);
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
const http = req('node:http');

// Deterministic static roots; contents never change between runs.
const parent = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-1209-golden-'));
const root = join(parent, 'root');
const root2 = join(parent, 'root2');
await mkdir(join(root, 'sub'), {recursive: true});
await mkdir(root2, {recursive: true});
const files: Array<[string, string]> = [
  [join(root, 'index.html'), '<!doctype html><html>index</html>\n'],
  [join(root, 'app.js'), 'console.log(1)\n'],
  [join(root, 'style.css'), 'body{}\n'],
  [join(root, 'data.json'), '{"a":1}\n'],
  [join(root, 'logo.svg'), '<svg/>\n'],
  [join(root, 'bin.bin'), '0,1,2,255,254\n'],
  [join(root, 'noext'), 'no extension\n'],
  [join(root, 'upper.TXT'), 'upper\n'],
  [join(root, 'sub', 'page.html'), 'sub page\n'],
  [join(root, 'enc%20oded'), 'encoded-name\n'],
  [join(root2, 'only.txt'), 'only file\n'],
];
for (const [path, content] of files) await writeFile(path, content);

const gateway = { async describeServer(){ return {name:'probe',capabilities:[]}; } };
const token = 'fixture-token';
const serverA = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe', staticAssetsPath: root});
const serverB = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe', staticAssetsPath: root2});
const serverC = await startGatewayServer({gateway: gateway as any, host: '127.0.0.2', port: 0, allowRemoteHost: true, token, serverVersion: 'probe', staticAssetsPath: root});
const serverD = await startGatewayServer({gateway: gateway as any, host: 'localhost', port: 0, token, serverVersion: 'probe'});
const servers: Record<string, {url: string}> = {a: serverA, b: serverB, c: serverC, d: serverD};

const maskDate = (bytes: string) => bytes.replace(/^date:.*$/gim, 'date: <masked>');
async function raw(server: string, request: string, quietMs = 250): Promise<{response: string, closed: boolean}> {
  const socket = connect(Number(new URL(servers[server].url).port), new URL(servers[server].url).hostname);
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
  if (!closed && !socket.destroyed) socket.destroy();
  return {response: maskDate(bytes), closed};
}

const H = (host = '127.0.0.1') => `Host: ${host}\r\n`;
const get = (target: string, host?: string) => `GET ${target} HTTP/1.1\r\n${H(host)}\r\n`;
const head = (target: string, host?: string) => `HEAD ${target} HTTP/1.1\r\n${H(host)}\r\n`;
const method = (m: string, target: string, host?: string) => `${m} ${target} HTTP/1.1\r\n${H(host)}\r\n`;
// Each cell: server under test and the exact request bytes (latin1).
const cells: Array<{id: string, server: string, request: string}> = [
  // health routing: every method llhttp accepts routes identically.
  {id: 'health-get', server: 'a', request: get('/health')},
  {id: 'health-head', server: 'a', request: head('/health')},
  {id: 'health-post', server: 'a', request: 'POST /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 5\r\n\r\nhello'},
  {id: 'health-put', server: 'a', request: method('PUT', '/health')},
  {id: 'health-delete', server: 'a', request: method('DELETE', '/health')},
  {id: 'health-options', server: 'a', request: method('OPTIONS', '/health')},
  {id: 'health-trace', server: 'a', request: method('TRACE', '/health')},
  {id: 'health-patch', server: 'a', request: method('PATCH', '/health')},
  {id: 'health-propfind', server: 'a', request: method('PROPFIND', '/health')},
  {id: 'health-mkcol', server: 'a', request: method('MKCOL', '/health')},
  {id: 'health-msearch', server: 'a', request: method('M-SEARCH', '/health')},
  {id: 'health-purge', server: 'a', request: method('PURGE', '/health')},
  {id: 'health-search', server: 'a', request: method('SEARCH', '/health')},
  {id: 'health-acl', server: 'a', request: method('ACL', '/health')},
  {id: 'health-report', server: 'a', request: method('REPORT', '/health')},
  {id: 'health-mkcalendar', server: 'a', request: method('MKCALENDAR', '/health')},
  // health path normalization and variants.
  {id: 'health-query', server: 'a', request: get('/health?x=1')},
  {id: 'health-fragment', server: 'a', request: get('/health#x')},
  {id: 'health-dot', server: 'a', request: get('/./health')},
  {id: 'health-dotdot', server: 'a', request: get('/sub/../health')},
  {id: 'health-pct-dotdot', server: 'a', request: get('/%2e%2e/health')},
  {id: 'health-pct-upper', server: 'a', request: get('/%2E%2E/health')},
  {id: 'health-backslash', server: 'a', request: get('/sub\\..\\health')},
  {id: 'health-dblslash', server: 'a', request: get('//health')},
  {id: 'health-absolute', server: 'a', request: get('http://127.0.0.1/health')},
  {id: 'health-upper-path', server: 'a', request: get('/HEALTH')},
  {id: 'health-trailing', server: 'a', request: get('/health/')},
  // a close request is answered with Connection: close and the socket closed.
  {id: 'health-conn-close', server: 'a', request: 'GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n'},
  {id: 'head-conn-close', server: 'a', request: 'HEAD /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n'},
  // local-token: loopback binds return the token, non-loopback 404s.
  {id: 'token-get', server: 'a', request: get('/auth/local-token')},
  {id: 'token-head', server: 'a', request: head('/auth/local-token')},
  {id: 'token-post', server: 'a', request: method('POST', '/auth/local-token')},
  {id: 'token-query', server: 'a', request: get('/auth/local-token?x=1')},
  {id: 'token-authorization', server: 'a', request: 'GET /auth/local-token HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer wrong\r\n\r\n'},
  {id: 'token-trailing', server: 'a', request: get('/auth/local-token/')},
  {id: 'token-localhost', server: 'd', request: get('/auth/local-token', 'localhost')},
  {id: 'token-remote', server: 'c', request: get('/auth/local-token')},
  {id: 'health-remote', server: 'c', request: get('/health')},
  {id: 'static-remote', server: 'c', request: get('/app.js')},
  // static fallback: extension map, directories, missing files.
  {id: 'static-index', server: 'a', request: get('/')},
  {id: 'static-index-head', server: 'a', request: head('/')},
  {id: 'static-appjs', server: 'a', request: get('/app.js')},
  {id: 'static-appjs-query', server: 'a', request: get('/app.js?v=2')},
  {id: 'static-css', server: 'a', request: get('/style.css')},
  {id: 'static-json', server: 'a', request: get('/data.json')},
  {id: 'static-svg', server: 'a', request: get('/logo.svg')},
  {id: 'static-bin', server: 'a', request: get('/bin.bin')},
  {id: 'static-noext', server: 'a', request: get('/noext')},
  {id: 'static-upper', server: 'a', request: get('/upper.TXT')},
  {id: 'static-upper-lower', server: 'a', request: get('/upper.txt')},
  {id: 'static-sub', server: 'a', request: get('/sub/page.html')},
  {id: 'static-dir', server: 'a', request: get('/sub/')},
  {id: 'static-missing', server: 'a', request: get('/missing')},
  {id: 'static-encoded', server: 'a', request: get('/enc%20oded')},
  {id: 'static-fragment', server: 'a', request: get('/app.js#x')},
  // absolute-form keeps the // in the path (unlike origin-form //app.js,
  // which new URL resolves against the base as scheme-relative).
  {id: 'static-absolute-dblslash', server: 'a', request: get('http://127.0.0.1//app.js')},
  // traversal attempts: URL normalization decides what is served.
  {id: 'traversal-parent', server: 'a', request: get('/../x')},
  {id: 'traversal-encoded', server: 'a', request: get('/..%2f..%2fetc/passwd')},
  {id: 'traversal-pct', server: 'a', request: get('/%2e%2e/etc/passwd')},
  {id: 'traversal-backslash', server: 'a', request: get('/..\\..\\x')},
  {id: 'traversal-pct-appjs', server: 'a', request: get('/%2e%2e/app.js')},
  {id: 'traversal-backslash-appjs', server: 'a', request: get('/sub\\..\\app.js')},
  {id: 'traversal-dotsegments', server: 'a', request: get('/sub/../app.js')},
  {id: 'traversal-null', server: 'a', request: get('/a%00b')},
  // root2 has no index.html: the fallback fails, so missing paths 404.
  {id: 'root2-missing', server: 'b', request: get('/missing')},
  {id: 'root2-index', server: 'b', request: get('/')},
  {id: 'root2-only', server: 'b', request: get('/only.txt')},
  {id: 'root2-only-head', server: 'b', request: head('/only.txt')},
  {id: 'root2-health', server: 'b', request: get('/health')},
  {id: 'root2-token', server: 'b', request: get('/auth/local-token')},
  // no static root configured: everything unmatched is the plain 404.
  {id: 'nostatic-missing', server: 'd', request: get('/missing')},
  {id: 'nostatic-index', server: 'd', request: get('/')},
  // parser-level behavior in front of the router.
  {id: 'bad-method-foo', server: 'a', request: method('FOO', '/health')},
  {id: 'bad-method-get', server: 'a', request: method('get', '/health')},
  {id: 'bad-method-getx', server: 'a', request: method('GETX', '/health')},
  {id: 'non-ascii-path', server: 'a', request: get('/cafÃ©.html')},
  {id: 'non-ascii-query', server: 'a', request: get('/health?x=Ã©')},
  {id: 'no-host', server: 'a', request: 'GET /health HTTP/1.1\r\n\r\n'},
  {id: 'garbage', server: 'a', request: 'GARBAGE\r\n\r\n'},
  {id: 'pct-target', server: 'a', request: get('%2Fhealth')},
  {id: 'authority-form', server: 'a', request: method('GET', '127.0.0.1:1')},
  {id: 'abs-scheme-nonalpha', server: 'a', request: get('a1://x/health')},
  {id: 'empty-request', server: 'a', request: '\r\nHost: 127.0.0.1\r\n\r\n'},
  {id: 'star', server: 'a', request: method('OPTIONS', '*')},
  {id: 'connect', server: 'a', request: method('CONNECT', '127.0.0.1:1')},
  // a non-ASCII target is llhttp's bare 400 even for CONNECT (parse-time).
  {id: 'connect-nonascii', server: 'a', request: method('CONNECT', 'cafÃ©:1')},
  {id: 'pipeline', server: 'a', request: get('/health') + get('/')},
  {id: 'unread-body-pipeline', server: 'a', request: 'POST /health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 5\r\n\r\nhello' + get('/app.js')},
];

const observations: any[] = [];
try {
  for (const cell of cells) {
    const {response, closed} = await raw(cell.server, cell.request);
    if (!response && !closed) { console.error(`#1209 cell ${cell.id} produced no bytes and stayed open`); process.exit(1); }
    observations.push({id: cell.id, server: cell.server, request: cell.request, response, closed});
  }
  const output = {sourceCommit, sourceSha256, node: process.version, methods: http.METHODS, cells: observations};
  await mkdir(process.argv[2], {recursive: true});
  await writeFile(join(process.argv[2], 'oracle.json'), JSON.stringify(output, null, 2) + '\n', {flag: 'wx'});
  console.log(JSON.stringify(output, null, 2));
} finally {
  await serverA.close().catch(() => {});
  await serverB.close().catch(() => {});
  await serverC.close().catch(() => {});
  await serverD.close().catch(() => {});
  clearTimeout(deadline);
}
