// #1349 golden fixture: raw-socket behavior of the TS GatewayServer's
// module-host transport (/api/module-host/describe and /api/module-host/call)
// over the real startGatewayServer + real publicHostHttpTransport with a
// scripted stub provider (the scenario is the request principal's agentId),
// recorded byte-exactly per cell with the Date value masked.
//
// The 32 MiB budget cells do not inline their padding: the recorded request
// carries the __PDRS_PAD__ marker and a pad.size the replayer substitutes
// 'a'.repeat(size) for, so the oracle stays small.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {mkdir, writeFile} from 'node:fs/promises';
import {realpathSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {connect} from 'node:net';
import {once} from 'node:events';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#1349 pinned Node');
assert.equal(process.versions.icu, '78.2', '#1349 pinned ICU');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, ['src/gateway/server/GatewayServer.ts', 'src/composition/publicHostHttpTransport.ts', 'src/composition/publicHostWire.ts']);
const deadline = setTimeout(() => { console.error('#1349 probe deadline'); process.exit(1); }, 240000);
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
const publicHostWire=await load('src/composition/publicHostWire.ts');
const hostTransport=await load('src/composition/publicHostHttpTransport.ts',{'./publicHostWire.js':publicHostWire});
const webRouter=await load('src/adapters/web/httpRouter.ts');
const {startGatewayServer}=await load('src/gateway/server/GatewayServer.ts',{
  './websocket.js':websocket,'./GatewayWsConnection.js':connection,'./staticAssets.js':staticAssets,
  '../../composition/publicApprovalHttpTransport.js':approval,
  '../../composition/publicHostHttpTransport.js':hostTransport,
  '../../adapters/web/httpRouter.js':webRouter,
});

const token = 'fixture-token';
async function* events(...items: unknown[]) { for (const item of items) yield item; }
type Scenario = () => any;
const scenarios: Record<string, Scenario> = {
  ok: () => ({status: 200, body: {scenario: 'ok'}}),
  plain: () => ({status: 200, headers: {'content-type': 'text/plain'}, body: 'hello'}),
  jsonstring: () => ({status: 200, headers: {'content-type': 'application/json'}, body: 'hello'}),
  emptystring_json: () => ({status: 200, headers: {'content-type': 'application/json'}, body: ''}),
  bytes: () => ({status: 200, headers: {'content-type': 'application/octet-stream'}, body: new Uint8Array([0, 1, 2, 250])}),
  empty_ct_string: () => ({status: 200, headers: {'content-type': '', 'etag': 'W/"x"'}, body: 'x'}),
  empty_ct_value: () => ({status: 200, headers: {'content-type': ''}, body: {ok: 1}}),
  empty_ct_bytes: () => ({status: 200, headers: {'content-type': ''}, body: new Uint8Array([0, 1, 2, 250])}),
  rawbody: () => ({status: 201, headers: {'content-type': 'text/xml'}, rawBody: '<raw/>', body: {ignored: true}}),
  rawbody_noct: () => ({status: 202, rawBody: 'raw-noct', body: {ignored: true}}),
  nullbody: () => ({status: 200, body: null}),
  undefbody: () => ({status: 200, body: undefined}),
  numberbody: () => ({status: 200, body: 5}),
  boolbody: () => ({status: 200, body: false}),
  wireheaders: () => ({
    status: 200,
    headers: {
      'content-type': 'application/json',
      'etag': 'W/"a"',
      'retry-after': '3',
      'x-request-id': 'req-1',
      'x-pilotdeck-model-id': 'm-1',
      'x-pilotdeck-provider-id': 'p-1',
      'x-drop-me': 'dropped',
      'cache-control': 'dropped',
    },
    body: {ok: 1},
  }),
  bad_etag: () => ({status: 200, headers: {'etag': 'bad\nvalue'}, body: {}}),
  bad_name: () => ({status: 200, headers: {'bad name': 'x'}, body: {}}),
  fail_default: () => { throw new Error('provider exploded'); },
  fail_custom: () => { throw Object.assign(new Error('nope'), {status: 400, code: 'X_CUSTOM'}); },
  fail_nomessage: () => { throw {status: 418}; },
  cancel_499: () => ({status: 499, body: {code: 'PUBLIC_HOST_CANCELLED'}}),
  bad_status: () => ({status: 600, body: {}}),
  ndjson_objects: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: events({a: 1}, {b: 'two'})}),
  ndjson_bytes: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: events(new Uint8Array([120, 121]), 'str-text')}),
  sse_strings: () => ({status: 200, headers: {'content-type': 'text/event-stream'}, body: events('data: a\n\n', 'data: b\n\n')}),
  ndjson_array_event: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: events({a: 1}, [1, 2])}),
  sse_object_event: () => ({status: 200, headers: {'content-type': 'text/event-stream'}, body: events({a: 1})}),
  stream_plain_ct: () => ({status: 200, headers: {'content-type': 'application/json'}, body: events({a: 1})}),
  stream_plain_ct_headers: () => ({status: 200, headers: {'content-type': 'application/json', 'etag': 'W/"x"', 'x-request-id': 'r1'}, body: events({a: 1})}),
  stream_missing: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: null}),
  stream_missing_headers: () => ({status: 200, headers: {'content-type': 'application/x-ndjson', 'etag': 'W/"x"', 'x-request-id': 'r1', 'retry-after': '3'}, body: null}),
  stream_noct_headers: () => ({status: 200, headers: {'etag': 'W/"x"', 'x-request-id': 'r1'}, body: events({a: 1})}),
  ndjson_empty_string: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: ''}),
  ndjson_string_body: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: 'abc'}),
  ndjson_string_emoji: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: 'a😀b'}),
  ndjson_string_surrogate: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: '\uD800a😀'}),
  ndjson_array_body: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: ['a', {b: 2}]}),
  ndjson_object_body: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: {x: 1}}),
  ndjson_bytes_body: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: new Uint8Array([65, 66])}),
  ndjson_empty_bytes_body: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, body: new Uint8Array(0)}),
  ndjson_rawbody: () => ({status: 200, headers: {'content-type': 'application/x-ndjson'}, rawBody: 'ignored', body: undefined}),
};
// The describe filter cells: 'default' mixes wire and non-wire operations in
// a fixed order, 'empty' advertises nothing, everything else advertises the
// one operation the call cells use.
const operationsFor = (agentId: string) => agentId === 'empty' ? []
  : agentId === 'default' ? ['list_tools', 'custom_op', 'list_general_skills', 'model_prepare', 'not_wire']
  : ['list_tools'];
