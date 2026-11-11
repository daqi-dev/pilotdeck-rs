// #1339 sms channel gold master: the real baseline SmsChannel closure runs
// against a mock Twilio REST client, a mock A7 gateway and a raw HTTP probe
// client. No network leaves loopback, no real account is touched. The event
// stream contract is modelled explicitly: after an elicitation/permission
// event the gateway holds the stream until respondElicitation/permissionDecide
// (the helper comments describe exactly that block), which is what lets the
// pending-answer replies arrive while the turn is still open.
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import { createRequire } from 'node:module';
import net from 'node:net';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1339 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1339 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, '#1339 PDRS_BASELINE');
assert.ok(process.argv[2], '#1339 output directory');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/adapters/channel/sms/SmsChannel.ts',
  'src/adapters/channel/sms/SmsSessionMapper.ts',
  'src/adapters/channel/sms/sms-render.ts',
  'src/adapters/channel/protocol/ImElicitationHelper.ts',
  'src/adapters/channel/protocol/ImPermissionHelper.ts',
  'src/adapters/channel/protocol/ImCronDelivery.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const require = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(require.resolve('tsx'))('esbuild');

const EXPECTED_CASE_COUNT = 61;
// #1901: the stalled-body cell can only end at Node's default `requestTimeout`
// (300 s), whose 408 lands on the 30 s connections-checker grid — worst case
// just under 330 s. The generator runs every case twice, so this cell alone
// costs ~660 s; the probe deadline and the fixture's regeneration budget in
// `tests/common/baseline.rs` both sit above that with room for a slow host.
// The deadline leaves two full checker intervals above the worst-case landing
// point: one interval is exactly the grid, so a loaded host could otherwise
// fail a healthy generation on event-loop lag alone.
const STALLED_PROBE_DEADLINE_MS = 480_000;
// #2499: the head-stall cells wait out the `headersTimeout` (60 s) whose 408
// also lands on the 30 s connections-checker grid — worst case just under
// delay + 90 s. The budget starts at the probe's own write, so the delay and
// the landing both sit inside it with a full checker interval of slack.
const HEAD_STALL_PROBE_BUDGET_MS = 150_000;

// ---------------------------------------------------------------------------
// real module graph, transpiled once, with `twilio` mocked at the require seam
// ---------------------------------------------------------------------------
const compiled = new Map<string, string>();
for (const source of sources) {
  compiled.set(
    source,
    transformSync(await readFile(join(baseline, source)), {
      loader: 'ts',
      format: 'cjs',
      target: 'es2022',
    }).code,
  );
}
const directoryOf = (source: string) => source.split('/').slice(0, -1).join('/');
const resolveFrom = (from: string, name: string): string => {
  if (!name.startsWith('./') && !name.startsWith('../')) return name;
  const base = join(directoryOf(from), name);
  for (const candidate of [base, `${base}.js`, base.replace(/\.js$/, '.ts')]) {
    const hit = sources.find((source) => source === candidate);
    if (hit) return hit;
  }
  throw new Error(`#1339 cannot resolve ${name} from ${from}`);
};
type Twilio = ReturnType<typeof makeTwilio>;
type Gateway = ReturnType<typeof makeGateway>;
function makeTwilio(script: any, sends: any[], gates: Map<string, any>) {
  const gate = (name: string) => {
    if (!gates.has(name)) {
      let release!: () => void;
      const promise = new Promise<void>((done) => { release = done; });
      gates.set(name, { promise, release });
    }
    return gates.get(name)!;
  };
  return (accountSid: string) => {
    if (accountSid === 'THROW') throw new Error('twilio init boom');
    return {
      messages: {
        create: async (params: any) => {
          sends.push({ body: params.body, from: params.from, to: params.to });
          if (script.sendBlockAt === sends.length) await gate('send-block').promise;
          if (script.sendFailAt?.includes(sends.length)) {
            throw new Error(script.sendError ?? 'mock send failure');
          }
        },
      },
    };
  };
}
function makeGateway(script: any, rpcs: any[], gates: Map<string, any>) {
  const gate = (name: string) => {
    if (!gates.has(name)) {
      let release!: () => void;
      const promise = new Promise<void>((done) => { release = done; });
      gates.set(name, { promise, release });
    }
    return gates.get(name)!;
  };
  return {
    async *submitTurn(request: any) {
      rpcs.push({ rpc: 'submitTurn', request: structuredClone(request) });
      if (script.turn?.gate) await gate(script.turn.gate).promise;
      if (script.turn?.error) throw new Error(script.turn.error);
      for (const event of script.turn?.events ?? []) yield structuredClone(event);
      if (script.turn?.holdAfter === 'elicit') await gate('elicited').promise;
      if (script.turn?.holdAfter === 'decide') await gate('decided').promise;
    },
    async respondElicitation(response: any) {
      rpcs.push({ rpc: 'respondElicitation', response: structuredClone(response) });
      gates.get('elicited')?.release();
    },
    async permissionDecide(decision: any) {
      rpcs.push({ rpc: 'permissionDecide', decision: structuredClone(decision) });
      if (script.permissionGate) await gate(script.permissionGate).promise;
      // A macrotask boundary keeps the resumed turn out of this reply's
      // microtask chain, so the confirmation and the turn tail are ordered
      // by the harness steps rather than by promise scheduling.
      await new Promise((done) => setImmediate(done));
      gates.get('decided')?.release();
      if (script.permissionError) throw new Error(script.permissionError);
      return { delivered: script.permissionDelivered ?? true };
    },
  };
}
function loadChannel(twilioFactory: Twilio, gateway: Gateway, mapperUuid: string, extra: Record<string, unknown>) {
  const cache = new Map<string, { exports: any }>();
  const load = (source: string): any => {
    if (cache.has(source)) return cache.get(source)!.exports;
    const module = { exports: {} as any };
    cache.set(source, module);
    const custom = (name: string) => {
      const target = resolveFrom(source, name);
      if (target === 'twilio') return twilioFactory;
      if (target === name) return require(name); // node:crypto, node:http
      return load(target);
    };
    new Function('require', 'module', 'exports', compiled.get(source)!)(
      custom, module, module.exports,
    );
    return module.exports;
  };
  const { SmsChannel } = load(sources[0]);
  const { SmsSessionMapper } = load(sources[1]);
  return new SmsChannel({
    extra,
    mapper: new SmsSessionMapper({ activeByChatId: {} }, () => mapperUuid),
  });
}

