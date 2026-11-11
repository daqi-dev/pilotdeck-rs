// #1344 slack golden fixture (stub-contract level, #1786 人拍 1): the real
// baseline channel (SlackChannel.ts + SlackSessionMapper.ts + slack-render.ts
// + the real ImElicitationHelper / ImPermissionHelper, transpiled unchanged)
// behind the committed seven-member `@slack/bolt` stub (`bolt-stub.mjs`),
// speaking a loopback mock that serves the Socket Mode REST endpoints and the
// WebSocket upgrade on one port. No real account, no off-loopback traffic. See
// README.md for the replay contract.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createRequire } from 'node:module';
import net from 'node:net';
import { mkdir, rename, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import * as fs from 'node:fs';
import * as http from 'node:http';

import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1344 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1344 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, '#1344 PDRS_BASELINE');
assert.ok(process.argv[2], '#1344 output directory');
// The constructor falls back to process.env.SLACK_*_TOKEN; the tokens come
// from the options only, so the missing-token cells cannot be spoofed by the
// host environment.
delete process.env.SLACK_BOT_TOKEN;
delete process.env.SLACK_APP_TOKEN;
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/adapters/channel/slack/SlackChannel.ts',
  'src/adapters/channel/slack/SlackSessionMapper.ts',
  'src/adapters/channel/slack/slack-render.ts',
  'src/adapters/channel/protocol/ImElicitationHelper.ts',
  'src/adapters/channel/protocol/ImPermissionHelper.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
const bolt = await import('./bolt-stub.mjs');

const UUID = 'aaaa1111-bbbb-4ccc-8ddd-eeeeeeeeeeee';
const BOT_TOKEN = 'xoxb-1';
const APP_TOKEN = 'xoxa-1';
const USER_ID = 'U123';
const BOT_NAME = 'slackbot';
// 18183-18198 (r2 triage's free segment). 36 cases run strictly sequentially,
// so the 16 ports round-robin; every listener is closed before the next case
// binds (both Node and the Rust replay set SO_REUSEADDR).
const PORT_BASE = 18183;
const PORT_COUNT = 16;

// ---- transpiled baseline modules -------------------------------------------

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
  throw new Error(`#1344 dependency module must not import anything: ${name}`);
};
const elicitationModule = compileModule(sources[3]);
const permissionModule = compileModule(sources[4]);
const mapperModule = compileModule(sources[1]);
const renderModule = compileModule(sources[2]);

/** A fresh channel; `without` drops the named token options (the
 * missing-token cells), leaving the constructor's env fallback empty. */
function loadChannel(mapperUuid: string, without: string[] = []): any {
  const channelModule = compileModule(sources[0]);
  const channelRequire = (name: string) => {
    // Type-only imports (gateway/index.js, ../protocol/ChannelAdapter.js) are
    // erased by esbuild; the value imports are the stub plus the real
    // helpers/mapper/render.
    if (name === '@slack/bolt') return bolt;
    if (name === '../protocol/ImElicitationHelper.js') return elicitationModule(nodeOnly);
    if (name === '../protocol/ImPermissionHelper.js') return permissionModule(nodeOnly);
    if (name === './SlackSessionMapper.js') return mapperModule(nodeOnly);
    if (name === './slack-render.js') return renderModule(nodeOnly);
    throw new Error(`#1344 unexercised import ${name}`);
  };
  const { SlackChannel } = channelModule(channelRequire);
  const { SlackSessionMapper } = mapperModule(nodeOnly);
  const mapper = new SlackSessionMapper(undefined, () => mapperUuid);
  const options: Record<string, unknown> = { mapper };
  if (!without.includes('botToken')) options.botToken = BOT_TOKEN;
  if (!without.includes('appToken')) options.appToken = APP_TOKEN;
  return new SlackChannel(options);
}

// ---- loopback mock: Socket Mode REST + WebSocket on one port ---------------

const WEBSOCKET_GUID = '258EAFA5-E914-47DA-95CA-C5AB0DC85B11';