const resolver = (principal: {agentId: string}) => ({
  operations: operationsFor(principal.agentId),
  call: async (_operation: string, _input: unknown, options: {principal: {agentId: string}}) =>
    (scenarios[options.principal.agentId] ?? scenarios.ok)(),
});
const gateway = { async describeServer(){ return {name:'probe',capabilities:[]}; } };
const serverMain = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe', publicHostCapabilities: resolver as any});
const serverBare = await startGatewayServer({gateway: gateway as any, host: '127.0.0.1', port: 0, token, serverVersion: 'probe'});
const servers: Record<string, {url: string}> = {main: serverMain, bare: serverBare};

const maskDate = (bytes: string) => bytes.replace(/^date:.*$/gim, 'date: <masked>');
async function raw(server: string, request: string, quietMs = 250, halfClose = false, pad?: {marker: string, size: number}): Promise<{response: string, closed: boolean}> {
  const socket = connect(Number(new URL(servers[server].url).port), new URL(servers[server].url).hostname);
  await once(socket, 'connect');
  let bytes = '';
  let last = Date.now();
  let closed = false;
  socket.on('data', (b: Buffer) => { bytes += b.toString('latin1'); last = Date.now(); });
  socket.on('close', () => { closed = true; last = 0; });
  socket.on('error', () => {});
  // The budget cells write the real padding (the whole 32 MiB body must
  // arrive) while the oracle records only the marker version of the request.
  const written = pad ? request.replace(pad.marker, 'a'.repeat(pad.size)) : request;
  await new Promise<void>(resolve => socket.write(Buffer.from(written, 'utf8'), () => resolve()));
  if (halfClose) socket.end();
  const start = Date.now();
  while (Date.now() - start < 10000) {
    await new Promise(r => setTimeout(r, 25));
    if (closed || socket.destroyed) break;
    if (bytes && Date.now() - last > quietMs) break;
  }
  if (!closed && !socket.destroyed) socket.destroy();
  return {response: maskDate(bytes), closed};
}

const principal = (agentId: string) => ({pilotDeckUserId: 'u1', tenantId: 't1', actorUserId: 'a1', agentId});
const describeBody = (agentId: string) => JSON.stringify({principal: principal(agentId)});
const callBody = (agentId: string, extra: Record<string, unknown> = {}) =>
  JSON.stringify({principal: principal(agentId), operation: 'list_tools', input: {}, ...extra});
