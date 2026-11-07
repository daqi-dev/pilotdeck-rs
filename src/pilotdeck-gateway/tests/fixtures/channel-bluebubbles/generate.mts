// #1343 bluebubbles golden fixture. Runs the real baseline channel
// (BlueBubblesChannel.ts + BlueBubblesSessionMapper.ts +
// bluebubbles-render.ts + the shared protocol/ImElicitationHelper.ts +
// ImPermissionHelper.ts, transpiled unchanged — no SDK, no stub, global fetch
// against a loopback mock) against scripted fakes: a fake A7 gateway and a
// loopback BlueBubbles server reached through the channel's own serverUrl. No
// real account, no off-loopback traffic. See README.md for the replay contract.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import * as fs from 'node:fs';
import * as http from 'node:http';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1343 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1343 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, '#1343 PDRS_BASELINE');
assert.ok(process.argv[2], '#1343 output directory');
// The constructor's `?? process.env.BLUEBUBBLES_*` fallback must stay inert.
assert.ok(!process.env.BLUEBUBBLES_SERVER_URL, '#1343 BLUEBUBBLES_SERVER_URL must be unset');
assert.ok(!process.env.BLUEBUBBLES_PASSWORD, '#1343 BLUEBUBBLES_PASSWORD must be unset');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/adapters/channel/bluebubbles/BlueBubblesChannel.ts',
  'src/adapters/channel/bluebubbles/BlueBubblesSessionMapper.ts',
  'src/adapters/channel/bluebubbles/bluebubbles-render.ts',
  'src/adapters/channel/protocol/ImElicitationHelper.ts',
  'src/adapters/channel/protocol/ImPermissionHelper.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');

const MOCK_PORT = 19461;

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
  throw new Error(`#1343 dependency module must not import anything: ${name}`);
};
const mapperModule = compileModule(sources[1]);
const renderModule = compileModule(sources[2]);
const elicitationModule = compileModule(sources[3]);
const permissionModule = compileModule(sources[4]);

function makeChannelClass(): any {
  const channelModule = compileModule(sources[0]);
  const channelRequire = (name: string) => {
    if (name === 'node:crypto' || name === 'node:url') return requireBaseline(name);
    // Type-only imports are erased by esbuild; the remaining value imports
    // are all satisfied by the transpiled real modules.
    if (name === './BlueBubblesSessionMapper.js') return mapperModule(nodeOnly);
    if (name === './bluebubbles-render.js') return renderModule(nodeOnly);
    if (name === '../protocol/ImElicitationHelper.js') return elicitationModule(nodeOnly);
    if (name === '../protocol/ImPermissionHelper.js') return permissionModule(nodeOnly);
    throw new Error(`#1343 unexercised import ${name}`);
  };
  return channelModule(channelRequire).BlueBubblesChannel;
}

// ---- helpers ---------------------------------------------------------------

// Both sides pin every guid and reply to scripted constants; the only values
// that cannot be pinned are the mapper uuids / tempGuids (masked here, and the
// Rust replay applies the same rules) and the `after` timestamp seeded from
// the wall clock at each start (masked by the recency rule below).
function maskDynamic(text: string): string {
  return text.replace(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g, '<uuid>');
}
function maskQuery(query: string): string {
  // `after` within ±300 s of the recording clock is start-seeded and varies
  // run to run; scripted timestamps stay in the 1600000000 / 1900000000
  // ranges and never trip the rule.
  return query.replace(/after=([0-9]+)/g, (all, value: string) => {
    const parsed = Number(value);
    return Math.abs(parsed - Date.now() / 1000) <= 300 ? 'after=<now-s>' : all;
  });
}

let diag = () => '';
function until(predicate: () => boolean): Promise<void> {
  const started = Date.now();
  return new Promise((resolve, reject) => {
    const check = () => {
      if (predicate()) return resolve();
      // One poll interval is 2500 ms; 10 s leaves room for the cases that
      // wait out two of them.
      if (Date.now() - started > 10_000) return reject(new Error(`#1343 wait timeout ${diag()}`));
      setImmediate(check);
    };
    check();
  });
}
async function ticks(n: number) {
  for (let i = 0; i < n; i++) await new Promise(done => setImmediate(done));
}

