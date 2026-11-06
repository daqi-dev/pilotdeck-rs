// #1329 feishu golden fixture. Runs the real baseline channel
// (FeishuChannel.ts + FeishuSessionMapper.ts + ImLiveReplyController.ts +
// ImChatSessionState.ts + status/agentStatus.ts, transpiled unchanged) behind
// a transcribed /feishu/webhook route (the GatewayServer.ts `handleHttpRequest`
// feishu branch plus the pilotdeckServer.ts `activeFeishu` lifecycle subset)
// against scripted fakes: a fake A7 gateway, no-op IM helpers, a no-op
// attachment store (diff 002 — nothing under ~/.pilotdeck is touched) and a
// loopback Lark Open API reached through a globalThis.fetch URL rewrite. No
// real account, no off-loopback traffic. See README.md for the replay contract.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import * as fs from 'node:fs';
import * as http from 'node:http';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1329 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1329 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, '#1329 PDRS_BASELINE');
assert.ok(process.argv[2], '#1329 output directory');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/adapters/channel/feishu/FeishuChannel.ts',
  'src/adapters/channel/feishu/FeishuSessionMapper.ts',
  'src/adapters/channel/protocol/ImLiveReplyController.ts',
  'src/adapters/channel/protocol/ImChatSessionState.ts',
  'src/status/agentStatus.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');

const APP_ID = 'app-1';
const APP_SECRET = 'sec-1';
const VERIFY_TOKEN = 'vt-1';
const UUID = 'aaaa1111-bbbb-4ccc-8ddd-eeeeeeeeeeee';
const WEBHOOK_BASE_PORT = 18960;
const LARK_PORT = 18990;

// ---- transpiled baseline modules ------------------------------------------

function compileModule(rel: string): (request: (name: string) => any) => any {
  const code = transformSync(fs.readFileSync(join(baseline, rel), 'utf8'), {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: rel,
  }).code;
  const module = { exports: {} as any };
  return request => {
    new Function('require', 'module', 'exports', code)(request, module, module.exports);
    return module.exports;
  };
}
const nodeOnly = (name: string) => {
  if (name.startsWith('node:')) return requireBaseline(name);
  throw new Error(`#1329 dependency module must not import anything: ${name}`);
};
const mapperModule = compileModule(sources[1]);
const liveControllerModule = compileModule(sources[2]);
const chatStateModule = compileModule(sources[3]);
const agentStatusModule = compileModule(sources[4]);

// No case drives an elicitation/permission interaction; the fake only has to
// keep `hasPending` false so inbound text reaches the gateway.
class NoopElicitation {
  hasPending() { return false; }
  capture(_chatId: string, _sessionKey: string, _event: any) { return '请选择'; }
  async answer() { return null; }
  clear() {}
}
class NoopPermission {
  hasPending() { return false; }
  capture(_chatId: string, _sessionKey: string, _event: any) { return null; }
  confirmInitialPrompt() {}
  async answerWithState() { return null; }
  releaseAnswer() {}
  takeNextPrompt() { return null; }
  getPromptRequestId() { return null; }
  confirmNextPrompt() {}
  clearAfterTurn() {}
  clear() {}
}