// ---------------------------------------------------------------------------
// raw HTTP probe client: one Connection: close request per step, full bytes
// ---------------------------------------------------------------------------
function connectProbe(port: number): Promise<any> {
  return new Promise((done, fail) => {
    const socket = net.connect(port, '127.0.0.1');
    socket.on('connect', () => { socket.destroy(); done({ refused: false }); });
    socket.on('error', (error: NodeJS.ErrnoException) => {
      if (error.code === 'ECONNREFUSED') done({ refused: true });
      else fail(error);
    });
    setTimeout(() => { socket.destroy(); fail(new Error('#1339 connect probe timeout')); }, 3000).unref();
  });
}
function rawRequest(port: number, raw: string, halfClose = false): Promise<any> {
  return new Promise((done, fail) => {
    const socket = net.connect(port, '127.0.0.1');
    const chunks: Buffer[] = [];
    socket.on('connect', () => {
      socket.write(raw);
      // The truncated-body steps half-close their write side after the
      // short body; the response is still readable on the same socket.
      if (halfClose) socket.end();
    });
    socket.on('data', (chunk: Buffer) => chunks.push(chunk));
    socket.on('close', () => {
      const text = Buffer.concat(chunks).toString('latin1');
      if (!text) {
        done({ closed: true });
        return;
      }
      const [head, ...rest] = text.split('\r\n\r\n');
      const status = Number.parseInt(head.split('\r\n')[0].split(' ')[1], 10);
      const contentType = /^content-type:\s*(.*)$/im.exec(head)?.[1] ?? null;
      done({ status, contentType, body: rest.join('\r\n\r\n') });
    });
    socket.on('error', (error: NodeJS.ErrnoException) => {
      if (error.code === 'ECONNREFUSED') done({ refused: true });
      else fail(error);
    });
    setTimeout(() => { socket.destroy(); fail(new Error('#1339 probe client timeout')); }, 3000).unref();
  });
}
// #1901: `rawRequest` gives up after 3 s, which is shorter than any Node
// `requestTimeout` the stalled-body cell waits out. The probe keeps the same
// recording semantics (first close wins, latin1 head/body split) but its own
// deadline, so a cell that only the server can finish is still recorded as a
// response instead of a client-side probe timeout.
function stalledRequest(port: number, raw: string, deadlineMs: number): Promise<any> {
  return new Promise((done, fail) => {
    const socket = net.connect(port, '127.0.0.1');
    const chunks: Buffer[] = [];
    socket.on('connect', () => { socket.write(raw); });
    socket.on('data', (chunk: Buffer) => chunks.push(chunk));
    socket.on('close', () => {
      const text = Buffer.concat(chunks).toString('latin1');
      if (!text) {
        done({ closed: true });
        return;
      }
      const [head, ...rest] = text.split('\r\n\r\n');
      const status = Number.parseInt(head.split('\r\n')[0].split(' ')[1], 10);
      const contentType = /^content-type:\s*(.*)$/im.exec(head)?.[1] ?? null;
      done({ status, contentType, body: rest.join('\r\n\r\n') });
    });
    socket.on('error', (error: NodeJS.ErrnoException) => {
      if (error.code === 'ECONNREFUSED') done({ refused: true });
      else fail(error);
    });
    setTimeout(() => {
      socket.destroy();
      fail(new Error(`#1901 stalled-probe timeout after ${deadlineMs} ms`));
    }, deadlineMs).unref();
  });
}
// #2499: a head-stall probe — connect, wait `delayMs`, then write the step's
// (possibly empty, always incomplete) head bytes and keep the write side open.
// Only the server's own `headersTimeout` can finish it: Node answers the
// 51-byte `408 Request Timeout` + `Connection: close` at the deadline's
// connections-checker grid landing (measured in
// `_tmp/20261107-issue-1955/smoke/ts-half-head.out` and
// `ts-zero-and-delayed.out`), which is why this probe carries the stalled
// deadline instead of `rawRequest`'s 3 s one. Recording semantics match
// `stalledRequest` (first close wins, latin1 head/body split) so the two
// timeout cells land in the oracle in the same shape.
function headStall(port: number, head: string, delayMs: number, deadlineMs: number): Promise<any> {
  return new Promise((done, fail) => {
    const socket = net.connect(port, '127.0.0.1');
    const chunks: Buffer[] = [];
    socket.on('connect', () => {
      setTimeout(() => { if (head) socket.write(head); }, delayMs).unref();
    });
    socket.on('data', (chunk: Buffer) => chunks.push(chunk));
    socket.on('close', () => {
      const text = Buffer.concat(chunks).toString('latin1');
      if (!text) {
        done({ closed: true });
        return;
      }
      const [head, ...rest] = text.split('\r\n\r\n');
      const status = Number.parseInt(head.split('\r\n')[0].split(' ')[1], 10);
      const contentType = /^content-type:\s*(.*)$/im.exec(head)?.[1] ?? null;
      done({ status, contentType, body: rest.join('\r\n\r\n') });
    });
    socket.on('error', (error: NodeJS.ErrnoException) => {
      if (error.code === 'ECONNREFUSED') done({ refused: true });
      else fail(error);
    });
    setTimeout(() => {
      socket.destroy();
      fail(new Error(`#2499 head-stall probe timeout after ${deadlineMs} ms`));
    }, delayMs + deadlineMs).unref();
  });
}
const REQUEST = (
  body: string | { repeat: string; count: number },
  headers: Record<string, string>,
  method: string,
  path: string,
  truncate = 0,
  // #1913: every pre-existing step was an HTTP/1.1 client, so the version was
  // implicit; the HTTP/1.0 cell records it explicitly instead of hardcoding
  // 1.1 and silently recording a different client than it claims.
  version: string = 'HTTP/1.1',
  // #2152: every pre-existing step carried a Host line; the missing-Host
  // cell records its absence explicitly instead of silently recording a
  // client the step never describes.
  omitHost = false,
) => {
  const bytes = typeof body === 'string' ? body : body.repeat.repeat(body.count);
  // `truncate` keeps the full Content-Length but sends fewer bytes, so the
  // server sees a request body cut short by the client (the half-close of
  // the `halfClose` step op finishes it).
  const wire = truncate > 0 ? bytes.slice(0, Math.max(0, bytes.length - truncate)) : bytes;
  // `Connection: close` is the replay default every pre-existing step
  // carries; the 1.0 cell spells its own `Connection: keep-alive`, so the
  // response is what must decide the close.
  const connection = 'Connection' in headers ? '' : 'Connection: close\r\n';
  const host = omitHost ? '' : 'Host: 127.0.0.1\r\n';
  return (
    `${method} ${path} ${version}\r\n${host}${connection}` +
    Object.entries(headers).map(([key, value]) => `${key}: ${value}\r\n`).join('') +
    `Content-Length: ${Buffer.byteLength(bytes)}\r\n\r\n${wire}`
  );
};
const FORM = { 'Content-Type': 'application/x-www-form-urlencoded' };

