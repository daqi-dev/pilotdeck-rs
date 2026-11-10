// Real baseline webhook channel over loopback HTTP: the channel's own
// listener, HMAC check, rate limit, dedup and rendering are the transpiled
// TS sources, unmodified; only the gateway is a mock that records submitTurn
// calls and replays scripted events. Requests are raw sockets so the wire
// bytes (status line, header order, body framing) are the recorded oracle.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { join, resolve, dirname } from 'node:path';
import net from 'node:net';
import { createHmac } from 'node:crypto';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1334 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1334 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, '#1334 PDRS_BASELINE');
assert.ok(process.argv[2], '#1334 output directory');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/adapters/channel/webhook/WebhookChannel.ts',
  'src/adapters/channel/webhook/WebhookSessionMapper.ts',
  'src/adapters/channel/webhook/webhook-render.ts',
  'src/adapters/channel/protocol/ImCronDelivery.ts',
  'src/status/agentStatus.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const require = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(require.resolve('tsx'))('esbuild');
const compiled: Record<string, string> = {};
for (const rel of sources) {
  compiled[rel] = transformSync(await readFile(join(baseline, rel)), {
    loader: 'ts',
    format: 'cjs',
    target: 'es2022',
  }).code;
}

// The channel's node:http import is wrapped so the Server object the TS code
// keeps private is still observable for the bound port.
const seenServers: any[] = [];
const realHttp = require('node:http');
const httpShim = {
  createServer(...args: any[]) {
    const server = realHttp.createServer(...args);
    seenServers.push(server);
    return server;
  },
};
const moduleCache = new Map<string, any>();
function loadModule(rel: string): any {
  if (moduleCache.has(rel)) return moduleCache.get(rel)!;
  const module = { exports: {} as any };
  const base = dirname(rel);
  const localRequire = (name: string) => {
    if (name === 'node:http') return httpShim;
    if (name.startsWith('node:')) return require(name);
    // The transpiled sources require each other with .js specifiers.
    return loadModule(join(base, name).replace(/\.js$/, '.ts'));
  };
  new Function('require', 'module', 'exports', compiled[rel])(
    localRequire,
    module,
    module.exports,
  );
  // The module body reassigns `module.exports` (esbuild's __toCommonJS).
  moduleCache.set(rel, module.exports);
  return module.exports;
}
const { WebhookChannel } = loadModule(sources[0]);
const { WebhookSessionMapper } = loadModule(sources[1]);
const FIXED_UUID = '00000000-0000-4000-8000-000000000000';

// The gateway mock: every submitTurn records its input and drains one
// scripted event list; the string "<throw>" rejects the iterator.
function makeGateway(state: any) {
  return {
    submitTurn(input: any) {
      state.gatewayCalls.push({
        sessionKey: input.sessionKey,
        channelKey: input.channelKey,
        message: input.message,
      });
      const script = state.scripts.shift() ?? [];
      let index = 0;
      return {
        [Symbol.asyncIterator]: () => ({
          next: async () => {
            if (index >= script.length) return { done: true, value: undefined };
            const step = script[index++];
            if (step === '<throw>') throw new Error('gateway exploded');
            return { done: false, value: step };
          },
        }),
      };
    },
  };
}

function sign(body: string, secret: string) {
  return createHmac('sha256', secret).update(body).digest('hex');
}

function requestBytes(step: any): string {
  const headers: Record<string, string> = { Host: '127.0.0.1', ...(step.headers ?? {}) };
  if (step.mode === 'close' && !('Connection' in headers)) headers['Connection'] = 'close';
  // #1684: every pre-existing step was an HTTP/1.1 client, so the version was
  // implicit; the HTTP/1.0 cell records it explicitly instead of hardcoding
  // 1.1 and silently recording a different client than it claims.
  const lines = [`${step.method} ${step.target} ${step.version ?? 'HTTP/1.1'}`];
  for (const [name, value] of Object.entries(headers)) lines.push(`${name}: ${value}`);
  if (step.body !== undefined && step.body !== null && !('Content-Length' in headers)) {
    lines.push(`Content-Length: ${Buffer.byteLength(step.body)}`);
  }
  return `${lines.join('\r\n')}\r\n\r\n${step.body ?? ''}`;
}

