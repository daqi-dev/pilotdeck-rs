/** Read-only source loader and production apparatus; Python isolates before startup. */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import { registerHooks, stripTypeScriptTypes, syncBuiltinESMExports } from 'node:module';
import { createHash } from 'node:crypto';
import { resolve, dirname, relative } from 'node:path';
import { pathToFileURL, fileURLToPath } from 'node:url';
import { execFileSync } from 'node:child_process';
import { Server } from 'node:net';
import { BASELINE_COMMIT } from './preregistration-b.mjs';
import { decodeJsonl } from './rules-b.mjs';
export const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
export const BASELINE = resolve(process.env.A52B_BASELINE!);
export const OUTPUT = resolve(process.env.A52B_OUTPUT!);
export const TIME = '2026-10-15T00:00:00.000Z';
const isolation = process.env.A52B_ISOLATION;
assert.ok(isolation, 'use run-b.py: environment must be isolated BEFORE Node startup');
for (const key of ['HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'TMPDIR', 'PILOT_HOME'])
  assert.ok(process.env[key] && resolve(process.env[key]!).startsWith(resolve(isolation) + '/'), key);
for (const key of ['PILOTDECK_SKIP_BROWSER_OPEN', 'SKIP_BROWSER_OPEN']) assert.equal(process.env[key], '1');
assert.equal(process.versions.node.split('.')[0], '22');
assert.equal(execFileSync('git', ['-C', BASELINE, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(), BASELINE_COMMIT);
const realListen = Server.prototype.listen;
Server.prototype.listen = function(...args: any[]) {
  const port = typeof args[0] === 'object' ? args[0].port : args[0];
  const host = typeof args[0] === 'object' ? args[0].host : args[1];
  assert.ok(process.env.A52B_COORDINATOR === '1' && process.env.A52B_WS === '1', 'no-listen apparatus; coordinator-only WS script');
  assert.ok(Number.isInteger(port) && port >= 40000 && port <= 59999 && String(port) === process.env.A52B_PORT && host === '127.0.0.1', 'only declared high 127.0.0.1 port');
  return realListen.apply(this, args as any);
} as any;
const baseUrl = pathToFileURL(BASELINE + '/').href;
export const imported = new Map<string, string>();
export const sha = (text: any) => createHash('sha256').update(text).digest('hex');
registerHooks({
  resolve(specifier, context, next) {
    if (context.parentURL?.startsWith(baseUrl) && specifier.startsWith('.') && specifier.endsWith('.js')) {
      const target = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
      if (fs.existsSync(target)) return { url: target.href, shortCircuit: true };
    }
    return next(specifier, context);
  },
  load(url, context, next) {
    if (url.startsWith(baseUrl) && url.endsWith('.ts')) {
      const path = fileURLToPath(url), source = fs.readFileSync(path, 'utf8');
      imported.set(path, sha(source));
      return { format: 'module', source: stripTypeScriptTypes(source, { mode: 'transform', sourceUrl: url }), shortCircuit: true };
    }
    return next(url, context);
  },
});
export const upstream = (path: string) => import(baseUrl + 'src/' + path + '.ts');
export const { InProcessGateway } = await upstream('gateway/client/InProcessGateway');
export const { SessionRouter } = await upstream('gateway/SessionRouter');
export const { FileRunRegistry } = await upstream('gateway/run/RunRegistry');
export const { JsonlTranscriptWriter } = await upstream('session/transcript/JsonlTranscriptWriter');
export const { JsonlSessionEventStore } = await upstream('session/events/JsonlSessionEventStore');
export const { AgentSession } = await upstream('agent/session/AgentSession');
export const { TurnRunner } = await upstream('agent/turn/TurnRunner');
export const { GatewayWsConnection } = await upstream('gateway/server/GatewayWsConnection');
export const { PILOTDECK_GATEWAY_PROTOCOL_VERSION } = await upstream('gateway/protocol/version');
export const { AgentLoopResultUnknownError } = await upstream('agent/modules/transport/operationLedger');
export function deferred<T = any>() { let resolve!: (v?: T) => void; const promise = new Promise<T>(r => { resolve = r as any; }); return { promise, resolve }; }
export const clone = (v: any) => v === undefined ? null : JSON.parse(JSON.stringify(v));
export const errorData = (e: any) => ({ name: e.name, message: e.message, code: e.code ?? null, stack: e.stack });
export class Journal {
  rows: any[] = []; waiters: any[] = [];
  constructor(readonly path: string) { fs.mkdirSync(dirname(path), { recursive: true }); fs.writeFileSync(path, ''); }
  add(kind: string, data: any = {}) {
    const row = { order: this.rows.length + 1, ...clone(data), kind };
    this.rows.push(row); fs.appendFileSync(this.path, JSON.stringify(row) + '\n');
    for (const waiter of [...this.waiters]) if (waiter.test(row)) { this.waiters.splice(this.waiters.indexOf(waiter), 1); waiter.resolve(row); }
    return row;
  }
  wait(test: (r: any) => boolean) {
    const found = this.rows.find(test); if (found) return Promise.resolve(found);
    return new Promise<any>(resolve => this.waiters.push({ test, resolve }));
  }
  barrier(name: string) { this.add('barrier', { name }); }
}

// Narrow filesystem interception on fixture-owned files. Real functions do all
// successful I/O. Partial writes/rename-after are actual bytes/rename THEN EIO.
export class FileTap {
  originals = { appendFile: fsp.appendFile, writeFile: fsp.writeFile, rename: fsp.rename, readFile: fsp.readFile };
  armed: any = null;
  constructor(readonly dir: string, readonly j: Journal) {
    for (const op of Object.keys(this.originals)) (fsp as any)[op] = (...args: any[]) => this.perform(op, args);
    syncBuiltinESMExports();
  }
  arm(op: string, match: (p: string, args: any[]) => boolean, mode = 'before') { assert.equal(this.armed, null); this.armed = { op, match, mode }; }
  async perform(op: string, args: any[]) {
    const path = String(args[0]);
    if (!path.startsWith(this.dir + '/')) return (this.originals as any)[op](...args);
    const armed = this.armed;
    const hit = armed?.op === op && armed.match(path, args);
    if (hit) this.armed = null;
    this.j.add(`fs.${op}.enter`, { path, ...(op === 'rename' ? { to: String(args[1]) } : {}) });
    if (hit && armed.mode !== 'after') {
      if (armed.mode === 'partial') {
        const text = String(args[1]).slice(0, Math.max(1, Math.floor(String(args[1]).length / 2)));
        await (this.originals as any)[op](args[0], text, args[2]);
        this.j.add('fs.partial', { path, text });
      }
      const error = Object.assign(new Error(`injected EIO at ${op}/${armed.mode}`), { code: 'EIO' });
      this.j.add('fs.fault', { op, path, mode: armed.mode, error: errorData(error) }); throw error;
    }
    const beforeRename = op === 'rename' ? fs.readFileSync(path, 'utf8') : null;
    const result = await (this.originals as any)[op](...args);
    if (op === 'appendFile') this.j.add('fs.append.commit', { path, text: String(args[1]) });
    if (op === 'rename') this.j.add('fs.rename.commit', { from: path, to: String(args[1]), text: beforeRename });
    if (hit) {
      const error = Object.assign(new Error(`injected EIO after ${op}`), { code: 'EIO' });
      this.j.add('fs.fault', { op, path, mode: 'after', error: errorData(error) }); throw error;
    }
    return result;
  }
  restore() { Object.assign(fsp, this.originals); syncBuiltinESMExports(); }
}

export class TextPeer {
  raw: string[] = []; frames: any[] = []; binding: any;
  messageHandlers: any[] = []; closeHandlers: any[] = []; conn: any;
  constructor(readonly name: string, readonly gateway: any, readonly j: Journal, readonly clientName = 'web') {
    const channel = {
      onMessage: (h: any) => this.messageHandlers.push(h), onClose: (h: any) => this.closeHandlers.push(h),
      sendText: (text: string) => { this.j.add('wire.send', { connection: name, text }); this.raw.push(text); this.frames.push(JSON.parse(text)); },
      close: (code?: number, reason?: string) => this.j.add('wire.server.close', { connection: name, code, reason }),
    };
    this.conn = new GatewayWsConnection(channel, { gateway, token: 'fixture-token', serverVersion: 'baseline-fixture' });
    const originalClose = this.conn.handleClose.bind(this.conn);
    this.conn.handleClose = async () => { try { await originalClose(); } finally { this.j.add('wire.close.done', { connection: name }); } };
  }
  async hello() {
    this.messageHandlers[0](JSON.stringify({ type: 'hello', protocolVersion: PILOTDECK_GATEWAY_PROTOCOL_VERSION,
      clientName: this.clientName, clientVersion: 'fixture', token: 'fixture-token' }));
    const row = await this.j.wait(r => r.kind === 'wire.send' && r.connection === this.name && JSON.parse(r.text).type === 'hello_ok');
    this.binding = JSON.parse(row.text).interactionBinding; return this.binding;
  }
  send(id: string, method: string, params: any) {
    this.j.add('wire.receive', { connection: this.name, id, method, params });
    this.messageHandlers[0](JSON.stringify({ type: 'request', id, method, params }));
  }
  async request(id: string, method: string, params: any) {
    this.send(id, method, params);
    const row = await this.j.wait(r => r.kind === 'wire.send' && r.connection === this.name && JSON.parse(r.text).type === 'response' && JSON.parse(r.text).id === id);
    return JSON.parse(row.text);
  }
  async final(id: string) {
    const row = await this.j.wait(r => r.kind === 'wire.send' && r.connection === this.name && JSON.parse(r.text).id === id && JSON.parse(r.text).final === true);
    return JSON.parse(row.text);
  }
  close() { this.j.add('wire.close', { connection: this.name }); for (const h of this.closeHandlers) h(); }
  observation() { return { name: this.name, binding: this.binding, clientName: this.clientName, raw: this.raw, frames: this.frames }; }
}

export class Apparatus {
  readonly j: Journal; readonly tap: FileTap; readonly registry: any; readonly router: any; readonly gateway: any;
  sessions = new Map<string, any>(); loops = new Map<string, any>(); peers: TextPeer[] = [];
  answerContext: any = null; answerCall = 0; bypassBinding = false;
  constructor(readonly dir: string, readonly options: any = {}) {
    fs.mkdirSync(dir, { recursive: true }); this.j = new Journal(options.journalPath ?? resolve(dir, 'timeline.ndjson')); this.tap = new FileTap(dir, this.j);
    this.registry = new FileRunRegistry(resolve(dir, 'registry.json'));
    const pendingRegistry: any[] = [];
    let registryCall = 0;
    for (const method of ['accept', 'append']) {
      const original = this.registry[method].bind(this.registry);
      this.registry[method] = (input: any) => {
        const label = method === 'accept' ? `${input.runId}/accept` : `${input.runId}/${input.event.type}/${input.event.code ?? input.event.event ?? ''}`;
        const key = `${++registryCall}:${label}`;
        const admitted = { key, entered: false }; pendingRegistry.push(admitted);
        this.j.add('registry.admit', { key });
        const promise = original(input);
        void promise.then(() => finish(), () => finish());
        const finish = () => {
          if (!admitted.entered) this.j.add('registry.skipped', { key, reason: 'mutation did not reach persist; error or duplicate' });
          pendingRegistry.splice(pendingRegistry.indexOf(admitted), 1);
        };
        return promise;
      };
    }
    const persist = this.registry.persist.bind(this.registry);
    this.registry.persist = () => {
      const row = pendingRegistry.find(r => !r.entered); if (row) row.entered = true;
      this.j.add('registry.enter', { key: row?.key ?? 'startup' }); return persist();
    };
    this.router = new SessionRouter({ idleSweepIntervalMs: 0, now: () => new Date(TIME),
      createSession: (context: any) => this.createSession(context.sessionKey) });
    this.gateway = new InProcessGateway(this.router, { runRegistry: this.registry, now: () => new Date(TIME),
      uuid: () => 'generated-run', recordAgentStatusMessage: async (i: any) => {
        const s = this.sessions.get(i.sessionKey); await s.writer.recordAgentStatusMessage(i.sessionKey, i.turnId, i.status); return { recorded: true };
      } });
    const originalSubmit = this.gateway.submitTurn.bind(this.gateway);
    this.gateway.submitTurn = (i: any) => { this.j.add('submit.input', i); return originalSubmit(i); };
    const abort = this.gateway.abortTurn.bind(this.gateway);
    this.gateway.abortTurn = (input: any) => { this.j.add('abort.input', { input, activeRun: this.router.activeTurnRunId(input.sessionKey) }); return abort(input); };
    this.instrumentAnswers();
  }
  instrumentAnswers() {
    for (const [kind, method, bus] of [['permission', 'permissionDecide', this.gateway.getPermissionBus()],
      ['question', 'respondElicitation', this.gateway.getElicitationBus()]] as any[]) {
      const consume = bus.consume.bind(bus);
      bus.consume = (session: string, requestId: string, binding: any) => {
        const actual = this.bypassBinding ? this.gateway.getInteractionBinding(session) : binding;
        const entry = consume(session, requestId, actual);
        this.j.add('answer.consume', { ...this.answerContext, session, requestId, kind, binding: actual, found: Boolean(entry) });
        return entry;
      };
      const original = this.gateway[method].bind(this.gateway);
      this.gateway[method] = (input: any) => {
        const call = ++this.answerCall;
        this.answerContext = { call, receivedBinding: input.interactionBinding };
        const promise = original(input);
        this.answerContext = null;
        return Promise.resolve(promise).then((result: any) => { this.j.add('answer.result', { call, delivered: result.delivered }); return result; });
      };
    }
  }
  createSession(session: string) {
    const path = resolve(this.dir, session, 'transcript.jsonl'); fs.mkdirSync(dirname(path), { recursive: true });
    let eid = 0;
    const store = new JsonlSessionEventStore({ path, now: () => new Date(TIME), uuid: () => `${session}-entry-${++eid}` });
    const append = store.append.bind(store);
    store.append = (sid: string, turn: string, event: any) => {
      if (event.type === 'agent_status_message') this.j.add('status.enter', { session, key: event.event });
      return append(sid, turn, event);
    };
    const writer = new JsonlTranscriptWriter({ path, eventStore: store });
    const record = writer.recordAgentStatusMessage.bind(writer);
    writer.recordAgentStatusMessage = (sid: string, turn: string, status: any) => {
      this.j.add('status.admit', { session, key: status.event }); return record(sid, turn, status);
    };
    const apparatus = this;
    const loop = { snapshotFileState: () => ({}), async *run(input: any): AsyncGenerator<any, any, any> {
      const hold = deferred(), returned = deferred();
      const state = { input, hold, returned, kind: apparatus.options.kind ?? null };
      apparatus.loops.set(input.turnId, state);
      input.abortSignal.addEventListener('abort', () => apparatus.j.add('loop.abort', { session, run: input.turnId, reason: input.abortSignal.reason ?? null }), { once: true });
      yield { type: 'model_request_started', sessionId: session, turnId: input.turnId, provider: 'fixture', model: 'controlled' };
      if (state.kind) {
        const requestId = `${input.turnId}-${state.kind}`;
        const bus = state.kind === 'permission' ? apparatus.gateway.getPermissionBus() : apparatus.gateway.getElicitationBus();
        bus.register(session, { requestId, toolCallId: 'fixture-tool', toolName: 'fixture_wait',
          resolve(answer: any) { apparatus.j.add('answer.resolve', { session, requestId, answer }); hold.resolve(answer); },
          reject(error: any) { apparatus.j.add('answer.reject', { session, requestId, error: errorData(error) }); hold.resolve({ error: errorData(error) }); }
        }, { payload: { prompt: 'fixture controlled request' } });
        apparatus.gateway.emitForSession(session, { type: state.kind === 'permission' ? 'permission_request' : 'elicitation_request',
          runId: input.turnId, requestId, toolCallId: 'fixture-tool', toolName: 'fixture_wait', prompt: 'fixture controlled request' });
      }
      apparatus.j.add('loop.wait', { session, run: input.turnId, interactionKind: state.kind });
      await hold.promise;
      apparatus.j.add('loop.held-return', { session, run: input.turnId });
      await returned.promise;
      if (apparatus.options.unknown) throw new AgentLoopResultUnknownError();
      const result = { type: 'success', sessionId: session, turnId: input.turnId, stopReason: 'completed', usage: {}, permissionDenials: [], turns: 1, startedAt: TIME, completedAt: TIME };
      yield { type: 'turn_completed', sessionId: session, turnId: input.turnId, result };
      return { result, messages: input.messages };
    } };
    const runner = new TurnRunner(loop, writer, undefined, () => new Date(TIME), undefined,
      { cwd: this.dir, transcriptPath: path, collectFileArtifacts: false }, { autoGenerateSessionTitle: false });
    const agent = new AgentSession({ sessionId: session, turnRunner: runner, cwd: this.dir, transcriptPath: path });
    this.sessions.set(session, { agent, writer, store, path }); return agent;
  }
  async peer(name: string, clientName = 'web') { const p = new TextPeer(name, this.gateway, this.j, clientName); this.peers.push(p); await p.hello(); return p; }
  async waitLoop(run: string) {
    await this.j.wait(r => r.kind === 'loop.wait' && r.run === run);
    await this.j.wait(r => r.kind === 'wire.send' && JSON.parse(r.text).type === 'event' && JSON.parse(r.text).event.type === 'model_request_started' && JSON.parse(r.text).event.runId === run);
    this.j.barrier(`loop:${run}`); return this.loops.get(run);
  }
  release(run: string) { const l = this.loops.get(run); l.hold.resolve(); l.returned.resolve(); this.j.barrier(`release:${run}`); }
  async observe(caseId: string, repetition: number, expectedConnections: string[], requiredBarriers: string[], extra: any = {}) {
    const durable: any[] = [];
    for (const [session, s] of this.sessions) {
      await s.writer.writeTail; await s.store.flush();
      const rawText = fs.existsSync(s.path) ? fs.readFileSync(s.path, 'utf8') : '';
      durable.push({ session, path: s.path, rawText, ...decodeJsonl(rawText), fresh: await s.store.read() });
    }
    const path = resolve(this.dir, 'registry.json');
    let registry: any = { applicable: false, reason: 'No snapshot created; acceptance failed before durable registry creation' };
    if (fs.existsSync(path)) {
      const rawText = fs.readFileSync(path, 'utf8'), disk = JSON.parse(rawText), fresh: any[] = [];
      const reread = new FileRunRegistry(path);
      for (const r of disk.records) {
        const query = { sessionKey: r.sessionKey, runId: r.runId, ...(r.projectKey ? { projectKey: r.projectKey } : {}) };
        fresh.push({ query, record: await reread.get(query) ?? null, events: await reread.events(query) });
      }
      registry = { applicable: true, path, rawText, records: disk.records, events: disk.events, fresh };
    }
    return { caseId, repetition, expectedConnections, requiredBarriers, connections: this.peers.map(p => p.observation()),
      durable, durableEmptyReason: durable.length ? null : 'No AgentSession was created; see acceptance failure or duplicate replay frames', registry, timeline: this.j.rows, ...extra };
  }
  async close() { this.gateway.dispose(); await this.router.shutdown(); for (const s of this.sessions.values()) await s.writer.close(); this.tap.restore(); }
}
export function verifySources() {
  for (const [path, hash] of imported) assert.equal(sha(fs.readFileSync(path)), hash, `baseline source changed: ${path}`);
  return [...imported].map(([path, sha256]) => ({ path: relative(BASELINE, path), sha256 }));
}
