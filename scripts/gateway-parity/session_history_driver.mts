// A7 U9/U10 live consumer. Inputs come from the existing independent TS
// history bundle; both hosts expose the same official GatewayWsClient path.
import assert from 'node:assert/strict';
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

const [baseline, fixture, project, home, address, token, out] = process.argv.slice(2);
assert(baseline && fixture && project && home && address && token && out);
const endpoint = new URL(address);
assert(endpoint.protocol === 'ws:' && endpoint.hostname === '127.0.0.1' && endpoint.pathname === '/ws'
  && Number(endpoint.port) >= 28000 && Number(endpoint.port) <= 28999);
mkdirSync(out, { recursive: true });
const save = (name: string, value: unknown) => writeFileSync(join(out, name), JSON.stringify(value, null, 2) + '\n', { flag: 'wx' });
const { GatewayWsClient } = await import(pathToFileURL(join(baseline, 'src/gateway/index.js')).href);
const { getPilotProjectChatDir } = await import(pathToFileURL(join(baseline, 'src/pilot/paths.ts')).href);
const recorded = JSON.parse(readFileSync(fixture, 'utf8'));
const seed = recorded.cases.find((row: any) => row.id === 'child');
assert(seed?.parent?.length === 1 && seed.child.length > 0 && seed.result.messages.length > 0);
const { sessionKey, subagentId } = seed.input;
assert.match(sessionKey, /^[a-z0-9_-]+$/);
assert.match(subagentId, /^[a-z0-9_-]+$/);
const chat = getPilotProjectChatDir(project, home);
const childDir = join(chat, sessionKey, 'subagents');
mkdirSync(childDir, { recursive: true });
const parentFile = join(chat, `${sessionKey}.jsonl`);
const childFile = join(childDir, `${subagentId}.jsonl`);
const jsonl = (rows: unknown[]) => rows.map(row => JSON.stringify(row)).join('\n') + '\n';
writeFileSync(parentFile, jsonl(seed.parent), { flag: 'wx' });
writeFileSync(childFile, jsonl(seed.child), { flag: 'wx' });
let seq = 0;
const Native = globalThis.WebSocket;
const sockets: WebSocket[] = [];
globalThis.WebSocket = class extends Native {
  id = sockets.length + 1;
  constructor(url: string | URL, protocols?: string | string[]) {
    super(url, protocols); sockets.push(this);
    this.addEventListener('message', event => this.record('receive', String(event.data)));
    this.addEventListener('close', event => appendFileSync(join(out, 'frames.jsonl'), JSON.stringify({
      seq: ++seq, connection: this.id, direction: 'receive', close: { code: event.code, reason: event.reason },
    }) + '\n'));
  }
  record(direction: string, raw: string) {
    appendFileSync(join(out, 'frames.jsonl'), JSON.stringify({ seq: ++seq, connection: this.id,
      direction, raw, frame: JSON.parse(raw) }) + '\n');
  }
  send(data: any) { this.record('send', String(data)); super.send(data); }
};
const client = new GatewayWsClient({ url: address, token, clientName: 'web' });
const historyKey = 'web:session-domain-u9';
const marker = 'SESSION_DOMAIN_U9_LIVE';
const result: Record<string, any> = { u10Seed: { id: seed.id, parentFile, childFile }, events: [] };
const timeout = setTimeout(() => { save('timeout.json', result); process.exit(1); }, 45000);
try {
  await client.connect();
  for await (const frame of client.stream('submit_turn', { projectKey: project, channelKey: 'web',
    sessionKey: historyKey, message: marker, runId: 'run-session-domain-u9' })) result.events.push(frame);
  result.u9 = await client.request('read_session_messages', { projectKey: project, sessionKey: historyKey, limit: 1000 });
  result.u10 = await client.request('read_subagent_messages', { projectKey: project, sessionKey, subagentId });
  result.u10Missing = await client.request('read_subagent_messages', { projectKey: project, sessionKey, subagentId: 'missing' });
  assert(result.events.some((frame: any) => frame.type === 'turn_completed'), 'U9 did not finish a real turn');
  assert(JSON.stringify(result.u9).includes(marker) && JSON.stringify(result.u9).includes('GP_MOCK_OK'),
    'U9 did not expose live user and model history');
  const retained = (value: any) => value.messages.map((row: any) => ({
    role: row.role, kind: row.kind, text: row.text, sequence: row.sequence, entryId: row.entryId,
  }));
  assert.deepEqual(retained(result.u10), retained(seed.result), 'U10 differs from independent fixed TS history fixture');
  assert.deepEqual(result.u10Missing.messages, [], 'unknown subagent must not resolve to the real child');
} finally {
  client.close();
  const deadline = Date.now() + 5000;
  while (sockets.some(socket => socket.readyState !== Native.CLOSED) && Date.now() < deadline) {
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  clearTimeout(timeout);
  result.closed = sockets.length > 0 && sockets.every(socket => socket.readyState === Native.CLOSED);
  save('result.json', result);
}
assert(result.closed, 'official client did not close');