// Index just past a complete response in `buffer`, or -1.
function responseEnd(buffer: string): number {
  const head = buffer.indexOf('\r\n\r\n');
  if (head < 0) return -1;
  const headText = buffer.slice(0, head);
  const chunked = /transfer-encoding:\s*chunked/i.test(headText);
  if (chunked) {
    const terminator = buffer.toLowerCase().lastIndexOf('0\r\n\r\n');
    return terminator < 0 ? -1 : terminator + 5;
  }
  const match = /content-length:\s*(\d+)/i.exec(headText);
  if (!match) return head + 4;
  const end = head + 4 + Number(match[1]);
  return buffer.length >= end ? end : -1;
}

// #2153: the recording-side twin of the Rust replay's `PARSER_ADMISSION_LABEL`
// + `fn parser_admission` (channel/webhook/tests.rs) — the label of the one
// step whose hyper bytes carry a `content-length: 0` the TS golden does not
// (the bare parser-level 400 of A8.4-NATIVE-HTTP-001 item 3). The oracle drops
// that line on the registered step only; every other step's length lines
// record verbatim, so a step that truly writes one keeps it in the golden.
const PARSER_ADMISSION_LABEL = 'unknown-method';

function parserAdmission(label: string): boolean {
  return label === PARSER_ADMISSION_LABEL;
}

function normalize(raw: string, parserAdmissionStep: boolean): any {
  const split = raw.indexOf('\r\n\r\n');
  if (split < 0) return { status: null, headers: [], body: '' };
  const head = raw.slice(0, split);
  const body = raw.slice(split + 4);
  const lines = head.split('\r\n');
  const headers = lines
    .slice(1)
    .map((line) => {
      const at = line.indexOf(':');
      const name = line.slice(0, at).trim().toLowerCase();
      const value = line.slice(at + 1).trim();
      return `${name}: ${name === 'date' ? '<date>' : value}`;
    })
    // hyper adds `content-length: 0` to the bare parser 400 where Node sends
    // none (A8.4-NATIVE-HTTP-001); the normalized oracle drops it on the
    // registered step only (#2153).
    .filter((line) => !(parserAdmissionStep && line === 'content-length: 0'));
  const chunked = /transfer-encoding:\s*chunked/i.test(head);
  const bodyNorm = chunked ? body.replace(/[0-9a-fA-F]+\r\n/g, (m) => m.toLowerCase()) : body;
  return { status: lines[0], headers, body: bodyNorm };
}

function pin(raw: string): string {
  return raw
    .split('\r\n')
    .map((line) => (/^date:/i.test(line) ? 'Date: <date>' : line))
    .join('\r\n');
}