async function waitUntil(predicate: () => boolean, label: string) {
  const deadline = Date.now() + 3000;
  while (!predicate()) {
    if (Date.now() > deadline) throw new Error(`#1339 wait timeout: ${label}`);
    await new Promise((done) => setImmediate(done));
  }
}

// ---------------------------------------------------------------------------
// scenario definitions
// ---------------------------------------------------------------------------
interface Definition {
  name: string;
  port: number;
  env?: Record<string, string>;
  extra?: Record<string, unknown>;
  blockPort?: boolean;
  /**
   * How this case's `start()` is supposed to end (#1339 fix-r4).
   *
   * `listen-failure` — the channel reaches the listener and is refused there:
   * exactly one `sms: HTTP server failed:` error line (busy port, port out of
   * range, port NaN, unavailable host). `no-listen` — start fails before it
   * ever builds a listener. Default `ok` — the webhook must log its ready line
   * exactly once; a case that claims to start but never bound (an external
   * occupier took the port during the run) is refused instead of recorded.
   */
  startOutcome?: 'ok' | 'listen-failure' | 'no-listen';
  /** Add the TS-computed X-Twilio-Signature to every request step. */
  sign?: boolean;
  turn?: { gate?: string; error?: string; events?: any[]; holdAfter?: string };
  permissionDelivered?: boolean;
  permissionError?: string;
  permissionGate?: string;
  sendFailAt?: number[];
  sendBlockAt?: number;
  sendError?: string;
  steps: any[];
}

const definitions: Definition[] = [];
const scenario = (definition: Definition) => definitions.push(definition);
const port = (index: number) => 18800 + index;
const request = (step: any) => ({ op: 'request', ...step });
// #2499: a stalled request head — connect, wait `delayMs`, write half a head
// (or none at all, `head: ''`) and keep the write side open. The oracle field
// names stay the step's own (`head`, `delayMs`), replayed verbatim by the
// Rust side.
const headStallStep = (head: string, delayMs: number) => ({ op: 'head-stall', head, delayMs });
const form = (body: string, extra: Record<string, string> = {}) =>
  request({ body, headers: { ...FORM, ...extra }, method: 'POST', path: '/sms/incoming' });
const text = (message: string, extra: Record<string, string> = {}) =>
  form(`From=%2B15550001&Body=${encodeURIComponent(message)}`, extra);
const waitLog = (level: string, message: string) => ({ op: 'wait-log', level, text: message });
const waitSends = (count: number) => ({ op: 'wait-sends', count });
const waitRpcs = (count: number) => ({ op: 'wait-rpcs', count });
const release = (gate: string) => ({ op: 'release', gate });
const settle = () => ({ op: 'settle' });
const SETTLE = settle();
const stop = (reason?: string) => ({ op: 'stop', ...(reason === undefined ? {} : { reason }) });
const connect = () => ({ op: 'connect' });
const cron = (delivery: any) => ({ op: 'cron', delivery });
const TWIML = '<?xml version="1.0" encoding="UTF-8"?><Response></Response>';

const textDelta = (text: string) => ({ type: 'assistant_text_delta', text });
const toolFailure = (toolName: string) => ({ type: 'tool_call_finished', ok: false, toolName });
const toolFailureById = (toolCallId: string) => ({ type: 'tool_call_finished', ok: false, toolCallId });
const elicitation = (requestId: string) => ({
  type: 'elicitation_request',
  requestId,
  questions: [{
    question: '选择一个选项',
    header: '选项',
    options: [{ label: 'One', description: 'first' }, { label: 'Two' }],
  }],
});
const permission = (requestId: string, payload: unknown = { command: 'ls -la' }) => ({
  type: 'permission_request',
  requestId,
  toolName: 'Bash',
  payload,
});
// The same events without any `requestId` key: the helpers store the raw
// `event.requestId` (undefined), and `JSON.stringify` omits the key from the
// recorded gateway rpc arguments.
const permissionWithoutRequestId = () => ({
  type: 'permission_request',
  toolName: 'Bash',
  payload: { command: 'ls -la' },
});
const elicitationWithoutRequestId = () => ({
  type: 'elicitation_request',
  questions: [{
    question: '选择一个选项',
    header: '选项',
    options: [{ label: 'One', description: 'first' }, { label: 'Two' }],
  }],
});
const multiElicitation = (requestId: string) => ({
  type: 'elicitation_request',
  requestId,
  questions: [{
    question: '多选',
    header: '',
    options: [{ label: '甲' }, { label: '乙' }],
    multiSelect: true,
  }],
});
const cronDelivery = (over: Record<string, unknown>) => ({
  originChannelKey: 'sms',
  sessionKey: 'sms:chat=+15550001:general',
  text: '定时结果',
  ...over,
});
// The signing URL `SmsChannel.validateTwilioSignature` builds: the publicUrl
// branch, else host:port — always the configured path, never the request's
// query string.
function signingUrl(definition: Definition): string {
  const extra = (definition.extra ?? {}) as Record<string, unknown>;
  const publicUrl = String(extra.publicUrl ?? '');
  const path = String(extra.webhookPath ?? '/sms/incoming');
  if (publicUrl) return `${publicUrl.replace(/\/$/, '')}${path}`;
  return `http://127.0.0.1:${definition.port}${path}`;
}
// The effective request headers of one step: a `sign` definition adds the
// TS-side `X-Twilio-Signature`, computed with Node's own `createHmac` over the
// parsed form params, exactly like `validateTwilioSignature` expects. The
// signature is recorded into the oracle steps, so the Rust replay replays TS
// bytes instead of re-signing with the port's own implementation.
function effectiveHeaders(definition: Definition, step: any): Record<string, string> {
  const headers: Record<string, string> = { ...(step.headers ?? {}) };
  if (definition.sign && step.op === 'request' && typeof step.body === 'string') {
    const extra = (definition.extra ?? {}) as Record<string, unknown>;
    const token = String(
      extra.authToken ?? extra.apiKey ?? definition.env?.TWILIO_AUTH_TOKEN ?? extra.token ?? '',
    );
    const params: Record<string, string> = Object.fromEntries(new URLSearchParams(step.body));
    const data = signingUrl(definition)
      + Object.keys(params).sort().map((key) => key + params[key]).join('');
    headers['X-Twilio-Signature'] = createHmac('sha1', token).update(data).digest('base64');
  }
  return headers;
}
const effectiveSteps = (definition: Definition) =>
  definition.steps.map((step) => step.op === 'request'
    ? { ...step, headers: effectiveHeaders(definition, step) }
    : step);