interface CaseRecord {
  requests: unknown[];
  ws: unknown[];
  rpcs: unknown[];
  logs: unknown[];
}

interface Mock {
  server: http.Server;
  scripted: { status: number; json: unknown }[];
  sockets: net.Socket[];
  /** Set once the case teardown starts: frames after it are ignored. */
  closing: boolean;
}

function writeTextFrame(socket: net.Socket, text: string) {
  const payload = Buffer.from(text, 'utf8');
  let header: Buffer;
  if (payload.length < 126) {
    header = Buffer.from([0x81, payload.length]);
  } else if (payload.length < 65536) {
    header = Buffer.alloc(4);
    header[0] = 0x81;
    header[1] = 126;
    header.writeUInt16BE(payload.length, 2);
  } else {
    header = Buffer.alloc(10);
    header[0] = 0x81;
    header[1] = 127;
    header.writeBigUInt64BE(BigInt(payload.length), 2);
  }
  socket.write(Buffer.concat([header, payload]));
}

/** Parse one (possibly masked) client frame off `buffer`; null while incomplete. */
function parseFrame(buffer: Buffer): { opcode: number; payload: Buffer; consumed: number } | null {
  if (buffer.length < 2) return null;
  const opcode = buffer[0] & 0x0f;
  const masked = (buffer[1] & 0x80) !== 0;
  let length = buffer[1] & 0x7f;
  let offset = 2;
  if (length === 126) {
    if (buffer.length < 4) return null;
    length = buffer.readUInt16BE(2);
    offset = 4;
  } else if (length === 127) {
    if (buffer.length < 10) return null;
    length = Number(buffer.readBigUInt64BE(2));
    offset = 10;
  }
  const maskOffset = offset;
  if (masked) offset += 4;
  if (buffer.length < offset + length) return null;
  let payload = buffer.subarray(offset, offset + length);
  if (masked) {
    const key = buffer.subarray(maskOffset, maskOffset + 4);
    const unmasked = Buffer.alloc(length);
    for (let i = 0; i < length; i++) unmasked[i] = payload[i] ^ key[i & 3];
    payload = unmasked;
  }
  return { opcode, payload, consumed: offset + length };
}

function attachFrameReader(mock: Mock, socket: net.Socket, record: CaseRecord) {
  let pending = Buffer.alloc(0);
  socket.on('data', (chunk: Buffer) => {
    if (mock.closing) return;
    pending = Buffer.concat([pending, chunk]);
    for (;;) {
      const frame = parseFrame(pending);
      if (!frame) return;
      pending = pending.subarray(frame.consumed);
      if (frame.opcode === 0x8) {
        // The client close handshake: record the code, echo the close.
        const code = frame.payload.length >= 2 ? frame.payload.readUInt16BE(0) : 1005;
        record.ws.push({ event: 'close', code });
        socket.write(Buffer.from([0x88, 0x02, code >> 8, code & 0xff]));
        socket.end();
        mock.sockets = mock.sockets.filter(entry => entry !== socket);
      } else if (frame.opcode === 0x9) {
        const pong = Buffer.alloc(2 + frame.payload.length);
        pong[0] = 0x8a;
        pong[1] = frame.payload.length;
        frame.payload.copy(pong, 2);
        socket.write(pong);
      }
      // Client text frames are not part of the contract surface; the matrix
      // never sends one.
    }
  });
  socket.on('error', () => { /* the case teardown owns the sockets */ });
}