// ---- loopback BlueBubbles server -------------------------------------------

type MockResponse = { status: number; json: unknown };
const DEFAULT_POLL: MockResponse = { status: 200, json: {} };
const DEFAULT_SEND: MockResponse = { status: 200, json: { sent: true } };

const mock = {
  armed: false,
  name: '',
  polls: 0,
  sends: 0,
  pollQueries: [] as string[],
  sendRecords: [] as any[],
  scriptedPolls: [] as MockResponse[],
  scriptedSends: [] as MockResponse[],
  strays: [] as string[],
};
const server = http.createServer((req, res) => {
  const chunks: Buffer[] = [];
  req.on('data', (c: Buffer) => chunks.push(c));
  req.on('end', () => {
    const url = new URL(req.url ?? '/', 'http://127.0.0.1');
    const body = chunks.length ? Buffer.concat(chunks).toString('utf8') : '';
    if (!mock.armed) {
      mock.strays.push(`${req.method} ${url.pathname}`);
      res.writeHead(200, { 'Content-Type': 'application/json' });
      res.end('{}');
      return;
    }
    if (req.method === 'GET') {
      // The channel only ever polls the one path.
      assert.equal(url.pathname, '/api/v1/message', `#1343 ${mock.name} unexpected GET path`);
      const masked = maskQuery(url.search ? url.search.slice(1) : '');
      if (!mock.pollQueries.includes(masked)) mock.pollQueries.push(masked);
      mock.polls += 1;
      const next = mock.scriptedPolls[mock.polls - 1] ?? DEFAULT_POLL;
      res.writeHead(next.status, { 'Content-Type': 'application/json' });
      res.end(JSON.stringify(next.json));
      return;
    }
    assert.equal(url.pathname, '/api/v1/message/text', `#1343 ${mock.name} unexpected POST path`);
    mock.sends += 1;
    mock.sendRecords.push({
      method: req.method,
      path: url.pathname,
      auth: (req.headers['authorization'] as string | undefined) ?? null,
      contentType: (req.headers['content-type'] as string | undefined) ?? null,
      body: maskDynamic(body),
    });
    const next = mock.scriptedSends[mock.sends - 1] ?? DEFAULT_SEND;
    res.writeHead(next.status, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify(next.json));
  });
});
await new Promise<void>(done => server.listen(MOCK_PORT, '127.0.0.1', done));

// ---- case runner -----------------------------------------------------------

type TurnScript = {
  gateBefore?: string;
  events: unknown[];
  holdForRespond?: boolean;
  resume?: unknown[];
  error?: string;
};
type Case = {
  name: string;
  password: string;
  serverUrl?: string;
  turns: TurnScript[];
  steps: any[];
  scriptedPolls: MockResponse[];
  scriptedSends: MockResponse[];
};

