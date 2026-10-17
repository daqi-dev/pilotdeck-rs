import assert from 'node:assert/strict';
import { appendFileSync, mkdirSync, writeFileSync, readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import net from 'node:net';
import { parseArgs } from 'node:util';

const { values: args } = parseArgs({ options: Object.fromEntries(['url', 'token', 'scenario', 'out'].map((key) => [key, { type: 'string' }])) });
for (const key of ['url', 'token', 'scenario', 'out']) assert(args[key], `--${key} required`);
const endpoint = new URL(args.url!);
assert(endpoint.protocol === 'ws:' && endpoint.hostname === '127.0.0.1' && endpoint.pathname === '/ws'
  && Number(endpoint.port) >= 28000 && Number(endpoint.port) <= 28999, 'only ws://127.0.0.1:28xxx/ws is permitted');
const out = args.out!;
mkdirSync(out, { recursive: true });
const save = (name: string, value: unknown) => writeFileSync(`${out}/${name}`, JSON.stringify(value, null, 2) + '\n');
let seq = 0, connection = 0;
const sockets: WebSocket[] = [];
const record = (row: object) => appendFileSync(`${out}/frames.jsonl`, JSON.stringify({ seq: ++seq, monotonicNs: process.hrtime.bigint().toString(), ...row }) + '\n');
const Native = globalThis.WebSocket;
globalThis.WebSocket = class extends Native {
  id = ++connection;
  constructor(url: string | URL, protocols?: string | string[]) {
    super(url, protocols); sockets.push(this);
    this.addEventListener('message', (event) => this.record('receive', String(event.data)));
    this.addEventListener('close', (event) => record({ connection: this.id, direction: 'receive', close: { code: event.code, reason: event.reason } }));
  }
  record(direction: string, raw: string) {
    let frame: unknown; try { frame = JSON.parse(raw); } catch { frame = raw; }
    record({ connection: this.id, direction, raw, frame });
  }
  send(data: any) { this.record('send', String(data)); super.send(data); }
};
const baseline = process.env.PDRS_BASELINE;
if (!baseline) throw new Error('PDRS_BASELINE is required; run via run_round.py (#187: no hard-coded fallback)');
// Use the actual public source entry point, resolved by the baseline tsx loader.
const { GatewayWsClient, RemoteGateway, PILOTDECK_GATEWAY_PROTOCOL_VERSION: version } =
  await import(pathToFileURL(`${baseline}/src/gateway/index.js`).href);
const project = process.env.GP_PROJECT ?? process.cwd();
const sessionKey = 'web:gateway-parity-text';
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(test: () => any, label: string, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { const value = test(); if (value) return value; await sleep(20); }
  throw new Error(`timeout: ${label}`);
}
async function connect() {
  const client = new GatewayWsClient({ url: args.url, token: args.token, clientName: 'web' });
  await client.connect(); return client;
}
async function close(client: any) {
  client.close(); await until(() => sockets.every((ws) => ws.readyState === Native.CLOSED), 'closed sockets');
}
const hello = { type: 'hello', protocolVersion: version, clientName: 'web', clientVersion: '0.1.0', token: args.token };
const results: any[] = [];

async function rawProbe(kind: string) {
  const url = new URL(args.url!);
  const id = ++connection;
  const socket = net.connect(Number(url.port), '127.0.0.1');
  let buffer = Buffer.alloc(0), upgraded = false, ended = false;
  const replies: any[] = [];
  socket.on('error', () => {});
  socket.on('close', () => { ended = true; record({ connection: id, direction: 'receive', transportEnd: true }); });
  socket.on('data', (chunk) => {
    buffer = Buffer.concat([buffer, chunk]);
    if (!upgraded) {
      const end = buffer.indexOf('\r\n\r\n'); if (end < 0) return;
      assert(buffer.subarray(0, end).toString().startsWith('HTTP/1.1 101'));
      buffer = buffer.subarray(end + 4); upgraded = true;
    }
    while (buffer.length >= 2) {
      const opcode = buffer[0] & 15, fin = !!(buffer[0] & 128), rsv = buffer[0] & 0x70,
        masked = !!(buffer[1] & 128), lengthCode = buffer[1] & 127;
      let size = lengthCode, offset = 2;
      if (size === 126) { if (buffer.length < 4) return; size = buffer.readUInt16BE(2); offset = 4; }
      if (size === 127) { if (buffer.length < 10) return; size = Number(buffer.readBigUInt64BE(2)); offset = 10; }
      const maskOffset = offset; offset += masked ? 4 : 0;
      if (buffer.length < offset + size) return;
      const wireHex = buffer.subarray(0, offset + size).toString('hex');
      const payload = Buffer.from(buffer.subarray(offset, offset + size));
      if (masked) for (let i = 0; i < payload.length; i++) payload[i] ^= buffer[maskOffset + i % 4];
      buffer = buffer.subarray(offset + size);
      let frame: any = payload.toString();
      if (opcode === 1) { try { frame = JSON.parse(frame); } catch {} }
      if (opcode === 8) frame = { code: size >= 2 ? payload.readUInt16BE() : 1005, reason: payload.subarray(2).toString() };
      replies.push({ opcode, frame });
      record({ connection: id, direction: 'receive', wire: { opcode, fin, rsv, masked, lengthCode, payloadLength: size }, wireHex, frame });
    }
  });
  try {
    await until(() => socket.readyState === 'open' || ended, 'raw connect');
    assert(!ended, 'raw socket must connect');
    socket.write(`GET /ws HTTP/1.1\r\nHost: 127.0.0.1:${url.port}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n`); // RFC 6455 §1.3 公开示例 nonce，非密钥 gitleaks:allow
    await until(() => upgraded, 'raw upgrade');
    function send(opcode: number, payload: Buffer, rsv = 0, masked = true) {
      const extended = payload.length >= 126;
      const header = Buffer.alloc(extended ? 4 : 2);
      header[0] = 0x80 | rsv | opcode; header[1] = (masked ? 0x80 : 0) | (extended ? 126 : payload.length);
      if (extended) header.writeUInt16BE(payload.length, 2);
      const mask = Buffer.from([1, 2, 3, 4]);
      const data = Buffer.from(payload); if (masked) for (let i = 0; i < data.length; i++) data[i] ^= mask[i % 4];
      const wire = Buffer.concat(masked ? [header, mask, data] : [header, data]);
      record({ connection: id, direction: 'send', wire: { opcode, fin: true, rsv, masked, lengthCode: extended ? 126 : payload.length, payloadLength: payload.length }, wireHex: wire.toString('hex'), frame: payload.toString() });
      socket.write(wire);
    }
    if (kind === 'continuation') {
      send(0, Buffer.from('{ignored}'));
      send(1, Buffer.from(JSON.stringify(hello)));
    } else if (kind === 'client-close') {
      send(8, Buffer.from([0x11, 0x5c])); // 4444; TS replies 1000, empty reason.
    } else send(1, Buffer.from(JSON.stringify(hello)), kind === 'rsv' ? 0x40 : 0, kind !== 'unmasked');
    if (kind === 'rsv' || kind === 'continuation') {
      await until(() => replies.some((reply) => reply.frame?.type === 'hello_ok') || ended, kind);
      assert(replies.some((reply) => reply.frame?.type === 'hello_ok'), `${kind}: TS accepts/ignores frame header`);
      send(8, Buffer.from([3, 232])); await until(() => ended, 'raw close');
    } else if (kind === 'unmasked') {
      // CLI catches the parser exception and closes its listener; the offending
      // socket can linger during graceful shutdown. Observe that externally.
      await sleep(200);
      let healthy = true;
      try { const response = await fetch(args.url!.replace('ws:', 'http:').replace('/ws', '/health'), { signal: AbortSignal.timeout(500) }); healthy = response.status === 200; }
      catch { healthy = false; }
      record({ connection: id, direction: 'receive', exceptionalShutdown: !healthy });
      assert.equal(healthy, false, 'unmasked: TS exceptional shutdown closes listener');
      assert.equal(replies.length, 0, 'unmasked: no protocol reply');
      socket.destroy(); await until(() => ended, 'unmasked local cleanup');
    } else {
      await until(() => ended, kind);
      if (kind === 'client-close') assert.deepEqual(replies.find((item) => item.opcode === 8)?.frame, { code: 1000, reason: '' });
    }
    return { kind, replies, ended };
  } finally { socket.destroy(); }
}

try {
  if (args.scenario === 'auth') {
    for (const [frame, expected] of [
      [{ type: 'request', id: 'before-hello', method: 'describe_server', params: {} }, 4001],
      [{ ...hello, protocolVersion: 'wrong' }, 4001], [{ ...hello, token: 'wrong' }, 4003], ['{invalid json', 4002],
    ] as const) {
      const ws = new WebSocket(args.url!);
      const closed = new Promise<any>((resolve) => ws.addEventListener('close', (event) => resolve({ code: event.code, reason: event.reason })));
      await until(() => ws.readyState === Native.OPEN, 'auth open'); ws.send(typeof frame === 'string' ? frame : JSON.stringify(frame));
      const result = await closed; assert.equal(result.code, expected); results.push(result);
    }
    const client = await connect(); assert(client.interactionBinding); await close(client);
  } else if (args.scenario === 'turn') {
    const client = await connect(), remote = new RemoteGateway(client);
    const events = [];
    for await (const event of remote.submitTurn({ sessionKey, channelKey: 'web', projectKey: project,
      message: 'GP_TEXT', runId: 'run-gateway-parity-text' })) events.push(event);
    assert(events.some((event) => event.type === 'turn_completed'));
    assert(JSON.stringify(events).includes('GP_MOCK_OK'));
    assert(!events.some((event) => event.type === 'error'));
    results.push({ events }); await close(client);
  } else if (args.scenario === 'readonly') {
    const client = await connect();
    const input = { sessionKey, projectKey: project };
    const cases = [ ['native_archive_manifest', input], ['native_archive_entries', { ...input, limit: 100 }],
      ['native_archive_artifact', { ...input, artifactName: 'fixture.txt' }], ['project_files_list', { projectKey: project }],
      ['project_file_read', { projectKey: project, path: 'note.txt' }], ['model_catalog_list', { projectKey: project }],
      ['session_model_get', input], ['supported_agents', {}], ['list_projects', {}], ['describe_project', { projectKey: project }] ];
    for (const [method, params] of cases) {
      const result = await client.request(method, params);
      assert(result !== undefined, `${method} must succeed`); results.push({ method, result });
    }
    assert(JSON.stringify(results).includes('gateway parity fixture')); await close(client);
  } else if (args.scenario === 'reconnect') {
    const first = await connect(); const previousBinding = first.interactionBinding;
    const key = 'web:gateway-parity-reconnect', events: any[] = [];
    const stream = (async () => { try { for await (const event of first.stream('submit_turn', { sessionKey: key,
      channelKey: 'web', projectKey: project, message: 'GP_QUESTION', runId: 'run-gateway-parity-reconnect' })) events.push(event); } catch {} })();
    const question = await until(() => events.find((event) => event.type === 'elicitation_request'), 'question');
    await close(first); await stream;
    const next = await connect();
    const replay = await next.request('reconnect_interaction', { sessionKey: key, previousBinding });
    assert.equal(replay.outcome, 'reconnected'); assert(replay.requests.some((item: any) => item.requestId === question.requestId));
    const active = await next.request('active_turn_snapshot', { sessionKey: key, includeEvents: true }); assert(active.active);
    const answer = await next.request('elicitation_respond', { requestId: question.requestId,
      answer: { type: 'answered', answers: { 'Continue gateway parity?': 'yes' } } });
    assert.equal(answer.delivered, false, 'known baseline post-reconnect rejection');
    await next.request('abort_turn', { sessionKey: key });
    results.push({ events, replay, active, answer, boundary: 'saved binding protocol replay; UI automatic replay remains untested' });
    await close(next);
  } else if (args.scenario === 'permission') {
    const client = await connect(), events: any[] = [];
    const key = 'web:gateway-parity-permission';
    const stream = (async () => { for await (const event of client.stream('submit_turn', { sessionKey: key, channelKey: 'web',
      projectKey: project, message: 'GP_PERMISSION', runId: 'run-gateway-parity-permission', permissionMode: 'default' })) events.push(event); })();
    const question = await until(() => events.find((event) => event.type === 'permission_request'), 'permission request');
    const answer = await client.request('permission_decide', { sessionKey: key, requestId: question.requestId, decision: 'deny' });
    assert.equal(answer.delivered, true); await stream;
    assert(events.some((event) => event.type === 'turn_completed'));
    results.push({ events, answer }); await close(client);
  } else if (args.scenario === 'writes') {
    const client = await connect();
    const cron = await client.request('cron_create', { projectKey: project, sessionKey: 'web:gateway-parity-cron',
      channelKey: 'web', message: 'GP_CRON', schedule: { type: 'once', runAt: '2099-01-01T00:00:00.000Z' } });
    assert(cron.task.taskId); results.push(cron);
    const content = '---\nname: parity-fixture\ndescription: gateway parity fixture\n---\nFixture body\n';
    const skill = await client.request('skill_create', { scope: 'project', slug: 'parity-fixture', projectKey: project, content });
    assert.equal(skill.ok, true);
    const written = await client.request('skill_write', { scope: 'project', slug: 'parity-fixture', projectKey: project, content: content + 'Written\n' });
    assert.equal(written.ok, true); results.push(skill, written); await close(client);
  } else if (args.scenario === 'http') {
    const base = args.url!.replace('ws:', 'http:').replace(/\/ws$/, '');
    const headers = { authorization: `Bearer ${args.token}` };
    async function fetchRecord(path: string, options: any = {}, expected = 200) {
      const start = performance.now();
      const response = await fetch(base + path, { headers, ...options, signal: AbortSignal.timeout(10000) });
      const chunks = [], buffers: Buffer[] = [];
      for await (const chunk of response.body!) {
        const buffer = Buffer.from(chunk); buffers.push(buffer);
        chunks.push({ elapsedMs: performance.now() - start, text: buffer.toString(), base64: buffer.toString('base64') });
      }
      const cookies = response.headers.getSetCookie();
      const responseHeaders = { ...Object.fromEntries(response.headers), ...(cookies.length ? { 'set-cookie': cookies } : {}) };
      const row = { path, status: response.status, headers: responseHeaders, chunks, body: Buffer.concat(buffers).toString() };
      assert.equal(row.status, expected, path); results.push(row); return row;
    }
    for (const path of ['/health', '/auth/local-token', '/api/web/projects',
      `/api/web/projects/${encodeURIComponent(project)}/files/read?path=note.txt`]) await fetchRecord(path);
    await fetchRecord(`/api/web/projects/${encodeURIComponent(project)}/files/write`, { method: 'POST',
      headers: { ...headers, 'content-type': 'application/json' }, body: JSON.stringify({ path: 'written.txt', content: 'HTTP written fixture\n' }) });
    assert.equal(readFileSync(`${project}/written.txt`, 'utf8'), 'HTTP written fixture\n');
    await fetchRecord('/api/web/projects', { headers: {} }, 401);
    for (const kind of ['sse', 'ndjson']) {
      const row = await fetchRecord(`/__ruler/stream/${kind}`);
      assert(row.chunks.length >= 2 && row.chunks.at(-1)!.elapsedMs - row.chunks[0].elapsedMs >= 300, 'incremental transport stream');
    }
    save('http.json', results);
  } else if (args.scenario === 'quirks') {
    // Each probe survives another's failure, so all four boundaries are observed.
    for (const kind of ['rsv', 'continuation', 'client-close', 'unmasked']) {
      try { results.push({ ...await rawProbe(kind), ok: true }); }
      catch (error: any) { results.push({ kind, ok: false, error: error.message }); }
    }
    assert(results.every((item) => item.ok), 'WS quirk oracles');
  } else throw new Error(`unknown scenario ${args.scenario}`);
  save('result.json', { scenario: args.scenario, status: 'PASS', results });
} catch (error: any) {
  save('result.json', { scenario: args.scenario, status: 'FAIL', error: error.message, results }); process.exitCode = 1;
} finally {
  for (const ws of sockets) if (ws.readyState === Native.OPEN) ws.close();
  await until(() => sockets.every((ws) => ws.readyState === Native.CLOSED), 'final close', 3000).catch(() => {});
  save('complete.json', { scenario: args.scenario, records: seq, connections: connection, complete: true });
}