function rawExchange(
  port: number | null,
  bytes: string,
  mode: string,
  count: number,
): Promise<{ error?: string; noResponse?: boolean; closed?: boolean; raws: string[] }> {
  return new Promise((done) => {
    if (port === null) {
      done({ error: 'no-listener' });
      return;
    }
    const raws: string[] = [];
    let buffer = '';
    let sent = 1;
    let settled = false;
    let socket: net.Socket | undefined;
    const finish = (value: any) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      socket?.destroy();
      done(value);
    };
    const timer = setTimeout(() => {
      // mode "none" expects silence; the bounded probe records whatever
      // arrived (the null-body cell never gets a response). #1690: the same
      // 5000 ms as every other mode — a shorter recording window than the
      // replay's would call a late answer "no response" here while the
      // replay still sees it (a wrong oracle); a longer one would record a
      // response the replay then misses (a false red). Equal windows, one
      // constant.
      finish(buffer === '' && raws.length === 0 ? { noResponse: true, raws: [] } : { raws: raws.concat(buffer ? [buffer] : []) });
    }, 5000);
    socket = net.createConnection({ port, host: '127.0.0.1' }, () => {
      socket!.write(bytes);
    });
    socket.setEncoding('latin1');
    socket.on('data', (chunk: string) => {
      buffer += chunk;
      if (mode === 'keepalive' && responseEnd(buffer) !== -1) {
        raws.push(buffer);
        buffer = '';
        if (sent < count) {
          sent++;
          socket!.write(bytes);
        } else {
          finish({ raws });
        }
      }
    });
    socket.on('close', () => {
      if (buffer !== '' || raws.length !== 0) finish({ raws: raws.concat(buffer ? [buffer] : []) });
      // The peer closed without sending a byte. Distinct from the deadline
      // arm above (`noResponse`): the oversize cell closes, the null-body
      // probe stays silent, and the oracle has to be able to tell them apart.
      else finish({ closed: true, raws: [] });
    });
    socket.on('error', (error: any) => finish({ error: String(error.code ?? error.message) }));
  });
}

async function runRequests(port: number | null, step: any, responses: any[]) {
  const count = step.mode === 'keepalive' ? (step.count ?? 1) : 1;
  const result = await rawExchange(port, requestBytes(step), step.mode ?? 'close', count);
  if ('error' in result && result.error !== undefined) {
    responses.push({ label: step.label, error: result.error });
    return;
  }
  if (result.noResponse) {
    responses.push({ label: step.label, noResponse: true });
    return;
  }
  if (result.closed) {
    responses.push({ label: step.label, closed: true });
    return;
  }
  if (count === 1) {
    const one = result.raws[0] ?? '';
    responses.push({ label: step.label, ...normalize(one, parserAdmission(step.label)) });
    if (step.pin) responses.push({ label: `${step.label}-pin`, raw: pin(one) });
  } else {
    result.raws.forEach((one, index) =>
      responses.push({
        label: `${step.label}-${index + 1}`,
        ...normalize(one, parserAdmission(step.label)),
      }),
    );
  }
}

const cases: any[] = [];
function scenario(name: string, options: any, steps: any[], scripts: any[][] = []) {
  cases.push({ name, options, steps, scripts });
}

const post = (label: string, deliveryId: string, body: string, extra: any = {}) => ({
  op: 'request',
  label,
  method: 'POST',
  target: '/webhooks/main',
  headers: { 'x-delivery-id': deliveryId, ...(extra.headers ?? {}) },
  body,
  mode: 'close',
  ...extra,
});
// A POST to an arbitrary webhook route name, signed with `signature`.
const namedPost = (label: string, deliveryId: string, target: string, signature: string) => ({
  op: 'request',
  label,
  method: 'POST',
  target,
  headers: { 'x-delivery-id': deliveryId, 'x-hub-signature-256': signature },
  body: json({ message: 'hi' }),
  mode: 'close',
});
const json = (value: any) => JSON.stringify(value);

// 1. health-and-routing
scenario(
  'health-and-routing',
  { port: 0, routes: { main: { secret: 'topsecret' } } },
  [
    { op: 'request', label: 'health', method: 'GET', target: '/health', mode: 'close', pin: true },
    { op: 'request', label: 'health-ka', method: 'GET', target: '/health', mode: 'keepalive', count: 2 },
    { op: 'request', label: 'get-webhooks', method: 'GET', target: '/webhooks/main', mode: 'close' },
    { op: 'request', label: 'get-unknown-path', method: 'GET', target: '/nope', mode: 'close' },
    { op: 'request', label: 'options-webhooks', method: 'OPTIONS', target: '/webhooks/main', mode: 'close' },
    { op: 'request', label: 'post-health', method: 'POST', target: '/health', body: '{}', mode: 'close' },
    { op: 'request', label: 'unknown-method', method: 'FROB', target: '/health', mode: 'close' },
  ],
);

