// Supplemental real-TS fetch corpus. Default: two owned loopback origins.
// --capture-transport: real Node fetch, scripted dispatcher, NOT HTTP evidence.
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = Object.keys(JSON.parse(await readFile(new URL('./oracle.json', import.meta.url), 'utf8')).sourceSha256);
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { TokenAccountingRuntime } = await import(pathToFileURL(join(baseline, 'src/context/budget/TokenAccountingRuntime.ts')).href);
const capture = process.argv.includes('--capture-transport');
const parent = process.argv.slice(2).find(arg => !arg.startsWith('--'));
const output = await mkdtemp(join(parent ? resolve(parent) : tmpdir(), 'token-count-http-'));
const headers = { authorization: 'Bearer fixture', 'proxy-authorization': 'Basic fixture', cookie: 'fixture=1', host: 'ignored.invalid',
  'content-encoding': 'identity', 'content-language': 'zh', 'content-location': '/body', 'x-fixture': 'kept' };
const request = { provider: 'p', model: 'm', messages: [{ role: 'user', content: [{ type: 'text', text: 'Hello 世界' }] }] };
const options = { maxContextTokens: 100 };
const config = { providers: { p: { id: 'p', protocol: 'anthropic', url: '$MOCK', apiKey: 'fixture-key', headers,
  models: { m: { id: 'claude-sonnet-4-6', capabilities: { maxOutputTokens: 4096, contextWindow: 200000 } } } } } };
const selectedHeaders = ['authorization', 'proxy-authorization', 'cookie', 'content-type', 'content-encoding',
  'content-language', 'content-location', 'x-api-key', 'anthropic-version', 'x-fixture'];
type Reply = { status?: number; location?: string; locations?: string[]; text?: string; bytes?: number[] };
const specs: { name: string; responses: Reply[] }[] = [];
const add = (name: string, responses: Reply[]) => specs.push({ name, responses });
for (const status of [301, 302, 303, 307, 308]) {
  for (const [name, location] of Object.entries({ relative: '../final?x=1#fragment', root: '/final',
    query: '?next=yes', absolute: '$MOCK/final', cross: '$OTHER/final', empty: '' })) {
    add(`redirect-${status}-${name}`, [{ status, location }, {}]);
  }
  add(`redirect-${status}-loop`, [{ status, location: '/loop' }]);
  add(`redirect-${status}-missing`, [{ status }]);
}
add('redirect-cross-return', [{ status: 307, location: '$OTHER/next' }, { status: 308, location: '$MOCK/final' }, {}]);
add('redirect-method-chain', [{ status: 307, location: '/keep' }, { status: 302, location: '../drop' }, { status: 308, location: '/end' }, {}]);
for (const n of [20, 21]) add(`redirect-limit-${n}`, [...Array.from({ length: n }, (_, i) => ({ status: 302, location: `/hop/${i}` })), {}]);
for (const [name, location] of Object.entries({ invalid: 'http://[invalid', scheme: 'data:text/plain,x',
  credentials: 'http://u:p@$MOCK_AUTHORITY/final', crossCredentials: 'http://u:p@$OTHER_AUTHORITY/final', badPort: 'http://127.0.0.1:25/next' })) {
  add(`redirect-${name}`, [{ status: 302, location }]);
}
add('redirect-final-http-error', [{ status: 301, location: '/final' }, { status: 429, text: '<html>error</html>' }]);
add('redirect-multiple-locations', [{ status: 302, locations: ['/one', '/two'] }, {}]);
const malformed = [
  '', ' ', '\n\r\t', '<', '<html>error</html>', 'not-json', 'undefined', 'NaN', 'Infinity', '[object Object]',
  'true false', 'nullx', '{}xyz', '[] 0', '{', '[', '"', '"a', '{"a":', '{"a":1', '[1',
  't', 'tr', 'tru', 'f', 'fa', 'nul', 'tr0', 'tr-', 'tr"', '{a:1}', '{a', '{,', '{]',
  '{"a"}', '{"a" 1}', '{"a",1}', '{"a":1,}', '{"a":1,a:2}', '{"a":1 "b":2}',
  '[1,]', '[,]', '[1 2]', '[1}', '[1,', '{"a":[1,]}', '-', '-x', '-01', '01', '1.', '1.e',
  '1e', '1e+', '1e-', '1eX', '.1', '+1', '00', '1.1.1', '"a\\q"', '"a\\uZZZZ"',
  '"a\\u0"', '"a\\u000"', '"a\\', '"a\\n', '"a\\u0001', '"a\0"', '"a\\uD800"x',
  '\r\n{"a":1,}', '\n{"中":1,}', '\n{"😀":1,}', '{"x":0}\r\nX', '{"x":0}\rX', '\tX',
  'x'.repeat(100), ' '.repeat(20) + 'x', '["' + 'a'.repeat(12) + '",x]',
  '😀', '["' + 'a'.repeat(8) + '😀",x' + ' '.repeat(20), '"a\\ÿ"', '"a\\Ā"', '"a\\😀"',
];
malformed.forEach((text, i) => add(`json-invalid-${i}`, [{ text }]));
for (const n of [1, 2, 3]) add(`json-bom-${n}`, [{ text: '\ufeff'.repeat(n) + '{"input_tokens":77}' }]);
add('json-invalid-utf8', [{ bytes: [0xff, 0xfe, 60] }]);
add('json-valid-utf8-replacement', [{ bytes: [...Buffer.from('{"ignored":"'), 0xff, ...Buffer.from('","input_tokens":79}')] }]);
add('json-valid-surrogate', [{ text: '{"ignored":"\\ud800","input_tokens":78}' }]);
add('json-mime-independent', [{ text: '{"input_tokens":75}' }]);
add('json-empty-204', [{ status: 204 }]);
add('json-http-error-precedes-parse', [{ status: 503, text: '<html>error</html>' }]);
let origins = ['http://127.0.0.1:41001', 'http://127.0.0.1:41002'];
const materialize = (text: string) => text.replaceAll('$MOCK_AUTHORITY', new URL(origins[0]).host)
  .replaceAll('$OTHER_AUTHORITY', new URL(origins[1]).host).replaceAll('$MOCK', origins[0]).replaceAll('$OTHER', origins[1]);
