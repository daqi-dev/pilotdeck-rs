// #1348 golden fixture: raw-socket behavior of the TS GatewayServer's
// `/api/web/*` router (adapters/web/httpRouter.ts) over the real
// `startGatewayServer` with a scripted gateway stub (the same seam
// `GatewayServer` passes to `handleWebApiRequest`), recorded byte-exactly per
// cell with the Date value masked.
//
// The fixture project tree is created under a temp dir and referenced in the
// requests as the `<root>` placeholder: on the wire the placeholder expands to
// `encodeURIComponent(root)`, and the recorded responses carry the raw temp
// path replaced back with `<root>`, so the oracle does not embed a machine
// path. Directory `size` is masked (`"type":"directory","size":0`) because it
// is filesystem-dependent (APFS 96/64 vs ext4 4096); every file and directory
// mtime is pinned with utimes, so `modifiedAt` is a stable literal.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdir, mkdtemp, writeFile, readFile, symlink, utimes} from 'node:fs/promises';
import {realpathSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {connect} from 'node:net';
import {once} from 'node:events';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1348 pinned Node');
assert.equal(process.versions.icu, '78.2', '#1348 pinned ICU');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/adapters/web/httpRouter.ts',
  'src/adapters/web/projectFiles.ts',
  'src/gateway/server/GatewayServer.ts',
]);
const deadline = setTimeout(() => {
  console.error('#1348 probe deadline');
  process.exit(1);
}, 240000);
const req = createRequire(join(baseline, 'package.json'));
const {transformSync} = createRequire(req.resolve('tsx'))('esbuild');
async function load(source: string, imports: Record<string, any> = {}) {
  const code = transformSync(await readFile(join(baseline, source), 'utf8'), {loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source}).code;
  const module = {exports: {} as any};
  new Function('require', 'module', 'exports', code)((name: string) => {
    if (name in imports) return imports[name];
    if (name.startsWith('node:')) return req(name);
    return new Proxy({}, {get(_t, p) { if (p === '__esModule') return true; throw new Error(`unexercised import ${source} -> ${name}.${String(p)}`); }});
  }, module, module.exports);
  return module.exports;
}
const version = await load('src/gateway/protocol/version.ts');
const errors = await load('src/gateway/dialog/errors.ts');
const skills = await load('src/extension/skills/SkillManager.ts');
const connection = await load('src/gateway/server/GatewayWsConnection.ts', {'../protocol/version.js': version, '../dialog/errors.js': errors, '../../extension/skills/index.js': skills});
const websocket = await load('src/gateway/server/websocket.ts');
const staticAssets = await load('src/gateway/server/staticAssets.ts');
const approval = await load('src/composition/publicApprovalHttpTransport.ts');
const hostTransport = await load('src/composition/publicHostHttpTransport.ts');
const projectFiles = await load('src/adapters/web/projectFiles.ts');
const webRouter = await load('src/adapters/web/httpRouter.ts', {'./projectFiles.js': projectFiles});
const {startGatewayServer} = await load('src/gateway/server/GatewayServer.ts', {
  './websocket.js': websocket,
  './GatewayWsConnection.js': connection,
  './staticAssets.js': staticAssets,
  '../../composition/publicApprovalHttpTransport.js': approval,
  '../../composition/publicHostHttpTransport.js': hostTransport,
  '../../adapters/web/httpRouter.js': webRouter,
});

// ------------------------------------------------------------- fixture tree
const parent = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-1348-'));
const root = join(parent, 'proj');
const OLD = new Date('2020-01-02T03:04:05.006Z');
const NEW = new Date('2021-02-03T04:05:06.789Z');
const DIRECTORIES: Array<[string, Date]> = [
  ['sub', OLD],
  ['sub2', NEW],
];
const FILES: Array<[string, string | Buffer, Date]> = [
  ['aaa.txt', 'alpha\n', OLD],
  ['bbb.js', 'console.log(2)\n', NEW],
  ['binary.png', Buffer.from([0, 1, 2, 255]), NEW],
  ['data.bin', Buffer.from('hi'), NEW],
  ['empty.txt', '', NEW],
  ['UPPER.TXT', 'upper\n', NEW],
  ['config.env', 'KEY=1\n', NEW],
  ['README', 'no extension\n', NEW],
  ['big.bin', Buffer.alloc(1024 * 1024 + 1, 0x61), NEW],
  ['space name.txt', 'space\n', NEW],
  ['sub/child.txt', 'child\n', OLD],
];
const LINKS: Array<[string, string]> = [['inside-link', 'aaa.txt']];
await mkdir(root, {recursive: true});
for (const [dir] of DIRECTORIES) await mkdir(join(root, dir), {recursive: true});
for (const [path, body] of FILES) await writeFile(join(root, path), body);
for (const [path, target] of LINKS) await symlink(target, join(root, path));
// utimes follows symlinks, so the link cells record the target's mtime (the
// TS service uses `stat`, which follows too).
for (const [path, , when] of FILES) await utimes(join(root, path), when, when);
for (const [dir, when] of [...DIRECTORIES].reverse()) await utimes(join(root, dir), when, when);
await utimes(root, NEW, NEW);