async function startMock(port: number, record: CaseRecord, scripted: { status: number; json: unknown }[]): Promise<Mock> {
  const mock: Mock = { server: null as any, scripted, sockets: [], closing: false };
  const server = http.createServer((req, res) => {
    const chunks: Buffer[] = [];
    req.on('data', (chunk: Buffer) => chunks.push(chunk));
    req.on('end', () => {
      record.requests.push({
        method: req.method,
        path: req.url,
        contentType: (req.headers['content-type'] as string | undefined) ?? null,
        body: Buffer.concat(chunks).toString('utf8'),
      });
      const next = mock.scripted.shift()
        ?? (req.url === '/api/apps.connections.open'
          ? { status: 200, json: { ok: true, url: `ws://127.0.0.1:${port}/link` } }
          : req.url === '/api/auth.test'
            ? { status: 200, json: { ok: true, user_id: USER_ID, user: BOT_NAME } }
            : { status: 200, json: { ok: true } });
      res.writeHead(next.status, { 'Content-Type': 'application/json', Connection: 'close' });
      res.end(JSON.stringify(next.json));
    });
  });
  server.on('upgrade', (req, socket) => {
    const key = (req.headers['sec-websocket-key'] as string | undefined) ?? '';
    const accept = createHash('sha1').update(`${key}${WEBSOCKET_GUID}`).digest('base64');
    socket.write(
      'HTTP/1.1 101 Switching Protocols\r\n'
      + 'Upgrade: websocket\r\nConnection: Upgrade\r\n'
      + `Sec-WebSocket-Accept: ${accept}\r\n\r\n`,
    );
    record.ws.push({ event: 'open' });
    attachFrameReader(mock, socket, record);
    mock.sockets.push(socket);
  });
  await new Promise<void>(done => server.listen(port, '127.0.0.1', () => done()));
  mock.server = server;
  return mock;
}

async function stopMock(mock: Mock) {
  // A failed start abandons its socket (the TS failure handle never calls
  // `app.stop()`), so the teardown closes it server-side with 1000: an
  // abrupt destroy would surface as an abnormal client close, and the stub's
  // error handler would race a stray `bolt error` log into (or past) the
  // already-snapshot records.
  mock.closing = true;
  for (const socket of mock.sockets) {
    socket.write(Buffer.from([0x88, 0x02, 0x03, 0xe8]));
    socket.end();
  }
  await new Promise<void>(done => mock.server.close(() => done()));
  // `close` only waits for idle connections; destroy any stragglers the
  // Connection: close responses left half-open.
  mock.server.closeAllConnections?.();
}

// ---- helpers -----------------------------------------------------------------

function until(predicate: () => boolean): Promise<void> {
  const started = Date.now();
  return new Promise((resolvePromise, reject) => {
    const check = () => {
      if (predicate()) return resolvePromise();
      if (Date.now() - started > 20000) return reject(new Error('#1344 wait timeout'));
      setImmediate(check);
    };
    check();
  });
}
async function ticks(n: number) {
  for (let i = 0; i < n; i++) await new Promise(done => setImmediate(done));
}

// ---- the fake A7 gateway (the sms #1339 shape, per-turn scripts) -------------

type TurnScript = { gate?: string; events?: unknown[]; error?: string; holdAfter?: string };

function makeGateway(turns: TurnScript[], rpcs: unknown[], gates: Map<string, any>) {
  const gate = (name: string) => {
    if (!gates.has(name)) {
      let release!: () => void;
      const promise = new Promise<void>(done => { release = done; });
      gates.set(name, { promise, release });
    }
    return gates.get(name)!;
  };
  let turnIndex = 0;
  return {
    async *submitTurn(request: any) {
      rpcs.push({ rpc: 'submitTurn', request: structuredClone(request) });
      const turn = turns[turnIndex++] ?? {};
      if (turn.gate) await gate(turn.gate).promise;
      if (turn.error) throw new Error(turn.error);
      for (const event of turn.events ?? []) yield structuredClone(event);
      // The stream stays open past the elicitation/permission event until the
      // matching respondElicitation/permissionDecide lands, exactly the A7
      // block the helper comments describe.
      if (turn.holdAfter === 'elicit') await gate('elicited').promise;
      if (turn.holdAfter === 'decide') await gate('decided').promise;
    },
    async respondElicitation(response: any) {
      rpcs.push({ rpc: 'respondElicitation', response: structuredClone(response) });
      gates.get('elicited')?.release();
    },
    async permissionDecide(decision: any) {
      rpcs.push({ rpc: 'permissionDecide', decision: structuredClone(decision) });
      // A macrotask boundary keeps the resumed turn out of this reply's
      // microtask chain, so the confirmation and the turn tail are ordered by
      // the harness steps rather than by promise scheduling.
      await new Promise(done => setImmediate(done));
      gates.get('decided')?.release();
      return { delivered: true };
    },
  };
}