const normalize = (text: string) => text.replaceAll(origins[0], '$MOCK').replaceAll(origins[1], '$OTHER');
let active: Reply[] = [];
let requests: any[] = [];
function record(origin: string, path: string, method: string, rawHeaders: Record<string, any>, body: string) {
  assert.ok(origins.includes(origin), 'no external fixture traffic');
  if (rawHeaders.host !== undefined) assert.equal(rawHeaders.host, new URL(origin).host, 'user Host must be discarded');
  const index = requests.length;
  requests.push({ origin: normalize(origin), path, method,
    headers: Object.fromEntries(Object.entries(rawHeaders).filter(([key]) => selectedHeaders.includes(key)).sort(([a], [b]) => a.localeCompare(b))),
    body: body ? JSON.parse(body) : null });
  const reply = active[Math.min(index, active.length - 1)];
  const status = reply.status ?? 200;
  return { status, headers: { 'content-type': 'text/plain', ...(reply.locations ? { location: reply.locations.map(materialize) } : reply.location === undefined ? {} : { location: materialize(reply.location) }) },
    body: [204, 205, 304].includes(status) ? Buffer.alloc(0) : reply.bytes ? Buffer.from(reply.bytes) : Buffer.from(reply.text ?? '{"input_tokens":73}') };
}
const servers = [0, 1].map(index => createServer(async (req, res) => {
  try {
    let body = ''; for await (const chunk of req) body += chunk;
    const reply = record(origins[index], req.url!, req.method!, req.headers, body);
    res.writeHead(reply.status, reply.headers); res.end(reply.body);
  } catch (error) { res.destroy(error as Error); }
}));
// Inject below fetch, so redirects, method changes, headers, decoding and JSON
// errors are still produced by the installed Node/undici, never reimplemented.
const dispatcher = { dispatch(opts: any, handler: any) {
  let aborted = false;
  handler.onConnect(() => { aborted = true; });
  Promise.resolve().then(async () => {
    let body = '';
    if (typeof opts.body === 'string' || Buffer.isBuffer(opts.body)) body = String(opts.body);
    else if (opts.body) for await (const chunk of opts.body) body += Buffer.from(chunk).toString('utf8');
    if (aborted) return;
    const rawHeaders = Array.isArray(opts.headers) ? Object.fromEntries(Array.from({ length: opts.headers.length / 2 }, (_, i) => [opts.headers[i * 2], opts.headers[i * 2 + 1]])) : opts.headers;
    const reply = record(String(opts.origin), opts.path, opts.method, rawHeaders, body);
    handler.onHeaders(reply.status, Object.entries(reply.headers).flatMap(([k, v]) => (Array.isArray(v) ? v : [v]).flatMap(value => [Buffer.from(k), Buffer.from(value)])), () => {}, '');
    if (!aborted) { handler.onData(reply.body); handler.onComplete([]); }
  }).catch(error => handler.onError(error));
  return true;
} };
try {
  if (!capture) for (const [i, server] of servers.entries()) {
    await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
    origins[i] = `http://127.0.0.1:${(server.address() as any).port}`;
  }
  const cases = [];
  for (const spec of specs) {
    active = spec.responses; requests = [];
    const runtime = new TokenAccountingRuntime({ modelConfig: JSON.parse(materialize(JSON.stringify(config))), cacheSize: 0,
      fetch: (url: any, init: any) => {
        assert.ok(origins.includes(new URL(String(url)).origin), 'no external providers');
        return fetch(url, capture ? { ...init, dispatcher } : init);
      } });
    const snapshot = await runtime.evaluateRequestBudget(request, options);
    assert.ok(requests.length > 0, `${spec.name}: fixture never reached the mock`);
    assert.equal(requests.length, spec.name.endsWith('-loop') || spec.name === 'redirect-limit-21' ? 21 : spec.responses.length, `${spec.name}: complete hop trace`);
    if (spec.name.startsWith('redirect-') && spec.responses.length > 1 && spec.name !== 'redirect-limit-21' && spec.name !== 'redirect-final-http-error') {
      assert.equal(snapshot.source, 'provider', `${spec.name}: redirect must reach valid JSON`);
      assert.equal(snapshot.tokens, 73);
    }
    cases.push({ ...spec, requests, snapshot });
  }
  assert.equal(cases.length, 143);
  assert.equal(cases.reduce((n, c) => n + c.requests.length, 0), 320);
  assertBaseline(baseline);
  assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
  await writeFile(join(output, 'http-oracle.json'), JSON.stringify({ sourceCommit, sourceSha256,
    node: process.version, v8: process.versions.v8, config, request, options, cases }, null, 2) + '\n');
  console.log(JSON.stringify({ output, transport: capture ? 'scripted dispatcher (HTTP not verified)' : 'loopback HTTP',
    cases: cases.length, requests: cases.reduce((n, c) => n + c.requests.length, 0) }));
} finally {
  for (const server of servers) await new Promise<void>(resolve => { server.closeAllConnections(); server.close(() => resolve()); });
}
