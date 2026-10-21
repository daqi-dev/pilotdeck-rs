// Run from the repository root with the fixed baseline's tsx loader; see README.md.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/gateway/SessionRouter.ts', 'src/cli/ProjectRuntimeRegistry.ts',
  'src/gateway/client/GatewayTurnCompletionFence.ts', 'src/gateway/client/InProcessGateway.ts',
  ...['AgentFactoryProvider', 'AgentRegistry', 'AgentHandle', 'ScopedServiceRegistry']
    .map(name => `src/agent/scope/${name}.ts`),
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
// Transpile entire, unchanged modules. Load the real scope dependencies of the
// Router rather than the unrelated agent barrel. Gateway constructor wiring is
// skipped; any attempt to use an unrelated imported service fails closed.
async function load(source: string, imports: Record<string, any> = {}) {
  const compiled = transformSync(await readFile(join(baseline, source), 'utf8'), {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
  }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', compiled)((name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name in imports) return imports[name];
    return new Proxy({}, { get(_target, property) {
      if (property === '__esModule') return true;
      throw new Error(`unexercised import accessed: ${source} -> ${name}.${String(property)}`);
    } });
  }, module, module.exports);
  return module.exports;
}
const scope = 'src/agent/scope/';
const services = await load(scope + 'ScopedServiceRegistry.ts');
const handles = await load(scope + 'AgentHandle.ts');
const registry = await load(scope + 'AgentRegistry.ts', {
  './AgentHandle.js': handles, './ScopedServiceRegistry.js': services,
});
const factory = await load(scope + 'AgentFactoryProvider.ts', {
  './AgentHandle.js': handles, './AgentRegistry.js': registry,
});
const { SessionRouter } = await load(sources[0], {
  '../agent/index.js': { ...handles, ...registry, ...factory },
});
const { ProjectRuntimeRegistry } = await load(sources[1]);
const { GatewayTurnCompletionFence } = await load(sources[2]);
const { InProcessGateway } = await load(sources[3]);
function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(done => { resolve = done; });
  return { promise, resolve };
}
async function until(predicate: () => boolean) {
  for (let n = 0; n < 1000; n++) { if (predicate()) return; await Promise.resolve(); }
  throw new Error('controlled operation did not progress');
}
const context = (key: string, extra = {}) => ({ sessionKey: key, channelKey: 'web', projectKey: 'project', ...extra });
function harness() {
  const events: string[] = [];
  let now = 0;
  let nextId = 0;
  const creates = new Map<string, ReturnType<typeof deferred>>();
  const disposals = new Map<number, ReturnType<typeof deferred>>();
  const sessions = new Map<number, any>();
  const setupFailures = new Set<number>();
  const disposeFailures = new Set<number>();
  const factoryFailures = new Set<string>();
  const abortFailures = new Set<number>();
  let maintenance: ReturnType<typeof deferred> | undefined;
  const make = async (ctx: any) => {
    const id = ++nextId;
    events.push(`create:${ctx.sessionKey}:${id}`);
    await creates.get(ctx.sessionKey)?.promise;
    if (factoryFailures.has(ctx.sessionKey)) throw new Error('factory failed');
    const session = {
      sessionId: `session-${id}`,
      snapshot: () => ({ sessionId: `session-${id}`, messages: [] }),
      abort: (reason: string) => {
        events.push(`abort:${id}:${reason}`);
        if (abortFailures.has(id)) throw new Error('abort failed');
      },
      dispose: async () => {
        events.push(`dispose:${id}`);
        await disposals.get(id)?.promise;
        if (disposeFailures.has(id)) throw new Error('dispose failed');
      },
      compact: async ({ abortSignal }: any) => {
        events.push(`maintenance:${id}`);
        await maintenance?.promise;
        events.push(`maintenance_done:${id}:${abortSignal.aborted}`);
        return { compacted: true };
      },
    };
    sessions.set(id, session);
    return session;
  };
  const router = new SessionRouter({
    createSession: make, recreateSession: make,
    setupSession: async (_ctx: any, handle: any) => {
      const id = Number(handle.sessionId.slice(8));
      events.push(`setup:${id}`);
      if (setupFailures.has(id)) throw new Error('setup failed');
    },
    now: () => new Date(now), idleSessionTimeoutMs: 100, idleSweepIntervalMs: 0,
    onSessionEvict: (key: string, reason: string) => events.push(`evict:${key}:${reason}`),
  });
  return { events, router, creates, disposals, setupFailures, disposeFailures, factoryFailures, abortFailures,
    time: (value: number) => { now = value; },
    maintenance: (gate: ReturnType<typeof deferred>) => { maintenance = gate; } };
}
const cases: any[] = [];
async function record(name: string, run: (h: ReturnType<typeof harness>) => Promise<any>) {
  const h = harness();
  const value = await run(h);
  cases.push({ name, value, events: [...h.events] });
  // Clean up outside the observation; every allocated handle is drained.
  await h.router.shutdown().catch(() => {});
}
await record('dirty_mark_during_recreation_is_cleared', async h => {
  const old = await h.router.getOrCreate(context('a'));
  h.router.markAllDirty();
  const gate = deferred(); h.creates.set('a', gate);
  const replacing = h.router.getOrCreate(context('a'));
  await until(() => h.events.includes('create:a:2'));
  const marked = h.router.markAllDirty();
  gate.resolve(); const fresh = await replacing;
  const next = await h.router.getOrCreate(context('a'));
  return { marked, replaced: old !== fresh, nextIsFresh: next === fresh };
});
await record('dirty_identity_and_publication', async h => {
  const old = await h.router.getOrCreate(context('a'));
  const drain = deferred(); h.disposals.set(1, drain);
  h.router.markAllDirty();
  let returned = false;
  const pending = h.router.getOrCreate(context('a')).then((handle: any) => { returned = true; return handle; });
  await until(() => h.events.includes('evict:a:dirty_recreate'));
  const current = h.router.agents.get('a');
  const during = { oldCurrent: current === old, newPublished: current !== old,
    oldState: old.state, returned, cached: h.router.cachedSessionCount() };
  drain.resolve(); const fresh = await pending;
  return { during, oldCurrent: h.router.agents.get('a') === old,
    freshCurrent: h.router.agents.get('a') === fresh, oldState: old.state };
});
await record('slow_factory_is_per_session', async h => {
  const gate = deferred(); h.creates.set('a', gate);
  let aDone = false;
  const a = h.router.getOrCreate(context('a')).then(() => { aDone = true; });
  await until(() => h.events.includes('create:a:1'));
  await h.router.getOrCreate(context('b'));
  const during = { aDone, cached: h.router.cachedSessionCount() };
  gate.resolve(); await a;
  return { during, cached: h.router.cachedSessionCount() };
});
await record('maintenance_blocks_turn_and_idle', async h => {
  await h.router.getOrCreate(context('a'));
  const gate = deferred(); h.maintenance(gate);
  const pending = h.router.compact('a', { turnId: 'maintenance' });
  await until(() => h.events.includes('maintenance:1'));
  h.time(101);
  const during = { same: h.router.beginTurn('a', 'blocked'), other: h.router.beginTurn('b', 'run'),
    cached: h.router.sessionCount() };
  gate.resolve(); await pending;
  return { during, after: h.router.beginTurn('a', 'run') };
});
await record('shutdown_during_creation', async h => {
  const gate = deferred(); h.creates.set('a', gate);
  const creation = h.router.getOrCreate(context('a')).then(() => null, (e: Error) => e.message);
  await until(() => h.events.includes('create:a:1'));
  let stopped = false;
  const shutdown = h.router.shutdown().then(() => { stopped = true; });
  const during = { stopped, admitted: h.router.beginTurn('b', 'run'), cached: h.router.cachedSessionCount() };
  gate.resolve(); const error = await creation; await shutdown;
  const lateError = await h.router.getOrCreate(context('b')).then(() => null, (e: Error) => e.message);
  return { during, error, lateError, cached: h.router.cachedSessionCount(), registry: h.router.agents.size };
});
await record('close_drains_before_recreate', async h => {
  await h.router.getOrCreate(context('a'));
  const gate = deferred(); h.disposals.set(1, gate);
  const closing = h.router.close('a');
  await until(() => h.events.includes('dispose:1'));
  let done = false;
  const creating = h.router.getOrCreate(context('a')).then(() => { done = true; });
  await Promise.resolve(); await Promise.resolve();
  const during = { done, cached: h.router.cachedSessionCount() };
  gate.resolve(); await closing; await creating;
  return { during, cached: h.router.cachedSessionCount() };
});
await record('idle_strict_boundary_and_turn', async h => {
  await h.router.getOrCreate(context('a'));
  h.time(100); const boundary = h.router.sessionCount();
  h.router.beginTurn('a', 'run'); h.time(101);
  const busy = h.router.sessionCount();
  h.router.endTurn('a', 'run'); h.time(201);
  const touched = h.router.sessionCount();
  h.time(202); const evicted = h.router.sessionCount();
  await until(() => h.events.includes('evict:a:idle'));
  return { boundary, busy, touched, evicted };
});
await record('setup_failure_keeps_old_writer', async h => {
  const old = await h.router.getOrCreate(context('a'));
  h.router.markAllDirty(); h.setupFailures.add(2);
  const error = await h.router.getOrCreate(context('a')).then(() => null, (e: Error) => e.message);
  return { error, oldCurrent: h.router.agents.get('a') === old, oldState: old.state, cached: h.router.cachedSessionCount() };
});
await record('factory_failure_keeps_old_writer', async h => {
  const old = await h.router.getOrCreate(context('a'));
  h.router.markProjectDirty('project'); h.factoryFailures.add('a');
  const error = await h.router.getOrCreate(context('a')).then(() => null, (e: Error) => e.message);
  return { error, oldCurrent: h.router.agents.get('a') === old, oldState: old.state };
});
await record('tool_filters_cache_and_dirty', async h => {
  const first = await h.router.getOrCreate(context('a', { allowedTools: ['read'] }));
  const retained = await h.router.getOrCreate(context('a', { projectKey: 'other', channelKey: '' }));
  const fresh = await h.router.getOrCreate(context('a', { allowedTools: ['write'] }));
  return { retained: first === retained, replaced: fresh !== first, cached: h.router.cachedSessionCount() };
});
await record('close_failure_still_unpublishes', async h => {
  const handle = await h.router.getOrCreate(context('a'));
  h.disposeFailures.add(1);
  const error = await h.router.close('a').then(() => null, (e: Error) => e.message);
  return { error, state: handle.state, cached: h.router.cachedSessionCount(), active: h.router.hasActiveTurn('a') };
});
await record('handle_dispose_drains_maintenance', async h => {
  const handle = await h.router.getOrCreate(context('a'));
  const gate = deferred();
  let cancelled = false;
  const running = handle.runMaintenance(async (signal: AbortSignal) => {
    h.events.push('maintenance:1'); await gate.promise; cancelled = signal.aborted;
  });
  await until(() => h.events.includes('maintenance:1'));
  let done = false;
  const disposing = handle.dispose('stop').then(() => { done = true; });
  const during = { done, state: handle.state };
  gate.resolve(); await running; await disposing;
  return { during, cancelled, state: handle.state };
});
await record('live_abort_with_fence', async h => {
  await h.router.getOrCreate(context('a'));
  h.router.beginTurn('a', 'old');
  const fence = new GatewayTurnCompletionFence();
  const old = fence.begin('a', 'old');
  h.router.endTurn('a', 'old'); fence.complete('a', old);
  h.router.markAllDirty(); await h.router.getOrCreate(context('a'));
  h.router.beginTurn('a', 'new'); const fresh = fence.begin('a', 'new');
  const gateway = Object.assign(Object.create(InProcessGateway.prototype), {
    router: h.router, turnCompletionFence: fence, abortedTurnReasons: new Map(),
  });
  await gateway.abortTurn({ sessionKey: 'a', runId: 'old' });
  await gateway.abortTurn({ sessionKey: 'a', runId: '' });
  const staleCancelled = fresh.signal.aborted;
  let done = false;
  const abort = gateway.abortTurn({ sessionKey: 'a', runId: 'new', reason: 'stop' }).then(() => { done = true; });
  await until(() => h.events.includes('abort:2:stop'));
  const during = { done, cancelled: fresh.signal.aborted };
  fence.complete('a', fresh); await abort;
  return { staleCancelled, during, done };
});
await record('shutdown_drains_all_handles', async h => {
  const a = await h.router.getOrCreate(context('a'));
  const b = await h.router.getOrCreate(context('b'));
  const first = deferred(), second = deferred(); h.disposals.set(1, first); h.disposals.set(2, second);
  let done = false;
  const shutdown = h.router.shutdown().then(() => { done = true; });
  const cached = h.router.cachedSessionCount();
  await until(() => h.events.includes('dispose:1') && h.events.includes('dispose:2'));
  const during = { done, cached, a: a.state, b: b.state };
  second.resolve(); await until(() => h.events.includes('evict:b:shutdown'));
  const afterSecond = done;
  first.resolve(); await shutdown;
  return { during, afterSecond, done };
});
await record('project_close_during_creation_is_local', async h => {
  const gate = deferred(); h.creates.set('a', gate);
  const creating = h.router.getOrCreate(context('a')).then(() => null, (e: Error) => e.message);
  await until(() => h.events.includes('create:a:1'));
  const closing = h.router.closeProject('project');
  const blocked = await h.router.getOrCreate(context('blocked')).then(() => null, (e: Error) => e.message);
  await h.router.getOrCreate(context('b', { projectKey: 'other' }));
  const during = h.router.cachedSessionCount();
  gate.resolve(); const error = await creating; const keys = await closing;
  h.router.resumeProject('project'); await h.router.getOrCreate(context('a'));
  return { blocked, during, error, keys, cached: h.router.cachedSessionCount() };
});
await record('abort_failure_does_not_wait_for_completion', async h => {
  await h.router.getOrCreate(context('a')); h.router.beginTurn('a', 'run');
  h.abortFailures.add(1);
  const fence = new GatewayTurnCompletionFence(); const handle = fence.begin('a', 'run');
  const gateway = Object.assign(Object.create(InProcessGateway.prototype), {
    router: h.router, turnCompletionFence: fence, abortedTurnReasons: new Map(),
  });
  const error = await gateway.abortTurn({ sessionKey: 'a', runId: 'run', reason: 'stop' })
    .then(() => null, (e: Error) => e.message);
  return { error, cancelled: handle.signal.aborted, current: fence.isCurrent('a', handle) };
});
await record('completion_owner_without_router_reservation', async h => {
  await h.router.getOrCreate(context('a'));
  const fence = new GatewayTurnCompletionFence(); const handle = fence.begin('a', 'run');
  const gateway = Object.assign(Object.create(InProcessGateway.prototype), {
    router: h.router, turnCompletionFence: fence, abortedTurnReasons: new Map(),
  });
  let done = false;
  const abort = gateway.abortTurn({ sessionKey: 'a', runId: 'run', reason: 'stop' }).then(() => { done = true; });
  await until(() => h.events.includes('abort:1:stop'));
  const during = { done, cancelled: handle.signal.aborted };
  fence.complete('a', handle); await abort;
  return { during, done };
});
await record('empty_project_pause_and_close', async h => {
  await h.router.getOrCreate(context('existing', { projectKey: '' }));
  await h.router.getOrCreate(context('unscoped', { projectKey: undefined }));
  const gate = deferred(); h.creates.set('pending', gate);
  const pending = h.router.getOrCreate(context('pending', { projectKey: '' }));
  await until(() => h.events.includes('create:pending:3'));
  let closed = false;
  const closing = h.router.closeProject('').then((keys: string[]) => { closed = true; return keys; });
  await Promise.resolve();
  const waitedForCreation = !closed;
  await h.router.getOrCreate(context('during', { projectKey: '' }));
  gate.resolve(); await pending;
  const keys = await closing;
  const duringClosed = !h.router.hasSession('during');
  await h.router.getOrCreate(context('after', { projectKey: '' }));
  return { waitedForCreation, keys, existingClosed: !h.router.hasSession('existing'),
    pendingClosed: !h.router.hasSession('pending'), duringClosed,
    unscopedKept: h.router.hasSession('unscoped'), afterAllowed: h.router.hasSession('after') };
});
// Exercise the real registry's exact lease/retirement methods. Only its resource
// bundle and background shutdown are fake; no generation/lease logic is copied.
const projectEvents: string[] = [];
const registryRuntime = Object.create(ProjectRuntimeRegistry.prototype);
registryRuntime.runtimeDisposals = new Set();
const runtime = { runtimeState: 'active', sessionLeases: 0,
  backgroundSubagents: { shutdown: () => projectEvents.push('project_shutdown') },
  resourcesBundle: { dispose: async () => projectEvents.push('project_dispose') } };
const release = registryRuntime.acquireRuntimeLease(runtime);
registryRuntime.retireRuntime(runtime);
const before = { state: runtime.runtimeState, leases: runtime.sessionLeases, events: [...projectEvents] };
await release(); await release();
cases.push({ name: 'project_lease_identity', value: { before,
  after: { state: runtime.runtimeState, leases: runtime.sessionLeases, events: projectEvents } }, events: [] });
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n');
console.log(`recorded ${cases.length} real TS cases`);
