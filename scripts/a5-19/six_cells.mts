/** TS-only observation harness; no product oracle and no network listener. */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync, mkdirSync, mkdtempSync, appendFileSync } from 'node:fs';
import { registerHooks, stripTypeScriptTypes } from 'node:module';
import { dirname, resolve, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { Server } from 'node:net';
import { checkObservation, runControls, RULES } from './observation_rules.mjs';

const WORKTREE = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
// The Python launcher isolates the environment BEFORE this Node starts.
const isolation = process.env.A52_ISOLATION;
assert.ok(isolation, 'Use python3 scripts/a5-19/run.py (isolated Node startup required)');
for (const key of ['HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'TMPDIR', 'PILOT_HOME']) {
  assert.ok(process.env[key] && resolve(process.env[key]!).startsWith(resolve(isolation) + '/'), `isolated ${key} required`);
}
for (const key of ['PILOTDECK_SKIP_BROWSER_OPEN', 'SKIP_BROWSER_OPEN']) assert.equal(process.env[key], '1');
const BASELINE = resolve(process.env.A52_BASELINE!);
const BASE_URL = pathToFileURL(BASELINE + '/').href;
const OUTPUT = resolve(process.env.A52_OUTPUT!);
const TMP = resolve(process.env.A52_TMP!);
mkdirSync(TMP, { recursive: true });
const RUN_DIR = mkdtempSync(resolve(TMP, 'run-'));
process.env.PILOT_HOME = resolve(RUN_DIR, 'home');
mkdirSync(process.env.PILOT_HOME, { recursive: true });
mkdirSync(OUTPUT, { recursive: true });
assert.equal(process.versions.node.split('.')[0], '22', 'Run with Node 22');
const baselineCommit = execFileSync('git', ['-C', BASELINE, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(baselineCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const importedSources = new Map<string, string>();
// A regression in imported module startup must fail before opening a socket.
Server.prototype.listen = function() {
  throw new Error('A5.2a forbids listen; hand listener work to the coordinator');
} as any;
// Resolve upstream .js specifiers to their ORIGINAL .ts files. Node's built-in
// transform is needed for upstream constructor parameter properties; no copied
// source, emitted JS, build, dependency install or source-text patch is used.
registerHooks({
  resolve(specifier, context, next) {
    if (context.parentURL?.startsWith(BASE_URL) && specifier.startsWith('.') && specifier.endsWith('.js')) {
      const candidate = new URL(specifier.slice(0, -3) + '.ts', context.parentURL);
      if (existsSync(candidate)) return { url: candidate.href, shortCircuit: true };
    }
    return next(specifier, context);
  },
  load(url, context, next) {
    if (url.startsWith(BASE_URL) && url.endsWith('.ts')) {
      const source = readFileSync(fileURLToPath(url), 'utf8');
      importedSources.set(fileURLToPath(url), createHash('sha256').update(source).digest('hex'));
      return { format: 'module', source: stripTypeScriptTypes(source, { mode: 'transform', sourceUrl: url }), shortCircuit: true };
    }
    return next(url, context);
  },
});
const { InProcessGateway } = await import(BASE_URL + 'src/gateway/client/InProcessGateway.ts');
const { SessionRouter } = await import(BASE_URL + 'src/gateway/SessionRouter.ts');
const { FileRunRegistry } = await import(BASE_URL + 'src/gateway/run/RunRegistry.ts');
const { JsonlTranscriptWriter } = await import(BASE_URL + 'src/session/transcript/JsonlTranscriptWriter.ts');
const { JsonlSessionEventStore } = await import(BASE_URL + 'src/session/events/JsonlSessionEventStore.ts');
const { AsyncQueue } = await import(BASE_URL + 'src/gateway/util/AsyncQueue.ts');
const { GatewayTurnCompletionFence } = await import(BASE_URL + 'src/gateway/client/GatewayTurnCompletionFence.ts');
const { AgentSession } = await import(BASE_URL + 'src/agent/session/AgentSession.ts');
const { TurnRunner } = await import(BASE_URL + 'src/agent/turn/TurnRunner.ts');
const { AgentLoopResultUnknownError } = await import(BASE_URL + 'src/agent/modules/transport/operationLedger.ts');

const TIME = '2026-10-14T08:00:00.000Z';
const TIMEOUT_MS = 123456; // Only this gateway timer is captured; never slept.
const realSetTimeout = globalThis.setTimeout;
const realClearTimeout = globalThis.clearTimeout;
function deferred<T = void>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(r => { resolve = r; });
  return { promise, resolve };
}
function clone(value: any) { return JSON.parse(JSON.stringify(value)); }
function label(event: any) { return `${event.type}:${event.event ?? event.code ?? event.finishReason ?? ''}`; }
function errorData(error: any) { return { name: error.name, message: error.message, code: error.code ?? null, stack: error.stack }; }
class Journal {
  private readonly path: string;
  constructor(path: string) { this.path = path; writeFileSync(path, ''); }
  rows: any[] = [];
  waiters: any[] = [];
  add(kind: string, data: any = {}) {
    const row = { order: this.rows.length + 1, kind, ...clone(data) };
    this.rows.push(row);
    appendFileSync(this.path, JSON.stringify(row) + '\n');
    for (const waiter of [...this.waiters]) if (waiter.test(row)) {
      this.waiters.splice(this.waiters.indexOf(waiter), 1); waiter.resolve(row);
    }
    return row;
  }
  wait(test: (row: any) => boolean): Promise<any> {
    const row = this.rows.find(test);
    if (row) return Promise.resolve(row);
    return new Promise(resolve => this.waiters.push({ test, resolve }));
  }
  has(kind: string, key?: string) { return this.rows.some(r => r.kind === kind && (key === undefined || r.key === key)); }
}

type Variant = {
  name: string;
  order?: 'timeout-first' | 'unknown-first' | 'known-final-first' | 'after-complete' | 'known-final-before-timeout-pending';
  slowStatus?: boolean;
  slowCatchStatus?: boolean;
  slowAppend?: 'turn_timeout' | 'gateway_submit_failed';
  failStatus?: 'turn_timeout' | 'gateway_submit_failed';
  failAppend?: string;
};
const cells: { name: string; registry: boolean; variants: Variant[] }[] = [
  { name: 'no-registry-fast', registry: false, variants: [{ name: 'fast' }] },
  { name: 'no-registry-slow', registry: false, variants: [{ name: 'status-slow', slowStatus: true }, { name: 'catch-status-slow', slowCatchStatus: true }] },
  { name: 'no-registry-failure', registry: false, variants: [
    { name: 'timeout-status-fails', failStatus: 'turn_timeout' },
    { name: 'catch-status-fails', failStatus: 'gateway_submit_failed' },
  ] },
  { name: 'registry-fast', registry: true, variants: [{ name: 'fast' }] },
  { name: 'registry-slow', registry: true, variants: [
    { name: 'status-slow-append-fast', slowStatus: true },
    { name: 'status-fast-append-slow', slowAppend: 'turn_timeout' },
    { name: 'both-slow', slowStatus: true, slowAppend: 'turn_timeout' },
    { name: 'status-fast-catch-append-slow', slowAppend: 'gateway_submit_failed' },
    { name: 'status-slow-catch-append-slow', slowStatus: true, slowAppend: 'gateway_submit_failed' },
    { name: 'catch-status-slow-append-fast', slowCatchStatus: true },
    { name: 'catch-status-slow-append-slow', slowCatchStatus: true, slowAppend: 'turn_timeout' },
  ] },
  { name: 'registry-failure', registry: true, variants: [
    { name: 'timeout-status-fails-append-fast', failStatus: 'turn_timeout' },
    { name: 'catch-status-fails-append-fast', failStatus: 'gateway_submit_failed' },
    { name: 'timeout-status-fails-append-slow', failStatus: 'turn_timeout', slowAppend: 'turn_timeout' },
    { name: 'catch-status-fails-append-slow', failStatus: 'gateway_submit_failed', slowAppend: 'turn_timeout' },
    { name: 'timeout-status-fails-catch-append-slow', failStatus: 'turn_timeout', slowAppend: 'gateway_submit_failed' },
    { name: 'catch-status-fails-catch-append-slow', failStatus: 'gateway_submit_failed', slowAppend: 'gateway_submit_failed' },
    { name: 'timeout-append-fails-status-fast', failAppend: 'error:turn_timeout' },
    { name: 'timeout-status-event-append-fails', failAppend: 'agent_status:turn_timeout' },
    { name: 'catch-status-event-append-fails', failAppend: 'agent_status:gateway_submit_failed' },
    { name: 'catch-append-fails-status-fast', failAppend: 'error:gateway_submit_failed' },
    { name: 'timeout-append-fails-status-slow', failAppend: 'error:turn_timeout', slowStatus: true },
    { name: 'catch-append-fails-status-slow', failAppend: 'error:gateway_submit_failed', slowStatus: true },
    { name: 'timeout-status-and-catch-append-fail', failStatus: 'turn_timeout', failAppend: 'error:gateway_submit_failed' },
    { name: 'catch-status-and-timeout-append-fail', failStatus: 'gateway_submit_failed', failAppend: 'error:turn_timeout' },
    { name: 'timeout-append-fails-catch-status-slow', failAppend: 'error:turn_timeout', slowCatchStatus: true },
  ] },
];
// Extra orderings are included in each topology's fast cell, never counted as
// substitutes for the six cells. Real TurnRunner decides unknown-first behavior.
for (const cell of cells.filter(c => c.name.endsWith('-fast'))) {
  for (const order of ['unknown-first', 'known-final-first', 'after-complete', 'known-final-before-timeout-pending'] as const) {
    cell.variants.push({ name: order, order });
  }
}

async function measure(cell: typeof cells[number], variant: Variant, phase: string, repetition: number) {
  const id = `${cell.name}/${variant.name}/${phase}/${repetition}`;
  const dir = resolve(RUN_DIR, id);
  mkdirSync(dir, { recursive: true });
  const transcriptPath = resolve(dir, 'transcript.jsonl');
  const registryPath = resolve(dir, 'registry.json');
  writeFileSync(transcriptPath, '');
  const j = new Journal(resolve(dir, 'timeline.ndjson'));
  const frames: any[] = [];
  const statusGate = deferred();
  const appendGate = deferred();
  const outcome = deferred<'unknown' | 'known'>();
  const knownReturn = deferred();
  const order = variant.order ?? 'timeout-first';
  let timer: any;
  let entryId = 0;
  const appendPromises = new Set<Promise<void>>();
  const activeQueue = new Set<any>();
  const oldEnqueue = AsyncQueue.prototype.enqueue;
  const oldClose = AsyncQueue.prototype.close;
  const oldWarn = console.warn;
  const oldInfo = console.info;
  const fault = (point: string) => Object.assign(new Error(`Injected storage failure at ${point}`), { code: 'EIO' });
  // Passive taps delegate synchronously and preserve the real queue semantics.
  AsyncQueue.prototype.enqueue = function(event: any) {
    activeQueue.add(this);
    j.add('queue.enqueue', { key: label(event), closed: this.closed, event });
    return oldEnqueue.call(this, event);
  };
  AsyncQueue.prototype.close = function() {
    activeQueue.add(this);
    j.add('queue.close', { alreadyClosed: this.closed });
    return oldClose.call(this);
  };
  console.warn = (...args: any[]) => j.add('warning', { args: args.map(x => x instanceof Error ? errorData(x) : x) });
  console.info = (...args: any[]) => j.add('info', { args });
  globalThis.setTimeout = ((callback: any, ms: number, ...args: any[]) => {
    if (ms !== TIMEOUT_MS) return realSetTimeout(callback, ms, ...args);
    assert.equal(timer, undefined, 'one gateway timeout per measurement');
    timer = { callback: () => callback(...args), cancelled: false, fired: false };
    j.add('timer.arm', { ms });
    return timer;
  }) as any;
  globalThis.clearTimeout = ((handle: any) => {
    if (handle === timer) { timer.cancelled = true; j.add('timer.clear'); }
    else realClearTimeout(handle);
  }) as any;

  // Both ordinary transcript entries and Gateway status use ONE real writer.
  // Only the underlying store append is delayed/failed: writeTail already owns
  // this operation. Outer record methods always delegate synchronously.
  const eventStore = new JsonlSessionEventStore({
    path: transcriptPath, now: () => new Date(TIME), uuid: () => `entry-${++entryId}`,
  });
  const originalStoreAppend = eventStore.append.bind(eventStore);
  let activeStoreWrites = 0;
  eventStore.append = async (sessionId: string, turnId: string, event: any) => {
    assert.equal(++activeStoreWrites, 1, 'real writer serializes underlying store appends');
    const key = event.type === 'agent_status_message' ? event.event : undefined;
    j.add('transcript.store.enter', { type: event.type, key });
    try {
      if (key !== undefined) j.add('status.store.enter', { key });
      if ((key === 'turn_timeout' && variant.slowStatus) || (key === 'gateway_submit_failed' && variant.slowCatchStatus)) {
        j.add('status.block', { key, boundary: 'eventStore.append inside writer.writeTail' });
        await statusGate.promise; j.add('status.release', { key });
      }
      if (key !== undefined && key === variant.failStatus) {
        const error = fault(`eventStore.append:${key}`);
        j.add('status.failure', { key, error: errorData(error) }); throw error;
      }
      const entry = await originalStoreAppend(sessionId, turnId, event);
      if (key !== undefined) j.add('status.commit', { key, sequence: entry.sequence, entry });
      return entry;
    } finally { activeStoreWrites--; }
  };
  const transcript = new JsonlTranscriptWriter({ path: transcriptPath, eventStore });
  const originalRecordStatus = transcript.recordAgentStatusMessage.bind(transcript);
  const recordStatus = (sessionId: string, turnId: string, status: any) => {
    j.add('status.attempt', { key: status.event, status });
    return originalRecordStatus(sessionId, turnId, status);
  };
  transcript.recordAgentStatusMessage = recordStatus;
  const loop = {
    snapshotFileState: () => ({}),
    async *run(input: any): AsyncGenerator<any, any, any> {
      j.add('loop.input', { execution: input.execution, phase });
      yield { type: 'model_request_started', sessionId: input.sessionId, turnId: input.turnId, provider: 'fixture', model: 'controlled' };
      if (phase === 'tool') yield { type: 'tool_calls_detected', sessionId: input.sessionId, turnId: input.turnId, calls: [{ id: 'tool-1', name: 'controlled_wait', input: {} }] };
      j.add('loop.wait', { phase });
      const answer = await outcome.promise;
      if (answer === 'unknown') {
        j.add('loop.final', { outcome: 'result_unknown', code: 'DEADLINE_EXCEEDED', aborted: input.abortSignal.aborted, reason: input.abortSignal.reason ?? null });
        throw new AgentLoopResultUnknownError();
      }
      const result = { type: 'success', sessionId: input.sessionId, turnId: input.turnId, stopReason: 'completed', usage: {}, permissionDenials: [], turns: 1, startedAt: TIME, completedAt: TIME };
      j.add('loop.final', { outcome: 'completed', result });
      yield { type: 'turn_completed', sessionId: input.sessionId, turnId: input.turnId, result };
      if (order === 'known-final-before-timeout-pending') { j.add('loop.return.block'); await knownReturn.promise; }
      return { result, messages: input.messages };
    },
  };
  const runner = new TurnRunner(loop, transcript, undefined, () => new Date(TIME), undefined,
    { cwd: dir, transcriptPath, collectFileArtifacts: false }, { autoGenerateSessionTitle: false });
  const session = new AgentSession({ sessionId: 'session', turnRunner: runner, cwd: dir, transcriptPath });
  const originalSubmit = session.submit.bind(session);
  session.submit = async function*(...args: any[]) {
    try { yield* originalSubmit(...args); } finally { j.add('session.submit.exit'); }
  };
  const originalAbort = session.abort.bind(session);
  session.abort = (reason: string) => { j.add('session.abort', { reason }); return originalAbort(reason); };
  const router = new SessionRouter({ idleSweepIntervalMs: 0, createSession: () => session });
  const fence = new GatewayTurnCompletionFence();
  const originalCurrent = fence.isCurrent.bind(fence);
  fence.isCurrent = (...args: any[]) => { const current = originalCurrent(...args); j.add('fence.current', { current }); return current; };
  const originalComplete = fence.complete.bind(fence);
  fence.complete = (...args: any[]) => { j.add('fence.complete'); return originalComplete(...args); };
  const registry = cell.registry ? new FileRunRegistry(registryPath) : undefined;
  if (registry) {
    // Both delay and fault occur inside the production mutation, AFTER memory changes and
    // BEFORE atomic snapshot persistence. Subsequent writes keep real refresh,
    // lock, terminal fencing, revision and seq semantics.
    const originalPersist = registry.persist.bind(registry);
    let failed = false;
    let activePersists = 0;
    registry.persist = async () => {
      assert.equal(++activePersists, 1, 'real mutationChain serializes persists');
      try {
        const entries = [...registry.eventMap.values()][0] ?? [];
        const event = entries.at(-1)?.event;
        const key = event ? label(event) : 'accept';
        j.add('registry.persist.enter', { key });
        if (variant.slowAppend && (key === `agent_status:${variant.slowAppend}` || key === `error:${variant.slowAppend}`)) {
          j.add('registry.persist.block', { key, boundary: 'persist inside mutationChain and file lock' });
          await appendGate.promise; j.add('registry.persist.release', { key });
        }
        if (!failed && event && label(event) === variant.failAppend) {
          failed = true;
          const error = fault(`registry.persist:${label(event)}`);
          j.add('registry.persist.failure', {
            key: label(event), error: errorData(error), memoryRecordsAtFailure: [...registry.records.values()],
            diskRecordAtFailure: JSON.parse(readFileSync(registryPath, 'utf8')).records[0],
          });
          throw error;
        }
        await originalPersist();
        j.add('registry.persist.commit', { key });
      } finally { activePersists--; }
    };
    // Passive outer tap: call the real append immediately, with no gate or
    // failure before admission to the production mutationChain.
    const originalAppend = registry.append.bind(registry);
    registry.append = (input: any) => {
      const key = label(input.event);
      j.add('registry.append.attempt', { key, event: input.event });
      const write = originalAppend(input);
      // Observe settlement without replacing the production promise or its
      // synchronous throw/admission semantics. Both handlers consume only the
      // observer branch; the caller still receives the original rejection.
      void write.then(() => {
        j.add('registry.append.commit', {
          key, event: input.event, diskRecordAfterAppend: JSON.parse(readFileSync(registryPath, 'utf8')).records[0],
        });
      }, (error: any) => { j.add('registry.append.failure', { key, error: errorData(error) }); });
      appendPromises.add(write);
      void write.then(() => appendPromises.delete(write), () => appendPromises.delete(write));
      return write;
    };
  }
  const gateway = new InProcessGateway(router, {
    uuid: () => 'run', now: () => new Date(TIME), turnCompletionFence: fence,
    ...(registry ? { runRegistry: registry } : {}),
    recordAgentStatusMessage: async (input: any) => { await recordStatus(input.sessionKey, input.turnId, input.status); return { recorded: true }; },
  });
  const iterator = gateway.submitTurn({ sessionKey: 'session', channelKey: 'experiment', message: 'controlled wait', timeoutMs: TIMEOUT_MS })[Symbol.asyncIterator]();
  const take = async () => {
    const next = await iterator.next();
    if (!next.done) { frames.push(clone(next.value)); j.add('client.frame', { event: next.value }); }
    else j.add('client.done');
    return next;
  };
  const consume = async () => { while (!(await take()).done) {} };
  const fire = () => {
    assert.ok(timer);
    if (timer.cancelled) { j.add('timer.fire.skipped', { reason: 'already cleared by baseline' }); return; }
    assert.equal(timer.fired, false); timer.fired = true; j.add('timer.fire'); timer.callback();
  };
  const waitClose = () => j.wait(r => r.kind === 'queue.close');
  const waitCatchPublished = () => j.wait(r => r.kind === 'queue.enqueue' && r.key === 'error:gateway_submit_failed');
  const releaseUnknown = () => { j.add('control.release.unknown'); outcome.resolve('unknown'); };
  const drainAppends = async () => { while (appendPromises.size) await Promise.allSettled([...appendPromises]); };
  try {
    // Reach the actual loop wait and consume all prefix frames. The client then
    // pauses at a generator yield, holding the outer completion fence current.
    const prefixEnd = phase === 'tool' ? 'tool_call_started' : 'model_request_started';
    while (true) {
      const next = await take();
      assert.equal(next.done, false, 'gateway must reach the controlled loop before ending');
      if (next.value.type === prefixEnd) break;
    }
    await j.wait(r => r.kind === 'loop.wait');
    j.add('client.pause', { purpose: 'hold outer finally until controlled release' });
    assert.ok(timer && !timer.fired);
    if (order === 'unknown-first' || order === 'known-final-first') {
      if (order === 'unknown-first') releaseUnknown(); else outcome.resolve('known');
      await waitClose();
      fire(); // Observes that baseline has cleared the deadline, does not force it.
    } else if (order === 'known-final-before-timeout-pending') {
      outcome.resolve('known');
      await j.wait(r => r.kind === 'loop.return.block');
      fire(); await waitClose(); knownReturn.resolve();
    } else {
      fire();
      await j.wait(r => r.kind === 'status.attempt' && r.key === 'turn_timeout');
      if (order === 'after-complete') {
        await waitClose(); await consume();
        assert.ok(j.has('fence.complete'));
        releaseUnknown();
      } else {
        if (variant.slowStatus) {
          await j.wait(r => r.kind === 'status.block');
          releaseUnknown();
          // G has called the same writer but cannot enter its store until T
          // leaves writeTail. Waiting for G publication here would deadlock.
          await j.wait(r => r.kind === 'status.attempt' && r.key === 'gateway_submit_failed');
          assert.ok(!j.has('status.store.enter', 'gateway_submit_failed'));
          j.add('control.status.fifo.checked', { queued: 'gateway_submit_failed', blocked: 'turn_timeout' });
          j.add('control.release.status'); statusGate.resolve();
        }
        await waitClose();
        if (!variant.slowStatus) releaseUnknown();
        if (variant.slowCatchStatus) {
          await j.wait(r => r.kind === 'status.block' && r.key === 'gateway_submit_failed');
          j.add('control.release.catch-status'); statusGate.resolve();
        }
        if (variant.slowAppend) {
          await j.wait(r => r.kind === 'registry.persist.block');
          // Both G append calls are now queued behind the held persist (T or
          // G). Observe admission, never demand completion behind a barrier.
          await j.wait(r => r.kind === 'registry.append.attempt' && r.key === 'error:gateway_submit_failed');
          assert.ok(!j.has('registry.persist.enter', 'error:gateway_submit_failed'));
          if (variant.slowAppend === 'turn_timeout') assert.ok(!j.has('registry.persist.enter', 'agent_status:gateway_submit_failed'));
          j.add('control.registry.fifo.checked', { queued: 'error:gateway_submit_failed', blocked: variant.slowAppend });
          j.add('control.release.append'); appendGate.resolve();
        } else if (variant.failAppend === 'error:gateway_submit_failed') {
          await j.wait(r => r.kind === 'registry.append.failure' && r.key === variant.failAppend);
        } else await waitCatchPublished();
      }
    }
    // Pump finally closes again after timeoutSettlement and all registry writes.
    await j.wait(r => r.kind === 'session.submit.exit');
    await j.wait(r => r.kind === 'queue.close' && (order === 'unknown-first' || order === 'known-final-first' || r.alreadyClosed));
    await drainAppends();
    if (!j.has('client.done')) { j.add('client.resume'); await consume(); }
    assert.ok(j.has('fence.complete'));
    assert.equal(activeQueue.size, 1, 'the passive tap must observe one queue only');
    await transcript.writeTail;
    await eventStore.flush();
    const statusText = readFileSync(transcriptPath, 'utf8');
    const statuses = statusEntries(statusText);
    const durable = { source: relative(WORKTREE, transcriptPath), rawText: statusText, entries: statuses,
      emptyReason: statuses.length ? null : 'Storage reread contained zero committed status entries; see attempts/failures or known final in timeline.' };
    let registryObservation: any = { applicable: false, reason: 'runRegistry was not configured' };
    if (registry) {
      const diskText = readFileSync(registryPath, 'utf8');
      const disk = JSON.parse(diskText);
      const reread = new FileRunRegistry(registryPath);
      const query = { sessionKey: 'session', runId: 'run' };
      const record = await reread.get(query);
      const events = await reread.events(query);
      assert.deepEqual(record, disk.records[0], 'fresh registry API agrees with disk');
      assert.deepEqual(events, Object.values(disk.events)[0] ?? [], 'events independently reread');
      registryObservation = { applicable: true, source: relative(WORKTREE, registryPath), rawText: diskText, disk, record, events };
    }
    const observation = { id, evidenceChecks: { frames: ['FRAME_PUBLICATIONS'], durableStatus: ['STATUS_RAW', 'STATUS_COMMITS', 'STATUS_FIFO'], registry: cell.registry ? Object.keys(RULES).filter(k => k.startsWith('REGISTRY_')) : 'N/A' }, variant, phase, repetition, frames, durableStatus: durable, registry: registryObservation, timeline: j.rows,
      transcriptRawText: readFileSync(transcriptPath, 'utf8') };
    checkCompleteness(observation);
    if (variant.failStatus) assert.ok(j.has('status.failure', variant.failStatus), 'status fault was exercised');
    if (variant.failAppend) assert.ok(j.has('registry.persist.failure', variant.failAppend), 'registry disk fault was exercised');
    if (variant.slowCatchStatus) assert.ok(j.has('status.block', 'gateway_submit_failed'), 'catch status barrier was exercised');
    if (variant.slowStatus) assert.ok(j.has('status.block', 'turn_timeout'), 'status barrier was exercised');
    if (variant.slowAppend) {
      assert.ok(j.has('registry.persist.block', `error:${variant.slowAppend}`), 'append barrier was exercised inside persist');
      assert.ok(j.has('control.registry.fifo.checked'), 'queued append did not overtake held persist');
    }
    return observation;
  } finally {
    await transcript.close();
    gateway.dispose();
    await router.shutdown();
    globalThis.setTimeout = realSetTimeout; globalThis.clearTimeout = realClearTimeout;
    AsyncQueue.prototype.enqueue = oldEnqueue; AsyncQueue.prototype.close = oldClose;
    console.warn = oldWarn; console.info = oldInfo;
  }
}

function statusEntries(rawText: string) {
  return rawText.trim() ? rawText.trim().split('\n').map(line => JSON.parse(line))
    .filter(entry => entry.type === 'agent_status_message') : [];
}

function checkCompleteness(o: any) {
  // Each plane has a named, independent preregistered rule. Empty status is
  // allowed only with an explanation; no terminal expectation is hard-coded.
  assert.ok(o.durableStatus.entries.length > 0 || o.durableStatus.emptyReason);
  assert.ok(o.timeline.some((r: any) => r.kind === 'loop.final'));
  checkObservation(o);
}
// Repetition comparison is local experiment bookkeeping, not a frozen scorer.
// Keep every event/detail, seq, state, revision and result. Random diagnostic
// errorId is ignored only inside status detail; registry wall timestamps/PID
// are ignored only on the record, never on an event or nested result.
function stableValue(value: any, path: string[] = []): any {
  if (Array.isArray(value)) return value.map(item => stableValue(item, path));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value)
    .filter(([key]) => !(key === 'errorId' && path.at(-1) === 'detail'))
    .map(([key, item]) => [key, stableValue(item, [...path, key])]));
  return value;
}
function signature(o: any) {
  const record = o.registry.applicable ? clone(o.registry.record) : undefined;
  if (record) for (const key of ['acceptedAt', 'updatedAt', 'ownerPid']) delete record[key];
  return stableValue({ frames: o.frames, statuses: o.durableStatus.entries,
    registry: o.registry.applicable ? { events: o.registry.events, record } : 'N/A',
    droppedEnqueues: o.timeline.filter((r: any) => r.kind === 'queue.enqueue' && r.closed).map((r: any) => r.event) });
}
function summarize(o: any) {
  const prefix = new Set(['turn_started', 'input_accepted', 'model_request_started', 'tool_call_started']);
  return {
    framesAfterPrefix: o.frames.filter((e: any) => !prefix.has(e.type)).map(label),
    durableStatuses: o.durableStatus.entries.map((e: any) => e.event),
    registryEventsAfterPrefix: o.registry.applicable ? o.registry.events.filter((e: any) => !prefix.has(e.event.type)).map((e: any) => `${e.seq}:${label(e.event)}`) : 'N/A',
    registryState: o.registry.applicable ? o.registry.record.state : 'N/A',
    registryResult: o.registry.applicable ? (o.registry.record.result ? label(o.registry.record.result) : '(absent)') : 'N/A',
    registryRevision: o.registry.applicable ? o.registry.record.revision : 'N/A',
    registryLastSeq: o.registry.applicable ? o.registry.record.lastSeq : 'N/A',
    droppedEnqueues: o.timeline.filter((r: any) => r.kind === 'queue.enqueue' && r.closed).map((r: any) => r.key),
  };
}
const summary: any[] = [];
let controls: any[] = [];
let firstRegistry: any;
let firstFrames: any;
let failedStatus: any;
let failedRegistry: any;
// Save the rules and complete input manifest BEFORE any baseline measurements.
writeFileSync(resolve(OUTPUT, 'preregistration.json'), JSON.stringify({ rules: RULES, cells, phases: ['model', 'tool'], repeats: 5 }, null, 2) + '\n');
for (const cell of cells) {
  const observations: any[] = [];
  const repetitions: any[] = [];
  for (const variant of cell.variants) for (const phase of ['model', 'tool']) {
    const runs: any[] = [];
    for (let repetition = 1; repetition <= 5; repetition++) {
      const id = `${cell.name}/${variant.name}/${phase}/${repetition}`;
      const saveIncomplete = (reason: string) => writeFileSync(resolve(OUTPUT, 'incomplete.json'),
        JSON.stringify({ id, reason, evidence: relative(WORKTREE, resolve(RUN_DIR, id)) }, null, 2) + '\n');
      const watchdog = realSetTimeout(() => {
        saveIncomplete('watchdog: controlled barrier not reached');
        console.error(`Harness stalled: ${id}`); process.exit(2);
      }, 15000);
      try { runs.push(await measure(cell, variant, phase, repetition)); }
      catch (error) { saveIncomplete(String(error)); throw error; }
      finally { realClearTimeout(watchdog); }
    }
    const keys = runs.map(r => JSON.stringify(signature(r)));
    const stable = new Set(keys).size === 1;
    const row = { cell: cell.name, variant: variant.name, phase, runs: runs.length, distinct: new Set(keys).size, stable,
      ...summarize(runs[0]), observedAlternatives: [...new Map(runs.map(r => [JSON.stringify(signature(r)), summarize(r)])).values()] };
    summary.push(row); repetitions.push(row); observations.push(...runs);
    console.log(JSON.stringify(row));
    if (cell.name === 'no-registry-fast' && variant.name === 'fast' && phase === 'model') firstFrames = runs[0];
    if (cell.name === 'no-registry-failure' && variant.name === 'timeout-status-fails' && phase === 'model') failedStatus = runs[0];
    if (cell.name === 'registry-failure' && variant.name === 'catch-append-fails-status-fast' && phase === 'model') failedRegistry = runs[0];
    if (cell.name === 'registry-fast' && variant.name === 'fast' && phase === 'model') firstRegistry = runs[0];
  }
  writeFileSync(resolve(OUTPUT, cell.name + '.json'), JSON.stringify({ cell: cell.name, baselineCommit, node: process.version, repetitions, observations }, null, 2) + '\n');
}
// Controls must reject with EXACT registered rule IDs, not just throw/exit.
controls = runControls(firstFrames, firstRegistry, failedStatus, failedRegistry);
for (const [path, hash] of importedSources) assert.equal(createHash('sha256').update(readFileSync(path)).digest('hex'), hash, `baseline changed: ${path}`);
const manifest = {
  generatedAt: new Date().toISOString(), node: process.version, baselineCommit,
  worktreeCommit: execFileSync('git', ['-C', WORKTREE, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
  command: 'python3 scripts/a5-19/run.py',
  preregisteredRules: RULES, expectedCases: cells.flatMap(c => c.variants.flatMap(v => ['model', 'tool'].map(phase => `${c.name}/${v.name}/${phase}`))),
  executedCases: summary.map(r => `${r.cell}/${r.variant}/${r.phase}`),
  isolation: Object.fromEntries(['HOME', 'XDG_CONFIG_HOME', 'XDG_CACHE_HOME', 'XDG_DATA_HOME', 'TMPDIR', 'PILOTDECK_SKIP_BROWSER_OPEN', 'SKIP_BROWSER_OPEN'].map(k => [k, process.env[k]])), pilotHome: process.env.PILOT_HOME,
  fixtureFiles: ['six_cells.mts', 'observation_rules.mjs', 'run.py'].map(name => ({ path: `scripts/a5-19/${name}`, sha256: createHash('sha256').update(readFileSync(resolve(WORKTREE, 'scripts/a5-19', name))).digest('hex') })),
  scriptSha256: createHash('sha256').update(readFileSync(fileURLToPath(import.meta.url))).digest('hex'),
  // 路径与哈希分两行写：文件名含 token / credential 时，同一行的 64 位哈希会被 gitleaks 误判为密钥。
  importedSources: [...importedSources].map(([path, hash]) => ({ path: relative(BASELINE, path), sha256: hash })),
  checks: { tcpListenForbidden: true, nonemptyObservers: true, diskRereadAgreesWithFreshRegistry: true, statusInjectionInsideWriteTail: true, registryInjectionInsideMutationChain: true, baselineHashesUnchanged: true, observerControls: controls },
  unstableGroups: summary.filter(r => !r.stable),
  repeats: 5, totalMeasurements: summary.reduce((n, r) => n + r.runs, 0), summary,
};
assert.deepEqual(manifest.executedCases, manifest.expectedCases, 'no missing/duplicate/implicit skip cases');
writeFileSync(resolve(OUTPUT, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(`DONE ${manifest.totalMeasurements} measurements; ${summary.filter(r => !r.stable).length} unstable variant/phase groups; ${importedSources.size} unchanged baseline source files.`);
