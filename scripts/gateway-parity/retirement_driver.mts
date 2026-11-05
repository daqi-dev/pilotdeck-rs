// E1 retirement suite (#1355): TS calibration driver.  Mirrors driver.mts
// (frame recording, official client import, 127.0.0.1:28xxx/ws assertion) but
// drives all 101 WsGatewayMethod entries from fixtures/retirement-methods.json
// and records one outcome per method plus live HTTP entry probes.  Run via
// session_domain.py --suite retirement only (PDRS_BASELINE/GP_PROJECT env,
// isolated home).
import assert from 'node:assert/strict';
import { appendFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';

const { values: args } = parseArgs({ options: Object.fromEntries(['url', 'token', 'methods', 'out'].map((key) => [key, { type: 'string' }])) });
for (const key of ['url', 'token', 'methods', 'out']) assert(args[key], `--${key} required`);
const endpoint = new URL(args.url!);
assert(endpoint.protocol === 'ws:' && endpoint.hostname === '127.0.0.1' && endpoint.pathname === '/ws'
  && Number(endpoint.port) >= 28000 && Number(endpoint.port) <= 28999, 'only ws://127.0.0.1:28xxx/ws is permitted');
const out = args.out!;
mkdirSync(out, { recursive: true });
const save = (name: string, value: unknown) => writeFileSync(`${out}/${name}`, JSON.stringify(value, null, 2) + '\n');
let seq = 0, connection = 0;
const sockets: WebSocket[] = [];
const record = (row: object) => appendFileSync(`${out}/frames.jsonl`, JSON.stringify({ seq: ++seq, ...row }) + '\n');
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
if (!baseline) throw new Error('PDRS_BASELINE is required; run via session_domain.py (#187: no hard-coded fallback)');
// Use the actual public source entry point, resolved by the baseline tsx loader.
const { GatewayWsClient } = await import(pathToFileURL(`${baseline}/src/gateway/index.js`).href);
const project = process.env.GP_PROJECT ?? process.cwd();
const sessionKey = 'web:retirement-calib';
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
async function until(test: () => any, label: string, timeout = 15000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) { const value = test(); if (value) return value; await sleep(20); }
  throw new Error(`timeout: ${label}`);
}
function withTimeout<T>(promise: Promise<T>, ms: number, label: string): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(Object.assign(new Error(`timeout: ${label}`), { timedOut: true })), ms);
    promise.then((value) => { clearTimeout(timer); resolve(value); },
      (error) => { clearTimeout(timer); reject(error); });
  });
}
const document = JSON.parse(readFileSync(args.methods!, 'utf8'));
assert(Array.isArray(document.methods) && document.methods.length === 101, 'methods fixture must hold 101 entries');
// Fixture placeholders are substituted with this run's isolated project/session.
function substitute(value: any): any {
  if (typeof value === 'string') return value.replaceAll('__PROJECT__', project).replaceAll('__SESSION__', sessionKey);
  if (Array.isArray(value)) return value.map(substitute);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, substitute(v)]));
  }
  return value;
}
const PER_METHOD_TIMEOUT_MS = 15000;
const outcomes: any[] = [];
const client = new GatewayWsClient({ url: args.url, token: args.token, clientName: 'web' });
await client.connect();
try {
  for (const entry of document.methods) {
    const params = substitute(entry.params);
    const started = Date.now();
    if (entry.shape === 'stream') {
      let events = 0, lastEventType: string | null = null, finalSeen = false;
      try {
        const iterator = client.stream(entry.method, params)[Symbol.asyncIterator]();
        for (;;) {
          const next = await withTimeout(iterator.next(), PER_METHOD_TIMEOUT_MS, entry.method);
          if (next.done) { finalSeen = true; break; }
          const event: any = next.value;
          events += 1; lastEventType = event?.type ?? null;
          if (event?.final === true) { finalSeen = true; break; }
          if (event?.type === 'error') break;
        }
        outcomes.push({ method: entry.method, shape: 'stream', outcome: 'ok', events, lastEventType, finalSeen, elapsedMs: Date.now() - started });
      } catch (error: any) {
        const timedOut = Boolean(error?.timedOut) || /timeout/.test(String(error?.message ?? ''));
        outcomes.push({ method: entry.method, shape: 'stream', outcome: timedOut ? 'timeout' : 'error',
          errorCode: error?.code ?? null, error: String(error?.message ?? error), events, lastEventType, finalSeen,
          elapsedMs: Date.now() - started });
      }
    } else {
      try {
        await withTimeout(client.request(entry.method, params), PER_METHOD_TIMEOUT_MS, entry.method);
        outcomes.push({ method: entry.method, shape: 'request', outcome: 'ok', elapsedMs: Date.now() - started });
      } catch (error: any) {
        const timedOut = Boolean(error?.timedOut) || /timeout/.test(String(error?.message ?? ''));
        outcomes.push({ method: entry.method, shape: 'request', outcome: timedOut ? 'timeout' : 'error',
          errorCode: error?.code ?? null, error: String(error?.message ?? error), elapsedMs: Date.now() - started });
      }
    }
    save('methods.json', { methods: outcomes });
  }
} finally {
  client.close();
  await until(() => sockets.every((ws) => ws.readyState === Native.CLOSED), 'closed sockets', 5000).catch(() => {});
}
// Live HTTP entry probes against the same gateway listener.  A route answers
// with any status; 404 means the production route was not reachable.
const base = args.url!.replace('ws:', 'http:').replace(/\/ws$/, '');
const bearer = { authorization: `Bearer ${args.token}` };
async function probe(path: string, options: any = {}) {
  try {
    const response = await fetch(base + path, { signal: AbortSignal.timeout(10000), ...options });
    const body = await response.text().catch(() => '');
    return { path, status: response.status, bodyBytes: body.length, method: options.method ?? 'GET' };
  } catch (error: any) {
    return { path, status: null, error: String(error?.message ?? error), method: options.method ?? 'GET' };
  }
}
const key = encodeURIComponent(project);
const probes = [
  await probe('/health'),
  await probe('/auth/local-token'),
  await probe('/feishu/webhook', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' }),
  await probe('/api/web/projects', { headers: bearer }),
  await probe(`/api/web/projects/${key}`, { headers: bearer }),
  await probe(`/api/web/projects/${key}/files/tree`, { headers: bearer }),
  await probe(`/api/web/projects/${key}/files/read?path=note.txt`, { headers: bearer }),
  await probe(`/api/web/projects/${key}/files/write`, { method: 'POST', headers: { ...bearer, 'content-type': 'application/json' },
    body: JSON.stringify({ path: 'retirement-written.txt', content: 'retirement calibration\n' }) }),
  await probe('/api/module-host/describe', { method: 'POST', headers: { 'content-type': 'application/json' }, body: '{}' }),
  await probe('/api/module-host/call', { method: 'POST', headers: { ...bearer, 'content-type': 'application/json' },
    body: JSON.stringify({ principal: { pilotDeckUserId: 'retirement', tenantId: 'retirement', actorUserId: 'retirement', agentId: 'retirement' },
      operation: 'retirement-none', input: {} }) }),
  await probe('/api/module-host/approvals/status', { headers: bearer }),
  await probe('/api/module-host/approvals/resume', { method: 'POST', headers: { ...bearer, 'content-type': 'application/json' }, body: '{}' }),
  await probe('/favicon.ico'),
];
save('methods.json', { methods: outcomes });
save('http.json', { probes });
save('result.json', { status: 'PASS', methods: outcomes.length, probes: probes.length,
  missing: outcomes.filter((row) => row.outcome === 'timeout').map((row) => row.method) });
save('complete.json', { records: seq, connections: connection, complete: true });