// ---- case runner ---------------------------------------------------------------

interface Step { [key: string]: unknown }
interface Case {
  name: string;
  port: number;
  turns: TurnScript[];
  steps: Step[];
  scripted?: { status: number; json: unknown }[];
}

async function runCase(caseDef: Case): Promise<CaseRecord> {
  const record: CaseRecord = { requests: [], ws: [], rpcs: [], logs: [] };
  const gates = new Map<string, any>();
  const gateway = makeGateway(caseDef.turns, record.rpcs, gates);
  process.env.PDRS_SLACK_MOCK = `http://127.0.0.1:${caseDef.port}`;
  const scripted = (caseDef.scripted ?? []).map(entry => ({ ...entry }));
  const mock = await startMock(caseDef.port, record, scripted);

  let channel: any = null;
  let handle: any = null;
  const logger = {
    info: (message: string) => record.logs.push(['info', message]),
    error: (message: string) => record.logs.push(['error', message]),
  };
  const deps = { gateway, logger };

  for (const step of caseDef.steps) {
    const op = step.op as string;
    if (op === 'start' || op === 'start-fresh') {
      channel = loadChannel(UUID, (step.without as string[] | undefined) ?? []);
      handle = await channel.start(deps);
    } else if (op === 'restart') {
      handle = await channel.start(deps);
    } else if (op === 'stop') {
      await handle.stop(step.reason as string | undefined);
    } else if (op === 'ws-send') {
      const socket = mock.sockets[mock.sockets.length - 1];
      if (!socket) {
        record.ws.push({ event: 'dropped' });
      } else {
        writeTextFrame(socket, JSON.stringify({ type: 'message', payload: { event: step.event } }));
      }
    } else if (op === 'release') {
      gates.get(step.gate as string)?.release();
    } else if (op === 'wait') {
      // Every named condition is waited in turn, so a step can gate on
      // several streams at once (the turn landing *and* its reply).
      if (step.requests !== undefined) await until(() => record.requests.length >= step.requests as number);
      if (step.ws !== undefined) await until(() => record.ws.length >= step.ws as number);
      if (step.rpcs !== undefined) await until(() => record.rpcs.length >= step.rpcs as number);
      if (step.logs !== undefined) await until(() => record.logs.length >= step.logs as number);
      if (step.ticks !== undefined) await ticks(step.ticks as number);
      if (step.requests === undefined && step.ws === undefined && step.rpcs === undefined
        && step.logs === undefined && step.ticks === undefined) {
        throw new Error('#1344 wait without condition');
      }
    } else {
      throw new Error(`#1344 unknown op ${op}`);
    }
  }
  // Every scripted response must have been served: a dry queue means the case
  // consumed the mock default and its cells are garbage.
  assert.equal(scripted.length, 0, `#1344 ${caseDef.name} left ${scripted.length} scripted responses unserved`);
  // Quiescence: nothing may arrive after the case's steps end.
  await ticks(50);
  const settled = JSON.stringify([record.requests, record.ws, record.rpcs, record.logs]);
  await ticks(50);
  assert.equal(
    JSON.stringify([record.requests, record.ws, record.rpcs, record.logs]),
    settled,
    `#1344 ${caseDef.name} records still arriving after the case ended`,
  );
  await stopMock(mock);
  return record;
}

// ---- scripted responses --------------------------------------------------------

const OPEN_FAILED = { status: 200, json: { ok: false, error: 'invalid_app_token' } };
const POST_FAILED = { status: 200, json: { ok: false, error: 'channel_not_found' } };
/** The queued ok response still names the case's own WS port, so the failed
 * auth.test start opened exactly that socket. */
