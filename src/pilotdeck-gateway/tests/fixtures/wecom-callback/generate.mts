// #1336 wecom-callback golden fixture. Runs the real baseline channel
// (WeComCallbackChannel.ts + mapper + render, transpiled unchanged) against
// scripted fakes: a fake A7 gateway, fake IM helpers and a loopback qyapi
// server reached through a globalThis.fetch URL rewrite. No real account,
// no off-loopback traffic. See README.md for the replay contract.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as http from 'node:http';
import * as net from 'node:net';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1336 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1336 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, '#1336 PDRS_BASELINE');
assert.ok(process.argv[2], '#1336 output directory');
for (const name of ['WECOM_CORP_ID', 'WECOM_AGENT_ID', 'WECOM_CB_SECRET', 'WECOM_CB_TOKEN', 'WECOM_ENCODING_AES_KEY', 'WECOM_CB_PORT']) {
  delete process.env[name];
}
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/adapters/channel/wecom-callback/WeComCallbackChannel.ts',
  'src/adapters/channel/wecom-callback/WeComCallbackSessionMapper.ts',
  'src/adapters/channel/wecom-callback/wecom-callback-render.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');

// #1336 (fixtures P1): `tcp-open` used to wait only for the *client's*
// `connect` callback, so the following `stop-pending` could run before the
// server had accepted the connection — libuv does not order those two
// callbacks, and the recorded drain cell then depended on which won. Wrapping
// the (shared, mutable) builtin `createServer` lets the case wait for the
// server's own `request` event, which is the same barrier the Rust replay
// takes from its per-channel accept counter (incremented on `service_fn`
// entry, i.e. once the request head is parsed and the body is still pending).
const nodeHttp = requireBaseline('node:http') as typeof http;
const recordedServers: http.Server[] = [];
const createServer = nodeHttp.createServer;
nodeHttp.createServer = ((...args: any[]) => {
  const server = (createServer as any)(...args);
  recordedServers.push(server);
  return server;
}) as any;

const CORP = 'corp-x';
const TOKEN = 'cbToken1';
const SECRET = 'sec-1';
const AGENT = '1000002';
const AESKEY = Buffer.from('0123456789abcdefghijklmnopqrstuv', 'utf8').toString('base64').replace(/=+$/, '');

// ---- message construction (fixed keys, fixed 0x41 AES prefix) --------------

function sha1(text: string): string {
  return crypto.createHash('sha1').update(text).digest('hex');
}
function verifySig(timestamp: string, nonce: string, encrypt: string): string {
  return sha1([TOKEN, timestamp, nonce, encrypt].sort().join(''));
}
function encryptXml(plain: string): string {
  const key = Buffer.from(AESKEY + '=', 'base64');
  const cipher = crypto.createCipheriv('aes-256-cbc', key, key.subarray(0, 16));
  const msg = Buffer.from(plain, 'utf8');
  const payload = Buffer.concat([
    Buffer.alloc(16, 0x41), Buffer.alloc(4), msg, Buffer.from(CORP, 'utf8'),
  ]);
  payload.writeUInt32BE(msg.length, 16);
  return Buffer.concat([cipher.update(payload), cipher.final()]).toString('base64');
}
function innerXml(from: string | null, msgType: string, content: string): string {
  const fromTag = from === null ? '' : `<FromUserName><![CDATA[${from}]]></FromUserName>`;
  return `<xml><ToUserName><![CDATA[${CORP}]]></ToUserName>${fromTag}` +
    `<MsgType><![CDATA[${msgType}]]></MsgType><Content><![CDATA[${content}]]></Content></xml>`;
}
function postStep(from: string | null, msgType: string, content: string, bad = false): any {
  const enc = encryptXml(innerXml(from, msgType, content));
  const timestamp = '1409659813';
  const nonce = 'n1';
  const sig = bad ? 'deadbeef' : verifySig(timestamp, nonce, enc);
  return {
    op: 'http', method: 'POST',
    path: `/cb?msg_signature=${sig}&timestamp=${timestamp}&nonce=${nonce}`,
    body: `<xml><Encrypt><![CDATA[${enc}]]></Encrypt></xml>`,
  };
}
function getStep(kind: 'ok' | 'missing' | 'bad'): any {
  const enc = encryptXml('echo-plain-1');
  const timestamp = '1409659813';
  const nonce = 'n1';
  const echo = kind === 'missing' ? '' : `&echostr=${encodeURIComponent(enc)}`;
  const sig = kind === 'bad' ? 'deadbeef' : verifySig(timestamp, nonce, enc);
  return { op: 'http', method: 'GET', path: `/cb?msg_signature=${sig}&timestamp=${timestamp}&nonce=${nonce}${echo}` };
}