// 2. unknown-route
scenario(
  'unknown-route',
  { port: 0, routes: { main: { secret: 'topsecret' } } },
  [post('nope', 'd1', '{}', { target: '/webhooks/nope' })],
);

// 3. rate-limit
scenario(
  'rate-limit',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } }, rateLimit: 2 },
  [
    post('first', 'd1', json({ message: 'hi' })),
    post('second', 'd2', json({ message: 'hi' })),
    post('third', 'd3', json({ message: 'hi' })),
  ],
  [[], []],
);

// 4. too-large
scenario(
  'too-large',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } }, maxBodyBytes: 10 },
  [
    post('oversize', 'd1', 'x'.repeat(20)),
    // Exactly maxBodyBytes (10) bytes: the TS size check is strictly `>`, so
    // the boundary body is read and accepted, not destroyed.
    post('exactly-max', 'd2', '"01234567"'),
  ],
  [[]],
);

// 5. invalid-signature
const signed = (label: string, deliveryId: string, body: string, secret: string, mode: 'bare' | 'prefix' | 'upper' | 'wrong') => {
  const headers: Record<string, string> = { 'x-delivery-id': deliveryId };
  const digest =
    mode === 'wrong' ? sign('tampered body', secret) : sign(body, secret);
  headers['x-hub-signature-256'] =
    mode === 'prefix' ? `sha256=${digest}` : mode === 'upper' ? `sha256=${digest.toUpperCase()}` : digest;
  return post(label, deliveryId, body, { headers });
};
scenario(
  'invalid-signature',
  { port: 0, routes: { main: { secret: 'topsecret' } } },
  [
    post('absent', 'd1', json({ message: 'hi' })),
    signed('bare-hex', 'd2', json({ message: 'hi' }), 'topsecret', 'bare'),
    signed('prefixed', 'd3', json({ message: 'hi' }), 'topsecret', 'prefix'),
    signed('uppercase', 'd4', json({ message: 'hi' }), 'topsecret', 'upper'),
    signed('wrong-body', 'd5', json({ message: 'hi' }), 'topsecret', 'wrong'),
  ],
  [[], [], []],
);

// 6. insecure-no-auth
scenario(
  'insecure-no-auth',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [post('no-signature', 'd1', json({ message: 'hi' }))],
  [[]],
);

// 7. duplicate-delivery
scenario(
  'duplicate-delivery',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [
    post('first', 'dup-1', json({ message: 'hello' })),
    post('repeat', 'dup-1', json({ message: 'hello' })),
  ],
  [[{ type: 'assistant_text_delta', text: 'first reply' }]],
);

// 8. no-text (and the null-body hang)
scenario(
  'no-text',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [
    post('empty-object', 'd1', '{}'),
    post('short-json', 'd2', '"ab"'),
    post('not-json', 'd3', 'not json at all'),
    // #1621: a bare string body falls to `extractText`'s last arm —
    // `JSON.stringify(body)`, so the units are the stringify'd ones and the
    // gate is `str.length > 5` (with `"{}"` excluded). The golden only had
    // 4-unit ("ab", this cell above) and 10-unit samples on the same side of
    // the gate. The 5/6-unit cells straddle it — 5 stays ignored and 6 turns
    // into text — so `>=` instead of `>` turns the five-unit cell red.
    post('five-units', 'd6', '"abc"'),
    post('six-units', 'd7', '"abcd"'),
    // #1681: `extractText` returns the empty string for a string `text` field,
    // and the call site's `if (!text)` treats it as no text. The cell pins the
    // ignored reply, no deliveryInfo and no gateway call.
    post('empty-text', 'd5', json({ text: '' })),
    post('null-body', 'd4', 'null', { mode: 'none' }),
  ],
  [[], []],
);

