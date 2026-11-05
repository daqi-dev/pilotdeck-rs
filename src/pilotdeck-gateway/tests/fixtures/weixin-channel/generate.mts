// #1328 weixin channel golden generator. The real baseline stack —
// WeixinChannel + WeixinSessionMapper + ChannelStatePersistence +
// pilotdeckServer + the Im* protocol family + ChannelRuntimeStatus — runs
// against a scripted loopback ilink mock. No real accounts, hosts, or network.
// The weixin-ilink package itself is the real one (imported by file URL,
// because its `exports` map has no require condition); only its servers are
// faked. The ChannelAdapterBundle weixin subset and the pilotdeck.ts
// `prepareWeixinLogin` callback are transcribed verbatim from the baseline.
import assert from 'node:assert/strict';
import http from 'node:http';
import net from 'node:net';
import { createRequire } from 'node:module';
import { mkdtemp, mkdir, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve, dirname } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1328 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1328 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, '#1328 PDRS_BASELINE');
assert.ok(process.argv[2], '#1328 output directory');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/adapters/channel/weixin/WeixinChannel.ts',
  'src/adapters/channel/weixin/WeixinSessionMapper.ts',
  'src/adapters/channel/protocol/ChannelStatePersistence.ts',
  'src/adapters/channel/protocol/ImChatSessionState.ts',
  'src/adapters/channel/protocol/ImLiveReplyController.ts',
  'src/adapters/channel/protocol/ImElicitationHelper.ts',
  'src/adapters/channel/protocol/ImPermissionHelper.ts',
  'src/adapters/channel/protocol/ImAttachmentStore.ts',
  'src/adapters/channel/protocol/ImAttachmentDelivery.ts',
  'src/adapters/channel/protocol/ImCronDelivery.ts',
  'src/adapters/channel/protocol/ChannelCommandRegistry.ts',
  'src/adapters/channel/protocol/ChannelRuntimeStatus.ts',
  'src/status/agentStatus.ts',
  'src/session/search/ChatSearchArgs.ts',
  'src/session/search/formatChatHistorySearch.ts',
  'src/cli/pilotdeckServer.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const baselineRequire = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(baselineRequire.resolve('tsx'))('esbuild');
const weixinIlink = await import(
  pathToFileURL(join(baseline, 'node_modules/weixin-ilink/dist/index.js')).href
);

const compiled = new Map<string, string>();
for (const rel of sources) {
  const abs = join(baseline, rel);
  compiled.set(abs, transformSync(await readFile(abs, 'utf8'), { loader: 'ts', format: 'cjs', target: 'es2022' }).code);
}
const moduleCache = new Map<string, { exports: any }>();
function loadModule(abs: string): any {
  if (!moduleCache.has(abs)) {
    const module = { exports: {} as any };
    moduleCache.set(abs, module);
    new Function('require', 'module', 'exports', compiled.get(abs)!)(
      (spec: string) => requireFrom(abs)(spec), module, module.exports,
    );
  }
  return moduleCache.get(abs)!.exports;
}
function requireFrom(fromAbs: string) {
  return (spec: string) => {
    if (spec === 'weixin-ilink') return weixinIlink;
    if (!spec.startsWith('.')) return baselineRequire(spec);
    const resolved = resolve(dirname(fromAbs), spec);
    const abs = resolved.endsWith('.ts') ? resolved : resolved.replace(/\.js$/, '.ts');
    if (!compiled.has(abs)) throw new Error(`#1328 unexpected import ${spec}`);
    return loadModule(abs);
  };
}
const serverSource = join(baseline, 'src/cli/pilotdeckServer.ts');