function makeChannelClass(): any {
  const channelModule = compileModule(sources[0]);
  const agentStatusRequire = (name: string) => {
    if (name === '../../../status/agentStatus.js') return agentStatusModule(nodeOnly);
    throw new Error(`#1329 unexercised controller import ${name}`);
  };
  const channelRequire = (name: string) => {
    if (name === 'node:crypto' || name === 'node:os' || name === 'node:path') return requireBaseline(name);
    // Type-only imports are erased by esbuild; the remaining value imports are
    // either replaced by fakes or satisfied by the transpiled real modules.
    if (name === '../protocol/ChannelCommandRegistry.js') {
      // Diff 003: the system command registry is not ported; the fixture
      // reports every command as unhandled so unknown text still reaches the
      // gateway.
      return { executeChannelCommand: async () => false, resolveCommand: () => undefined };
    }
    if (name === '../protocol/ImAttachmentDelivery.js') {
      return { ImAttachmentDelivery: class { async send() { return true; } }, guessMimeTypeFromName: () => 'application/octet-stream' };
    }
    if (name === '../protocol/ImAttachmentStore.js') {
      // Diff 002: attachment persistence is not ported; the fake must not
      // create anything under ~/.pilotdeck.
      return { ImAttachmentStore: class { constructor(..._args: unknown[]) {} saveFromUrl() { throw new Error('#1329 attachments out of scope'); } } };
    }
    if (name === '../protocol/ImChatSessionState.js') return chatStateModule(nodeOnly);
    if (name === '../protocol/ImCronDelivery.js') return { deliverChatCronResult: async () => false };
    if (name === '../protocol/ImElicitationHelper.js') return { ImElicitationHelper: NoopElicitation };
    if (name === '../protocol/ImPermissionHelper.js') return { ImPermissionHelper: NoopPermission };
    if (name === '../protocol/ImLiveReplyController.js') return liveControllerModule(agentStatusRequire);
    if (name === '../../../status/agentStatus.js') return agentStatusModule(nodeOnly);
    if (name === './FeishuSessionMapper.js') return mapperModule(nodeOnly);
    throw new Error(`#1329 unexercised import ${name}`);
  };
  return channelModule(channelRequire).FeishuChannel;
}

// ---- loopback Lark Open API mock ------------------------------------------

const larkCalls: any[] = [];
let larkQueue: { status: number; json: unknown }[] = [];
const larkServer = http.createServer((req, res) => {
  const chunks: Buffer[] = [];
  req.on('data', (c: Buffer) => chunks.push(c));
  req.on('end', () => {
    const url = new URL(req.url ?? '/', 'http://open.feishu.cn');
    larkCalls.push({
      method: req.method,
      path: url.pathname,
      query: url.search ? url.search.slice(1) : null,
      auth: (req.headers['authorization'] as string | undefined) ?? null,
      contentType: (req.headers['content-type'] as string | undefined) ?? null,
      body: chunks.length ? Buffer.concat(chunks).toString('utf8') : '',
    });
    const next = larkQueue.shift() ?? { status: 200, json: { code: 0 } };
    res.writeHead(next.status, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(next.json));
  });
});
await new Promise<void>(done => larkServer.listen(LARK_PORT, '127.0.0.1', done));
const realFetch = globalThis.fetch;
globalThis.fetch = (async (input: any, init?: any) =>
  realFetch(String(input).replace('https://open.feishu.cn', `http://127.0.0.1:${LARK_PORT}`), init)) as typeof fetch;

// ---- helpers ---------------------------------------------------------------

// Both sides pin every id to scripted constants, so nothing dynamic should
// survive into a cell; the mask is defense in depth and the Rust replay
// applies the same two rules.
function maskDynamic(text: string): string {
  return text
    .replace(/\b\d{13}\b/g, '<now>')
    .replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g, '<uuid>');
}