// 9. extract-github
scenario(
  'extract-github',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [
    post('issue', 'd1', json({ action: 'opened', issue: { title: 'Fix bug', body: 'please fix' } })),
    post('pr-without-title', 'd2', json({ action: 'reopened', pull_request: { body: null } })),
    post('pr', 'd3', json({ action: 'opened', pull_request: { title: 'Add feature', body: 'does things' } })),
    post('comment', 'd4', json({ comment: { body: 'nice one' } })),
    post('json-fallback', 'd5', json({ foo: 'bar' })),
    post('issue-nonstring-body', 'd6', json({ action: 'x', issue: { body: 42, title: 't' } })),
  ],
  [[], [], [], [], [], []],
);

// 10. submit-and-render
scenario(
  'submit-and-render',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [
    post('full-turn', 'd1', json({ message: 'hi' })),
    post('error-turn', 'd2', json({ message: 'hi' })),
  ],
  [
    [
      { type: 'assistant_text_delta', text: 'Hello ' },
      { type: 'assistant_text_delta', text: 'world' },
      { type: 'assistant_thinking_delta' },
      { type: 'tool_call_started', name: 'web_search' },
      { type: 'tool_call_finished', toolCallId: 'call_1', ok: true },
      { type: 'tool_call_finished', toolName: 'bash', ok: false },
      { type: 'agent_status', event: 'model_progress', detail: { severity: 'info' } },
      { type: 'agent_status', event: 'turn_timeout', detail: { severity: 'error', message: 'turn timed out' } },
      { type: 'error', message: 'swallowed after a visible failure' },
      { type: 'assistant_text_delta', text: 'y'.repeat(260) },
    ],
    [
      { type: 'error', message: 'early failure' },
      { type: 'assistant_text_delta', text: 'never rendered' },
    ],
  ],
);

// 11. deliver-other
scenario(
  'deliver-other',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__', deliver: 'http' } } },
  [post('http-deliver', 'd1', json({ message: 'hi' }))],
  [[{ type: 'assistant_text_delta', text: 'x'.repeat(150) }]],
);

// 12. submit-error
scenario(
  'submit-error',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [post('throwing', 'd1', json({ message: 'hi' }))],
  [['<throw>']],
);

// 13. new-command
scenario(
  'new-command',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } }, mapperFixedUuid: true },
  [
    post('new-empty', 'd1', '/new'),
    post('new-with-message', 'd2', '/new hello'),
    post('follow-up', 'd3', 'follow up'),
  ],
  [
    [{ type: 'assistant_text_delta', text: 'new session reply' }],
    [{ type: 'assistant_text_delta', text: 'general reply' }],
  ],
);

// 14. gateway-missing
scenario('gateway-missing', { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } }, [
  post('no-gateway', 'd1', json({ message: 'hi' })),
]);

// 15. start failures: no secret, port in use, invalid port
scenario('no-secret-route', { port: 0, routes: { r1: {} } }, [
  post('after-failed-start', 'd1', json({ message: 'hi' })),
]);
const blocker = net.createServer(() => {});
await new Promise<void>((ready) => blocker.listen(0, '127.0.0.1', ready));
const blockerPort = (blocker.address() as any).port;
// The oracle records the sentinel, not the ephemeral blocker port; the Rust
// replay binds its own blocker and substitutes the real port.
const BUSY_PORT = '<busy-port>';
scenario('port-in-use', { port: BUSY_PORT, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } }, []);
scenario('invalid-port', { port: 99999, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } }, []);