scenario({
  name: 'start-missing-config', port: port(0), startOutcome: 'no-listen', steps: [
    connect(), stop(),
  ],
});
scenario({
  name: 'start-twilio-init-failed', port: port(1), startOutcome: 'no-listen',
  extra: { accountSid: 'THROW', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    connect(), stop(),
  ],
});
scenario({
  name: 'start-port-busy', port: port(2), blockPort: true, startOutcome: 'listen-failure',
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    connect(), stop(),
  ],
});
scenario({
  name: 'start-port-out-of-range', port: port(3), startOutcome: 'listen-failure',
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000', webhookPort: 65536 }, steps: [
    connect(), stop(),
  ],
});
scenario({
  name: 'start-port-nan', port: port(4), startOutcome: 'listen-failure',
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000', webhookPort: 'not-a-port' }, steps: [
    connect(), stop(),
  ],
});
scenario({
  name: 'start-ok', port: port(5),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('hello'), waitSends(1), stop('done'),
  ],
  turn: { events: [textDelta('reply text')] },
});
scenario({
  name: 'start-ok-public-url', port: port(6), sign: true,
  extra: {
    accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000',
    publicUrl: 'https://sms.example.com/',
  }, steps: [
    // The signature is computed over the publicUrl form of the signing URL,
    // so the publicUrl branch of `fullUrl` is exercised by a signed request.
    text('via public url'), waitSends(1), stop(),
  ],
  turn: { events: [textDelta('public url reply')] },
});
scenario({
  name: 'start-env-config', port: port(7),
  env: {
    TWILIO_ACCOUNT_SID: 'ACenv', TWILIO_AUTH_TOKEN: 'tokenv',
    TWILIO_PHONE_NUMBER: '+15550env', TWILIO_WEBHOOK_PORT: String(port(7)),
  }, steps: [
    text('hello'), waitSends(1), stop(),
  ],
  turn: { events: [textDelta('env reply')] },
});
scenario({
  name: 'start-extra-coercion', port: port(8),
  extra: {
    accountSid: 5, authToken: true, phoneNumber: ['+15550000'],
    apiKey: 'unused', webhookPort: `0x${port(8).toString(16)}`,
  }, steps: [
    text('hello'), waitSends(1), stop(),
  ],
  turn: { events: [textDelta('coerced')] },
});
scenario({
  name: 'auth-token-chain', port: port(9),
  extra: { accountSid: 'AC1', phoneNumber: '+15550000', token: 'tok9' },
  env: { TWILIO_AUTH_TOKEN: 'env-ignored' }, steps: [
    text('hello'), waitSends(1), stop(),
  ],
  turn: { events: [textDelta('key reply')] },
});
scenario({
  name: 'wrong-path-and-method', port: port(10),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({ body: '', headers: {}, method: 'GET', path: '/sms/incoming' }),
    request({ body: 'Body=hi&From=%2B1', headers: FORM, method: 'POST', path: '/other' }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('nope')] },
});
scenario({
  name: 'query-ignored', port: port(11),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({ body: 'From=%2B15550001&Body=hi', headers: FORM, method: 'POST', path: '/sms/incoming?x=1&y=2' }),
    waitSends(1), stop(),
  ],
  turn: { events: [textDelta('queried')] },
});
scenario({
  name: 'payload-too-large', port: port(12),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    // The megabyte bodies are recorded as a repeat spec; the raw bytes only
    // exist while the request is on the wire.
    form({ repeat: 'x', count: 1024 * 1024 + 1 }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  name: 'payload-exactly-1mib', port: port(13),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    form({ repeat: 'x', count: 1024 * 1024 }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  name: 'bad-json', port: port(14),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({ body: '{"Body":', headers: { 'Content-Type': 'application/json' }, method: 'POST', path: '/sms/incoming' }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  name: 'content-type-json-charset', port: port(15),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({
      body: '{"Body":"你好","From":"+15550001"}',
      headers: { 'Content-Type': 'application/json; charset=utf-8' }, method: 'POST', path: '/sms/incoming',
    }),
    waitSends(1), stop(),
  ],
  turn: { events: [textDelta('json 你好')] },
});
scenario({
  name: 'content-type-uppercase-not-json', port: port(16),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({
      body: '{"Body":"hi","From":"+15550001"}',
      headers: { 'Content-Type': 'APPLICATION/JSON' }, method: 'POST', path: '/sms/incoming',
    }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  name: 'bad-signature', port: port(17),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('hi', { 'X-Twilio-Signature': 'bogus' }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  name: 'valid-signature', port: port(18), sign: true,
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('signed'),
    waitSends(1), stop(),
  ],
  turn: { events: [textDelta('verified')] },
});
scenario({
  name: 'empty-from-and-whitespace-body', port: port(19),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    form('Body=hi'),
    form('From=%2B15550001&Body=%20%20%C2%A0%09'),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  name: 'form-plus-and-percent', port: port(20),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    form('From=+15550001&Body=1+2'),
    waitRpcs(1), stop(),
  ],
  turn: { events: [textDelta('plus')] },
});
scenario({
  name: 'dup-keys-last-wins', port: port(21),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    form('From=1&Body=a&Body=b&Body=c&Flag&Empty=&Pct=%E4%BD%A0%E5%A5%BD&Bad=%FF'),
    waitRpcs(1), stop(),
  ],
  turn: { events: [textDelta('dup')] },
});
scenario({
  name: 'non-string-json-body', port: port(22),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({ body: '{"Body":5,"From":77}', headers: { 'Content-Type': 'application/json' }, method: 'POST', path: '/sms/incoming' }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  name: 'turn-reply-fragments', port: port(23),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('multi'), waitSends(1), stop(),
  ],
  turn: {
    events: [
      textDelta('  think '),
      { type: 'assistant_thinking_delta' },
      { type: 'tool_call_started', toolName: 'Bash' },
      toolFailure('Bash'),
      { type: 'tool_call_finished', ok: true, toolName: 'Bash' },
      toolFailureById('call_7'),
      { type: 'error', message: 'soft failure' },
      { type: 'mystery_event' },
      textDelta(' done  '),
    ],
  },
});
scenario({
  name: 'turn-error', port: port(24),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('boom'), waitSends(1), stop(),
  ],
  turn: { error: 'gateway exploded' },
});
scenario({
  name: 'send-fail', port: port(25),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' },
  sendFailAt: [1], steps: [
    text('hello'), waitLog('error', 'sms: sendMessage failed: Error: mock send failure'), stop(),
  ],
  turn: { events: [textDelta('undelivered')] },
});
scenario({
  name: 'active-dedupe', port: port(26),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('first'), waitRpcs(1),
    text('second'), waitLog('info', 'sms: chat +15550001 already active, skipping'),
    release('turn'), waitSends(1),
    text('third'), waitRpcs(2), waitSends(2), stop(),
  ],
  turn: { gate: 'turn', events: [textDelta('first reply')] },
});
scenario({
  name: 'new-session', port: port(27),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('/new'), waitSends(1),
    text('in new'), waitSends(2),
    text('/new with a message'), waitSends(3),
    text('still new'), waitSends(4),
    text(' /new '), waitSends(5),
    text('after reset'), waitSends(6), stop(),
  ],
  turn: { events: [textDelta('r')] },
});
scenario({
  name: 'elicitation-cancel', port: port(28),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('pick one'), waitSends(1),
    text('0'), waitRpcs(2), waitSends(2), stop(),
  ],
  turn: { events: [elicitation('e1')], holdAfter: 'elicit' },
});
scenario({
  name: 'elicitation-answer', port: port(29),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('pick one'), waitSends(1),
    text('2'), waitRpcs(2), stop(),
  ],
  turn: { events: [elicitation('e1')], holdAfter: 'elicit' },
});
scenario({
  name: 'permission-allow-once', port: port(30),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('run it'), waitSends(1),
    text('1'), waitRpcs(2), waitSends(2), waitSends(3), stop(),
  ],
  turn: { events: [permission('p1'), textDelta(' after permit')], holdAfter: 'decide' },
});
scenario({
  name: 'permission-deny', port: port(31),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('run it'), waitSends(1),
    text('0'), waitRpcs(2), waitSends(2), stop(),
  ],
  turn: { events: [permission('p1')], holdAfter: 'decide' },
});
scenario({
  name: 'permission-allow-session', port: port(32),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('run it'), waitSends(1),
    text('2'), waitRpcs(2), waitSends(2), stop(),
  ],
  turn: { events: [permission('p1')], holdAfter: 'decide' },
});
scenario({
  name: 'permission-invalid-reply', port: port(33),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('run it'), waitSends(1),
    text('huh?'), waitSends(2),
    text('0'), waitRpcs(2), waitSends(3), stop(),
  ],
  turn: { events: [permission('p1')], holdAfter: 'decide' },
});
scenario({
  name: 'permission-decide-in-flight-status', port: port(34),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' },
  permissionGate: 'decide-block', steps: [
    text('run it'), waitSends(1),
    text('1'), waitRpcs(2),
    text('again'), waitSends(2),
    release('decide-block'), waitSends(3), stop(),
  ],
  turn: { events: [permission('p1')], holdAfter: 'decide' },
});
scenario({
  name: 'permission-prompt-sending-status', port: port(35),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' },
  sendBlockAt: 1, steps: [
    text('run it'), waitSends(1),
    text('again'), waitSends(2),
    release('send-block'), SETTLE, stop(),
  ],
  turn: { events: [permission('p1')], holdAfter: 'decide' },
});
scenario({
  name: 'permission-prompt-fail-retry', port: port(36),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' },
  sendFailAt: [1], steps: [
    text('run it'), waitLog('error', 'sms: sendMessage failed: Error: mock send failure'),
    text('again'), waitSends(3),
    text('1'), waitRpcs(2), waitSends(5), stop(),
  ],
  turn: { events: [permission('p1'), textDelta(' tail')], holdAfter: 'decide' },
});
scenario({
  name: 'permission-decide-not-delivered', port: port(37),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' },
  permissionDelivered: false, steps: [
    text('run it'), waitSends(1),
    text('1'), waitRpcs(2), waitSends(3), stop(),
  ],
  turn: { events: [permission('p1'), textDelta(' tail')], holdAfter: 'decide' },
});
scenario({
  name: 'permission-decide-error', port: port(38),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' },
  permissionError: 'decide boom', steps: [
    text('run it'), waitSends(1),
    text('1'), waitRpcs(2),
    waitLog('error', 'sms: permission answer error: Error: decide boom'),
    text('1'), waitRpcs(3), waitSends(2), stop(),
  ],
  turn: { events: [permission('p1')], holdAfter: 'decide' },
});
scenario({
  name: 'permission-confirmation-send-fail', port: port(39),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' },
  // The released turn tail overtakes the confirmation (the decide gate opens
  // before permissionDecide's return reaches handleIncoming), so the
  // confirmation is the third send. Its failure is the releaseAnswer path.
  sendFailAt: [3], steps: [
    text('run it'), waitSends(1),
    text('1'), waitRpcs(2),
    waitLog('error', 'sms: sendMessage failed: Error: mock send failure'),
    waitSends(3), stop(),
  ],
  turn: { events: [permission('p1'), textDelta(' tail')], holdAfter: 'decide' },
});
scenario({
  name: 'elicitation-multiselect', port: port(40),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('pick two'), waitSends(1),
    text('1，2'), waitRpcs(2), stop(),
  ],
  turn: { events: [multiElicitation('e2')], holdAfter: 'elicit' },
});
scenario({
  name: 'elicitation-answer-fallback', port: port(41),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('pick one'), waitSends(1),
    text('red'), waitRpcs(2), stop(),
  ],
  turn: { events: [elicitation('e1')], holdAfter: 'elicit' },
});
scenario({
  name: 'cron-delivery', port: port(42),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    cron(cronDelivery({})),
    cron(cronDelivery({ originChannelKey: 'telegram' })),
    cron(cronDelivery({ sessionKey: 'sms:chat=+15550001:s_not-a-uuid' })),
    cron(cronDelivery({ text: '第二次' })),
    waitSends(2), stop(),
  ],
});
scenario({
  name: 'cron-after-stop', port: port(43),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    stop('drained'),
    cron(cronDelivery({})),
    settle(),
  ],
});
scenario({
  name: 'auth-token-apikey', port: port(44), sign: true,
  extra: { accountSid: 'AC1', apiKey: 'k2', phoneNumber: '+15550000' },
  env: { TWILIO_AUTH_TOKEN: 'env-ignored' }, steps: [
    text('signed'),
    waitSends(1), stop(),
  ],
  turn: { events: [textDelta('api key reply')] },
});
scenario({
  name: 'start-ok-custom-path', port: port(45),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000', webhookPath: '/custom/hook' }, steps: [
    request({ body: 'From=%2B15550001&Body=path', headers: FORM, method: 'POST', path: '/custom/hook' }),
    request({ body: 'From=%2B15550001&Body=old', headers: FORM, method: 'POST', path: '/sms/incoming' }),
    waitSends(1), stop(),
  ],
  turn: { events: [textDelta('custom path')] },
});
scenario({
  name: 'stop-during-turn', port: port(46),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('hold'), waitRpcs(1),
    stop('drain'),
    release('turn'), settle(), stop(),
  ],
  turn: { gate: 'turn', events: [textDelta('after release')] },
});
scenario({
  name: 'valid-signature-query', port: port(47), sign: true,
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    // The signing URL is the configured path without any query string; a
    // request that carries one still validates and dispatches.
    request({ body: 'From=%2B15550001&Body=query', headers: FORM, method: 'POST', path: '/sms/incoming?x=1&y=2' }),
    waitSends(1), stop(),
  ],
  turn: { events: [textDelta('query signed')] },
});
scenario({
  name: 'json-body-number-empty-from', port: port(48),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    // `!from` short-circuits before `body.trim()`: the 200 TWIML is written,
    // the number Body never throws and nothing is dispatched.
    request({ body: '{"Body":5,"From":""}', headers: { 'Content-Type': 'application/json' }, method: 'POST', path: '/sms/incoming' }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  name: 'permission-missing-request-id', port: port(49),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('run it'), waitSends(1),
    text('1'), waitRpcs(2), waitSends(3), stop(),
  ],
  turn: { events: [permissionWithoutRequestId(), textDelta(' tail')], holdAfter: 'decide' },
});
scenario({
  name: 'elicitation-missing-request-id', port: port(50),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    text('pick one'), waitSends(1),
    text('1'), waitRpcs(2), stop(),
  ],
  turn: { events: [elicitationWithoutRequestId()], holdAfter: 'elicit' },
});
scenario({
  name: 'empty-webhook-host', port: port(51),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000', webhookHost: '' }, steps: [
    // Node `listen(port, "")` binds every address (the dual-stack `::`).
    text('hello'), waitSends(1), stop(),
  ],
  turn: { events: [textDelta('any address')] },
});
scenario({
  name: 'start-host-unavailable', port: port(52), startOutcome: 'listen-failure',
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000', webhookHost: '203.0.113.7' }, steps: [
    // A non-local webhookHost fails the bind with Node's EADDRNOTAVAIL line.
    connect(), stop(),
  ],
});
scenario({
  name: 'body-truncated', port: port(53),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    // Content-Length declares the full body but the wire stops 3 bytes
    // short, then the write side half-closes: the request read fails and
    // Node answers a bare 400 with an empty body.
    request({ body: 'From=%2B15550001&Body=truncated', headers: FORM, method: 'POST', path: '/sms/incoming', truncate: 3, halfClose: true }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  // #1901: a client declares `Content-Length: 64`, sends 8 bytes and keeps the
  // connection open. Node's default `requestTimeout` (300 s, the sms channel
  // never overrides it) answers with the 51-byte `408 Request Timeout` +
  // `Connection: close`; without a deadline the Rust port held the connection
  // task and the socket open forever. `truncate` keeps the declared length on
  // the wire and `stall` keeps the write side open, so only the server can
  // finish the request. This cell takes ~300 s per run (two runs per
  // generation).
  name: 'body-read-timeout', port: port(55),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({ body: 'x'.repeat(64), headers: FORM, method: 'POST', path: '/sms/incoming', truncate: 56, stall: true }),
    stop(),
  ],
});
scenario({
  // Not replayed by its own test: this case exists so the Rust port's
  // port-busy preflight check (fix-r1 P2 #13) has an oracle port it can
  // drive while a test holds it busy. Its own replay cell would race that
  // test for the port in the parallel suite, so the preflight test is the
  // only consumer.
  name: 'port-busy-preflight', port: port(54), startOutcome: 'listen-failure',
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000', webhookHost: '203.0.113.7' }, steps: [
    connect(), stop(),
  ],
});
scenario({
  // #1913: an HTTP/1.0 client asking to keep alive. Node answers its own
  // HTTP/1.1 status line with `Connection: close` on a closed socket, and
  // the body is close-delimited (no content-length — the same 1.0 shape the
  // webhook lane recorded for #1684, measured for this handler in
  // `_tmp/20261108-sms-residual/probe-sms-http10.out`). hyper answers its
  // own 1.0 status line (the accepted-diff entry A8.3-CH-SMS-015) and,
  // before the finalize, kept the socket open waiting out the keep-alive
  // the request carried.
  name: 'http10', port: port(56),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({
      body: 'From=%2B15550001&Body=hello',
      headers: { ...FORM, Connection: 'keep-alive' },
      method: 'POST', path: '/sms/incoming', version: 'HTTP/1.0',
    }),
    waitSends(1), stop(),
  ],
  turn: { events: [textDelta('1.0 reply')] },
});
scenario({
  // #2152: `FROB` is outside Node's `http.METHODS`, so llhttp rejects it at
  // the method token before any handler runs: the bare parser 400 (no Date,
  // zero body bytes), not the route guard's 404. The step records the method
  // verbatim; the Rust replay's `build_raw` reads it fail-closed, so the cell
  // cannot silently degrade to a served method.
  name: 'unknown-method', port: port(57),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({ body: '', headers: {}, method: 'FROB', path: '/sms/incoming' }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  // #2152: an HTTP/1.1 request with no `Host` header. `_http_server.js`
  // rejects it before any handler runs: a 400 that carries a Date and a
  // chunked empty body, unlike the bare parser 400 above. The step records
  // `omitHost: true`; a Host line would turn the cell into an admitted 200.
  name: 'missing-host', port: port(58),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    request({
      body: 'From=%2B15550001&Body=hostless',
      headers: FORM, method: 'POST', path: '/sms/incoming', omitHost: true,
    }),
    settle(), stop(),
  ],
  turn: { events: [textDelta('never')] },
});
scenario({
  // #2499 (#2026 decision B): the request-head deadline anchors at the
  // awaited head's first byte — Node's `on_message_begin` sets
  // `last_message_start_`. Connect, wait 1.5 s, then write half a request
  // head and keep the write side open; the answer is Node's 51-byte `408
  // Request Timeout` + `Connection: close` at the first connections-checker
  // grid point after the head's own +60 s deadline (measured with a 35 s
  // delay at 120.008 s in
  // `_tmp/20261107-issue-1955/smoke/ts-zero-and-delayed.out`). The recording
  // keeps the delay small — the oracle records only the semantic triple
  // `{status, contentType, body}`, the anchor/grid discrimination lives in
  // the Rust paused-clock cells (`head_timeout_tests.rs`). This cell costs
  // ~90 s per run (two runs per generation).
  name: 'headers-timeout-first-byte', port: port(59),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    headStallStep('POST /sms/incoming HTTP/1.1\r\nHost: 127.0.0.1\r\n', 1500),
    stop(),
  ],
});
scenario({
  // #2499 (#2026 decision B): a connection that never sends a byte — the
  // first head's deadline anchors at the connection's establishment (Node
  // arms `last_message_start_` at the accept), and the zero-byte first head
  // is answered with the same 408 (measured at 90.004 s,
  // `_tmp/20261107-issue-1955/smoke/ts-zero-and-delayed.out`).
  name: 'headers-timeout-connect-anchor', port: port(60),
  extra: { accountSid: 'AC1', authToken: 'tok', phoneNumber: '+15550000' }, steps: [
    headStallStep('', 0),
    stop(),
  ],
});
assert.equal(definitions.length, EXPECTED_CASE_COUNT, '#1339 generator case count');