// --- Golden masking ---------------------------------------------------------
// Identical rules on both sides: client_id by key, X-WECHAT-UIN by header,
// random UUIDs, wall-clock ISO stamps, the loopback port, the scenario temp
// dir (it reaches the -14 console lines), and `formatWeixinError` stack tails
// (`; at=...` carries the generating host's file paths).
const UUID_RE = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g;
const ISO_RE = /\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z/g;
function makeMask(scenarioDir: string) {
  const maskString = (text: string) => text
    .replaceAll(scenarioDir, '<tmp>')
    .replace(UUID_RE, '<uuid>')
    .replace(ISO_RE, '<now>')
    .replace(/; at=.*/, '')
    .replace(/http:\/\/127\.0\.0\.1:\d+/g, 'http://127.0.0.1:<port>');
  return function mask(value: any): any {
    if (typeof value === 'string') return maskString(value);
    if (Array.isArray(value)) return value.map(mask);
    if (value && typeof value === 'object') {
      const out: Record<string, any> = {};
      for (const [key, item] of Object.entries(value)) {
        out[key] = key === 'client_id' && typeof item === 'string' ? '<client-id>' : mask(item);
      }
      return out;
    }
    return value;
  };
}
const HEADER_KEYS = ['content-type', 'authorization', 'authorizationtype', 'ilink-app-clientversion', 'x-wechat-uin'];

// --- Scripted loopback ilink server ----------------------------------------
// getupdates long-polls are HELD until the scenario pushes the next scripted
// item, and are recorded when the server answers — so request order in the
// oracle follows the scenario's gated steps, not connection races. A held
// poll parked when a stop begins is answered with the AbortError shape.
class FakeIlink {
  requests: any[] = [];
  private scripts = new Map<string, any[]>();
  private held: { res: http.ServerResponse; record: any }[] = [];
  private sockets = new Set<net.Socket>();
  private lastQrStatus: any;
  private server!: http.Server;
  constructor(private mask: (value: any) => any) {}
  listen(): Promise<number> {
    return new Promise((done) => {
      this.server = http.createServer((req, res) => void this.handle(req, res));
      this.server.on('connection', (socket) => {
        this.sockets.add(socket);
        socket.on('close', () => this.sockets.delete(socket));
      });
      this.server.listen(0, '127.0.0.1', () => done((this.server.address() as net.AddressInfo).port));
    });
  }
  pushScript(endpoint: string, items: any[]) {
    const queue = this.scripts.get(endpoint) ?? [];
    queue.push(...items);
    this.scripts.set(endpoint, queue);
    this.pump();
  }
  countPath(path: string): number {
    // Synthetic markers (client-factory injections) carry no path.
    return this.requests.filter((request) => typeof request.path === 'string'
      && (request.path === path || request.path.startsWith(`${path}?`))).length;
  }
  releaseHeld() {
    for (const pending of this.held.splice(0)) {
      this.requests.push(pending.record);
      const body = pending.record.body ?? {};
      pending.res.writeHead(200, { 'content-type': 'application/json' });
      pending.res.end(JSON.stringify({ ret: 0, msgs: [], get_updates_buf: body.get_updates_buf ?? '' }));
    }
  }
  destroy() {
    this.releaseHeld();
    for (const socket of this.sockets) socket.destroy();
    this.server.close();
  }
  private pump() {
    const queue = this.scripts.get('/ilink/bot/getupdates');
    while (this.held.length > 0 && queue && queue.length > 0) {
      const item = queue.shift()!;
      const pending = this.held.shift()!;
      this.requests.push(pending.record);
      this.respond(pending.res, '/ilink/bot/getupdates', item);
    }
  }
  private async handle(req: http.IncomingMessage, res: http.ServerResponse) {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(Buffer.from(chunk));
    const body = chunks.length > 0 ? Buffer.concat(chunks).toString('utf8') : null;
    const url = new URL(req.url ?? '/', 'http://127.0.0.1');
    const path = `${url.pathname}${url.search}`;
    const record = {
      method: req.method,
      path,
      headers: this.pickHeaders(req.headers),
      body: this.parseBody(body),
    };
    if (req.method === 'POST' && url.pathname === '/ilink/bot/getupdates') {
      this.held.push({ res, record });
      this.pump();
      return;
    }
    this.requests.push(record);
    const queue = this.scripts.get(url.pathname);
    const item = queue && queue.length > 0 ? queue.shift()! : undefined;
    this.respond(res, url.pathname, item);
  }
  private pickHeaders(headers: http.IncomingHttpHeaders) {
    const out: Record<string, any> = {};
    for (const key of HEADER_KEYS) {
      if (headers[key] === undefined) continue;
      out[key] = key === 'x-wechat-uin' ? '<uin>' : this.mask(headers[key]);
    }
    return out;
  }
  private parseBody(body: string | null) {
    if (body === null) return null;
    try {
      return this.mask(JSON.parse(body));
    } catch {
      return { raw: this.mask(body) };
    }
  }
  private respond(res: http.ServerResponse, pathname: string, item: any) {
    if (item && typeof item.status === 'number') {
      res.writeHead(item.status, { 'content-type': 'text/plain' });
      res.end(item.text ?? '');
      return;
    }
    const json = item ?? this.defaultFor(pathname);
    if (pathname === '/ilink/bot/get_qrcode_status') this.lastQrStatus = json;
    res.writeHead(200, { 'content-type': 'application/json' });
    res.end(JSON.stringify(json));
  }
  private defaultFor(pathname: string) {
    if (pathname === '/ilink/bot/getconfig') return { ret: 0, typing_ticket: 'tt-1' };
    if (pathname === '/ilink/bot/get_bot_qrcode') return { qrcode: 'qr-1', qrcode_img_content: 'https://mock.example/qr-1' };
    if (pathname === '/ilink/bot/get_qrcode_status') return this.lastQrStatus ?? { status: 'wait' };
    return { ret: 0 };
  }
}