let diag = () => '';
function until(predicate: () => boolean): Promise<void> {
  const started = Date.now();
  return new Promise((resolve, reject) => {
    const check = () => {
      if (predicate()) return resolve();
      if (Date.now() - started > 5000) return reject(new Error(`#1329 wait timeout ${diag()}`));
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

// ---- feishu webhook envelopes ----------------------------------------------

function v2Event(opts: {
  eventId: string; chatId: string; text: string; messageId?: string;
  headerToken?: string; topLevelToken?: unknown;
}): string {
  const envelope: Record<string, unknown> = {
    header: {
      event_id: opts.eventId,
      event_type: 'im.message.receive_v1',
      ...(opts.headerToken !== undefined ? { token: opts.headerToken } : {}),
    },
    event: {
      message: {
        chat_id: opts.chatId,
        message_type: 'text',
        content: JSON.stringify({ text: opts.text }),
        ...(opts.messageId !== undefined ? { message_id: opts.messageId } : {}),
      },
    },
  };
  if (opts.topLevelToken !== undefined) envelope.token = opts.topLevelToken;
  return JSON.stringify(envelope);
}
function postStep(body: string): any {
  return { op: 'http', method: 'POST', path: '/feishu/webhook', body };
}

// ---- transcribed route (GatewayServer.ts + pilotdeckServer.ts subsets) ----

// `handleHttpRequest` (GatewayServer.ts:78-119) keeps only the branches this
// fixture drives: the `/feishu/webhook` branch (lines 100-107) and the final
// 404 fall-through (lines 117-119). `/health`, `/auth/local-token`, the public
// approval/host bridges, `/api/web/` and static assets are not exercised; the
// Rust replay mounts `native_http::serve_with_body` with no static root, whose
// own fall-through is the same 404.
async function readBody(request: http.IncomingMessage): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const chunk of request) {
    chunks.push(Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk));
  }
  return Buffer.concat(chunks).toString('utf8');
}

function startRouter(port: number, options: {
  feishuWebhook: (request: http.IncomingMessage, response: http.ServerResponse, body: string) => Promise<boolean>;
}): Promise<http.Server> {
  const server = http.createServer((request, response) => {
    void (async () => {
      const url = new URL(request.url ?? '/', 'http://127.0.0.1');
      if (url.pathname === '/feishu/webhook' && options.feishuWebhook) {
        const body = await readBody(request);
        const handled = await options.feishuWebhook(request, response, body);
        if (handled) {
          return;
        }
      }
      response.writeHead(404, { 'content-type': 'text/plain; charset=utf-8' });
      response.end('not found');
    })().catch(() => response.destroy());
  });
  return new Promise(done => server.listen(port, '127.0.0.1', () => done(server)));
}

// ---- case runner -----------------------------------------------------------

type TurnScript = { gateBefore?: string; events: unknown[]; error?: string };
type Case = { name: string; port: number; turns: TurnScript[]; steps: any[] };

async function runCase(caseDef: Case): Promise<any> {
  diag = () => `(case ${caseDef.name} lark=${JSON.stringify(larkCalls)} logs=${JSON.stringify(logs)})`;
  const logs: unknown[] = [];
  const gateway: unknown[] = [];
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
      gateway.push({
        sessionKey: maskDynamic(input.sessionKey),
        channelKey: input.channelKey,
        message: input.message,
      });
      const turn = caseDef.turns[turnIndex++] ?? { events: [] };
      return (async function* () {
        if (turn.gateBefore) await gate(turn.gateBefore).promise;
        for (const event of turn.events) yield event;
        if (turn.error) throw new Error(turn.error);
      })();
    },
    // Diff 004: `/new` aborts the previous run in TS. The A7 port has no
    // abort, so the call is accepted but never recorded.
    abortTurn: async () => undefined,
  };
  const FeishuChannel = makeChannelClass();
  // Deterministic session uuids: the mapper seam the Rust replay pins with
  // `FeishuChannel::with_seams`.
  const mapper = new (mapperModule(nodeOnly).FeishuSessionMapper)(undefined, () => UUID);
  // Diff 005: the live-reply controller's throttled intermediate sends and
  // edits are not ported. `liveReplyOptions` is the baseline's own test seam
  // ("used in tests"); raising the thresholds and disabling the activity
  // delay keeps the golden to the semantics Rust implements — chunking at
  // `liveTextLimit()` and one final `post` per chunk — so the oracle compares
  // cell by cell without masking.
  const channel = new FeishuChannel({
    appId: APP_ID,
    appSecret: APP_SECRET,
    verifyToken: VERIFY_TOKEN,
    connectionMode: 'webhook',
    mapper,
    liveReplyOptions: {
      throttleMs: 3_600_000,
      bufferThreshold: 1_000_000,
      initialThrottleMs: 3_600_000,
      initialBufferThreshold: 1_000_000,
      activityDelayMs: -1,
      turnTimeoutMs: 0,
    },
  });
  // pilotdeckServer.ts:89 — `let activeFeishu = options.feishu`.
  let activeFeishu: any = channel;
  const feishuWebhook = (request: http.IncomingMessage, response: http.ServerResponse, body: string) =>
    // pilotdeckServer.ts:271 — `activeFeishu?.handleWebhook(...) ?? false`.
    activeFeishu?.handleWebhook(request, response, body) ?? false;
  const router = await startRouter(caseDef.port, { feishuWebhook });
  let handle: any = null;
  const logger = {
    info: (m: string) => logs.push(['info', maskDynamic(m)]),
    warn: (m: string) => logs.push(['warn', maskDynamic(m)]),
    error: (m: string) => logs.push(['error', maskDynamic(m)]),
  };

  for (const step of caseDef.steps) {
    if (step.op === 'start') {
      handle = await channel.start({ gateway: gatewayFake, logger });
      // pilotdeckServer.ts:113 — `activeFeishu = ch` after start resolves.
      activeFeishu = channel;
    } else if (step.op === 'http') {
      httpResults.push(await httpRequest(caseDef.port, step.method, step.path, step.body));
    } else if (step.op === 'stop') {
      // pilotdeckServer.ts:121-160 — `stopRunningChannel` awaits the handle's
      // stop, then clears the active channel for every reason but a hot
      // reload, whose replacement is already publishing.
      await handle.stop(step.reason);
      if (step.reason !== 'hot-reload' && activeFeishu === channel) {
        activeFeishu = undefined;
      }
    } else if (step.op === 'release') {
      gate(step.gate).release();
    } else if (step.op === 'wait') {
      if (step.lark !== undefined) await until(() => larkCalls.length >= step.lark);
      else if (step.gateway !== undefined) await until(() => gateway.length >= step.gateway);
      else if (step.logs !== undefined) await until(() => logs.length >= step.logs);
      else if (step.ticks !== undefined) await ticks(step.ticks);
      else throw new Error('#1329 wait without condition');
    } else {
      throw new Error(`#1329 unknown op ${step.op}`);
    }
  }
  await new Promise<void>(done => router.close(() => done()));
  // Return the live recorders; the driver snapshots them after quiescence.
  return { http: httpResults, lark: larkCalls, gateway, logs };
}