// ---------------------------------------------------------------------------
// execution
// ---------------------------------------------------------------------------
async function run(definition: Definition) {
  const record: any = { logs: [], sends: [], rpcs: [], responses: [], unhandled: [], cronResults: [] };
  const unhandled = (error: unknown) => { record.unhandled.push(String(error)); };
  process.on('unhandledRejection', unhandled);
  const logger = {
    info: (message: string) => record.logs.push(['info', message]),
    warn: (message: string) => record.logs.push(['warn', message]),
    error: (message: string) => record.logs.push(['error', message]),
  };
  const gates = new Map<string, any>();
  const gateway = makeGateway(definition, record.rpcs, gates);
  const twilioFactory = makeTwilio(definition, record.sends, gates);
  const previousEnv: Record<string, string | undefined> = {};
  for (const key of ['TWILIO_ACCOUNT_SID', 'TWILIO_AUTH_TOKEN', 'TWILIO_PHONE_NUMBER', 'TWILIO_WEBHOOK_PORT']) {
    previousEnv[key] = process.env[key];
    delete process.env[key];
  }
  for (const [key, value] of Object.entries(definition.env ?? {})) process.env[key] = value;
  let blocker: net.Server | null = null;
  if (definition.blockPort) {
    blocker = net.createServer();
    await new Promise<void>((done) => blocker!.listen(definition.port, '127.0.0.1', done));
  }
  const extra = { webhookHost: '127.0.0.1', webhookPort: definition.port, ...definition.extra };
  const channel = loadChannel(twilioFactory, gateway, 'aaaaaaaa-1111-2222-3333-444444444444', extra);
  const handle = await (channel as any).start({ gateway, logger });
  // The recorded steps carry the effective (signed) headers; the replay then
  // reuses the TS-computed signature bytes instead of re-signing them.
  const steps = effectiveSteps(definition);
  try {
    for (const step of steps) {
      switch (step.op) {
        case 'request': {
          const raw = REQUEST(step.body, step.headers, step.method, step.path, step.truncate ?? 0, step.version, step.omitHost === true);
          record.responses.push(step.stall === true
            // #1901: only the server's own deadline can finish this request.
            ? await stalledRequest(definition.port, raw, STALLED_PROBE_DEADLINE_MS)
            : await rawRequest(definition.port, raw, step.halfClose === true));
          break;
        }
        case 'connect': {
          record.responses.push(await connectProbe(definition.port));
          break;
        }
        case 'head-stall': {
          const delayMs = step.delayMs ?? 0;
          // #2499: the probe budget starts at the step's own start (the
          // delay) so a head written late still has its full budget.
          record.responses.push(
            await headStall(definition.port, step.head ?? '', delayMs, HEAD_STALL_PROBE_BUDGET_MS),
          );
          break;
        }
        case 'wait-log':
          await waitUntil(() => record.logs.some(([level, text]: any[]) => level === step.level && text === step.text), `log ${step.text}`);
          break;
        case 'wait-sends':
          await waitUntil(() => record.sends.length >= step.count, `sends ${step.count}`);
          break;
        case 'wait-rpcs':
          await waitUntil(() => record.rpcs.length >= step.count, `rpcs ${step.count}`);
          break;
        case 'release': gates.get(step.gate)?.release(); break;
        case 'settle':
          await new Promise((done) => setImmediate(() => done(null)));
          await new Promise((done) => setTimeout(() => done(null), 25));
          break;
        case 'stop': await handle.stop(step.reason); break;
        case 'cron': record.cronResults.push(await channel.deliverCronResult(step.delivery)); break;
        default: throw new Error(`#1339 unknown step ${step.op}`);
      }
    }
  } finally {
    await handle.stop('cleanup');
    process.off('unhandledRejection', unhandled);
    for (const [key, value] of Object.entries(previousEnv)) {
      if (value === undefined) delete process.env[key];
      else process.env[key] = value;
    }
    await new Promise<void>((done) => (blocker ? blocker.close(() => done()) : done()));
    // Let the final stop's async tail land in the record.
    await new Promise((done) => setImmediate(() => done(null)));
    await new Promise((done) => setTimeout(() => done(null), 25));
  }
  return record;
}