const authFailed = (port: number) => [
  { status: 200, json: { ok: true, url: `ws://127.0.0.1:${port}/link` } },
  { status: 200, json: { ok: false, error: 'invalid_bot_token' } },
];

// ---- event builders --------------------------------------------------------------

const baseEvent = (over: Record<string, unknown>) => ({
  channel: 'C1',
  user: 'U999',
  ts: '300.1',
  ...over,
});
const messageEvent = (text: string, over: Record<string, unknown> = {}) => baseEvent({ text, ...over });

// ---- case definitions --------------------------------------------------------

const textDelta = (text: string) => ({ type: 'assistant_text_delta', text });
const HELLO_TURN: TurnScript[] = [{ events: [textDelta('你好，处理完成。')] }];
const LONG_TEXT = `${'x'.repeat(38500)}\n${'y'.repeat(1500)}`;
const QUESTION_EVENT = {
  type: 'elicitation_request',
  requestId: 'er-1',
  questions: [{
    question: '选择一个选项',
    header: '选项',
    options: [{ label: 'One', description: 'first' }, { label: 'Two' }],
  }],
};
const PERMISSION_EVENT = {
  type: 'permission_request',
  requestId: 'pr-1',
  toolName: 'Bash',
  payload: { command: 'ls -la' },
};

const definitions: Case[] = [];
const scenario = (definition: Omit<Case, 'port' | 'scripted'> & { scripted?: (port: number) => { status: number; json: unknown }[] }) => {
  const port = PORT_BASE + definitions.length % PORT_COUNT;
  definitions.push({ ...definition, port, scripted: definition.scripted?.(port) });
};
const start = { op: 'start' };
const stop = (reason?: string) => ({ op: 'stop', ...(reason === undefined ? {} : { reason }) });
const send = (anEvent: Record<string, unknown>) => ({ op: 'ws-send', event: anEvent });
const release = (gate: string) => ({ op: 'release', gate });
const wait = (over: Record<string, unknown>) => ({ op: 'wait', ...over });
const settle = () => wait({ ticks: 50 });