async function runCase(caseDef: Case): Promise<any> {
  const logs: unknown[] = [];
  const gateway: unknown[] = [];
  const responds: string[] = [];
  const decides: string[] = [];
  const gates = new Map<string, { promise: Promise<void>; release: () => void }>();
  const gate = (name: string) => {
    if (!gates.has(name)) {
      let release!: () => void;
      const promise = new Promise<void>(done => { release = done; });
      gates.set(name, { promise, release });
    }
    return gates.get(name)!;
  };
  // The elicitation/permission helpers call `gateway.respondElicitation` /
  // `gateway.permissionDecide`; recording + resolving the turn's
  // hold-for-respond gate is all the fake needs to do.
  let releaseResponded!: () => void;
  const responded = new Promise<void>(done => { releaseResponded = done; });
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
        if (turn.holdForRespond) await responded;
        for (const event of turn.resume ?? []) yield event;
        if (turn.error) throw new Error(turn.error);
        gateway.push({ turnEnd: true });
      })();
    },
    respondElicitation(input: any) {
      responds.push(JSON.stringify(input));
      releaseResponded();
      return Promise.resolve();
    },
    permissionDecide(input: any) {
      decides.push(JSON.stringify(input));
      releaseResponded();
      return Promise.resolve({ delivered: true });
    },
  };
  const makeChannel = () => new (makeChannelClass())({
    ...(caseDef.serverUrl !== undefined ? { serverUrl: caseDef.serverUrl } : {}),
    password: caseDef.password,
  });
  let channel = makeChannel();
  let handle: any = null;
  const logger = {
    info: (m: string) => logs.push(['info', maskDynamic(m)]),
    warn: (m: string) => logs.push(['warn', maskDynamic(m)]),
    error: (m: string) => logs.push(['error', maskDynamic(m)]),
  };
  diag = () => `(case ${caseDef.name} polls=${mock.polls} sends=${mock.sends} gateway=${JSON.stringify(gateway)} logs=${JSON.stringify(logs)})`;

  for (const step of caseDef.steps) {
    if (step.op === 'start') {
      // `fresh` is the hot-reload shape: a new instance with its own mapper
      // and helper state, started after the old one stopped.
      if (step.fresh) channel = makeChannel();
      handle = await channel.start({ gateway: gatewayFake, logger });
    } else if (step.op === 'stop') {
      await handle.stop(step.reason);
    } else if (step.op === 'release') {
      gate(step.gate).release();
    } else if (step.op === 'wait') {
      if (step.polls !== undefined) await until(() => mock.polls >= step.polls);
      else if (step.sends !== undefined) await until(() => mock.sends >= step.sends);
      else if (step.gateway !== undefined) await until(() => gateway.length >= step.gateway);
      else if (step.responds !== undefined) await until(() => responds.length >= step.responds);
      else if (step.decides !== undefined) await until(() => decides.length >= step.decides);
      else if (step.logs !== undefined) await until(() => logs.length >= step.logs);
      else if (step.ticks !== undefined) await ticks(step.ticks);
      else throw new Error('#1343 wait without condition');
    } else {
      throw new Error(`#1343 unknown op ${step.op}`);
    }
  }
  return { logs, gateway, responds, decides };
}

// ---- scripted responses ----------------------------------------------------

function rows(list: unknown[]): MockResponse {
  return { status: 200, json: list };
}