// ---- scripted fakes --------------------------------------------------------

type Case = {
  name: string;
  config: Record<string, unknown>;
  qyapi: { status: number; json: unknown }[];
  turns: { gateBefore?: string; events: unknown[]; error?: string }[];
  elicitation: { pending: string[]; capture: string; answer: string | null };
  permission: {
    pending: string[];
    capture: string | null;
    answer: { text: string; canAdvance: boolean; retryPrompt: boolean; answerToken: number } | null;
    nextPrompt: string | null;
    promptRequestId: string | null;
  };
  steps: any[];
};

function makeChannelClass(definition: Case, helpers: unknown[]) {
  const read = (rel: string) => fs.readFileSync(join(baseline, rel), 'utf8');
  const compile = (rel: string) =>
    transformSync(read(rel), { loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: rel }).code;
  const evaluate = (code: string) => {
    const module = { exports: {} as any };
    return (request: (name: string) => any) => {
      new Function('require', 'module', 'exports', code)(request, module, module.exports);
      return module.exports;
    };
  };
  const script = definition;
  class FakeElicitation {
    hasPending(chatId: string) { helpers.push(['elicitation.hasPending', chatId]); return script.elicitation.pending.includes(chatId); }
    capture(chatId: string, sessionKey: string, event: any) {
      helpers.push(['elicitation.capture', chatId, sessionKey, event.type]);
      return script.elicitation.capture;
    }
    async answer(chatId: string, text: string) {
      helpers.push(['elicitation.answer', chatId, text]);
      return script.elicitation.answer;
    }
    clear(chatId: string) { helpers.push(['elicitation.clear', chatId]); }
  }
  class FakePermission {
    hasPending(chatId: string) { helpers.push(['permission.hasPending', chatId]); return script.permission.pending.includes(chatId); }
    capture(chatId: string, sessionKey: string, event: any) {
      helpers.push(['permission.capture', chatId, sessionKey, event.type]);
      return script.permission.capture;
    }
    confirmInitialPrompt(chatId: string, delivered: boolean, requestId?: string) {
      helpers.push(['permission.confirmInitialPrompt', chatId, delivered, requestId ?? null]);
    }
    async answerWithState(chatId: string, text: string) {
      helpers.push(['permission.answerWithState', chatId, text]);
      return script.permission.answer;
    }
    releaseAnswer(chatId: string, token?: number) { helpers.push(['permission.releaseAnswer', chatId, token ?? null]); }
    takeNextPrompt(chatId: string, token?: number) {
      helpers.push(['permission.takeNextPrompt', chatId, token ?? null]);
      return script.permission.nextPrompt;
    }
    getPromptRequestId(chatId: string, token?: number) {
      helpers.push(['permission.getPromptRequestId', chatId, token ?? null]);
      return script.permission.promptRequestId;
    }
    confirmNextPrompt(chatId: string, delivered: boolean, requestId: string | null, token?: number) {
      helpers.push(['permission.confirmNextPrompt', chatId, delivered, requestId, token ?? null]);
    }
    clearAfterTurn(chatId: string) { helpers.push(['permission.clearAfterTurn', chatId]); }
  }
  const mapperModule = evaluate(compile(sources[1]));
  const renderModule = evaluate(compile(sources[2]));
  const channelModule = evaluate(compile(sources[0]));
  const nodeOnly = (name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    throw new Error(`#1336 dependency module must not import anything: ${name}`);
  };
  const channelRequire = (name: string) => {
    if (name === 'node:http' || name === 'node:crypto' || name === 'node:url') return requireBaseline(name);
    if (name === '../gateway/index.js' || name === '../cron/index.js' || name === '../protocol/ChannelAdapter.js') return {};
    if (name === '../protocol/ImCronDelivery.js') return { deliverChatCronResult: async () => false };
    if (name === '../protocol/ImElicitationHelper.js') return { ImElicitationHelper: FakeElicitation };
    if (name === '../protocol/ImPermissionHelper.js') return { ImPermissionHelper: FakePermission };
    if (name === './WeComCallbackSessionMapper.js') return mapperModule(nodeOnly);
    if (name === './wecom-callback-render.js') return renderModule(nodeOnly);
    throw new Error(`#1336 unexercised import ${name}`);
  };
  return {
    channel: channelModule(channelRequire).WeComCallbackChannel,
    mapper: mapperModule(nodeOnly).WeComCallbackSessionMapper,
  };
}