const A = `Authorization: Bearer ${token}\r\n`;
// The auth-variation cells must carry their own Authorization header (or
// none), not this one stacked next to it — Node would join the duplicates.
const post = (target: string, body: string, headers = '', authorized = true) =>
  `POST ${target} HTTP/1.1\r\nHost: 127.0.0.1\r\n${headers}${authorized ? A : ''}Content-Length: ${Buffer.byteLength(body)}\r\n\r\n${body}`;
const D = (agentId: string, headers = '') => post('/api/module-host/describe', describeBody(agentId), headers);
const C = (agentId: string, headers = '', extra?: Record<string, unknown>) =>
  post('/api/module-host/call', extra === undefined ? callBody(agentId) : callBody(agentId, extra), headers);

const MARKER = '__PDRS_PAD__';
// The budget body is a full valid call so the exact-budget cell proves the
// body at exactly 32 MiB is served, not just parsed.
const budgetBase = `{"principal":${JSON.stringify(principal('ok'))},"operation":"list_tools","input":{},"pad":"${MARKER}"}`;
const EXACT = 32 * 1024 * 1024;
const exactPad = EXACT - (budgetBase.length - MARKER.length);
const budgetRequest = (total: number) =>
  `POST /api/module-host/call HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}Content-Length: ${total}\r\n\r\n${budgetBase}`;