// --- Scenario definitions ---------------------------------------------------
const CONFIG_OFF = {};
const CONFIG_ON = { adapters: { weixin: { enabled: true } } };
const CONFIG_DISABLE = { adapters: { weixin: { enabled: false } } };
function message(from: string, text: string, context: string | undefined, id: string) {
  return {
    message_type: 1,
    message_id: id,
    from_user_id: from,
    ...(context ? { context_token: context } : {}),
    item_list: [{ type: 1, text_item: { text } }],
  };
}
const update = (cursor: string, msgs: any[]) => ({ ret: 0, get_updates_buf: cursor, msgs });
const typingWait = (getconfig: number, sendtyping: number) => ({
  waitRequests: [
    { path: '/ilink/bot/getconfig', count: getconfig },
    { path: '/ilink/bot/sendtyping', count: sendtyping },
  ],
});
const delta = (text: string) => ({ event: { type: 'assistant_text_delta', text } });
const started = (runId: string) => ({ event: { type: 'turn_started', runId } });

const definitions: any[] = [];
function scenario(name: string, steps: any[], options: any = {}) {
  definitions.push({ name, steps, ...options });
}

// c1: the unsupported guard before any server, the CLI success shape, a full
// QR login, and one turn.
scenario('prepare-shapes-qr-login-and-reply', [
  { op: 'prepare' },
  { op: 'server', config: CONFIG_OFF },
  { op: 'prepare' },
  {
    op: 'script', endpoint: '/ilink/bot/get_qrcode_status',
    items: [{ status: 'wait' }, { status: 'scaned' }, { status: 'confirmed', ilink_bot_id: 'acct-1', bot_token: 'tok-1' }],
  },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-2', [message('user-1', 'hello', 'ctx-1', 'm-1')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 1 },
  { op: 'wait-submits', count: 1 },
  { op: 'close' },
], { turns: [{ events: [typingWait(1, 1), started('run-1'), delta('你好'), delta('！')] }] });

// c2: startup from saved credentials restores the persisted cursor into the
// first getupdates body.
scenario('saved-credentials-startup-and-cursor', [
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-9', accountId: 'acct-9', cursor: 'cur-9' } },
  { op: 'server', config: CONFIG_ON },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-10', [message('user-2', '你好', 'ctx-9', 'm-9')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 1 },
  { op: 'wait-submits', count: 1 },
  { op: 'close' },
], { turns: [{ events: [typingWait(1, 1), started('run-9'), delta('在的')] }] });