// ---- scripted Lark responses ----------------------------------------------

const TOKEN_OK = { status: 200, json: { code: 0, tenant_access_token: 't-1', expire: 7200 } };
const REACTION_OK = { status: 200, json: { code: 0, data: { reaction_id: 're-1' } } };
const POST_OK = { status: 200, json: { code: 0, data: { message_id: 'om-1' } } };
const POST_NO_ID = { status: 200, json: { code: 0, msg: 'sent' } };
const TEXT_OK = { status: 200, json: { code: 0, data: { message_id: 'om-2' } } };
const DELETE_OK = { status: 200, json: { code: 0 } };

function scriptedResponses(name: string): { status: number; json: unknown }[] {
  switch (name) {
    case 'duplicate-event-dedup':
    case 'text-turn-outbound-sequence':
      return [TOKEN_OK, REACTION_OK, POST_OK];
    case 'new-command-session-key':
      return [TOKEN_OK, TEXT_OK, REACTION_OK, POST_OK];
    case 'second-message-serialized':
      // The mock pops per request, so the mid-sequence DELETE needs its own
      // scripted slot to keep the turn-2 responses aligned.
      return [TOKEN_OK, REACTION_OK, POST_OK, DELETE_OK, REACTION_OK, POST_OK];
    case 'long-text-chunked':
      return [TOKEN_OK, REACTION_OK, POST_OK, POST_OK];
    case 'post-without-message-id-falls-back-to-text':
      return [TOKEN_OK, REACTION_OK, POST_NO_ID, TEXT_OK];
    default:
      // Cases with no outbound traffic; any unexpected call would run the
      // queue dry and be recorded against the default response.
      return [];
  }
}

// ---- case definitions ------------------------------------------------------

const HELLO_TURN: TurnScript[] = [{ events: [
  { type: 'turn_started', runId: 'run-1' },
  { type: 'assistant_text_delta', text: '你好，处理完成。' },
] }];
const LONG_TEXT = 'x'.repeat(5000);

