/** TS-only observation harness; no product oracle and no network listener. */
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync, appendFileSync, mkdirSync, mkdtempSync } from 'node:fs';
import { registerHooks, stripTypeScriptTypes } from 'node:module';
import { dirname, resolve, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';

const WORKTREE = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const BASELINE = '/Users/da/ws/pilotdeck-rs/resources/PilotDeck';
const BASE_URL = pathToFileURL(BASELINE + '/').href;
const OUTPUT = resolve(WORKTREE, 'docs/experiments/exp012-a5-19-six-cells/raw');
const TMP = resolve(WORKTREE, '_tmp/20261014-a5-19');
mkdirSync(TMP, { recursive: true });
const RUN_DIR = mkdtempSync(resolve(TMP, 'run-'));
process.env.PILOT_HOME = resolve(RUN_DIR, 'home');
mkdirSync(process.env.PILOT_HOME, { recursive: true });
mkdirSync(OUTPUT, { recursive: true });
assert.equal(process.versions.node.split('.')[0], '22', 'Run with Node 22');
const baselineCommit = execFileSync('git', ['-C', BASELINE, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(baselineCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const importedSources = new Map<string, string>();
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
  rows: any[] = [];
  waiters: any[] = [];
  add(kind: string, data: any = {}) {
    const row = { order: this.rows.length + 1, kind, ...clone(data) };
    this.rows.push(row);
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
  slowAppend?: boolean;
  failStatus?: 'turn_timeout' | 'gateway_submit_failed';
  failAppend?: string;
};
const cells: { name: string; registry: boolean; variants: Variant[] }[] = [
  { name: 'no-registry-fast', registry: false, variants: [{ name: 'fast' }] },
  { name: 'no-registry-slow', registry: false, variants: [{ name: 'status-slow', slowStatus: true }] },
  { name: 'no-registry-failure', registry: false, variants: [
    { name: 'timeout-status-fails', failStatus: 'turn_timeout' },
    { name: 'catch-status-fails', failStatus: 'gateway_submit_failed' },
  ] },
  { name: 'registry-fast', registry: true, variants: [{ name: 'fast' }] },
  { name: 'registry-slow', registry: true, variants: [
    { name: 'status-slow-append-fast', slowStatus: true },
    { name: 'status-fast-append-slow', slowAppend: true },
    { name: 'both-slow', slowStatus: true, slowAppend: true },
  ] },
  { name: 'registry-failure', registry: true, variants: [
    { name: 'timeout-status-fails-append-fast', failStatus: 'turn_timeout' },
    { name: 'catch-status-fails-append-fast', failStatus: 'gateway_submit_failed' },
    { name: 'timeout-status-fails-append-slow', failStatus: 'turn_timeout', slowAppend: true },
    { name: 'catch-status-fails-append-slow', failStatus: 'gateway_submit_failed', slowAppend: true },
    { name: 'timeout-append-fails-status-fast', failAppend: 'error:turn_timeout' },
    { name: 'catch-append-fails-status-fast', failAppend: 'error:gateway_submit_failed' },
    { name: 'timeout-append-fails-status-slow', failAppend: 'error:turn_timeout', slowStatus: true },
    { name: 'catch-append-fails-status-slow', failAppend: 'error:gateway_submit_failed', slowStatus: true },
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
  const statusPath = resolve(dir, 'status.jsonl');
  const transcriptPath = resolve(dir, 'transcript.jsonl');
  const registryPath = resolve(dir, 'registry.json');
  writeFileSync(statusPath, ''); writeFileSync(transcriptPath, '');
  const j = new Journal();
  const frames: any[] = [];
  const statusGate = deferred();
  const appendGate = deferred();
  const outcome = deferred<'unknown' | 'known'>();
  const knownReturn = deferred();
  const order = variant.order ?? 'timeout-first';
  let timer: any;
  let statusSeq = 0;
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

  const recordStatus = async (sessionId: string, turnId: string, status: any) => {
    const key = status.event;
    j.add('status.attempt', { key, status });
    if (key === 'turn_timeout' && variant.slowStatus) {
      j.add('status.block', { key }); await statusGate.promise; j.add('status.release', { key });
    }
    if (key === variant.failStatus) {
      const error = fault(`status:${key}`); j.add('status.failure', { key, error: errorData(error) }); throw error;
    }
    const entry = { sequence: ++statusSeq, sessionId, turnId, status: clone(status) };
    appendFileSync(statusPath, JSON.stringify(entry) + '\n');
    j.add('status.commit', { key, sequence: entry.sequence });
  };
  const transcript = {
    recordSessionEvent: (sessionId: string, turnId: string, event: any) => appendFileSync(transcriptPath, JSON.stringify({ sessionId, turnId, event }) + '\n'),
    recordAcceptedInput: (sessionId: string, turnId: string, messages: any) => appendFileSync(transcriptPath, JSON.stringify({ sessionId, turnId, messages }) + '\n'),
    recordDurableMessage: (sessionId: string, turnId: string, message: any) => appendFileSync(transcriptPath, JSON.stringify({ sessionId, turnId, message }) + '\n'),
    recordTurnResult: (sessionId: string, turnId: string, result: any) => appendFileSync(transcriptPath, JSON.stringify({ sessionId, turnId, result }) + '\n'),
    recordAgentStatusMessage: recordStatus,
  };
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
    // Fault occurs inside the production mutation, AFTER memory changes and
    // BEFORE atomic snapshot persistence. Subsequent writes keep real refresh,
    // lock, terminal fencing, revision and seq semantics.
    const originalPersist = registry.persist.bind(registry);
    let failed = false;
    registry.persist = async () => {
      const entries = [...registry.eventMap.values()][0] ?? [];
      const event = entries.at(-1)?.event;
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
    };
    const originalAppend = registry.append.bind(registry);
    registry.append = (input: any) => {
      const key = label(input.event);
      j.add('registry.append.attempt', { key, event: input.event });
      const write = (async () => {
        if (variant.slowAppend && (key === 'agent_status:turn_timeout' || key === 'error:turn_timeout')) {
          j.add('registry.append.block', { key }); await appendGate.promise; j.add('registry.append.release', { key });
        }
        try {
          await originalAppend(input);
          j.add('registry.append.commit', {
            key, diskRecordAfterAppend: JSON.parse(readFileSync(registryPath, 'utf8')).records[0],
          });
        }
        catch (error) { j.add('registry.append.failure', { key, error: errorData(error) }); throw error; }
      })();
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
      } else if (variant.slowStatus) {
        await j.wait(r => r.kind === 'status.block');
        releaseUnknown();
        if (variant.failAppend === 'error:gateway_submit_failed') {
          await j.wait(r => r.kind === 'queue.enqueue' && r.key === 'error:result_unknown');
        } else await waitCatchPublished();
        j.add('control.release.status'); statusGate.resolve();
        await waitClose();
      } else {
        await waitClose(); releaseUnknown();
        if (variant.failAppend === 'error:gateway_submit_failed') {
          await j.wait(r => r.kind === 'registry.append.failure' && r.key === variant.failAppend);
        } else await waitCatchPublished();
      }
      j.add('control.release.append'); appendGate.resolve();
    }
    // Pump finally closes again after timeoutSettlement and all registry writes.
    await j.wait(r => r.kind === 'session.submit.exit');
    await j.wait(r => r.kind === 'queue.close' && (order === 'unknown-first' || order === 'known-final-first' || r.alreadyClosed));
    await drainAppends();
    if (!j.has('client.done')) { j.add('client.resume'); await consume(); }
    assert.ok(j.has('fence.complete'));
    assert.equal(activeQueue.size, 1, 'the passive tap must observe one queue only');
    const statusText = readFileSync(statusPath, 'utf8');
    const statuses = statusText.trim() ? statusText.trim().split('\n').map(line => JSON.parse(line)) : [];
    const durable = { source: relative(WORKTREE, statusPath), rawText: statusText, entries: statuses,
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
    const observation = { id, variant, phase, repetition, frames, durableStatus: durable, registry: registryObservation, timeline: j.rows,
      transcriptRawText: readFileSync(transcriptPath, 'utf8') };
    checkCompleteness(observation);
    if (variant.failStatus) assert.ok(j.has('status.failure', variant.failStatus), 'status fault was exercised');
    if (variant.failAppend) assert.ok(j.has('registry.persist.failure', variant.failAppend), 'registry disk fault was exercised');
    if (variant.slowStatus) assert.ok(j.has('status.block', 'turn_timeout'), 'status barrier was exercised');
    if (variant.slowAppend) assert.ok(j.has('registry.append.block', 'error:turn_timeout'), 'append barrier was exercised');
    return observation;
  } finally {
    gateway.dispose();
    await router.shutdown();
    globalThis.setTimeout = realSetTimeout; globalThis.clearTimeout = realClearTimeout;
    AsyncQueue.prototype.enqueue = oldEnqueue; AsyncQueue.prototype.close = oldClose;
    console.warn = oldWarn; console.info = oldInfo;
  }
}

function checkCompleteness(o: any) {
  assert.ok(o.frames.length > 0, 'client observer is nonempty');
  assert.ok(o.durableStatus.entries.length > 0 || o.durableStatus.emptyReason, 'durable empty explicitly explained');
  assert.ok(o.timeline.some((r: any) => r.kind === 'loop.final'), 'loop reached a controlled outcome');
  if (o.registry.applicable) {
    assert.ok(o.registry.events.length > 0, 'registry observer is nonempty');
    assert.ok(o.registry.record, 'registry record read');
    const disk = JSON.parse(o.registry.rawText);
    assert.deepEqual(o.registry.record, disk.records[0], 'record observer agrees with raw disk snapshot');
    assert.deepEqual(o.registry.events, Object.values(disk.events)[0] ?? [], 'event observer agrees with raw disk snapshot');
  } else assert.ok(o.registry.reason);
  const storedStatus = o.durableStatus.rawText.trim();
  assert.deepEqual(o.durableStatus.entries, storedStatus ? storedStatus.split('\n').map((line: string) => JSON.parse(line)) : [], 'status observer agrees with raw file');
  const committed = o.timeline.filter((r: any) => r.kind === 'status.commit').map((r: any) => r.key);
  assert.deepEqual(o.durableStatus.entries.map((r: any) => r.status.event), committed, 'durable reread vs commit journal (not frames)');
  const published = o.timeline.filter((r: any) => r.kind === 'queue.enqueue' && !r.closed).map((r: any) => r.event);
  assert.deepEqual(o.frames, published, 'actual client frames vs open queue publications');
  if (o.registry.applicable) {
    assert.deepEqual(o.registry.events.map((e: any) => e.seq), o.registry.events.map((_: any, i: number) => i + 1));
    assert.equal(o.registry.record.lastSeq, o.registry.events.length);
  }
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
    registry: o.registry.applicable ? { events: o.registry.events, record } : 'N/A' });
}
function summarize(o: any) {
  const prefix = new Set(['turn_started', 'input_accepted', 'model_request_started', 'tool_call_started']);
  return {
    framesAfterPrefix: o.frames.filter((e: any) => !prefix.has(e.type)).map(label),
    durableStatuses: o.durableStatus.entries.map((e: any) => e.status.event),
    registryEventsAfterPrefix: o.registry.applicable ? o.registry.events.filter((e: any) => !prefix.has(e.event.type)).map((e: any) => `${e.seq}:${label(e.event)}`) : 'N/A',
    registryState: o.registry.applicable ? o.registry.record.state : 'N/A',
    registryResult: o.registry.applicable ? (o.registry.record.result ? label(o.registry.record.result) : '(absent)') : 'N/A',
    registryRevision: o.registry.applicable ? o.registry.record.revision : 'N/A',
    registryLastSeq: o.registry.applicable ? o.registry.record.lastSeq : 'N/A',
    droppedEnqueues: o.timeline.filter((r: any) => r.kind === 'queue.enqueue' && r.closed).map((r: any) => r.key),
  };
}
function observerControls(o: any) {
  const controls: any[] = [];
  const missing = clone(o); missing.frames = missing.frames.filter((e: any) => e.code !== 'turn_timeout');
  assert.throws(() => checkCompleteness(missing)); controls.push({ mutation: 'drop observed timeout error frame', detected: true });
  const missingStatus = clone(o); missingStatus.durableStatus.entries.pop();
  assert.throws(() => checkCompleteness(missingStatus)); controls.push({ mutation: 'drop durable status entry', detected: true });
  return controls;
}

const summary: any[] = [];
let controls: any[] = [];
let firstRegistry: any;
for (const cell of cells) {
  const observations: any[] = [];
  const repetitions: any[] = [];
  for (const variant of cell.variants) for (const phase of ['model', 'tool']) {
    const runs: any[] = [];
    for (let repetition = 1; repetition <= 5; repetition++) {
      const watchdog = realSetTimeout(() => { console.error(`Harness stalled: ${cell.name}/${variant.name}/${phase}/${repetition}`); process.exit(2); }, 15000);
      try { runs.push(await measure(cell, variant, phase, repetition)); }
      finally { realClearTimeout(watchdog); }
    }
    const keys = runs.map(r => JSON.stringify(signature(r)));
    const stable = new Set(keys).size === 1;
    const row = { cell: cell.name, variant: variant.name, phase, runs: runs.length, distinct: new Set(keys).size, stable,
      ...summarize(runs[0]), observedAlternatives: [...new Map(runs.map(r => [JSON.stringify(signature(r)), summarize(r)])).values()] };
    summary.push(row); repetitions.push(row); observations.push(...runs);
    console.log(JSON.stringify(row));
    if (cell.name === 'no-registry-fast' && variant.name === 'fast' && phase === 'model') controls = observerControls(runs[0]);
    if (cell.name === 'registry-fast' && variant.name === 'fast' && phase === 'model') firstRegistry = runs[0];
  }
  writeFileSync(resolve(OUTPUT, cell.name + '.json'), JSON.stringify({ cell: cell.name, baselineCommit, node: process.version, repetitions, observations }, null, 2) + '\n');
}
// Independent observer mutations: events omission and result overwrite cannot
// silently disappear from the experiment's semantic comparison.
const removedEvents = clone(firstRegistry); removedEvents.registry.events.pop();
assert.throws(() => checkCompleteness(removedEvents));
controls.push({ mutation: 'drop registry event', detected: true });
const ignoredResult = clone(firstRegistry); ignoredResult.registry.record.result = { type: 'error', code: 'turn_timeout' };
assert.throws(() => checkCompleteness(ignoredResult));
controls.push({ mutation: 'hide registry result overwrite', detected: true });
for (const [path, hash] of importedSources) assert.equal(createHash('sha256').update(readFileSync(path)).digest('hex'), hash, `baseline changed: ${path}`);
const manifest = {
  generatedAt: new Date().toISOString(), node: process.version, baselineCommit,
  worktreeCommit: execFileSync('git', ['-C', WORKTREE, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
  command: 'node scripts/a5-19/six_cells.mts', pilotHome: process.env.PILOT_HOME,
  scriptSha256: createHash('sha256').update(readFileSync(fileURLToPath(import.meta.url))).digest('hex'),
  // 路径与哈希分两行写：文件名含 token / credential 时，同一行的 64 位哈希会被 gitleaks 误判为密钥。
  importedSources: [...importedSources].map(([path, hash]) => ({ path: relative(BASELINE, path), sha256: hash })),
  checks: { nonemptyObservers: true, diskRereadAgreesWithFreshRegistry: true, baselineHashesUnchanged: true, observerControls: controls },
  repeats: 5, totalMeasurements: summary.reduce((n, r) => n + r.runs, 0), summary,
};
writeFileSync(resolve(OUTPUT, 'manifest.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(`DONE ${manifest.totalMeasurements} measurements; ${summary.filter(r => !r.stable).length} unstable variant/phase groups; ${importedSources.size} unchanged baseline source files.`);