// c3: /new aborts the in-flight turn (no reply from it — generation bumped),
// rotates the mapper, and a follow-up queued message runs on the new key.
scenario('new-session-rotation', [
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-3', accountId: 'acct-3', cursor: 'cur-0' } },
  { op: 'server', config: CONFIG_ON },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-1', [message('user-3', 'start', 'ctx-1', 'm-1')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendtyping', count: 1 },
  // Gate the /new arrival on the held turn having consumed turn_started, so
  // the abort payload deterministically carries runId.
  { op: 'wait-held', turn: 0 },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-2', [message('user-3', '/new', 'ctx-2', 'm-2')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 1 },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-3', [message('user-3', 'after', 'ctx-3', 'm-3')])] },
  { op: 'release-turn', turn: 0, events: [delta('ignored')] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 2 },
  { op: 'wait-submits', count: 2 },
  { op: 'close' },
], { turns: [
  { events: [typingWait(1, 1), started('run-1')], hold: true },
  { events: [typingWait(2, 2), started('run-2'), delta('second reply')] },
] });

// c4: a 4500-code-point reply is chunked at the weixin maxLength 2000.
scenario('chunked-reply', [
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-4', accountId: 'acct-4', cursor: 'cur-0' } },
  { op: 'server', config: CONFIG_ON },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-1', [message('user-4', 'long', 'ctx-4', 'm-4')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 3 },
  { op: 'wait-submits', count: 1 },
  { op: 'close' },
], { turns: [{ events: [typingWait(1, 1), started('run-4'), delta('A'.repeat(1500)), delta('B'.repeat(1500)), delta('C'.repeat(1500))] }] });

// c5: errcode -14 reports expired, notifies the active chat, breaks the loop;
// the in-flight turn still completes and replies.
scenario('session-expired', [
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-5', accountId: 'acct-5', cursor: 'cur-0' } },
  { op: 'server', config: CONFIG_ON },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-1', [message('user-5', 'hi', 'ctx-5', 'm-5')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendtyping', count: 1 },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [{ errcode: -14 }] },
  { op: 'wait-reports', state: 'expired' },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 1 },
  { op: 'release-turn', turn: 0, events: [delta('late')] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 2 },
  { op: 'close' },
], { turns: [{ events: [typingWait(1, 1), started('run-5')], hold: true }] });

// c6: a real HTTP 500 hits the poll exception branch (the error text pins the
// TS `${endpoint} ${res.status}: ${text}` shape), then ret!=0 retries, then
// recovery. Both retries sleep the real 3s.
scenario('poll-500-and-ret-retry', [
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-6', accountId: 'acct-6', cursor: 'cur-0' } },
  { op: 'server', config: CONFIG_ON },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [{ status: 500, text: 'server exploded' }] },
  { op: 'wait-log', text: 'weixin: poll error #1: Error: ilink/bot/getupdates 500: server exploded' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [{ ret: 1, errmsg: 'busy' }] },
  { op: 'wait-log', text: 'weixin: poll ret=1 errmsg=busy' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-6', [message('user-6', 'ping', 'ctx-6', 'm-6')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 1 },
  { op: 'wait-submits', count: 1 },
  { op: 'close' },
], { turns: [{ events: [typingWait(1, 1), started('run-6'), delta('pong')] }] });