// ---- qyapi loopback mock ---------------------------------------------------

const QYAPI_PORT = 18998;
const qyapiCalls: any[] = [];
let qyapiQueue: { status: number; json: unknown }[] = [];
const qyapiServer = http.createServer((req, res) => {
  const chunks: Buffer[] = [];
  req.on('data', (c: Buffer) => chunks.push(c));
  req.on('end', () => {
    const url = new URL(req.url ?? '/', 'http://qyapi');
    qyapiCalls.push({
      method: req.method,
      path: url.pathname,
      query: url.search ? url.search.slice(1) : null,
      contentType: req.headers['content-type'] ?? null,
      body: chunks.length ? Buffer.concat(chunks).toString('utf8') : null,
    });
    const next = qyapiQueue.shift() ?? { status: 200, json: { errcode: 0 } };
    res.writeHead(next.status, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(next.json));
  });
});
await new Promise<void>(done => qyapiServer.listen(QYAPI_PORT, '127.0.0.1', done));
const realFetch = globalThis.fetch;
globalThis.fetch = (async (input: any, init?: any) =>
  realFetch(String(input).replace('https://qyapi.weixin.qq.com', `http://127.0.0.1:${QYAPI_PORT}`), init)) as typeof fetch;

// ---- step executors -------------------------------------------------------

function until(predicate: () => boolean): Promise<void> {
  const started = Date.now();
  return new Promise((resolve, reject) => {
    const check = () => {
      if (predicate()) return resolve();
      if (Date.now() - started > 5000) return reject(new Error('#1336 wait timeout'));
      setImmediate(check);
    };
    check();
  });
}
async function ticks(n: number) {
  for (let i = 0; i < n; i++) await new Promise(done => setImmediate(done));
}
function httpRequest(port: number, method: string, path: string, body?: string): Promise<any> {
  return new Promise(resolve => {
    const req = http.request(
      { host: '127.0.0.1', port, method, path, headers: { Connection: 'close' } },
      response => {
        const chunks: Buffer[] = [];
        response.on('data', (c: Buffer) => chunks.push(c));
        response.on('end', () => resolve({
          status: response.statusCode,
          contentType: response.headers['content-type'] ?? null,
          body: Buffer.concat(chunks).toString('utf8'),
        }));
      },
    );
    req.on('error', (e: NodeJS.ErrnoException) => resolve({ error: e.code ?? String(e) }));
    if (body !== undefined) req.write(body);
    req.end();
  });
}
function parseRawResponse(raw: Buffer): any {
  const headerEnd = raw.indexOf('\r\n\r\n');
  assert(headerEnd >= 0, '#1336 response header end');
  const lines = raw.subarray(0, headerEnd).toString('utf8').split('\r\n');
  const status = Number(lines[0].split(' ')[1]);
  const headers = new Map<string, string>();
  for (const line of lines.slice(1)) {
    const at = line.indexOf(':');
    headers.set(line.slice(0, at).trim().toLowerCase(), line.slice(at + 1).trim());
  }
  let body = raw.subarray(headerEnd + 4);
  if (headers.get('transfer-encoding') === 'chunked') {
    const decoded: Buffer[] = [];
    let at = 0;
    for (;;) {
      const lineEnd = body.indexOf('\r\n', at);
      if (lineEnd < 0) break;
      const size = parseInt(body.subarray(at, lineEnd).toString('utf8'), 16);
      if (size === 0) break;
      decoded.push(body.subarray(lineEnd + 2, lineEnd + 2 + size));
      at = lineEnd + 2 + size + 2;
    }
    body = Buffer.concat(decoded);
  }
  return { status, contentType: headers.get('content-type') ?? null, body: body.toString('utf8') };
}