// ------------------------------------------------------------- TS server
const token = 'fixture-token';
const gateway = {
  async listProjects() {
    return {projects: [{projectKey: root, name: 'proj', fullPath: root, sessionCount: 2, lastActivity: 1234567890, createdAt: 1234500000}]};
  },
  async describeProject(input: {projectKey: string}) {
    if (input.projectKey === 'boom') throw new Error('catalog exploded');
    return {projectKey: input.projectKey, name: 'described', fullPath: input.projectKey, sessionCount: 2};
  },
};
const server = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'fixture'});
const port = Number(new URL(server.url).port);

// ------------------------------------------------------------- recording
const encoded = encodeURIComponent(root);
const wire = (text: string) => text.replaceAll('<root>', encoded);
function mask(text: string): string {
  return text
    .replace(/^date:.*$/gim, 'date: <masked>')
    .replaceAll(encoded, '<root>')
    .replaceAll(root, '<root>')
    .replace(/"type":"directory","size":\d+/g, '"type":"directory","size":0');
}
async function raw(request: string, quietMs = 250) {
  const socket = connect(port, '127.0.0.1');
  await once(socket, 'connect');
  let bytes = '';
  let last = Date.now();
  let closed = false;
  socket.on('data', (b: Buffer) => { bytes += b.toString('latin1'); last = Date.now(); });
  socket.on('close', () => { closed = true; last = 0; });
  socket.on('error', () => {});
  socket.write(Buffer.from(wire(request), 'latin1'));
  const start = Date.now();
  while (Date.now() - start < 3000) {
    await new Promise(r => setTimeout(r, 25));
    if (closed || socket.destroyed) break;
    if (bytes && Date.now() - last > quietMs) break;
  }
  if (!closed && !socket.destroyed) socket.destroy();
  return {response: mask(bytes), closed};
}

const H = 'Host: 127.0.0.1\r\n';
const AUTH = `Authorization: Bearer ${token}\r\n`;
const get = (target: string, auth = true) => `GET ${target} HTTP/1.1\r\n${H}${auth ? AUTH : ''}\r\n`;
const head = (target: string, auth = true) => `HEAD ${target} HTTP/1.1\r\n${H}${auth ? AUTH : ''}\r\n`;
const method = (m: string, target: string, auth = true) => `${m} ${target} HTTP/1.1\r\n${H}${auth ? AUTH : ''}\r\n`;
const post = (target: string, body: string, auth = true) => `POST ${target} HTTP/1.1\r\n${H}${auth ? AUTH : ''}Content-Type: application/json\r\nContent-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`;
const PROJECT = '/api/web/projects/<root>';
const write = (body: unknown) => post(`${PROJECT}/files/write`, typeof body === 'string' ? body : JSON.stringify(body));