const out = resolve(process.argv[2]!);
await mkdir(out, { recursive: true });
// #1339 fix-r2 (P1): the replay suite binds these same fixed ports, and this
// generator writes the committed oracle. A listener left on one of them would
// be recorded as the expectation (a refused probe becomes `{"refused":false}`,
// a taken port becomes an EADDRINUSE start error), and both determinism runs
// would hit it, so the two-run check cannot catch it. Refuse to record while
// any case port is taken instead of publishing a corrupted oracle.
//
// This probe is a fast path only, and it covers IPv4 loopback alone: the
// authoritative guard is the `startOutcome` check after each run, which reads
// the channel's own logs whatever address family it binds
// (`empty-webhook-host` binds the dual-stack `::`).
//
// #1339 fix-r7 (P2, minimax sibling #3): ask by *connecting*, not by binding.
// libuv sets `SO_REUSEADDR` on `listen`, and on macOS/BSD such a socket binds a
// concrete address even while a wildcard listener owns the port — the same
// blind spot the Rust-side guard dropped in fix-r6.
//
// #1339 fix-r8 (P2, Codex): only `ECONNREFUSED` proves the port free. Any other
// error (`EADDRNOTAVAIL`, resource exhaustion, ...) and a probe that never
// settles say nothing about the port, so they abort the recording instead of
// being read as "free".
const portIsFree = (port: number) =>
  new Promise<boolean>((done, fail) => {
    const probe = net.connect(port, '127.0.0.1');
    const timer = setTimeout(() => {
      probe.destroy();
      fail(new Error(`#1339 port ${port} probe timed out; refusing to record an oracle`));
    }, 1000);
    probe.once('connect', () => {
      clearTimeout(timer);
      probe.destroy();
      done(false);
    });
    probe.once('error', (error: NodeJS.ErrnoException) => {
      clearTimeout(timer);
      probe.destroy();
      if (error.code === 'ECONNREFUSED') {
        done(true);
        return;
      }
      fail(new Error(`#1339 port ${port} probe failed with ${error.code ?? error.message}; refusing to record an oracle`));
    });
  });