// 16. cron-delivery
scenario(
  'cron-delivery',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [
    post('seed', 'd1', json({ message: 'hi' })),
    { op: 'cron', sessionKey: 'webhook:chat=webhook:main:d1:general', text: 'cron reply' },
    { op: 'cron', sessionKey: 'webhook:chat=webhook:main:d1:general', originChannelKey: 'telegram', text: 'nope' },
    // #1699: an empty `originChannelKey` is falsy in TS, so it means "not set"
    // and the delivery runs — only a non-empty foreign channel gates it. The
    // cell records the delivered `true` next to the gated `false` above.
    { op: 'cron', sessionKey: 'webhook:chat=webhook:main:d1:general', originChannelKey: '', text: 'empty origin reply' },
    { op: 'cron', sessionKey: 'unparseable', originSessionKey: 'webhook:chat=orig:general', text: 'origin reply' },
    { op: 'cron', sessionKey: 'webhook:unparseable' },
  ],
  [[]],
);

// 17. stop-and-restart
scenario(
  'stop-and-restart',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [
    post('first', 'd1', json({ message: 'hi' })),
    { op: 'stop', reason: 'drain' },
    post('refused', 'd2', json({ message: 'hi' })),
    { op: 'start' },
    post('second', 'd3', json({ message: 'hi' })),
    { op: 'stop' },
  ],
  [
    [{ type: 'assistant_text_delta', text: 'first reply' }],
    [{ type: 'assistant_text_delta', text: 'second reply' }],
  ],
);

// 18. http10: an HTTP/1.0 client asking to keep alive. Node answers its own
// HTTP/1.1 status line with `Connection: close` on a closed socket; hyper
// answers a 1.0 line (the accepted-diff entry A8.3-CH-WEBHOOK-004, approved
// by its R1 correction `A8.3-CH-WEBHOOK-004-R1`) and, before
// #1684, kept the socket open with `Connection: keep-alive`. The recorded
// response is close-delimited (Node leaves off the content-length it uses for
// 1.1), so the Rust side replays this cell in a pinned direct test that
// substitutes only the registered status line.
scenario(
  'http10',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [
    {
      op: 'request',
      label: 'http10-keepalive',
      method: 'GET',
      target: '/health',
      version: 'HTTP/1.0',
      headers: { Connection: 'keep-alive' },
      mode: 'close',
    },
  ],
);

// 19. object-prototype-route-names (#1682): `const route = this.routes[routeName]`
// walks the prototype chain, so a name that is not an own key but IS an own
// property of `Object.prototype` still resolves — to a function, or to
// `Object.prototype` itself for the `__proto__` accessor (measured on the
// pinned baseline: the getter returns the prototype object, not `null`, so
// `__proto__` is NOT falsy and is NOT an exception). Every one of them is
// truthy and carries neither `secret` nor `deliver`, so TS treats the request
// exactly like one for an empty route object: the HMAC falls back to the
// global secret (`route.secret || this.globalSecret`) and `deliver ?? "log"`
// accepts it. A name absent from both the record and the prototype (`nope`)
// still answers 404, and a wrong/absent signature still answers 401 — the
// prototype hit skips only the unknown-route branch, not authentication.
scenario(
  'object-prototype-route-names',
  { port: 0, secret: 'topsecret', routes: { main: { secret: 'topsecret' } } },
  [
    namedPost('constructor-signed', 'd1', '/webhooks/constructor', sign(json({ message: 'hi' }), 'topsecret')),
    namedPost('proto-signed', 'd2', '/webhooks/__proto__', sign(json({ message: 'hi' }), 'topsecret')),
    {
      op: 'request',
      label: 'to-string-unsigned',
      method: 'POST',
      target: '/webhooks/toString',
      headers: { 'x-delivery-id': 'd3' },
      body: json({ message: 'hi' }),
      mode: 'close',
    },
    namedPost('nope-signed', 'd4', '/webhooks/nope', sign(json({ message: 'hi' }), 'topsecret')),
  ],
  [[], []],
);