async function runCase(index: number, definition: Case): Promise<any> {
  const port = Number(definition.config.port);
  assert(Number.isInteger(port) && port > 0, '#1336 case port');
  const helpers: unknown[] = [];
  const gateway: unknown[] = [];
  const logs: unknown[] = [];
  const events: unknown[] = [];
  const httpResults: unknown[] = [];
  const gates = new Map<string, { promise: Promise<void>; release: () => void }>();
  const gate = (name: string) => {
    if (!gates.has(name)) {
      let release!: () => void;
      const promise = new Promise<void>(done => { release = done; });
      gates.set(name, { promise, release });
    }
    return gates.get(name)!;
  };
  let turnIndex = 0;
  const gatewayFake = {
    submitTurn(input: any) {
      gateway.push({ sessionKey: input.sessionKey, channelKey: input.channelKey, message: input.message });
      const turn = definition.turns[turnIndex++] ?? { events: [] };
      return (async function* () {
        if (turn.gateBefore) await gate(turn.gateBefore).promise;
        for (const event of turn.events) yield event;
        if (turn.error) throw new Error(turn.error);
      })();
    },
  };
  const modules = makeChannelClass(definition, helpers);
  let uuidCounter = 0;
  const mapper = new modules.mapper(undefined, () => `s_uuid-${++uuidCounter}`);
  const channel = new modules.channel({ ...definition.config, mapper });
  let handle: any = null;
  const pendingStops = new Map<string, Promise<void>>();
  const sockets = new Map<string, { socket: net.Socket; chunks: Buffer[] }>();
  let occupier: net.Server | null = null;

  for (const step of definition.steps) {
    if (step.op === 'occupy') {
      occupier = net.createServer(() => {});
      await new Promise<void>(done => occupier!.listen(port, '0.0.0.0', done));
    } else if (step.op === 'start') {
      handle = await channel.start({
        gateway: gatewayFake,
        logger: {
          info: (m: string) => logs.push(['info', m]),
          warn: (m: string) => logs.push(['warn', m]),
          error: (m: string) => logs.push(['error', m]),
        },
      });
    } else if (step.op === 'http') {
      httpResults.push(await httpRequest(port, step.method, step.path, step.body));
    } else if (step.op === 'stop') {
      await handle.stop(step.reason);
      events.push({ stop: step.reason });
    } else if (step.op === 'stop-pending') {
      pendingStops.set(step.label, handle.stop(step.reason));
    } else if (step.op === 'await') {
      await pendingStops.get(step.label);
      events.push({ stop: step.reason });
    } else if (step.op === 'tcp-open') {
      const server = recordedServers[recordedServers.length - 1];
      // The next step (`stop-pending`) must not run before the server has
      // parsed this request head: that is the point the Rust replay waits at
      // (the `service_fn` entry in `wecom_callback::http`, i.e. headers in,
      // body still pending). Waiting only for the client's `connect` — or for
      // the server's `connection` (accept) event, which still precedes header
      // parsing — left the recorded stop/drain order to libuv: Node >= 19's
      // `server.close()` reaps a connection it still counts as idle, so the
      // same case could record a reset instead of the drained 200.
      const headRead = new Promise<void>(resolve => server.once('request', () => resolve()));
      const socket = net.connect(port, '127.0.0.1');
      const record = { socket, chunks: [] as Buffer[] };
      sockets.set(step.label, record);
      socket.on('data', (c: Buffer) => record.chunks.push(c));
      // The connection must be established before the request head is written,
      // otherwise the listener may close first and the connect fails.
      await new Promise<void>((resolve, reject) => {
        socket.once('connect', () => resolve());
        socket.once('error', error => reject(error));
      });
      socket.write(step.request);
      await headRead;
    } else if (step.op === 'tcp-chunk') {
      sockets.get(step.label)!.socket.write(step.chunk);
    } else if (step.op === 'tcp-end') {
      const record = sockets.get(step.label)!;
      await new Promise<void>(resolve => {
        record.socket.on('close', () => resolve());
        record.socket.end();
      });
      httpResults.push(parseRawResponse(Buffer.concat(record.chunks)));
      sockets.delete(step.label);
    } else if (step.op === 'release') {
      gate(step.gate).release();
    } else if (step.op === 'wait') {
      if (step.qyapi !== undefined) await until(() => qyapiCalls.length >= step.qyapi);
      else if (step.helpers !== undefined) await until(() => helpers.length >= step.helpers);
      else if (step.gateway !== undefined) await until(() => gateway.length >= step.gateway);
      else if (step.ticks !== undefined) await ticks(step.ticks);
      else throw new Error('#1336 wait without condition');
    } else {
      throw new Error(`#1336 unknown op ${step.op}`);
    }
  }
  if (occupier) await new Promise<void>(done => occupier!.close(() => done()));
  // `qyapiCalls` is reset per case; snapshot so this case's record survives.
  return { http: httpResults, qyapi: [...qyapiCalls], gateway, helpers, logs, events };
}