// c7: a synthetic recoverable transport failure (identical wrapper on both
// sides) rebuilds the client and keeps the cursor; no server contact on the
// failed poll, so the marker records in request order.
scenario('recoverable-poll-rebuilds-client', [
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-7', accountId: 'acct-7', cursor: 'cur-7' } },
  { op: 'server', config: CONFIG_ON },
  { op: 'wait-reports', state: 'connected' },
  { op: 'wait-log', text: 'weixin: rebuilt iLink client after recoverable poll error' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-8', [message('user-7', 'hey', 'ctx-7', 'm-7')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 1 },
  { op: 'wait-submits', count: 1 },
  { op: 'close' },
], { turns: [{ events: [typingWait(1, 1), started('run-7'), delta('ho')] }], syntheticPollFailures: 1 });

// c8: repeat prepare hot-reloads (stop:hot-reload) and restores the mapper
// state persisted through the debounce flush.
scenario('repeat-prepare-replaces-with-restored-state', [
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-8', accountId: 'acct-8', cursor: 'cur-0' } },
  { op: 'server', config: CONFIG_ON },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-1', [message('user-8', '/new', 'ctx-8', 'm-8')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 1 },
  { op: 'flush' },
  { op: 'prepare' },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-2', [message('user-8', 'after', 'ctx-8b', 'm-8b')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 2 },
  { op: 'wait-submits', count: 1 },
  { op: 'close' },
], { turns: [{ events: [typingWait(1, 1), started('run-8'), delta('restored')] }] });

// c9: a confirmed QR response missing bot_token fails the login, reports
// failed, and the 2s credentials watcher recovers into a connected poll.
scenario('qr-login-failed-recovery', [
  { op: 'server', config: CONFIG_ON },
  { op: 'script', endpoint: '/ilink/bot/get_qrcode_status', items: [{ status: 'confirmed', ilink_bot_id: 'acct-9' }] },
  { op: 'wait-reports', state: 'failed' },
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-9b', accountId: 'acct-9b' } },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-9', [message('user-9', 'back', 'ctx-9', 'm-9')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 1 },
  { op: 'wait-submits', count: 1 },
  { op: 'close' },
], { turns: [{ events: [typingWait(1, 1), started('run-9'), delta('recovered')] }] });

// c10: config-disable stops with config-disabled; close then stops nothing,
// runs the host close, and memoizes.
scenario('disable-then-close', [
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-10', accountId: 'acct-10', cursor: 'cur-0' } },
  { op: 'server', config: CONFIG_ON },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-1', [message('user-10', 'hey', 'ctx-10', 'm-10')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 1 },
  { op: 'wait-submits', count: 1 },
  { op: 'reload', config: CONFIG_DISABLE },
  { op: 'wait-reports', state: 'stopped' },
  { op: 'close' },
  { op: 'close' },
], { turns: [{ events: [typingWait(1, 1), started('run-10'), delta('yo')] }] });

// c11: a reply whose send fails with HTTP 500 queues (queueOnFailure); the
// next poll flushes it before dispatching the follow-up turn, and both the
// retried queue entry and the follow-up reply reach sendmessage.
scenario('pending-reply-queue', [
  { op: 'credentials', json: { baseUrl: '$BASE', botToken: 'tok-11', accountId: 'acct-11', cursor: 'cur-0' } },
  { op: 'server', config: CONFIG_ON },
  { op: 'wait-reports', state: 'connected' },
  { op: 'script', endpoint: '/ilink/bot/sendmessage', items: [{ status: 500, text: 'send failed' }] },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-1', [message('user-11', 'first', 'ctx-11', 'm-11')])] },
  { op: 'wait-log', text: 'weixin: sendText failed: Error: ilink/bot/sendmessage 500: send failed' },
  { op: 'script', endpoint: '/ilink/bot/getupdates', items: [update('cur-2', [message('user-11', 'second', 'ctx-11b', 'm-12')])] },
  { op: 'wait-requests', path: '/ilink/bot/sendmessage', count: 3 },
  { op: 'wait-submits', count: 2 },
  { op: 'close' },
], { turns: [
  { events: [typingWait(1, 1), started('run-11'), delta('queued reply')] },
  { events: [typingWait(2, 2), started('run-12'), delta('second reply')] },
] });

const EXPECTED_CASE_COUNT = 11;
assert.equal(definitions.length, EXPECTED_CASE_COUNT, '#1328 weixin generator case count');

// --- Harness ----------------------------------------------------------------
async function run(definition: any) {
  const scenarioDir = await mkdtemp(join(tmpdir(), 'weixin-golden-'));
  const pilotHome = join(scenarioDir, 'pilot-home');
  const credentialsPath = join(scenarioDir, 'weixin-credentials.json');
  await mkdir(pilotHome, { recursive: true });
  const mask = makeMask(scenarioDir);
  const fake = new FakeIlink(mask);
  const port = await fake.listen();
  const baseUrl = `http://127.0.0.1:${port}`;

  const reports: any[] = [], logs: any[] = [], submits: any[] = [], aborts: any[] = [], results: any[] = [];
  let hostCloses = 0;
  let syntheticPollFailures = definition.syntheticPollFailures ?? 0;

  // pilotdeckServer is re-evaluated per scenario (its Maps are per-server
  // state). Only its host/status boundaries are mocks — the runtime-status
  // reporter is the real compiled module wrapped to record, as #1327 did.
  const runtimeStatus = requireFrom(serverSource)('../adapters/channel/protocol/ChannelRuntimeStatus.js');
  const pilotdeckModule = { exports: {} as any };
  new Function('require', 'module', 'exports', compiled.get(serverSource)!)(
    (spec: string) => {
      if (spec === '../gateway/index.js') {
        return { startGatewayServer: async () => ({ close: async () => { hostCloses++; } }) };
      }
      if (spec === '../adapters/index.js' || spec === '../pilot/index.js' || spec === '../session/index.js') return {};
      if (spec === '../adapters/channel/protocol/ChannelRuntimeStatus.js') {
        return {
          createChannelRuntimeStatusReporter: (home: string) => {
            const real = runtimeStatus.createChannelRuntimeStatusReporter(home);
            return (key: string, update: any) => {
              reports.push(mask({ channelKey: key, ...update }));
              real(key, update);
            };
          },
        };
      }
      return requireFrom(serverSource)(spec);
    },
    pilotdeckModule, pilotdeckModule.exports,
  );
  const startPilotDeckServer = pilotdeckModule.exports.startPilotDeckServer;
  const { WeixinChannel, WeixinSessionMapper } = loadModule(join(baseline, 'src/adapters/channel/weixin/WeixinChannel.ts'));
  const { ChannelStatePersistence } = loadModule(join(baseline, 'src/adapters/channel/protocol/ChannelStatePersistence.ts'));

  const turns = definition.turns ?? [];
  const heldTurns = new Map<number, (events: any[]) => void>();
  let turnIndex = 0;
  class HarnessGateway {
    // `InProcessGatewayOptions.prepareWeixinLogin`.
    options: { prepareWeixinLogin?: () => Promise<any> } = {};
    async *submitTurn(input: any) {
      submits.push(mask(input));
      const script = turns[turnIndex] ?? { events: [] };
      const index = turnIndex++;
      for (const item of script.events ?? []) {
        if (item.waitRequests) await waitRequests(item.waitRequests);
        else yield item.event;
      }
      if (script.hold) {
        // Recorded only once the consumer has resumed the generator, i.e. every
        // pre-hold event (turn_started → activeRun registration) was consumed.
        fake.requests.push({ synthetic: `turn-${index}-held` });
        const extra = await new Promise<any[]>((done) => heldTurns.set(index, done));
        for (const item of extra) {
          if (item.waitRequests) await waitRequests(item.waitRequests);
          else yield item.event;
        }
      }
    }
    async abortTurn(input: any) {
      aborts.push(mask(input));
    }
    // InProcessGateway.ts:2912-2921, verbatim.
    async prepareWeixinLogin(): Promise<any> {
      if (!this.options.prepareWeixinLogin) {
        return {
          requested: false,
          requestedAt: new Date().toISOString(),
          reason: "unsupported",
        };
      }
      return this.options.prepareWeixinLogin();
    }
    setPrepareWeixinLogin(handler: () => Promise<any>) {
      this.options.prepareWeixinLogin = handler;
    }
  }
  const gateway = new HarnessGateway();

  // The ChannelAdapterBundle weixin subset (hotStartWeixin ChannelAdapterBundle
  // .ts:116-122, reload :96-113, createWeixin :145-152, flush :124-126), with
  // the golden's loopback seams added at the same constructor options a bundle
  // test uses.
  const state = new ChannelStatePersistence({ stateDir: resolve(pilotHome, 'channels') });
  function wrapClient(real: any) {
    return {
      get cursor() { return real.cursor; },
      set cursor(value: string) { real.cursor = value; },
      async poll() {
        if (syntheticPollFailures > 0) {
          syntheticPollFailures--;
          fake.requests.push({ synthetic: 'poll-failure' });
          throw new TypeError('fetch failed');
        }
        return real.poll();
      },
      sendTextChunked: (...args: any[]) => real.sendTextChunked(...args),
      sendMedia: (...args: any[]) => real.sendMedia(...args),
      getUploadUrl: (...args: any[]) => real.getUploadUrl(...args),
      sendTyping: (...args: any[]) => real.sendTyping(...args),
    };
  }
  // WeixinChannel routes poll/lifecycle diagnostics through `deps.logger`,
  // not console; the harness records both into the same sorted log list.
  const channelLogger = (level: string) => (text: string) => {
    logs.push([level, mask(String(text))]);
  };
  function makeWeixin(saved: any) {
    return new WeixinChannel({
      credentialsPath,
      mapper: saved ? new WeixinSessionMapper(saved) : undefined,
      onStateChange: (value: any) => state.save('weixin', value),
      loginWithQR: (callbacks: any) => weixinIlink.loginWithQR(callbacks, baseUrl),
      clientFactory: (options: any) => wrapClient(new weixinIlink.ILinkClient(options)),
      logger: {
        info: channelLogger('info'),
        warn: channelLogger('warn'),
        error: channelLogger('error'),
      },
    });
  }
  async function createStartupAdapters(config: any) {
    const saved = await state.load('weixin');
    return config.adapters?.weixin?.enabled === true ? { weixin: makeWeixin(saved) } : {};
  }
  const channelAdapters = {
    async hotStartWeixin(server: any) {
      const saved = await state.load('weixin');
      await server.hotStartChannel(makeWeixin(saved));
    },
    async reload(server: any, config: any) {
      const startup = await createStartupAdapters(config);
      const result = await server.reconcileChannels({
        channels: [...(startup.weixin ? [startup.weixin] : [])],
        managedChannelKeys: ['weixin'],
      });
      return [
        ...result.started.map((key: string) => `${key}=started`),
        ...result.stopped.map((key: string) => `${key}=stopped`),
      ];
    },
    flush: () => state.flush(),
  };

  let serverRef: any;
  // pilotdeck.ts:195-198, verbatim.
  async function hotStartWeixinChannel(): Promise<void> {
    if (!serverRef) return;
    await channelAdapters.hotStartWeixin(serverRef);
  }

  // Gates cover real timers (QR poll interval, retry sleeps), so they are
  // time-bounded, not iteration-bounded.
  async function until(predicate: () => boolean) {
    const deadline = Date.now() + 10000;
    while (!predicate()) {
      if (Date.now() > deadline) throw new Error('#1328 weixin harness gate did not resolve');
      await new Promise((done) => setTimeout(done, 5));
    }
  }
  async function waitRequests(items: any[]) {
    for (const item of items) await until(() => fake.countPath(item.path) >= item.count);
  }
  // A stop waits for the poll loop, which is parked on a held long poll; the
  // abort is issued first, so answering it lets the loop exit at its top check.
  async function settleStop<T>(promise: Promise<T>): Promise<T> {
    let settled = false;
    const guarded = promise.then(
      (value) => { settled = true; return value; },
      (error) => { settled = true; throw error; },
    );
    while (!settled) {
      fake.releaseHeld();
      await new Promise((done) => setImmediate(done));
    }
    return guarded;
  }
  const capture = (promise: Promise<any>) => promise.then(
    (value) => mask(value ?? null),
    (error: any) => ({ error: String(error) }),
  );

  const original = { log: console.log, warn: console.warn, error: console.error };
  const previousHome = process.env.HOME;
  process.env.HOME = scenarioDir;
  console.log = (text: string) => { logs.push(['info', mask(String(text))]); };
  console.warn = (text: string) => { logs.push(['warn', mask(String(text))]); };
  console.error = (text: string) => { logs.push(['error', mask(String(text))]); };
  try {
    for (const step of definition.steps) {
      try {
        await runStep(step);
      } catch (error) {
        throw new Error(`#1328 scenario ${definition.name} failed at ${JSON.stringify(step)}: ${String(error)}`);
      }
    }
    async function runStep(step: any) {
      if (step.op === 'server') {
        const startup = await createStartupAdapters(step.config);
        const nextServer = await startPilotDeckServer({
          gateway, config: step.config, pilotHome, sessionSearch: {},
          ...(startup.weixin ? { weixin: startup.weixin } : {}),
        });
        serverRef = nextServer;
        // pilotdeck.ts:233-247, verbatim.
        (
          gateway as {
            setPrepareWeixinLogin?: (handler: () => Promise<any>) => void;
          }
        ).setPrepareWeixinLogin?.(async () => {
          const requestedAt = new Date().toISOString();
          if (!serverRef) {
            return { requested: false, requestedAt, reason: "unsupported" };
          }
          await hotStartWeixinChannel();
          return { requested: true, requestedAt };
        });
      } else if (step.op === 'prepare') {
        results.push(await capture(gateway.prepareWeixinLogin()));
      } else if (step.op === 'credentials') {
        const json = JSON.parse(JSON.stringify(step.json).replaceAll('$BASE', baseUrl));
        await mkdir(dirname(credentialsPath), { recursive: true });
        await writeFile(credentialsPath, JSON.stringify(json, null, 2), 'utf8');
      } else if (step.op === 'script') {
        fake.pushScript(step.endpoint, step.items);
      } else if (step.op === 'wait-requests') {
        await until(() => fake.countPath(step.path) >= step.count);
      } else if (step.op === 'wait-reports') {
        await until(() => reports.at(-1)?.state === step.state);
      } else if (step.op === 'wait-log') {
        await until(() => logs.some((entry) => entry[1] === step.text));
      } else if (step.op === 'wait-submits') {
        await until(() => submits.length >= step.count);
      } else if (step.op === 'wait-held') {
        await until(() => fake.requests.some((request: any) => request.synthetic === `turn-${step.turn}-held`));
      } else if (step.op === 'release-turn') {
        heldTurns.get(step.turn)?.(step.events);
        heldTurns.delete(step.turn);
      } else if (step.op === 'reload') {
        results.push(await capture(settleStop(channelAdapters.reload(serverRef, step.config))));
      } else if (step.op === 'flush') {
        await channelAdapters.flush();
      } else if (step.op === 'close') {
        results.push(await capture(settleStop(serverRef.close())));
      } else {
        throw new Error(`#1328 unknown step ${step.op}`);
      }
    }
    await new Promise((done) => setImmediate(done));
    const readJson = async (path: string) => {
      try {
        return mask(JSON.parse(await readFile(path, 'utf8')));
      } catch {
        return null;
      }
    };
    logs.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0));
    return {
      requests: fake.requests,
      reports,
      logs,
      submits,
      aborts,
      results,
      hostCloses,
      files: {
        credentials: await readJson(credentialsPath),
        weixinState: await readJson(join(pilotHome, 'channels', 'weixin.state.json')),
        runtimeStatus: await readJson(join(pilotHome, 'channels', 'runtime-status.json')),
      },
    };
  } finally {
    if (serverRef) {
      await Promise.race([
        settleStop(serverRef.close()).catch(() => {}),
        new Promise((done) => setTimeout(done, 2000)),
      ]);
    }
    fake.destroy();
    Object.assign(console, original);
    process.env.HOME = previousHome;
    await rm(scenarioDir, { recursive: true, force: true });
  }
}

const cases = [];
for (const definition of definitions) {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    const timeout = new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(`#1328 scenario timeout: ${definition.name}`)), 20000);
    });
    cases.push({ ...definition, expected: await Promise.race([run(definition), timeout]) });
  } finally {
    clearTimeout(timer);
  }
}
await mkdir(process.argv[2], { recursive: true });
await writeFile(
  join(process.argv[2], 'oracle.json'),
  JSON.stringify({ sourceCommit, sourceSha256, node: process.version, icu: process.versions.icu, expectedCaseCount: EXPECTED_CASE_COUNT, cases }, null, 2) + '\n',
  { flag: 'wx' },
);
console.log(`PASS #1328 weixin channel: ${cases.length} real TS scenarios`);
// A failed scenario can leave a detached poll loop; the oracle is already on
// disk, so the process must not hang on stray timers.
process.exit(0);