// 20. fallback-delivery-id (#1621): neither request carries a delivery-id
// header, so the id falls back to `${routeName}-${Date.now()/1000}`. The
// scenario records `frozenNow` (milliseconds) and the generator shims
// `Date.now` to it for the scenario's whole lifetime, so the fallback id is
// the literal `main-1700000000` — pinned in the accepted reply, the
// duplicate reply and the gateway session key — and the second request,
// frozen to the same instant, deduplicates against the first: the same-key
// dedup of fallback ids a real clock only gives by landing inside one
// millisecond.
scenario(
  'fallback-delivery-id',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } }, frozenNow: 1_700_000_000_000 },
  [
    { op: 'request', label: 'no-id-first', method: 'POST', target: '/webhooks/main', body: json({ message: 'hi' }), mode: 'close' },
    { op: 'request', label: 'no-id-repeat', method: 'POST', target: '/webhooks/main', body: json({ message: 'hi' }), mode: 'close' },
  ],
  [[{ type: 'assistant_text_delta', text: 'first reply' }]],
);

// 21. order-combos (#1621): precedence pairs the single-check cells cannot
// express. Route lookup runs before the size cap, so an oversize body to an
// unknown route answers 404 instead of the zero-byte close of the too-large
// cell. The rate window runs before the signature check and a rejected
// signature still consumes quota, so three wrong-signed in-limit requests
// answer 401, 401, then 429 (the signed bodies are exactly maxBodyBytes —
// any larger and the size close would mask the signature arm).
scenario(
  'order-combos',
  { port: 0, routes: { main: { secret: 'topsecret' } }, rateLimit: 2, maxBodyBytes: 10 },
  [
    post('unknown-route-oversize', 'd1', 'x'.repeat(20), { target: '/webhooks/nope' }),
    signed('oversign-1', 'd2', '"01234567"', 'topsecret', 'wrong'),
    signed('oversign-2', 'd3', '"01234567"', 'topsecret', 'wrong'),
    signed('oversign-3', 'd4', '"01234567"', 'topsecret', 'wrong'),
  ],
);

// 22. utf8-body (#1690): a non-ASCII body rides the request as real UTF-8
// bytes — the generator's `socket.write(string)` uses Node's default utf8
// encoding, not the socket's latin1 *read* encoding — and the rendered reply
// comes back through the same channel. Pins the whole non-ASCII decode
// round-trip; invalid-sequence lossy replacement is a registered remaining
// gap (README), not covered here.
scenario(
  'utf8-body',
  { port: 0, routes: { main: { secret: '__INSECURE_NO_AUTH__' } } },
  [post('utf8-message', 'd1', json({ message: '你好 webhook' }))],
  [[{ type: 'assistant_text_delta', text: '你好回复' }]],
);

const EXPECTED_CASE_COUNT = 24;
assert.equal(cases.length, EXPECTED_CASE_COUNT, '#1334 generator case count');

// The null-body cell rejects the voided handleRequest promise; keep the
// generator alive instead of crashing on it.
process.on('unhandledRejection', () => {});