const definitions: Case[] = [
  {
    name: 'url-verification',
    port: WEBHOOK_BASE_PORT + 0,
    turns: [],
    steps: [
      { op: 'start' },
      // With a matching token both sides stay silent; a mismatched or absent
      // token still answers the challenge but also logs the warn.
      postStep('{"type":"url_verification","challenge":"c-1","token":"vt-1"}'),
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'verify-token-mismatch',
    port: WEBHOOK_BASE_PORT + 1,
    turns: [],
    steps: [
      { op: 'start' },
      postStep(v2Event({ eventId: 'ev-bad', chatId: 'c1', text: 'hi', messageId: 'om-in-2', headerToken: 'wrong' })),
      { op: 'wait', ticks: 20 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'numeric-top-level-token-shadows-header',
    port: WEBHOOK_BASE_PORT + 2,
    turns: [],
    steps: [
      { op: 'start' },
      postStep(v2Event({ eventId: 'ev-num', chatId: 'c1', text: 'hi', messageId: 'om-in-3', headerToken: VERIFY_TOKEN, topLevelToken: 12345 })),
      { op: 'wait', ticks: 20 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'non-json-body',
    port: WEBHOOK_BASE_PORT + 3,
    turns: [],
    steps: [
      { op: 'start' },
      postStep('not-json{{{'),
      { op: 'wait', ticks: 20 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'duplicate-event-dedup',
    port: WEBHOOK_BASE_PORT + 4,
    turns: HELLO_TURN,
    steps: [
      { op: 'start' },
      postStep(v2Event({ eventId: 'ev-dup', chatId: 'c1', text: 'hi', messageId: 'om-in-5', headerToken: VERIFY_TOKEN })),
      postStep(v2Event({ eventId: 'ev-dup', chatId: 'c1', text: 'hi', messageId: 'om-in-5', headerToken: VERIFY_TOKEN })),
      { op: 'wait', lark: 4 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'text-turn-outbound-sequence',
    port: WEBHOOK_BASE_PORT + 5,
    turns: HELLO_TURN,
    steps: [
      { op: 'start' },
      postStep(v2Event({ eventId: 'ev-turn', chatId: 'c1', text: '做个总结', messageId: 'om-in-6', headerToken: VERIFY_TOKEN })),
      { op: 'wait', lark: 4 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'new-command-session-key',
    port: WEBHOOK_BASE_PORT + 6,
    turns: [{ events: [{ type: 'assistant_text_delta', text: '好的。' }] }],
    steps: [
      { op: 'start' },
      postStep(v2Event({ eventId: 'ev-new', chatId: 'c1', text: '/new', messageId: 'om-in-7', headerToken: VERIFY_TOKEN })),
      { op: 'wait', lark: 2 },
      postStep(v2Event({ eventId: 'ev-follow', chatId: 'c1', text: '继续', messageId: 'om-in-8', headerToken: VERIFY_TOKEN })),
      { op: 'wait', lark: 5 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    // While a turn is running, a second webhook message for the same chat
    // waits in the per-chat inbound batch (the single-drainer invariant); it
    // only reaches the gateway after the first turn finished, so the two
    // turns' Lark calls serialize.
    name: 'second-message-serialized',
    port: WEBHOOK_BASE_PORT + 7,
    turns: [
      { gateBefore: 'turn-1', events: [{ type: 'assistant_text_delta', text: '第一条回复。' }] },
      { events: [{ type: 'assistant_text_delta', text: '第二条回复。' }] },
    ],
    steps: [
      { op: 'start' },
      postStep(v2Event({ eventId: 'ev-q1', chatId: 'c1', text: '第一句', messageId: 'om-in-9', headerToken: VERIFY_TOKEN })),
      { op: 'wait', gateway: 1 },
      // handleWebhook enqueues synchronously before the 200 lands, so once
      // this step resolves the second message is parked in the batch.
      postStep(v2Event({ eventId: 'ev-q2', chatId: 'c1', text: '第二句', messageId: 'om-in-10', headerToken: VERIFY_TOKEN })),
      { op: 'release', gate: 'turn-1' },
      { op: 'wait', lark: 7 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'long-text-chunked',
    port: WEBHOOK_BASE_PORT + 8,
    turns: [{ events: [{ type: 'assistant_text_delta', text: LONG_TEXT }] }],
    steps: [
      { op: 'start' },
      postStep(v2Event({ eventId: 'ev-long', chatId: 'c1', text: '写五千字', messageId: 'om-in-11', headerToken: VERIFY_TOKEN })),
      { op: 'wait', lark: 5 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'post-without-message-id-falls-back-to-text',
    port: WEBHOOK_BASE_PORT + 9,
    turns: [{ events: [{ type: 'assistant_text_delta', text: '回落文本。' }] }],
    steps: [
      { op: 'start' },
      postStep(v2Event({ eventId: 'ev-fb', chatId: 'c1', text: '再答一次', messageId: 'om-in-12', headerToken: VERIFY_TOKEN })),
      { op: 'wait', lark: 5 },
      { op: 'stop', reason: 'done' },
    ],
  },
  {
    name: 'stopped-route-falls-through-404',
    port: WEBHOOK_BASE_PORT + 10,
    turns: [],
    steps: [
      { op: 'start' },
      { op: 'stop', reason: 'done' },
      postStep(v2Event({ eventId: 'ev-after-stop', chatId: 'c1', text: 'hi', headerToken: VERIFY_TOKEN })),
    ],
  },
  {
    name: 'not-started-503',
    port: WEBHOOK_BASE_PORT + 11,
    turns: [],
    steps: [
      postStep(v2Event({ eventId: 'ev-nostart', chatId: 'c1', text: 'hi', headerToken: VERIFY_TOKEN })),
    ],
  },
  {
    name: 'hot-reload-keeps-route',
    port: WEBHOOK_BASE_PORT + 12,
    turns: [],
    steps: [
      { op: 'start' },
      { op: 'stop', reason: 'hot-reload' },
      postStep('{"type":"url_verification","challenge":"c-9","token":"vt-1"}'),
    ],
  },
];

const EXPECTED_CASE_COUNT = 13;
assert.equal(definitions.length, EXPECTED_CASE_COUNT, '#1329 case count');

const output = resolve(process.argv[2]);
await mkdir(output, { recursive: true });
const cases: unknown[] = [];
for (const caseDef of definitions) {
  larkCalls.length = 0;
  const scripted = scriptedResponses(caseDef.name);
  // `scripted` goes into the oracle, so the runner needs its own copy — the
  // mock drains the queue with `shift()`.
  larkQueue = [...scripted];
  const recorded = await runCase(caseDef);
  // Every scripted response must have been served: a dry queue means the
  // case consumed the mock default and its cells are garbage.
  assert.equal(larkQueue.length, 0, `#1329 ${caseDef.name} left ${larkQueue.length} scripted responses unserved`);
  // Quiescence: late retries from this case must not leak into the next
  // case's cells.
  await ticks(50);
  const settled = larkCalls.length;
  await ticks(50);
  assert.equal(larkCalls.length, settled, `#1329 ${caseDef.name} lark calls still arriving after the case ended`);
  const expected = {
    http: recorded.http,
    lark: (recorded.lark as any[]).map(c => ({ ...c, body: maskDynamic(c.body) })),
    gateway: recorded.gateway,
    logs: recorded.logs,
  };
  // The scripted responses are part of the oracle: the Rust replay arms its
  // loopback Lark mock with exactly this queue (pop per request in arrival
  // order, default `{code:0}` when dry), like the wecom `qyapi` field.
  cases.push({ name: caseDef.name, port: caseDef.port, turns: caseDef.turns, steps: caseDef.steps, scripted, expected });
  console.log(`case ${caseDef.name}: lark=${expected.lark.length} gateway=${expected.gateway.length} logs=${expected.logs.length}`);
}
globalThis.fetch = realFetch;
await new Promise<void>(done => larkServer.close(() => done()));

const command = 'PDRS_BASELINE=/absolute/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/channel-feishu/generate.mts src/pilotdeck-gateway/tests/fixtures/channel-feishu';
await writeFile(
  join(output, 'oracle.json'),
  JSON.stringify({ sourceCommit, sourceSha256, command, node: process.version, icu: process.versions.icu, expectedCaseCount: EXPECTED_CASE_COUNT, cases }, null, 2) + '\n',
  { flag: 'wx' },
);
console.log(`PASS #1329 feishu: ${cases.length} real TS scenarios`);
process.exit(0);