const definitions: Case[] = [
  {
    // A1: no serverUrl — the error log, the empty handle (stop logs nothing),
    // and never a poll.
    name: 'missing-config',
    password: 'pw-1',
    turns: [],
    steps: [
      { op: 'start' },
      { op: 'wait', ticks: 20 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [],
    scriptedSends: [],
  },
  {
    // B1 + C7: first poll delivers one row, the turn replies through the mock.
    name: 'first-poll-hello-reply',
    password: 'pw-2',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{ events: [{ type: 'assistant_text_delta', text: '你好，处理完成。' }] }],
    steps: [
      { op: 'start' },
      { op: 'wait', polls: 1 },
      { op: 'wait', sends: 1 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'g-1', chatGuid: 'BB-1', text: '做个总结', dateCreated: 1600000000 }]),
    ],
    scriptedSends: [],
  },
  {
    // B6 + B5: the same guid re-delivered inside one poll and by later polls
    // never re-dispatches (one submit, one turnEnd).
    name: 'duplicate-guid-never-re-executes',
    password: 'pw-3',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{ events: [] }],
    steps: [
      { op: 'start' },
      { op: 'wait', polls: 3 },
      { op: 'wait', gateway: 2 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'g-d', chatGuid: 'BB-D', text: '一次', dateCreated: 1600000000 }]),
      rows([{ guid: 'g-d', chatGuid: 'BB-D', text: '一次', dateCreated: 1600000000 }]),
      rows([{ guid: 'g-d', chatGuid: 'BB-D', text: '一次', dateCreated: 1600000000 }]),
    ],
    scriptedSends: [],
  },
  {
    // B2 + B4 + B7 + B8 + B9 + B5 (id fallback): row filters, the non-2xx
    // poll warning, and a `chats`-array chatGuid fallback.
    name: 'row-filters-and-poll-error',
    password: 'pw-4',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{ events: [] }, { events: [] }, { events: [] }],
    steps: [
      { op: 'start' },
      { op: 'wait', polls: 3 },
      { op: 'wait', gateway: 6 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([
        null,
        42,
        '字符串',
        { isFromMe: true, guid: 'f-1', chatGuid: 'BB-F', text: '我自己' },
        { is_from_me: 1, guid: 'f-2', chatGuid: 'BB-F', text: '我方' },
        { guid: 'f-3', chatGuid: 'BB-F', text: '   ' },
        { guid: 'f-4', text: '没有会话' },
        { chatGuid: 'BB-F', text: '无guid可派发' },
        { guid: 'f-5', chatGuid: 'BB-F' },
        { guid: 'f-6', text: 'chats兜底', chats: ['BB-G'] },
        { id: 'i-9', chatGuid: 'BB-I', text: 'id兜底' },
      ]),
      { status: 500, json: {} },
      rows([{ id: 'i-9', chatGuid: 'BB-I', text: 'id兜底' }]),
    ],
    scriptedSends: [],
  },
  {
    // B3 + B10: the three response shapes, the string timestamp that advances
    // `after`, and a stale/non-numeric timestamp that does not.
    name: 'response-shapes-and-timestamp-advance',
    password: 'pw-5',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{ events: [] }, { events: [] }, { events: [] }, { events: [] }],
    steps: [
      { op: 'start' },
      { op: 'wait', polls: 3 },
      { op: 'wait', gateway: 8 },
      { op: 'stop', reason: 'done' },
    ],
    // The advancing timestamp must sit above the start-seeded `after`
    // (`now - 5`), so it uses a fixed 2030-era value: far from the recording
    // clock (never masked) and still comparable. The 0200 value stays below
    // the advanced 0300 mark, so the second poll does not advance again.
    scriptedPolls: [
      rows([
        { guid: 't-1', chatGuid: 'BB-T', text: '数组形态', dateCreated: 1600000000 },
        { guid: 't-2', chatGuid: 'BB-T2', text: '字符串时间', timestamp: '1900000300' },
      ]),
      { status: 200, json: { data: [{ guid: 't-3', chatGuid: 'BB-T3', text: 'data形态', time: 1900000200 }] } },
      { status: 200, json: { messages: [{ guid: 't-4', chatGuid: 'BB-T4', text: 'messages形态', dateCreated: '非数字' }] } },
    ],
    scriptedSends: [],
  },
  {
    // C1 + C2 + C3: `/new` alone confirms without a turn; `/new <text>` maps
    // to a fresh s_ session the follow-up reuses.
    name: 'new-command-session-mapper',
    password: 'pw-6',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [
      { events: [{ type: 'assistant_text_delta', text: '分析完成。' }] },
      { events: [{ type: 'assistant_text_delta', text: '继续完成。' }] },
    ],
    steps: [
      { op: 'start' },
      { op: 'wait', polls: 3 },
      { op: 'wait', gateway: 4 },
      { op: 'wait', sends: 3 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'n-1', chatGuid: 'BB-N', text: '/new' }]),
      rows([{ guid: 'n-2', chatGuid: 'BB-N', text: '/new 开始分析' }]),
      rows([{ guid: 'n-3', chatGuid: 'BB-N', text: '继续' }]),
    ],
    scriptedSends: [],
  },
  {
    // B14: while a turn holds the chat, a second row for the same chat logs
    // the skip and never queues a second turn.
    name: 'active-chat-skip',
    password: 'pw-7',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{ gateBefore: 'turn-1', events: [{ type: 'assistant_text_delta', text: '第一条回复。' }] }],
    steps: [
      { op: 'start' },
      { op: 'wait', gateway: 1 },
      { op: 'wait', logs: 2 },
      { op: 'release', gate: 'turn-1' },
      { op: 'wait', gateway: 2 },
      { op: 'wait', sends: 1 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 's-1', chatGuid: 'BB-S', text: '第一句' }]),
      rows([{ guid: 's-2', chatGuid: 'BB-S', text: '第二句' }]),
    ],
    scriptedSends: [],
  },
  {
    // A3 + A4 + B11: stop does not await the in-flight turn — it drains after
    // stop, and the final reply POST is suppressed by `running === false`.
    name: 'stop-drains-inflight-turn',
    password: 'pw-8',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{ gateBefore: 'turn-1', events: [{ type: 'assistant_text_delta', text: '收尾文本' }] }],
    steps: [
      { op: 'start' },
      { op: 'wait', gateway: 1 },
      { op: 'stop', reason: 'drain' },
      { op: 'release', gate: 'turn-1' },
      { op: 'wait', gateway: 2 },
      { op: 'wait', ticks: 50 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'd-1', chatGuid: 'BB-D2', text: '停下' }]),
    ],
    scriptedSends: [],
  },
  {
    // A5: a same-instance restart resets seenGuids/lastTimestamp, so the same
    // guid re-dispatches channel-side (upstream admission owns
    // no-re-execution; registered diff A8.3-CH-BLUEBUBBLES-004).
    name: 'restart-clears-guids',
    password: 'pw-9',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [
      { events: [{ type: 'assistant_text_delta', text: '第一次完成。' }] },
      { events: [{ type: 'assistant_text_delta', text: '第二次完成。' }] },
    ],
    steps: [
      { op: 'start' },
      { op: 'wait', gateway: 2 },
      { op: 'wait', sends: 1 },
      { op: 'stop', reason: 'restart' },
      { op: 'start' },
      { op: 'wait', polls: 2 },
      { op: 'wait', gateway: 4 },
      { op: 'wait', sends: 2 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'r-1', chatGuid: 'BB-R', text: '一次' }]),
      rows([{ guid: 'r-1', chatGuid: 'BB-R', text: '一次' }]),
    ],
    scriptedSends: [],
  },
  {
    // A6: a hot reload constructs a fresh instance — the old mapper's active
    // session does not carry over (`:general`).
    name: 'hot-reload-new-instance',
    password: 'pw-10',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [
      { events: [{ type: 'assistant_text_delta', text: '新会话回复。' }] },
      { events: [{ type: 'assistant_text_delta', text: '沿用回复。' }] },
    ],
    steps: [
      { op: 'start' },
      { op: 'wait', gateway: 2 },
      { op: 'wait', sends: 1 },
      { op: 'stop', reason: 'hot-reload' },
      { op: 'start', fresh: true },
      { op: 'wait', polls: 2 },
      { op: 'wait', gateway: 4 },
      { op: 'wait', sends: 2 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'h-1', chatGuid: 'BB-H', text: '/new 新题目' }]),
      rows([{ guid: 'h-2', chatGuid: 'BB-H', text: '接着说' }]),
    ],
    scriptedSends: [],
  },
  {
    // B12: an elicitation reply arrives as an answer (respondElicitation
    // record) and the held turn resumes to its final text.
    name: 'elicitation-answer-and-resume',
    password: 'pw-11',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{
      events: [{
        type: 'elicitation_request',
        requestId: 'q-1',
        questions: [{ header: '模式', question: '选择', options: [{ label: 'A', description: '甲' }, { label: 'B' }] }],
      }],
      holdForRespond: true,
      resume: [{ type: 'assistant_text_delta', text: '好的。' }],
    }],
    steps: [
      { op: 'start' },
      { op: 'wait', sends: 1 },
      { op: 'wait', polls: 2 },
      { op: 'wait', responds: 1 },
      { op: 'wait', gateway: 2 },
      { op: 'wait', sends: 2 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'e-1', chatGuid: 'BB-E', text: '选择一下' }]),
      rows([{ guid: 'e-2', chatGuid: 'BB-E', text: '1' }]),
    ],
    scriptedSends: [],
  },
  {
    // B12 (cancel): reply "0" cancels — respondElicitation carries the
    // cancelled answer and the channel confirms with 已取消。
    name: 'elicitation-cancel-zero',
    password: 'pw-12',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{
      events: [{
        type: 'elicitation_request',
        requestId: 'q-2',
        questions: [{ question: '选择', options: [{ label: 'A' }, { label: 'B' }] }],
      }],
      holdForRespond: true,
      resume: [],
    }],
    steps: [
      { op: 'start' },
      { op: 'wait', sends: 1 },
      { op: 'wait', polls: 2 },
      { op: 'wait', responds: 1 },
      { op: 'wait', gateway: 2 },
      { op: 'wait', sends: 2 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'e-3', chatGuid: 'BB-E2', text: '选一下' }]),
      rows([{ guid: 'e-4', chatGuid: 'BB-E2', text: '0' }]),
    ],
    scriptedSends: [],
  },
  {
    // B13 (allow once): reply "1" — permissionDecide allow/remember:false, the
    // confirmation text, no next prompt, and the resumed turn's reply.
    name: 'permission-allow-once',
    password: 'pw-13',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{
      events: [{
        type: 'permission_request',
        requestId: 'pr-1',
        toolName: 'Bash',
        payload: { command: 'ls' },
      }],
      holdForRespond: true,
      resume: [{ type: 'assistant_text_delta', text: '完成。' }],
    }],
    steps: [
      { op: 'start' },
      { op: 'wait', sends: 1 },
      { op: 'wait', polls: 2 },
      { op: 'wait', decides: 1 },
      { op: 'wait', gateway: 2 },
      { op: 'wait', sends: 3 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'p-1', chatGuid: 'BB-P', text: '执行' }]),
      rows([{ guid: 'p-2', chatGuid: 'BB-P', text: '1' }]),
    ],
    scriptedSends: [],
  },
  {
    // B13 (deny): reply "0" — permissionDecide deny with the fixed reason, the
    // 已拒绝 confirmation, no resumed reply.
    name: 'permission-deny-zero',
    password: 'pw-14',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{
      events: [{
        type: 'permission_request',
        requestId: 'pr-2',
        toolName: 'Bash',
        payload: { command: 'rm -rf /tmp/x' },
      }],
      holdForRespond: true,
      resume: [],
    }],
    steps: [
      { op: 'start' },
      { op: 'wait', sends: 1 },
      { op: 'wait', polls: 2 },
      { op: 'wait', decides: 1 },
      { op: 'wait', gateway: 2 },
      { op: 'wait', sends: 2 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'p-3', chatGuid: 'BB-P2', text: '执行' }]),
      rows([{ guid: 'p-4', chatGuid: 'BB-P2', text: '0' }]),
    ],
    scriptedSends: [],
  },
  {
    // C6: submitTurn rejects — the error log and the fixed retry text reply.
    name: 'submit-turn-error',
    password: 'pw-15',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{ events: [], error: 'boom' }],
    steps: [
      { op: 'start' },
      { op: 'wait', gateway: 1 },
      { op: 'wait', sends: 1 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'x-1', chatGuid: 'BB-X', text: '坏请求' }]),
    ],
    scriptedSends: [],
  },
  {
    // C7 (non-2xx): the POST fails — the HTTP error log with the body's
    // `message` field, and no retry.
    name: 'send-failure-logs-error',
    password: 'pw-16',
    serverUrl: `http://127.0.0.1:${MOCK_PORT}`,
    turns: [{ events: [{ type: 'assistant_text_delta', text: '回复正文' }] }],
    steps: [
      { op: 'start' },
      { op: 'wait', sends: 1 },
      { op: 'wait', logs: 2 },
      { op: 'stop', reason: 'done' },
    ],
    scriptedPolls: [
      rows([{ guid: 'w-1', chatGuid: 'BB-W', text: '发失败' }]),
    ],
    scriptedSends: [{ status: 500, json: { message: 'boom' } }],
  },
];