const cells: Array<{id: string; request: string; closed?: boolean}> = [
  // auth
  {id: 'projects-ok', request: get('/api/web/projects')},
  {id: 'projects-no-auth', request: get('/api/web/projects', false)},
  {id: 'projects-wrong-token', request: `GET /api/web/projects HTTP/1.1\r\n${H}Authorization: Bearer nope\r\n\r\n`},
  {id: 'projects-lower-bearer', request: `GET /api/web/projects HTTP/1.1\r\n${H}Authorization: bearer ${token}\r\n\r\n`},
  {id: 'projects-bearer-tab', request: `GET /api/web/projects HTTP/1.1\r\n${H}Authorization: Bearer\t${token}\r\n\r\n`},
  {id: 'projects-bearer-trailing-space', request: `GET /api/web/projects HTTP/1.1\r\n${H}Authorization: Bearer ${token} \r\n\r\n`},
  {id: 'projects-basic', request: `GET /api/web/projects HTTP/1.1\r\n${H}Authorization: Basic ${token}\r\n\r\n`},
  {id: 'projects-empty-auth', request: `GET /api/web/projects HTTP/1.1\r\n${H}Authorization: \r\n\r\n`},
  {id: 'projects-duplicate-bad-first', request: `GET /api/web/projects HTTP/1.1\r\n${H}Authorization: Bearer nope\r\nAuthorization: Bearer ${token}\r\n\r\n`},
  {id: 'projects-duplicate-good-first', request: `GET /api/web/projects HTTP/1.1\r\n${H}Authorization: Bearer ${token}\r\nAuthorization: Bearer nope\r\n\r\n`},
  // methods and routing
  {id: 'projects-head', request: head('/api/web/projects')},
  {id: 'projects-post', request: method('POST', '/api/web/projects')},
  {id: 'projects-put', request: method('PUT', '/api/web/projects')},
  {id: 'projects-trailing-slash', request: get('/api/web/projects/')},
  {id: 'api-web-bare', request: get('/api/web/')},
  {id: 'api-web-no-slash', request: get('/api/web')},
  {id: 'api-web-other', request: get('/api/web/nope')},
  {id: 'files-tree-post', request: method('POST', `${PROJECT}/files/tree`)},
  {id: 'files-unknown-sub', request: get(`${PROJECT}/files/nope`)},
  {id: 'files-encoded-slash', request: get('/api/web/projects/<root>/files%2Ftree')},
  {id: 'files-write-get', request: get(`${PROJECT}/files/write`)},
  {id: 'files-read-head', request: head(`${PROJECT}/files/read?path=aaa.txt`)},
  // describe
  {id: 'project-describe', request: get(PROJECT)},
  {id: 'project-describe-trailing-slash', request: get(`${PROJECT}/`)},
  {id: 'project-describe-post', request: method('POST', PROJECT)},
  {id: 'project-describe-badpct', request: get('/api/web/projects/%E0%A4%A')},
  {id: 'project-describe-plus', request: get('/api/web/projects/a+b')},
  {id: 'project-describe-throws', request: get('/api/web/projects/boom')},
  // tree
  {id: 'files-tree-root', request: get(`${PROJECT}/files/tree`)},
  {id: 'files-tree-sub', request: get(`${PROJECT}/files/tree?path=sub`)},
  {id: 'files-tree-empty-path', request: get(`${PROJECT}/files/tree?path=`)},
  {id: 'files-tree-dot', request: get(`${PROJECT}/files/tree?path=.`)},
  {id: 'files-tree-dotdot-inside', request: get(`${PROJECT}/files/tree?path=sub/..`)},
  {id: 'files-tree-missing', request: get(`${PROJECT}/files/tree?path=missing`)},
  {id: 'files-tree-file', request: get(`${PROJECT}/files/tree?path=aaa.txt`)},
  {id: 'files-tree-escape', request: get(`${PROJECT}/files/tree?path=..`)},
  {id: 'files-tree-absolute', request: get(`${PROJECT}/files/tree?path=/etc`)},
  {id: 'files-tree-dotdot-prefix', request: get(`${PROJECT}/files/tree?path=..foo`)},
  // read
  {id: 'files-read-text', request: get(`${PROJECT}/files/read?path=aaa.txt`)},
  {id: 'files-read-dot', request: get(`${PROJECT}/files/read?path=./aaa.txt`)},
  {id: 'files-read-dotdot-inside', request: get(`${PROJECT}/files/read?path=sub/../aaa.txt`)},
  {id: 'files-read-two-paths', request: get(`${PROJECT}/files/read?path=aaa.txt&path=bbb.js`)},
  {id: 'files-read-plus-space', request: get(`${PROJECT}/files/read?path=space+name.txt`)},
  {id: 'files-read-encoded-path', request: get(`${PROJECT}/files/read?path=sub%2Fchild.txt`)},
  {id: 'files-read-binary-ext', request: get(`${PROJECT}/files/read?path=binary.png`)},
  {id: 'files-read-no-ext', request: get(`${PROJECT}/files/read?path=README`)},
  {id: 'files-read-upper-ext', request: get(`${PROJECT}/files/read?path=UPPER.TXT`)},
  {id: 'files-read-env', request: get(`${PROJECT}/files/read?path=config.env`)},
  {id: 'files-read-empty-file', request: get(`${PROJECT}/files/read?path=empty.txt`)},
  {id: 'files-read-large', request: get(`${PROJECT}/files/read?path=big.bin`)},
  {id: 'files-read-symlink', request: get(`${PROJECT}/files/read?path=inside-link`)},
  {id: 'files-read-missing-query', request: get(`${PROJECT}/files/read`)},
  {id: 'files-read-empty-path', request: get(`${PROJECT}/files/read?path=`)},
  {id: 'files-read-escape', request: get(`${PROJECT}/files/read?path=../../etc/passwd`)},
  {id: 'files-read-absolute', request: get(`${PROJECT}/files/read?path=/etc/passwd`)},
  {id: 'files-read-nonexistent', request: get(`${PROJECT}/files/read?path=missing.txt`)},
  {id: 'files-read-dir', request: get(`${PROJECT}/files/read?path=sub`)},
  {id: 'files-read-bad-percent-path', request: get(`${PROJECT}/files/read?path=%E0%A4%A`)},
  // write
  {id: 'files-write-text', request: write({path: 'new.txt', content: 'hello'})},
  {id: 'files-write-base64', request: write({path: 'b64.bin', content: 'AAEC/w==', encoding: 'base64'})},
  {id: 'files-write-base64-lenient', request: write({path: 'lenient.bin', content: '!!!!', encoding: 'base64'})},
  {id: 'files-write-nested', request: write({path: 'deep/dir/n.txt', content: 'n'})},
  {id: 'files-write-extra-field', request: write({path: 'extra.txt', content: 'x', extra: 1})},
  {id: 'files-write-bad-json', request: write('not json')},
  {id: 'files-write-empty-body', request: write('')},
  {id: 'files-write-missing-path', request: write({content: 'x'})},
  {id: 'files-write-empty-path', request: write({path: '', content: 'x'})},
  {id: 'files-write-nonstring-path', request: write({path: 5, content: 'x'})},
  {id: 'files-write-nonstring-content', request: write({path: 'x.txt', content: 5})},
  {id: 'files-write-bad-encoding', request: write({path: 'x.txt', content: 'x', encoding: 'utf16'})},
  {id: 'files-write-null-encoding', request: write({path: 'n.txt', content: 'x', encoding: null})},
  {id: 'files-write-escape', request: write({path: '../escape.txt', content: 'x'})},
  {id: 'files-write-absolute', request: write({path: '/tmp/escape.txt', content: 'x'})},
  {id: 'files-write-dotdot-prefix', request: write({path: '..foo', content: 'x'})},
  // A write cell and the read-back cell run as two separate requests (in
  // this order): a pipelined write+read on one connection races in Node,
  // because the second request's handler starts before the first one's
  // `await writeFile` resolves.
  {id: 'files-write-round', request: write({path: 'round.txt', content: 'round trip\n'})},
  {id: 'files-read-round', request: get(`${PROJECT}/files/read?path=round.txt`)},
  {id: 'keep-alive-reuse', request: get('/api/web/projects') + get(`${PROJECT}/files/read?path=aaa.txt`)},
  {id: 'conn-close', request: `GET /api/web/projects HTTP/1.1\r\n${H}${AUTH}Connection: close\r\n\r\n`},
  {id: 'http10', request: `GET /api/web/projects HTTP/1.0\r\n${H}${AUTH}\r\n`},
];