// 矩阵 1：C × start(全配置)。
scenario({ name: 'start-ok', turns: [], steps: [
  start, wait({ ws: 1, logs: 1 }), stop('done'),
] });
// 矩阵 2：C × start 无 botToken — the env fallback is empty, the mock stays up
// as the witness that nothing ever leaves the channel.
scenario({ name: 'start-no-bot-token', turns: [], steps: [
  { op: 'start', without: ['botToken'] }, wait({ logs: 1 }), settle(), stop('done'),
] });
// 矩阵 3：C × start 无 appToken。
scenario({ name: 'start-no-app-token', turns: [], steps: [
  { op: 'start', without: ['appToken'] }, wait({ logs: 1 }), settle(), stop('done'),
] });
// 矩阵 4：C × connections.open ok:false — `start failed` log, no socket.
scenario({ name: 'start-connections-open-failed', turns: [], scripted: () => [OPEN_FAILED], steps: [
  start, wait({ requests: 1, logs: 1 }), stop('done'),
] });
// 矩阵 5：C × auth.test ok:false — the socket did open; the failure handle's
// stop still logs and closes it.
scenario({ name: 'start-auth-test-failed', turns: [], scripted: port => authFailed(port), steps: [
  start, wait({ requests: 2, ws: 1, logs: 1 }), stop('done'),
] });
// 矩阵 6：R × 普通消息（无 thread）。
scenario({ name: 'text-turn-reply', turns: HELLO_TURN, steps: [
  start, send(messageEvent('做个总结')), wait({ rpcs: 1, requests: 3 }), stop('done'),
] });
// 矩阵 7：R × thread 内消息。
scenario({ name: 'thread-reply', turns: HELLO_TURN, steps: [
  start, send(messageEvent('线程内', { thread_ts: '100.1', ts: '100.2' })),
  wait({ rpcs: 1, requests: 3 }), stop('done'),
] });
// 矩阵 8：R × thread_ts === ts：会话键退化为裸 channelId，回复仍带 thread_ts。
scenario({ name: 'thread-root-own-key', turns: HELLO_TURN, steps: [
  start, send(messageEvent('根消息', { thread_ts: '100.1', ts: '100.1' })),
  wait({ rpcs: 1, requests: 3 }), stop('done'),
] });
// 矩阵 9：R × bot_id 事件（判别力正控目标）。
scenario({ name: 'bot-id-filtered', turns: HELLO_TURN, steps: [
  start, send(messageEvent('hi', { bot_id: 'B1' })), settle(), stop('done'),
] });
// 矩阵 10：R × subtype bot_message。
scenario({ name: 'bot-message-filtered', turns: HELLO_TURN, steps: [
  start, send(messageEvent('hi', { subtype: 'bot_message' })), settle(), stop('done'),
] });
// 矩阵 11：R × subtype message_changed / message_deleted。
scenario({ name: 'message-changed-filtered', turns: HELLO_TURN, steps: [
  start, send(messageEvent('hi', { subtype: 'message_changed' })), settle(), stop('done'),
] });
scenario({ name: 'message-deleted-filtered', turns: HELLO_TURN, steps: [
  start, send(messageEvent('hi', { subtype: 'message_deleted' })), settle(), stop('done'),
] });
// 矩阵 12：R × user === botUserId（自身消息）。
scenario({ name: 'self-message-filtered', turns: HELLO_TURN, steps: [
  start, send(messageEvent('hi', { user: 'U123' })), settle(), stop('done'),
] });
// 矩阵 13：R × 提及 strip：仅提及（strip 后空）与提及 + 正文。
scenario({ name: 'mention-only-no-turn', turns: HELLO_TURN, steps: [
  start, send(messageEvent('<@U123>')), settle(), stop('done'),
] });
scenario({ name: 'mention-stripped', turns: HELLO_TURN, steps: [
  start, send(messageEvent('<@U123> 你好')), wait({ rpcs: 1, requests: 3 }), stop('done'),
] });
// 矩阵 14：R × 同 chat 第二条（turn 进行中）。
scenario({ name: 'active-skip', turns: [{ gate: 'turn-1', events: [textDelta('第一条回复。')] }], steps: [
  start, send(messageEvent('第一句')), wait({ rpcs: 1 }), send(messageEvent('第二句')),
  wait({ logs: 2 }), release('turn-1'), wait({ requests: 3 }), stop('done'),
] });
// 矩阵 14 的姊妹格（#2313）：R × 连发两帧（两帧之间无 wait）——TS stub 对每个
// message 帧同步调 handler，`handleSlackMessage` 到 `activeChats.add` 为同步段，
// 帧序即入场序：第一帧入回合，第二帧判 already active。此前 35 格两帧之间都有
// wait 或 gate，对拍覆盖不到保序（P2 #2313 的覆盖缺口）。
scenario({ name: 'back-to-back-frames', turns: HELLO_TURN, steps: [
  start, send(messageEvent('第一句')), send(messageEvent('第二句')),
  wait({ rpcs: 1, requests: 3 }), stop('done'),
] });
// 矩阵 15：R × 同一文本 turn 完成后再投：再执行（无事件级去重）。
scenario({
  name: 'redelivery-after-turn',
  turns: [{ events: [textDelta('回复。')] }, { events: [textDelta('回复。')] }],
  steps: [
    start, send(messageEvent('再来一次')), wait({ requests: 3 }), settle(),
    send(messageEvent('再来一次')), wait({ requests: 4, rpcs: 2 }), stop('done'),
  ],
});
// 矩阵 16：R × /new（无正文）。
scenario({ name: 'new-session', turns: HELLO_TURN, steps: [
  start, send(messageEvent('/new')), wait({ requests: 3 }),
  send(messageEvent('新会话消息')), wait({ rpcs: 1, requests: 4 }), stop('done'),
] });
// 矩阵 17：R × /new <正文>——不发「已创建新会话。」确认。
scenario({ name: 'new-with-message', turns: HELLO_TURN, steps: [
  start, send(messageEvent('/new 直接处理')), wait({ rpcs: 1, requests: 3 }), stop('done'),
] });
// 矩阵 18：R × 回复 > 39000：lastIndexOf(\n) 优先的分段。
scenario({ name: 'long-text-chunked', turns: [{ events: [textDelta(LONG_TEXT)] }], steps: [
  start, send(messageEvent('写四万字')), wait({ requests: 4 }), stop('done'),
] });
// 矩阵 19：R × submitTurn 抛错。
scenario({ name: 'turn-error', turns: [{ error: 'agent exploded' }], steps: [
  start, send(messageEvent('出错的回合')), wait({ rpcs: 1, requests: 3, logs: 2 }), stop('done'),
] });
// 矩阵 20：R × elicitation 用户答 "1"。
scenario({ name: 'elicitation-answer', turns: [{ events: [QUESTION_EVENT], holdAfter: 'elicit' }], steps: [
  start, send(messageEvent('选择')), wait({ requests: 3 }),
  send(messageEvent('1')), wait({ rpcs: 2 }), settle(), stop('done'),
] });
// 矩阵 21：R × elicitation 用户答 "0"。
scenario({ name: 'elicitation-cancel', turns: [{ events: [QUESTION_EVENT], holdAfter: 'elicit' }], steps: [
  start, send(messageEvent('选择')), wait({ requests: 3 }),
  send(messageEvent('0')), wait({ rpcs: 2, requests: 4 }), stop('done'),
] });
// 矩阵 22：R × permission "1"/"2"/"0"。
scenario({ name: 'permission-allow-once', turns: [{ events: [PERMISSION_EVENT], holdAfter: 'decide' }], steps: [
  start, send(messageEvent('跑命令')), wait({ requests: 3 }),
  send(messageEvent('1')), wait({ rpcs: 2, requests: 4 }), settle(), stop('done'),
] });
scenario({ name: 'permission-allow-session', turns: [{ events: [PERMISSION_EVENT], holdAfter: 'decide' }], steps: [
  start, send(messageEvent('跑命令')), wait({ requests: 3 }),
  send(messageEvent('2')), wait({ rpcs: 2, requests: 4 }), settle(), stop('done'),
] });
scenario({ name: 'permission-deny', turns: [{ events: [PERMISSION_EVENT], holdAfter: 'decide' }], steps: [
  start, send(messageEvent('跑命令')), wait({ requests: 3 }),
  send(messageEvent('0')), wait({ rpcs: 2, requests: 4 }), settle(), stop('done'),
] });
// 矩阵 23：R × 权限确认发出时 postMessage 失败 → 状态回复 + 重发提示。
// The queue must cover open+auth.test too (it serves every request in order),
// so the failure lands exactly on the prompt's postMessage.
scenario({ name: 'permission-prompt-fail-retry', turns: [{ events: [PERMISSION_EVENT], holdAfter: 'decide' }], scripted: port => [
  { status: 200, json: { ok: true, url: `ws://127.0.0.1:${port}/link` } },
  { status: 200, json: { ok: true, user_id: USER_ID, user: BOT_NAME } },
  POST_FAILED,
], steps: [
  start, send(messageEvent('跑命令')), wait({ requests: 3, logs: 2 }),
  send(messageEvent('再试试')), wait({ requests: 5 }), stop('done'),
] });
// 矩阵 24：R/D × stop(reason)，turn 在途：postMessage 不再发出。
scenario({ name: 'stop-during-turn', turns: [{ gate: 'turn-1', events: [textDelta('迟到的回复。')] }], steps: [
  start, send(messageEvent('慢慢想')), wait({ rpcs: 1 }), stop('done'),
  release('turn-1'), settle(),
] });
// 矩阵 25：D × stop 后消息：无连接可推。
scenario({ name: 'stop-then-silence', turns: HELLO_TURN, steps: [
  start, stop('done'), send(messageEvent('还有人吗')), settle(),
] });
// 矩阵 26：D × 同实例再 start（mapper 状态保留：/new 的会话键续用）。
scenario({ name: 'restart-after-stop', turns: HELLO_TURN, steps: [
  start, send(messageEvent('/new')), wait({ requests: 3 }), stop('done'),
  { op: 'restart' }, send(messageEvent('继续')), wait({ rpcs: 1, requests: 6 }), stop('done'),
] });
// 矩阵 27：R × stop("hot-reload") 后新实例 start（mapper 状态重置）。
scenario({ name: 'hot-reload-restart', turns: HELLO_TURN, steps: [
  start, send(messageEvent('/new')), wait({ requests: 3 }), stop('hot-reload'),
  { op: 'start-fresh' }, send(messageEvent('继续')), wait({ rpcs: 1, requests: 6 }), stop('done'),
] });
// 矩阵 28：R × postMessage ok:false。The failed delivery is the turn's final
// reply (the queue's third response), logged and swallowed by sendReply.
scenario({ name: 'post-message-failure', turns: HELLO_TURN, scripted: port => [
  { status: 200, json: { ok: true, url: `ws://127.0.0.1:${port}/link` } },
  { status: 200, json: { ok: true, user_id: USER_ID, user: BOT_NAME } },
  POST_FAILED,
], steps: [
  start, send(messageEvent('发不出去')), wait({ requests: 3, logs: 2 }), stop('done'),
] });
// 矩阵 29：R × mrkdwn / 链接转换。
scenario({ name: 'mrkdwn-and-link-formats', turns: [{ events: [textDelta('**加粗** 和 [链接](https://example.com)')] }], steps: [
  start, send(messageEvent('格式化')), wait({ rpcs: 1, requests: 3 }), stop('done'),
] });
// 覆盖 `if (!text) return`（矩阵 13 的空文本姊妹格）。
scenario({ name: 'empty-text-silent', turns: HELLO_TURN, steps: [
  start, send(messageEvent('')), settle(), stop('done'),
] });
// 覆盖 `if (!channelId) return`。
scenario({ name: 'no-channel-silent', turns: HELLO_TURN, steps: [
  start, send(messageEvent('hi', { channel: undefined })), settle(), stop('done'),
] });