const EXPECTED_CASE_COUNT = 16;
assert.equal(definitions.length, EXPECTED_CASE_COUNT, '#1343 case count');

const output = resolve(process.argv[2]);
await mkdir(output, { recursive: true });
const cases: unknown[] = [];
for (const caseDef of definitions) {
  mock.armed = true;
  mock.name = caseDef.name;
  mock.polls = 0;
  mock.sends = 0;
  mock.pollQueries = [];
  mock.sendRecords = [];
  // The runner drains the queues with per-request indexes, so it needs its
  // own copy of the scripted slots.
  mock.scriptedPolls = [...caseDef.scriptedPolls];
  mock.scriptedSends = [...caseDef.scriptedSends];
  const recorded = await runCase(caseDef);
  mock.armed = false;
  // Every scripted slot must have been served; polls beyond the scripted ones
  // only ever see the dry default (an empty `{}` body) and are invisible in
  // the deduplicated pollQueries projection.
  assert.ok(mock.polls >= caseDef.scriptedPolls.length, `#1343 ${caseDef.name} left scripted polls unserved (${mock.polls}/${caseDef.scriptedPolls.length})`);
  assert.ok(mock.sends >= caseDef.scriptedSends.length, `#1343 ${caseDef.name} left scripted sends unserved (${mock.sends}/${caseDef.scriptedSends.length})`);
  // Quiescence: an in-flight poll aborted by the case's stop may still be
  // recording server-side, but it cannot touch the reply projections.
  await ticks(50);
  const settled = { sends: mock.sendRecords.length, gateway: recorded.gateway.length };
  await ticks(50);
  assert.equal(mock.sendRecords.length, settled.sends, `#1343 ${caseDef.name} sends still arriving after the case ended`);
  assert.equal(recorded.gateway.length, settled.gateway, `#1343 ${caseDef.name} gateway records still arriving after the case ended`);
  assert.equal(mock.strays.length, 0, `#1343 ${caseDef.name} strays: ${JSON.stringify(mock.strays)}`);
  const expected = {
    pollQueries: mock.pollQueries,
    sends: mock.sendRecords,
    gateway: recorded.gateway,
    responds: recorded.responds,
    decides: recorded.decides,
    logs: recorded.logs,
  };
  cases.push({
    name: caseDef.name,
    port: MOCK_PORT,
    password: caseDef.password,
    ...(caseDef.serverUrl !== undefined ? { serverUrl: caseDef.serverUrl } : {}),
    turns: caseDef.turns,
    steps: caseDef.steps,
    scripted: { polls: caseDef.scriptedPolls, sends: caseDef.scriptedSends },
    expected,
  });
  console.log(`case ${caseDef.name}: polls=${mock.pollQueries.length} sends=${expected.sends.length} gateway=${expected.gateway.length} logs=${expected.logs.length}`);
}
await new Promise<void>(done => server.close(() => done()));

const command = 'PDRS_BASELINE=/absolute/path/to/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" src/pilotdeck-gateway/tests/fixtures/channel-bluebubbles/generate.mts src/pilotdeck-gateway/tests/fixtures/channel-bluebubbles';
await writeFile(
  join(output, 'oracle.json'),
  JSON.stringify({ sourceCommit, sourceSha256, command, node: process.version, icu: process.versions.icu, expectedCaseCount: EXPECTED_CASE_COUNT, cases }, null, 2) + '\n',
  { flag: 'wx' },
);
console.log(`PASS #1343 bluebubbles: ${cases.length} real TS scenarios`);
process.exit(0);