// ---- case definitions -----------------------------------------------------

const OK_TOKEN = { status: 200, json: { errcode: 0, access_token: 'tok-abc', expires_in: 7200 } };
const noHelpers = {
  elicitation: { pending: [], capture: '', answer: null },
  permission: { pending: [], capture: null, answer: null, nextPrompt: null, promptRequestId: null },
};
const baseConfig = { corpId: CORP, secret: SECRET, token: TOKEN, encodingAesKey: AESKEY, agentId: AGENT };
const slowPost = (() => {
  const step = postStep('chat1', 'text', 'slow body');
  const headers = `POST ${step.path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: ${Buffer.byteLength(step.body)}\r\nConnection: close\r\n\r\n`;
  return { request: headers, chunk: step.body };
})();

const definitions: Case[] = [
  {
    name: 'missing-required-config',
    config: { token: TOKEN, encodingAesKey: AESKEY, secret: SECRET, agentId: AGENT, port: 18900 },
    qyapi: [], turns: [], ...noHelpers,
    steps: [
      { op: 'start' },
      { op: 'http', method: 'GET', path: '/cb?x=1' },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'missing-outbound-config',
    // 18921, not 18901: `channel/sms/tests.rs` binds 18901 as its fixed
    // `webhookPort`, and both suites share the `pilotdeck-gateway` lib test
    // binary — an overlap would turn this case's expected `ECONNREFUSED` into
    // a connection to the sms listener (a false FAIL, or a false PASS for the
    // missing-config path).
    config: { corpId: CORP, token: TOKEN, encodingAesKey: AESKEY, port: 18921 },
    qyapi: [], turns: [], ...noHelpers,
    steps: [
      { op: 'start' },
      { op: 'http', method: 'GET', path: '/cb?x=1' },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'bind-failure',
    config: { ...baseConfig, port: 18902 },
    qyapi: [], turns: [], ...noHelpers,
    steps: [
      { op: 'occupy' },
      { op: 'start' },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'bad-port',
    config: { ...baseConfig, port: 70000 },
    qyapi: [], turns: [], ...noHelpers,
    steps: [
      { op: 'start' },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'http-get-faces',
    config: { ...baseConfig, port: 18903 },
    qyapi: [], turns: [], ...noHelpers,
    steps: [
      { op: 'start' },
      getStep('missing'),
      getStep('bad'),
      getStep('ok'),
      { op: 'http', method: 'PUT', path: '/cb' },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'post-rejects',
    config: { ...baseConfig, port: 18904 },
    qyapi: [], turns: [], ...noHelpers,
    steps: [
      { op: 'start' },
      { op: 'http', method: 'POST', path: '/cb?msg_signature=x&timestamp=1&nonce=n', body: '<xml><Foo/></xml>' },
      postStep('chat1', 'text', 'hi', true),
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'post-dispatches-turn',
    config: { ...baseConfig, port: 18905 },
    qyapi: [OK_TOKEN],
    turns: [{ events: [{ type: 'assistant_text_delta', text: 'Hello ' }, { type: 'assistant_text_delta', text: 'world' }] }],
    ...noHelpers,
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', 'hello'),
      { op: 'wait', qyapi: 2 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'post-ignores',
    config: { ...baseConfig, port: 18906 },
    qyapi: [], turns: [], ...noHelpers,
    steps: [
      { op: 'start' },
      postStep('chat1', 'event', 'hi'),
      postStep('chat1', 'text', '   '),
      postStep(null, 'text', 'hi'),
      { op: 'wait', ticks: 30 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'post-duplicate-active',
    config: { ...baseConfig, port: 18907 },
    qyapi: [OK_TOKEN],
    turns: [{ gateBefore: 'turn-1', events: [{ type: 'assistant_text_delta', text: '完成回复' }] }],
    ...noHelpers,
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', 'first'),
      { op: 'wait', gateway: 1 },
      postStep('chat1', 'text', 'second'),
      { op: 'wait', ticks: 30 },
      { op: 'release', gate: 'turn-1' },
      { op: 'wait', helpers: 6 },
      { op: 'wait', qyapi: 2 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'mapper-commands',
    config: { ...baseConfig, port: 18908 },
    qyapi: [OK_TOKEN],
    turns: [
      { events: [{ type: 'assistant_text_delta', text: '回答一' }] },
      { events: [{ type: 'assistant_text_delta', text: '回答二' }] },
    ],
    ...noHelpers,
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', '/new'),
      { op: 'wait', qyapi: 2 },
      postStep('chat1', 'text', '/new foo'),
      { op: 'wait', gateway: 1 },
      { op: 'wait', qyapi: 3 },
      postStep('chat1', 'text', 'plain'),
      { op: 'wait', gateway: 2 },
      { op: 'wait', qyapi: 4 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'post-renders-events',
    config: { ...baseConfig, port: 18909 },
    qyapi: [OK_TOKEN],
    turns: [{ events: [
      { type: 'assistant_text_delta', text: 'a' },
      { type: 'assistant_thinking_delta' },
      { type: 'tool_call_started' },
      { type: 'tool_call_finished', ok: true },
      { type: 'tool_call_finished', ok: false, toolName: 'bash' },
      { type: 'elicitation_request', questions: [{ header: 'H', question: 'Q', options: [{ label: 'A' }] }] },
      { type: 'permission_request', requestId: 'req-9' },
      { type: 'error', message: 'boom' },
    ] }],
    elicitation: { pending: [], capture: '请选择：A', answer: null },
    permission: { pending: [], capture: '确认权限？', answer: null, nextPrompt: null, promptRequestId: null },
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', 'mixed'),
      { op: 'wait', qyapi: 4 },
      { op: 'wait', helpers: 7 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'elicitation-pending',
    config: { ...baseConfig, port: 18910 },
    qyapi: [OK_TOKEN],
    turns: [],
    elicitation: { pending: ['chat1'], capture: '请选择：', answer: '已收到答复。' },
    permission: { pending: [], capture: null, answer: null, nextPrompt: null, promptRequestId: null },
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', 'yes'),
      { op: 'wait', qyapi: 2 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'permission-success',
    config: { ...baseConfig, port: 18911 },
    qyapi: [OK_TOKEN],
    turns: [],
    elicitation: { pending: [], capture: '', answer: null },
    permission: {
      pending: ['chat1'], capture: null,
      answer: { text: '确认收到。', canAdvance: true, retryPrompt: true, answerToken: 7 },
      nextPrompt: '下一个问题：', promptRequestId: 'req-2',
    },
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', '1'),
      { op: 'wait', qyapi: 3 },
      { op: 'wait', helpers: 6 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'permission-confirm-fails',
    config: { ...baseConfig, port: 18912 },
    qyapi: [OK_TOKEN, { status: 500, json: { errcode: 1, errmsg: 'server boom' } }],
    turns: [],
    elicitation: { pending: [], capture: '', answer: null },
    permission: {
      pending: ['chat1'], capture: null,
      answer: { text: '确认。', canAdvance: true, retryPrompt: true, answerToken: 8 },
      nextPrompt: '不应对出现', promptRequestId: null,
    },
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', '1'),
      { op: 'wait', qyapi: 2 },
      { op: 'wait', helpers: 4 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'permission-no-text',
    config: { ...baseConfig, port: 18913 },
    qyapi: [],
    turns: [],
    elicitation: { pending: [], capture: '', answer: null },
    permission: {
      pending: ['chat1'], capture: null,
      answer: { text: '', canAdvance: false, retryPrompt: false, answerToken: 9 },
      nextPrompt: null, promptRequestId: null,
    },
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', '1'),
      { op: 'wait', helpers: 3 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'turn-error-fallback',
    config: { ...baseConfig, port: 18914 },
    qyapi: [OK_TOKEN],
    turns: [{ events: [{ type: 'assistant_text_delta', text: '部分' }], error: 'turn exploded' }],
    ...noHelpers,
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', 'go'),
      { op: 'wait', qyapi: 2 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'token-expiry-refresh',
    config: { ...baseConfig, port: 18915 },
    qyapi: [
      OK_TOKEN,
      { status: 200, json: { errcode: 40014, errmsg: 'invalid credential' } },
      { status: 200, json: { errcode: 0, access_token: 'tok-abc2', expires_in: 7200 } },
    ],
    turns: [
      { events: [{ type: 'assistant_text_delta', text: '一' }] },
      { events: [{ type: 'assistant_text_delta', text: '二' }] },
    ],
    ...noHelpers,
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', 'a'),
      { op: 'wait', qyapi: 2 },
      postStep('chat1', 'text', 'b'),
      { op: 'wait', qyapi: 4 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'stop-closes-listener',
    config: { ...baseConfig, port: 18916 },
    qyapi: [], turns: [], ...noHelpers,
    steps: [
      { op: 'start' },
      { op: 'stop', reason: 'done' },
      { op: 'http', method: 'GET', path: '/cb?x=1' },
    ],
  },
  {
    name: 'restart-no-replay',
    config: { ...baseConfig, port: 18917 },
    qyapi: [OK_TOKEN],
    turns: [
      { events: [{ type: 'assistant_text_delta', text: '一' }] },
      { events: [{ type: 'assistant_text_delta', text: '二' }] },
    ],
    ...noHelpers,
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', 'a'),
      { op: 'wait', qyapi: 2 },
      { op: 'stop', reason: 'first' },
      { op: 'start' },
      postStep('chat1', 'text', 'b'),
      { op: 'wait', qyapi: 3 },
      { op: 'stop', reason: 'second' },
    ],
  },
  {
    name: 'stop-drains-inflight',
    config: { ...baseConfig, port: 18918 },
    qyapi: [OK_TOKEN],
    turns: [{ events: [{ type: 'assistant_text_delta', text: '好' }] }],
    ...noHelpers,
    steps: [
      { op: 'start' },
      { op: 'tcp-open', label: 'slow', request: slowPost.request },
      { op: 'stop-pending', label: 'stop-1', reason: 'drain' },
      { op: 'tcp-chunk', label: 'slow', chunk: slowPost.chunk },
      { op: 'tcp-end', label: 'slow' },
      { op: 'await', label: 'stop-1', reason: 'drain' },
      { op: 'wait', qyapi: 2 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'stop-detached-turn',
    config: { ...baseConfig, port: 18919 },
    qyapi: [OK_TOKEN],
    turns: [{ gateBefore: 'turn-1', events: [{ type: 'assistant_text_delta', text: '迟到的回复' }] }],
    ...noHelpers,
    steps: [
      { op: 'start' },
      postStep('chat1', 'text', 'a'),
      { op: 'wait', gateway: 1 },
      { op: 'stop', reason: 'done' },
      { op: 'release', gate: 'turn-1' },
      { op: 'wait', qyapi: 2 },
    ],
  },
  {
    name: 'stop-idempotent',
    config: { ...baseConfig, port: 18920 },
    qyapi: [], turns: [], ...noHelpers,
    steps: [
      { op: 'start' },
      { op: 'stop', reason: 'first' },
      { op: 'stop', reason: 'second' },
    ],
  },
];

const EXPECTED_CASE_COUNT = 22;
assert.equal(definitions.length, EXPECTED_CASE_COUNT, '#1336 case count');

const output = resolve(process.argv[2]);
await mkdir(output, { recursive: true });
const cases: unknown[] = [];
for (let index = 0; index < definitions.length; index++) {
  qyapiCalls.length = 0;
  // The queue is consumed while the case runs; snapshot the scripted inputs
  // so the recorded oracle keeps them.
  const scripted = definitions[index].qyapi.slice();
  qyapiQueue = definitions[index].qyapi;
  const expected = await runCase(index, definitions[index]);
  cases.push({ ...definitions[index], qyapi: scripted, expected });
  console.log(`case ${definitions[index].name}: qyapi=${expected.qyapi.length} helpers=${expected.helpers.length} gateway=${expected.gateway.length}`);
}
globalThis.fetch = realFetch;

const command = 'PDRS_BASELINE=/absolute/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/wecom-callback/generate.mts src/pilotdeck-gateway/tests/fixtures/wecom-callback';
await writeFile(
  join(output, 'oracle.json'),
  JSON.stringify({ sourceCommit, sourceSha256, command, node: process.version, icu: process.versions.icu, expectedCaseCount: EXPECTED_CASE_COUNT, cases }, null, 2) + '\n',
  { flag: 'wx' },
);
console.log(`PASS #1336 wecom-callback: ${cases.length} real TS scenarios`);
// Channels from cases that never stop keep their listening sockets; the
// recorded oracle is complete, so the process can exit now.
process.exit(0);