const EXPECTED_CASE_COUNT = 36;
assert.equal(definitions.length, EXPECTED_CASE_COUNT, '#1344 case count');

// ---- run every case twice; the two records must be deep-equal ------------------

const cases: unknown[] = [];
for (const caseDef of definitions) {
  const first = await runCase(caseDef);
  const second = await runCase(caseDef);
  assert.deepEqual(second, first, `#1344 ${caseDef.name} is not deterministic`);
  cases.push({
    name: caseDef.name,
    port: caseDef.port,
    turns: caseDef.turns,
    steps: caseDef.steps,
    scripted: caseDef.scripted ?? [],
    expected: first,
  });
  console.log(
    `case ${caseDef.name}: requests=${first.requests.length} ws=${first.ws.length} `
    + `rpcs=${first.rpcs.length} logs=${first.logs.length}`,
  );
}

const output = resolve(process.argv[2]);
await mkdir(output, { recursive: true });
const command = 'PDRS_BASELINE=/absolute/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/channel-slack/generate.mts src/pilotdeck-gateway/tests/fixtures/channel-slack';
// tmp + rename so a reader never sees a half-written oracle (and a crashed
// run leaves `.tmp` behind instead of a truncated `oracle.json`).
await writeFile(
  join(output, 'oracle.json.tmp'),
  JSON.stringify({ sourceCommit, sourceSha256, command, node: process.version, icu: process.versions.icu, expectedCaseCount: EXPECTED_CASE_COUNT, cases }, null, 2) + '\n',
  { flag: 'wx' },
);
await rename(join(output, 'oracle.json.tmp'), join(output, 'oracle.json'));
console.log(`PASS #1344 slack: ${cases.length} real TS scenarios`);
process.exit(0);