// Each cell: server, the exact request bytes (utf8; the pad cells carry the
// marker plus a size), and flags the replayer must reproduce.
const cells: Array<{id: string, server: string, request: string, pad?: {marker: string, size: number}, halfClose?: boolean}> = [
  // authorization: byte-exact value, checked before the method.
  {id: 'auth-missing', server: 'main', request: post('/api/module-host/describe', describeBody('default'), '', false)},
  {id: 'auth-wrong', server: 'main', request: post('/api/module-host/describe', describeBody('default'), 'Authorization: Bearer wrong\r\n', false)},
  {id: 'auth-lower', server: 'main', request: post('/api/module-host/describe', describeBody('default'), 'Authorization: bearer fixture-token\r\n', false)},
  {id: 'auth-dblspace', server: 'main', request: post('/api/module-host/describe', describeBody('default'), 'Authorization: Bearer  fixture-token\r\n', false)},
  {id: 'auth-get-wrong', server: 'main', request: `GET /api/module-host/describe HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer wrong\r\n\r\n`},
  // method: anything but POST, with the body unread like TS.
  {id: 'method-get', server: 'main', request: `GET /api/module-host/describe HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}\r\n`},
  {id: 'method-put', server: 'main', request: `PUT /api/module-host/call HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}\r\n`},
  {id: 'method-head', server: 'main', request: `HEAD /api/module-host/describe HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}\r\n`},
  {id: 'method-get-body', server: 'main', request: `GET /api/module-host/describe HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}Content-Length: 5\r\n\r\nhello`},
  // routing: WHATWG path parse decides the two endpoints.
  {id: 'route-trailing', server: 'main', request: post('/api/module-host/describe/', describeBody('default'))},
  {id: 'route-calls', server: 'main', request: post('/api/module-host/calls', callBody('ok'))},
  {id: 'route-dot', server: 'main', request: post('/api/module-host/x/../describe', describeBody('default'))},
  {id: 'route-pct', server: 'main', request: post('/api/module-host/%64escribe', describeBody('default'))},
  {id: 'route-upper', server: 'main', request: post('/API/MODULE-HOST/DESCRIBE', describeBody('default'))},
  {id: 'route-dblslash', server: 'main', request: post('//api/module-host/describe', describeBody('default'))},
  {id: 'describe-query', server: 'main', request: post('/api/module-host/describe?x=1', describeBody('default'))},
  // no resolver configured: both endpoints are the 501.
  {id: 'unconfigured-describe', server: 'bare', request: D('default')},
  {id: 'unconfigured-call', server: 'bare', request: C('ok')},
  // JSON.parse gate and the principal's shape.
  {id: 'body-bad-json', server: 'main', request: post('/api/module-host/call', '{oops')},
  {id: 'body-empty', server: 'main', request: post('/api/module-host/call', '')},
  {id: 'body-number', server: 'main', request: post('/api/module-host/call', '5')},
  {id: 'body-array', server: 'main', request: post('/api/module-host/call', '[]')},
  {id: 'body-null', server: 'main', request: post('/api/module-host/call', 'null')},
  {id: 'body-string', server: 'main', request: post('/api/module-host/call', '"x"')},
  {id: 'principal-missing', server: 'main', request: post('/api/module-host/call', JSON.stringify({operation: 'list_tools', input: {}}))},
  {id: 'principal-empty', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: {}, operation: 'list_tools', input: {}}))},
  {id: 'principal-empty-string', server: 'main', request: C('')},
  {id: 'principal-whitespace', server: 'main', request: C(' ')},
  // U+2028/U+2029/U+FEFF: ECMAScript whitespace outside the ASCII set, so
  // the trim gate is pinned beyond the NBSP cell above.
  {id: 'principal-unicode-ws', server: 'main', request: C('\u2028\u2029\uFEFF')},
  {id: 'principal-nbsp', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: {pilotDeckUserId: 'u1', tenantId: 't1', actorUserId: 'a1', agentId: ' '}, operation: 'list_tools', input: {}}))},
  {id: 'principal-number', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: {pilotDeckUserId: 'u1', tenantId: 5, actorUserId: 'a1', agentId: 'ok'}, operation: 'list_tools', input: {}}))},
  {id: 'principal-three', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: {pilotDeckUserId: 'u1', tenantId: 't1', actorUserId: 'a1'}, operation: 'list_tools', input: {}}))},
  // describe: the wire filter over the provider's own order.
  {id: 'describe-default', server: 'main', request: D('default')},
  {id: 'describe-empty', server: 'main', request: D('empty')},
  {id: 'describe-ok', server: 'main', request: D('ok')},
  // the operation gate runs before the input checks.
  {id: 'call-unknown', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('default'), operation: 'custom_op', input: {}}))},
  {id: 'call-missing-op', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('ok'), input: {}}))},
  {id: 'call-number-op', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('ok'), operation: 5, input: {}}))},
  {id: 'call-not-advertised', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('ok'), operation: 'nope', input: {}}))},
  {id: 'call-on-empty', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('empty'), operation: 'list_tools', input: {}}))},
  // input shape and the scope-override keys.
  {id: 'input-missing', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('ok'), operation: 'list_tools'}))},
  {id: 'input-null', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('ok'), operation: 'list_tools', input: null}))},
  {id: 'input-string', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('ok'), operation: 'list_tools', input: 'x'}))},
  {id: 'input-number', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('ok'), operation: 'list_tools', input: 5}))},
  {id: 'input-array', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('ok'), operation: 'list_tools', input: []}))},
  {id: 'input-false', server: 'main', request: post('/api/module-host/call', JSON.stringify({principal: principal('ok'), operation: 'list_tools', input: false}))},
  {id: 'scope-tenantid', server: 'main', request: C('ok', '', {input: {tenantId: 'x'}})},
  {id: 'scope-snake', server: 'main', request: C('ok', '', {input: {agent_id: 'x'}})},
  {id: 'scope-nested', server: 'main', request: C('ok', '', {input: {deep: {tenantId: 'x'}}})},
  // success bodies: the response serialization branches.
  {id: 'ok', server: 'main', request: C('ok')},
  {id: 'plain', server: 'main', request: C('plain')},
  {id: 'jsonstring', server: 'main', request: C('jsonstring')},
  {id: 'emptystring-json', server: 'main', request: C('emptystring_json')},
  {id: 'bytes', server: 'main', request: C('bytes')},
  {id: 'empty-ct-string', server: 'main', request: C('empty_ct_string')},
  {id: 'empty-ct-value', server: 'main', request: C('empty_ct_value')},
  {id: 'empty-ct-bytes', server: 'main', request: C('empty_ct_bytes')},
  {id: 'rawbody', server: 'main', request: C('rawbody')},
  {id: 'rawbody-noct', server: 'main', request: C('rawbody_noct')},
  {id: 'nullbody', server: 'main', request: C('nullbody')},
  {id: 'undefbody', server: 'main', request: C('undefbody')},
  {id: 'numberbody', server: 'main', request: C('numberbody')},
  {id: 'boolbody', server: 'main', request: C('boolbody')},
  {id: 'wireheaders', server: 'main', request: C('wireheaders')},
  {id: 'bad-etag', server: 'main', request: C('bad_etag')},
  {id: 'bad-header-name', server: 'main', request: C('bad_name')},
  // provider failures and typed responses.
  {id: 'fail-default', server: 'main', request: C('fail_default')},
  {id: 'fail-custom', server: 'main', request: C('fail_custom')},
  {id: 'fail-nomessage', server: 'main', request: C('fail_nomessage')},
  {id: 'cancel-499', server: 'main', request: C('cancel_499')},
  {id: 'bad-status', server: 'main', request: C('bad_status')},
  // streaming bodies.
  {id: 'ndjson-objects', server: 'main', request: C('ndjson_objects')},
  {id: 'ndjson-bytes', server: 'main', request: C('ndjson_bytes')},
  {id: 'sse-strings', server: 'main', request: C('sse_strings')},
  {id: 'ndjson-array-event', server: 'main', request: C('ndjson_array_event')},
  {id: 'sse-object-event', server: 'main', request: C('sse_object_event')},
  {id: 'stream-plain-ct', server: 'main', request: C('stream_plain_ct')},
  {id: 'stream-plain-ct-headers', server: 'main', request: C('stream_plain_ct_headers')},
  {id: 'stream-missing', server: 'main', request: C('stream_missing')},
  {id: 'stream-missing-headers', server: 'main', request: C('stream_missing_headers')},
  {id: 'stream-noct-headers', server: 'main', request: C('stream_noct_headers')},
  {id: 'ndjson-empty-string', server: 'main', request: C('ndjson_empty_string')},
  {id: 'ndjson-string-body', server: 'main', request: C('ndjson_string_body')},
  {id: 'ndjson-string-emoji', server: 'main', request: C('ndjson_string_emoji')},
  {id: 'ndjson-string-surrogate', server: 'main', request: C('ndjson_string_surrogate')},
  {id: 'ndjson-array-body', server: 'main', request: C('ndjson_array_body')},
  {id: 'ndjson-object-body', server: 'main', request: C('ndjson_object_body')},
  {id: 'ndjson-bytes-body', server: 'main', request: C('ndjson_bytes_body')},
  {id: 'ndjson-empty-bytes-body', server: 'main', request: C('ndjson_empty_bytes_body')},
  {id: 'ndjson-rawbody', server: 'main', request: C('ndjson_rawbody')},
  // the 32 MiB chunk-summed budget: exactly the budget is served, one byte
  // more is the 413 (the request carries the pad marker, not the padding).
  {id: 'budget-exact', server: 'main', request: budgetRequest(EXACT), pad: {marker: MARKER, size: exactPad}},
  {id: 'budget-over', server: 'main', request: budgetRequest(EXACT + 1), pad: {marker: MARKER, size: exactPad + 1}},
  // connection framing.
  {id: 'conn-close', server: 'main', request: D('default', 'Connection: close\r\n')},
  // Node answers an HTTP/1.0 request with an HTTP/1.1 status line and its own
  // close decision; hyper's version flip is the registered #1209 diff.
  {id: 'http10', server: 'main', request: `POST /api/module-host/describe HTTP/1.0\r\nHost: 127.0.0.1\r\n${A}Content-Length: ${Buffer.byteLength(describeBody('default'))}\r\n\r\n${describeBody('default')}`},
  {id: 'keep-alive-reuse', server: 'main', request: D('default') + D('ok')},
  {id: 'premature-close', server: 'main', request: `POST /api/module-host/call HTTP/1.1\r\nHost: 127.0.0.1\r\n${A}Content-Length: 100\r\n\r\n`, halfClose: true},
];

const observations: any[] = [];
try {
  for (const cell of cells) {
    const {response, closed} = await raw(cell.server, cell.request, cell.id === 'budget-over' ? 400 : 250, cell.halfClose === true, cell.pad);
    if (!response && !closed) { console.error(`#1349 cell ${cell.id} produced no bytes and stayed open`); process.exit(1); }
    observations.push({id: cell.id, server: cell.server, request: cell.request, pad: cell.pad ?? null, halfClose: cell.halfClose === true, response, closed});
  }
  const output = {sourceCommit, sourceSha256, node: process.version, icu: process.versions.icu, cells: observations};
  await mkdir(process.argv[2], {recursive: true});
  await writeFile(join(process.argv[2], 'oracle.json'), JSON.stringify(output, null, 2) + '\n', {flag: 'wx'});
  console.log(JSON.stringify(output, null, 2));
} finally {
  await serverMain.close().catch(() => {});
  await serverBare.close().catch(() => {});
  clearTimeout(deadline);
}