const observations: any[] = [];
try {
  for (const cell of cells) {
    const {response, closed} = await raw(cell.request);
    if (!response && !closed) {
      console.error(`#1348 cell ${cell.id} produced no bytes and stayed open`);
      process.exit(1);
    }
    observations.push({id: cell.id, request: cell.request, response, closed});
  }
  const output = {
    sourceCommit,
    sourceSha256,
    node: process.version,
    icu: process.versions.icu,
    fixture: {
      root: '<root>',
      directories: DIRECTORIES.map(([path, when]) => ({path, modifiedAt: when.toISOString()})),
      files: FILES.map(([path, body, when]) => ({
        path,
        bytes: Buffer.isBuffer(body) ? body.length : Buffer.byteLength(body as string),
        modifiedAt: when.toISOString(),
      })),
      symlinks: LINKS.map(([path, target]) => ({path, target})),
    },
    cells: observations,
  };
  await mkdir(process.argv[2], {recursive: true});
  await writeFile(join(process.argv[2], 'oracle.json'), JSON.stringify(output, null, 2) + '\n', {flag: 'wx'});
  console.log(JSON.stringify({cells: observations.length, bytes: JSON.stringify(output).length}));
} finally {
  await server.close().catch(() => {});
  clearTimeout(deadline);
}