const unhandledBefore = process.listenerCount('unhandledRejection');
const realDateNow = Date.now;
const oracle: any[] = [];
for (const definition of cases) {
  const state = {
    gatewayCalls: [] as any[],
    scripts: definition.scripts.map((script: any) => [...script]),
    logs: [] as any[],
    responses: [] as any[],
    cronBools: [] as boolean[],
  };
  const logger = {
    info: (text: string) => state.logs.push(['info', text]),
    error: (text: string) => state.logs.push(['error', text]),
  };
  const options: any = {
    port: definition.options.port === BUSY_PORT ? blockerPort : (definition.options.port ?? 0),
    routes: definition.options.routes,
  };
  if (definition.options.secret !== undefined) options.secret = definition.options.secret;
  if (definition.options.rateLimit !== undefined) options.rateLimit = definition.options.rateLimit;
  if (definition.options.maxBodyBytes !== undefined) options.maxBodyBytes = definition.options.maxBodyBytes;
  if (definition.options.mapperFixedUuid) {
    options.mapper = new WebhookSessionMapper({ activeByChatId: {} }, () => FIXED_UUID);
  }
  const channel = new WebhookChannel(options);
  // #1621: a scenario recorded with `frozenNow` shims `Date.now` for the
  // channel's whole lifetime — start through stop — so the fallback
  // delivery id (and the rate/dedup windows keyed on the same clock) record
  // literal, replayable values. `frozenNow` itself is never passed to the TS
  // constructor; it is recorded in `options` for the Rust replay's `now`
  // seam, like `mapperFixedUuid`.
  const frozenNow = definition.options.frozenNow;
  if (frozenNow !== undefined) Date.now = () => frozenNow;
  const deps: any = { logger };
  if (definition.name !== 'gateway-missing') deps.gateway = makeGateway(state);
  const serversBefore = seenServers.length;
  let handle = await channel.start(deps);
  // A failed start either never creates the server (no secret) or leaves it
  // unbound (port in use, invalid port): address() is null either way.
  const server = seenServers.length > serversBefore ? seenServers[seenServers.length - 1] : null;
  let port = server?.address()?.port ?? null;
  for (const step of definition.steps) {
    if (step.op === 'request') {
      await runRequests(port, step, state.responses);
    } else if (step.op === 'stop') {
      await handle.stop(step.reason);
    } else if (step.op === 'start') {
      handle = await channel.start(deps);
      // Port 0 means the restart binds a fresh ephemeral port; re-capture it
      // so later steps hit the restarted listener.
      const restarted = seenServers[seenServers.length - 1];
      port = restarted?.address()?.port ?? null;
    } else if (step.op === 'cron') {
      const taken = await channel.deliverCronResult({
        taskId: 't',
        runId: 'r',
        sessionKey: step.sessionKey,
        channelKey: 'webhook',
        originSessionKey: step.originSessionKey,
        originChannelKey: step.originChannelKey,
        text: step.text ?? 'cron text',
      });
      state.cronBools.push(taken);
    }
  }
  // handleIncoming is fire-and-forget: wait for the log/gateway streams to go
  // quiet (two consecutive stable polls) before capturing the oracle.
  let last = -1;
  for (let n = 0; n < 200; n++) {
    const count = state.logs.length + state.gatewayCalls.length + state.cronBools.length;
    if (count === last) break;
    last = count;
    await new Promise((done) => setTimeout(done, 25));
  }
  const masked = state.logs.map(([level, text]: any) => [
    level,
    text.startsWith('webhook: failed to start: ')
      ? `${text.slice(0, 'webhook: failed to start: '.length)}<masked>`
      : text,
  ]);
  oracle.push({
    name: definition.name,
    options: definition.options,
    steps: definition.steps,
    // The scripted gateway event streams, so the replay is self-contained.
    scripts: definition.scripts,
    expected: {
      responses: state.responses,
      gateway: state.gatewayCalls,
      logs: masked,
      cron: state.cronBools,
    },
  });
  // Stop after the capture: the stopping log line is intentionally not part
  // of the oracle, and closing the listener lets the process exit.
  await handle.stop();
  if (frozenNow !== undefined) Date.now = realDateNow;
}
assert.equal(process.listenerCount('unhandledRejection'), unhandledBefore, '#1334 no stray handlers');

const out = resolve(process.argv[2]!);
await mkdir(out, { recursive: true });
const document = { sourceCommit, sourceSha256, node: process.version, icu: process.versions.icu, expectedCaseCount: EXPECTED_CASE_COUNT, cases: oracle };
await writeFile(join(out, 'oracle.json.tmp'), `${JSON.stringify(document, null, 1)}\n`, { flag: 'wx' });
await rename(join(out, 'oracle.json.tmp'), join(out, 'oracle.json'));
blocker.close();
console.log(`#1334 webhook-channel: ${oracle.length} cases`);