for (const definition of definitions) {
  assert.ok(
    await portIsFree(definition.port),
    `#1339 port ${definition.port} for ${definition.name} is already in use; refusing to record an oracle from a taken port`,
  );
}
const cases: any[] = [];
// #1339 fix-r3 (P1): the preflight above only proves the port was free at t0;
// a listener that takes it during the recording run makes `start` fail with
// EADDRINUSE, which the channel records as an error instead of throwing — the
// probe requests then reach the occupant and both determinism runs record the
// same contamination, so `deepEqual` cannot tell it from a good case.
//
// #1339 fix-r4 (P1): the first cut of this guard keyed on `blockPort` alone and
// therefore rejected the four cases that fail *by design* at the listener
// (`start-port-out-of-range` / `start-port-nan` / `start-host-unavailable` /
// `port-busy-preflight` all record `sms: HTTP server failed:`), aborting the
// whole recording. Each definition now declares its `startOutcome`, and the
// `ok` branch proves the listener actually came up (its ready line) instead of
// only proving that no failure line was logged.
const LISTEN_FAILURE = 'sms: HTTP server failed:';
const WEBHOOK_READY = 'sms: Twilio webhook ';
const textOf = (entry: [string, string]) => entry[1];
for (const definition of definitions) {
  const first = await run(definition);
  const second = await run(definition);
  assert.deepEqual(second, first, `#1339 case ${definition.name} is not deterministic`);
  const outcome = definition.startOutcome ?? 'ok';
  for (const [index, record] of [first, second].entries()) {
    const logs = record.logs as [string, string][];
    const failed = logs.filter(([level, text]) => level === 'error' && text.startsWith(LISTEN_FAILURE));
    const ready = logs.filter(([, text]) => text.startsWith(WEBHOOK_READY));
    const where = `#1339 case ${definition.name} (${outcome}, run ${index + 1})`;
    // #1339 fix-r5 (P1): a case that never served our webhook must also prove
    // its probe was refused. Without this, a third party that takes the port
    // during either run answers with `refused: false`, both runs agree, and the
    // contamination is recorded — the log checks above cannot see it.
    // `start-port-busy` is the one exception: its blocker owns the port by
    // design, so it answers the probe and the case records `refused: false`.
    const responses = record.responses as { refused?: boolean }[];
    if (outcome !== 'ok' && !definition.blockPort) {
      assert.ok(
        responses.length > 0 && responses.every((response) => response?.refused === true),
        `${where} must record only refused probes, got ${JSON.stringify(responses)}; a foreign listener answered the webhook`,
      );
    }
    if (outcome === 'listen-failure') {
      assert.equal(failed.length, 1, `${where} must record exactly one listen failure: ${failed.map(textOf).join('; ') || 'none'}`);
      assert.equal(ready.length, 0, `${where} must not log a live webhook: ${ready.map(textOf).join('; ')}`);
    } else if (outcome === 'no-listen') {
      assert.equal(failed.length, 0, `${where} must not reach the listener: ${failed.map(textOf).join('; ')}`);
      assert.equal(ready.length, 0, `${where} must not log a live webhook: ${ready.map(textOf).join('; ')}`);
    } else {
      assert.deepEqual(
        failed, [],
        `${where} never bound its port: ${failed.map(textOf).join('; ')}; refusing to record an oracle the webhook never reached`,
      );
      assert.equal(
        ready.length, 1,
        `${where} must log its webhook ready line exactly once, got ${ready.length}; refusing to record an oracle the webhook never reached`,
      );
    }
  }
  cases.push({
    name: definition.name,
    port: definition.port,
    env: definition.env ?? {},
    extra: { webhookHost: '127.0.0.1', webhookPort: definition.port, ...definition.extra },
    sign: definition.sign ?? false,
    turn: definition.turn ?? null,
    permissionDelivered: definition.permissionDelivered ?? true,
    permissionError: definition.permissionError ?? null,
    permissionGate: definition.permissionGate ?? null,
    sendFailAt: definition.sendFailAt ?? [],
    sendBlockAt: definition.sendBlockAt ?? null,
    sendError: definition.sendError ?? null,
    steps: effectiveSteps(definition),
    expected: first,
  });
  console.log(`#1339 recorded ${definition.name}`);
}
const oracle = { sourceCommit, sourceSha256, node: process.version, icu: process.versions.icu, expectedCaseCount: EXPECTED_CASE_COUNT, cases };
const target = join(out, 'oracle.json.tmp');
await writeFile(target, JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
const { rename } = await import('node:fs/promises');
await rename(target, join(out, 'oracle.json'));
console.log(`#1339 wrote ${join(out, 'oracle.json')} with ${cases.length} cases`);
