// #2294 A7-W7-pr3: TS baseline probe — the always-on provider-assembly,
// scheduler-tick, and stop/reload semantics this issue ports. Every scenario
// prints one `### <id> <json>` line so the raw Node output can be pasted into
// the issue evidence verbatim; the same records land in `<out>/oracle.json`
// for the Rust replay tests (byte-faithful provider conditions, start/stop
// order, reload-only-changed-families, gate chain, envelopes, state/lock
// bytes). Same loader, pinned clock, and locale as the cron-scheduler probe
// (#2295): every module is the real baseline file, loaded with tsx's esbuild
// transform; only the narrow ports the scenario drives are faked.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { mock } from 'node:test';
import { assertBaseline } from '../_shared/baseline.mts';
import { installFixedClock } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
assert.equal(process.version, 'v22.22.0', '#2294 pinned Node');
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US',
  'run with LANG=en_US.UTF-8 and LC_ALL/LC_MESSAGES unset (got ' +
  `${new Intl.Collator().resolvedOptions().locale})`);
const baseline = resolve(process.env.PDRS_BASELINE);
assertBaseline(baseline);
installFixedClock();

const out = resolve(process.argv[2]);
await rm(join(out, 'scratch'), { recursive: true, force: true });
await mkdir(join(out, 'scratch'), { recursive: true });
const golden: Array<{ id: string; value: unknown }> = [];
function emit(id: string, value: unknown): void {
  golden.push({ id, value });
  process.stdout.write(`### ${id} ${JSON.stringify(value)}\n`);
}
async function settle(rounds = 8): Promise<void> {
  for (let i = 0; i < rounds; i++) await new Promise((r) => setImmediate(r));
}

const req = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(req.resolve('tsx'))('esbuild');
async function load(source: string, imports: Record<string, any> = {}) {
  const code = transformSync(await readFile(join(baseline, source), 'utf8'),
    { loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', code)((name: string) => {
    if (name in imports) return imports[name];
    if (name.startsWith('node:')) return req(name);
    return new Proxy({}, { get(_t, p) { if (p === '__esModule') return true;
      throw new Error(`unexercised import ${source} -> ${name}.${String(p)}`); } });
  }, module, module.exports);
  return module.exports;
}

// --- module graph -----------------------------------------------------------
const pathsMod = await load('src/pilot/paths.ts');
const alwaysOnPathsMod = await load('src/always-on/storage/AlwaysOnPaths.ts',
  { '../../pilot/paths.js': pathsMod });
const configMod = await load('src/always-on/config/parseAlwaysOnConfig.ts');
const errorsMod = await load('src/always-on/protocol/errors.ts');
const gatesMod = await load('src/always-on/runtime/DiscoveryGates.ts');
const leaseMod = await load('src/always-on/runtime/ChannelLeaseRegistry.ts');
const stateStoreMod = await load('src/always-on/storage/DiscoveryStateStore.ts');
const cycleStoreMod = await load('src/always-on/storage/WorkCycleStore.ts');
const planStoreMod = await load('src/always-on/storage/DiscoveryPlanStore.ts',
  { './AlwaysOnPaths.js': alwaysOnPathsMod });
const reportStoreMod = await load('src/always-on/storage/DiscoveryReportStore.ts',
  { './AlwaysOnPaths.js': alwaysOnPathsMod });
const eventStoreMod = await load('src/always-on/storage/AlwaysOnEventStore.ts');
const storageProviderMod = await load('src/always-on/runtime/AlwaysOnProjectStorageProvider.ts', {
  '../storage/AlwaysOnPaths.js': alwaysOnPathsMod,
  '../storage/DiscoveryPlanStore.js': planStoreMod,
  '../storage/WorkCycleStore.js': cycleStoreMod,
  '../storage/AlwaysOnEventStore.js': eventStoreMod,
  '../storage/DiscoveryReportStore.js': reportStoreMod,
  '../storage/DiscoveryStateStore.js': stateStoreMod,
});
const signalWatcherMod = await load('src/always-on/runtime/SignalWatcher.ts');
const runContextsMod = await load('src/always-on/runtime/AlwaysOnRunContextRegistry.ts');
const sessionOverridesMod = await load('src/always-on/runtime/SessionConfigOverrides.ts');
// DiscoveryFire: the deep pipeline stays behind fakes the scenarios never
// drive; only the lock pair and the class shape are exercised.
const fireMod = await load('src/always-on/runtime/DiscoveryFire.ts', {
  '../../pilot/paths.js': pathsMod,
  '../contracts/ReportContract.js': {
    buildFallbackReport: () => '',
    parseReportMarkdown: () => { throw new Error('probe: report parse'); },
  },
  '../protocol/errors.js': errorsMod,
  '../workspace/WorkspaceApply.js': {
    generateWorkspaceDiff: () => { throw new Error('probe: workspace diff must not run'); },
  },
  './discoveryPrompts.js': {
    buildDiscoveryPrompt: () => '', buildExecutionPrompt: () => '',
    buildWorkspacePrompt: () => '', buildReportPrompt: () => '', buildApplyPrompt: () => '',
  },
  './SessionConfigOverrides.js': { UNATTENDED_SESSION_EXCLUDED_TOOLS: [] },
});
const schedulerMod = await load('src/always-on/runtime/DiscoveryScheduler.ts', {
  '../config/parseAlwaysOnConfig.js': configMod,
  '../protocol/errors.js': errorsMod,
  './ChannelLeaseRegistry.js': leaseMod,
  './DiscoveryFire.js': fireMod,
  './DiscoveryGates.js': gatesMod,
  './SignalWatcher.js': signalWatcherMod,
});
const workspaceRegistryMod = await load('src/always-on/workspace/WorkspaceProviderRegistry.ts',
  { '../protocol/errors.js': errorsMod });
const runtimeImports = {
  '../config/parseAlwaysOnConfig.js': configMod,
  '../storage/AlwaysOnPaths.js': alwaysOnPathsMod,
  '../workspace/GitWorktreeProvider.js': { GitWorktreeProvider: class { constructor(_o: unknown) {} } },
  '../workspace/SnapshotCopyProvider.js': { SnapshotCopyProvider: class { constructor(_o: unknown) {} } },
  '../workspace/WorkspaceProviderRegistry.js': workspaceRegistryMod,
  './AlwaysOnRunContextRegistry.js': runContextsMod,
  './ChannelLeaseRegistry.js': leaseMod,
  './DiscoveryFire.js': fireMod,
  './DiscoveryScheduler.js': schedulerMod,
  './SessionConfigOverrides.js': sessionOverridesMod,
  './AlwaysOnProjectStorageProvider.js': storageProviderMod,
};
const toolFakes = {
  '../tool/AlwaysOnDiscoveryPlanTool.js': { createAlwaysOnDiscoveryPlanTool: () => ({ name: 'ao-plan' }) },
  '../tool/AlwaysOnReportTool.js': { createAlwaysOnReportTool: () => ({ name: 'ao-report' }) },
  '../tool/AlwaysOnWorkspaceTool.js': { createAlwaysOnWorkspaceTool: () => ({ name: 'ao-workspace' }) },
  '../tool/AlwaysOnChatHistoryTool.js': { createAlwaysOnChatHistoryTool: () => ({ name: 'ao-history' }) },
};
const runtimeMod = await load('src/always-on/runtime/AlwaysOnRuntime.ts', runtimeImports);
const managerMod = await load('src/always-on/runtime/AlwaysOnManager.ts', {
  ...runtimeImports, ...toolFakes, './AlwaysOnRuntime.js': runtimeMod,
});
const bundleMod = await load('src/cli/ProjectAutomationBundle.ts', {
  '../session/index.js': {
    createProjectSessionReadSideBundle: () => ({ catalog: { kind: 'fake-catalog' }, transcriptReader: { kind: 'fake-reader' } }),
  },
});

// --- shared fixtures --------------------------------------------------------
const NOW = new Date('2026-01-01T00:05:00.000Z');
function config(projects: Record<string, { enabled: boolean }>, trigger: Record<string, any> = {}) {
  const cfg = configMod.defaultAlwaysOnConfig();
  cfg.projects = projects;
  Object.assign(cfg.trigger, trigger);
  return cfg;
}
async function scratch(id: string): Promise<string> {
  const dir = join(out, 'scratch', id);
  await mkdir(dir, { recursive: true });
  return dir;
}
let uuidSeq = 0;
const uuid = () => `probe-run-${++uuidSeq}`;
const logger = {
  info: (m: string, d?: unknown) => { /* not recorded; scenarios needing it pass their own */ },
  warn: (m: string, d?: unknown) => { /* not recorded; scenarios needing it pass their own */ },
};
function recordingLogger(log: string[]) {
  return {
    info: (m: string, d?: unknown) => { log.push(`info:${m}:${JSON.stringify(d)}`); },
    warn: (m: string, d?: unknown) => { log.push(`warn:${m}:${JSON.stringify(d)}`); },
  };
}
function storage(pilotHome: string, projectKey: string) {
  return storageProviderMod.createNativeAlwaysOnProjectStorageProvider().create({ pilotHome, projectKey });
}
async function writeJson(path: string, value: unknown): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  await writeFile(path, JSON.stringify(value, null, 2));
}

// --- P1: gate chain (DiscoveryGates.ts:26-80, first failing gate wins) ------
{
  const evaluate = gatesMod.evaluateAlwaysOnDiscoveryGates;
  const cfg = config({ '/proj/a': { enabled: true }, '/proj/off': { enabled: false } });
  const baseState = { schemaVersion: 1, todayKey: '2026-01-01', todayRunCount: 0, consecutiveFailures: 0 };
  const okLease = (extra: Record<string, any> = {}) => ({
    schemaVersion: 1, writtenAt: '2026-01-01T00:04:30.000Z', channelKey: 'web', writerId: 'w1',
    projectKey: '/proj/a', sessionKey: 's1', agentBusy: false, lastUserMsgAt: null, ...extra,
  });
  const input = (over: Record<string, any>) => ({
    projectKey: '/proj/a', config: cfg, state: { ...baseState }, leases: [], now: NOW,
    projectExists: true, lockHeld: false, sessionInFlight: false, ...over,
  });
  emit('g1-key-not-in-config', evaluate(input({ projectKey: '/not-listed' })));
  emit('g2-disabled', evaluate(input({ projectKey: '/proj/off' })));
  emit('g3-missing', evaluate(input({ projectExists: false })));
  emit('g4-dormant', evaluate(input({ state: { ...baseState, dormant: { since: '2026-01-01T00:00:00.000Z', lastBaselineAt: '2026-01-01T00:00:00.000Z' } } })));
  emit('g5-session-in-flight', evaluate(input({ sessionInFlight: true })));
  emit('g6-lease-agent-busy', evaluate(input({ leases: [okLease({ agentBusy: true })] })));
  emit('g7-recent-user-msg', evaluate(input({ leases: [okLease({ lastUserMsgAt: '2026-01-01T00:04:00.000Z' })] })));
  emit('g7b-user-msg-exactly-window', evaluate(input({ leases: [okLease({ lastUserMsgAt: '2026-01-01T00:00:00.000Z' })] })));
  emit('g8-cooldown', evaluate(input({ state: { ...baseState, lastFireCompletedAt: '2026-01-01T00:04:00.000Z' } })));
  emit('g8b-cooldown-elapsed', evaluate(input({ state: { ...baseState, lastFireCompletedAt: '2026-01-01T00:00:00.000Z' }, config: config({ '/proj/a': { enabled: true } }, { cooldownMinutes: 5 }) })));
  emit('g9-daily-budget', evaluate(input({ state: { ...baseState, todayRunCount: 4 } })));
  emit('g10-lock-held', evaluate(input({ lockHeld: true })));
  emit('g11-ok-no-leases', evaluate(input({})));
  emit('g12-ok-preferred-lease', evaluate(input({ leases: [okLease({ channelKey: 'cli' }), okLease({ channelKey: 'web', writerId: 'w2' })] })));
  emit('g13-ok-fallback-first-lease', evaluate(input({ leases: [okLease({ channelKey: 'cli' })], config: config({ '/proj/a': { enabled: true } }, { preferChannel: 'zzz' }) })));
  emit('g14-first-fail-wins', evaluate(input({ projectKey: '/not-listed', projectExists: false, lockHeld: true })));
  emit('g15-order-missing-before-dormant', evaluate(input({ projectExists: false, state: { ...baseState, dormant: { since: 'x', lastBaselineAt: 'x' } } })));
  // The gate trusts its input list — staleness filtering is the scheduler's
  // listFresh, so a (stale) lease handed straight to the gate still blocks.
  emit('g16-lease-list-not-filtered', evaluate(input({ leases: [okLease({ agentBusy: true, writtenAt: '2025-12-01T00:00:00.000Z' })] })));
}

// --- P7: state store bytes ---------------------------------------------------
{
  const home = await scratch('state');
  const { stateStore, paths } = storage(home, '/proj/a');
  const read = async () => JSON.parse(await readFile(paths.stateFile, 'utf8'));
  emit('s1-default', await stateStore.read(NOW));
  await stateStore.markFireStarted('run-1', NOW);
  emit('s2-markFireStarted', await read());
  await stateStore.markFireCompleted({ outcome: 'failed', runId: 'run-1', now: NOW });
  emit('s3-markFireCompleted', await read());
  await stateStore.setDormant(NOW);
  emit('s4-setDormant', await read());
  await stateStore.clearDormant(NOW);
  emit('s5-clearDormant', await read());
  await stateStore.setActiveWorkCycleId('cycle-1', NOW);
  emit('s6-activeCycle', await read());
  // Daily reset: a stale todayKey resets the run count on read.
  await writeJson(paths.stateFile, { schemaVersion: 1, todayKey: '2025-12-31', todayRunCount: 5, consecutiveFailures: 2 });
  emit('s7-daily-reset', await stateStore.read(NOW));
  // Malformed / wrong schemaVersion fall back to the default state.
  await writeFile(paths.stateFile, '{oops', 'utf8');
  emit('s8-malformed', await stateStore.read(NOW));
  await writeJson(paths.stateFile, { schemaVersion: 2, todayRunCount: 9 });
  emit('s9-wrong-schema', await stateStore.read(NOW));
  // The exact bytes markFireStarted writes (no trailing newline).
  await rm(paths.stateFile);
  await stateStore.markFireStarted('run-bytes', NOW);
  emit('s10-markFireStarted-bytes', { bytes: await readFile(paths.stateFile, 'utf8') });
}

// --- P8: discovery lock (wx + payload bytes) --------------------------------
{
  const home = await scratch('lock');
  const { paths } = storage(home, '/proj/a');
  const first = await fireMod.acquireDiscoveryLock(paths, { pid: 4242, runId: 'lock-run-1', startedAt: '2026-01-01T00:05:00.000Z' });
  const bytes = await readFile(paths.discoveryLockFile, 'utf8');
  const second = await fireMod.acquireDiscoveryLock(paths, { pid: 99, runId: 'other', startedAt: 'x' });
  const bytesAfter = await readFile(paths.discoveryLockFile, 'utf8');
  await fireMod.releaseDiscoveryLock(paths);
  const afterRelease = existsSync(paths.discoveryLockFile);
  await fireMod.releaseDiscoveryLock(paths);
  emit('l1-acquire-release', { first, bytes, second, bytesAfter, afterRelease, doubleReleaseOk: true });
}

// --- P13: scheduler tick (gates -> cycle -> lock -> fire) --------------------
function fakeFire(log: string[], behavior: { run?: (input: any) => any } = {}) {
  return {
    calls: [] as any[],
    async run(input: any) {
      this.calls.push(input);
      log.push(`fire.run:${input.runId}`);
      if (behavior.run) return behavior.run(input);
      return { outcome: 'executed', runId: input.runId, startedAt: input.startedAt.toISOString(), finishedAt: input.startedAt.toISOString() };
    },
  };
}
interface TickSetup {
  config?: any;
  trigger?: Record<string, any>;
  isSessionInFlight?: () => boolean;
  leases?: boolean;
  pre?: (deps: any) => Promise<void>;
  fireBehavior?: { run: (input: any) => any };
}
async function tickScenario(id: string, setup: TickSetup = {}): Promise<void> {
  const home = await scratch(id);
  const projectKey = join(home, 'proj');
  await mkdir(projectKey, { recursive: true });
  const log: string[] = [];
  const fire = fakeFire(log, setup.fireBehavior);
  const store = storage(home, projectKey);
  const leases = new leaseMod.ChannelLeaseRegistry(() => NOW);
  if (setup.leases) {
    leases.set({ channelKey: 'web', writerId: 'w1', projectKey, sessionKey: 's', agentBusy: false, lastUserMsgAt: '2026-01-01T00:04:30.000Z' });
  }
  const deps = {
    config: setup.config ?? config({ [projectKey]: { enabled: true } }, setup.trigger ?? {}),
    projectKey,
    paths: store.paths,
    stateStore: store.stateStore,
    cycleStore: store.cycleStore,
    leases,
    fire,
    uuid,
    now: () => NOW,
    logger: recordingLogger(log),
    isSessionInFlight: setup.isSessionInFlight ?? (() => false),
  };
  if (setup.pre) await setup.pre(deps);
  const scheduler = new schedulerMod.DiscoveryScheduler(deps);
  const result = await scheduler.runTickOnce();
  emit(id, { result, fireCalls: fire.calls.length, lockAfter: existsSync(deps.paths.discoveryLockFile), log });
  await scheduler.stop();
}
{
  // t2..t9: every blocked gate leaves no lock and no fire (t1 covers the ok
  // path with its own scratch below).
  await tickScenario('t2-disabled', { config: null as any, pre: async (deps) => { deps.config = config({ [deps.projectKey]: { enabled: false } }); } });
  await tickScenario('t3-missing', { pre: async (deps) => { deps.config = config({ [`${dirname(deps.projectKey)}/gone`]: { enabled: true } }); deps.projectKey = `${dirname(deps.projectKey)}/gone`; } });
  await tickScenario('t4-dormant', { pre: async (deps) => writeJson(deps.paths.stateFile, { schemaVersion: 1, todayKey: '2026-01-01', todayRunCount: 0, consecutiveFailures: 0, dormant: { since: '2026-01-01T00:00:00.000Z', lastBaselineAt: '2026-01-01T00:00:00.000Z' } }) });
  await tickScenario('t5-agent-busy', { isSessionInFlight: () => true });
  await tickScenario('t6-cooldown', { pre: async (deps) => writeJson(deps.paths.stateFile, { schemaVersion: 1, todayKey: '2026-01-01', todayRunCount: 0, consecutiveFailures: 0, lastFireCompletedAt: '2026-01-01T00:04:00.000Z' }) });
  await tickScenario('t7-daily-budget', { pre: async (deps) => writeJson(deps.paths.stateFile, { schemaVersion: 1, todayKey: '2026-01-01', todayRunCount: 4, consecutiveFailures: 0 }) });
  await tickScenario('t8-lock-busy', { pre: async (deps) => writeJson(deps.paths.discoveryLockFile, { pid: 1, runId: 'held', startedAt: 'x' }) });
  await tickScenario('t9-recent-user-msg', { leases: true });
}
{
  // t1: clean project fires; state + lock bookkeeping visible.
  const home = await scratch('t1');
  const projectKey = join(home, 'proj');
  await mkdir(projectKey, { recursive: true });
  const log: string[] = [];
  const fire = fakeFire(log);
  const store = storage(home, projectKey);
  const deps = {
    config: config({ [projectKey]: { enabled: true } }),
    projectKey, paths: store.paths, stateStore: store.stateStore, cycleStore: store.cycleStore,
    leases: new leaseMod.ChannelLeaseRegistry(() => NOW), fire, uuid, now: () => NOW,
    logger: recordingLogger(log), isSessionInFlight: () => false,
  };
  const scheduler = new schedulerMod.DiscoveryScheduler(deps);
  const result = await scheduler.runTickOnce();
  const state = JSON.parse(await readFile(deps.paths.stateFile, 'utf8'));
  emit('t1-fired', { result, fireCalls: fire.calls.map((c: any) => c.runId), state, lockAfter: existsSync(deps.paths.discoveryLockFile), log });
}
{
  // t10: cycle full blocks before the lock; an under-full cycle proceeds.
  const home = await scratch('t10');
  const projectKey = join(home, 'proj');
  await mkdir(projectKey, { recursive: true });
  const log: string[] = [];
  const fire = fakeFire(log);
  const store = storage(home, projectKey);
  const deps = {
    config: config({ [projectKey]: { enabled: true } }),
    projectKey, paths: store.paths, stateStore: store.stateStore, cycleStore: store.cycleStore,
    leases: new leaseMod.ChannelLeaseRegistry(() => NOW), fire, uuid, now: () => NOW,
    logger: recordingLogger(log), isSessionInFlight: () => false,
  };
  const record = (planIds: string[]) => ({
    schemaVersion: 1,
    cycles: [{ id: 'c1', projectKey, status: 'active', workspace: { strategy: 'snapshot-copy', cwd: '/w', metadata: {} }, planIds, createdAt: '2026-01-01T00:00:00.000Z', createdByRunId: 'r0' }],
  });
  await writeJson(store.paths.cycleIndexFile, record(['p1', 'p2', 'p3']));
  await writeJson(store.paths.stateFile, { schemaVersion: 1, todayKey: '2026-01-01', todayRunCount: 0, consecutiveFailures: 0, activeWorkCycleId: 'c1' });
  const full = await new schedulerMod.DiscoveryScheduler(deps).runTickOnce();
  await writeJson(store.paths.cycleIndexFile, record(['p1', 'p2']));
  const notFull = await new schedulerMod.DiscoveryScheduler(deps).runTickOnce();
  emit('t10-cycle-full', { full, notFull, fireCalls: fire.calls.length, log });
}
{
  // t11: a fire crash rethrows with the code preserved, counts against the
  // daily budget, and still releases the lock.
  const home = await scratch('t11');
  const projectKey = join(home, 'proj');
  await mkdir(projectKey, { recursive: true });
  const log: string[] = [];
  const boom = new errorsMod.AlwaysOnError('workspace_prepare_failed', 'probe: fire crashed');
  const fire = fakeFire(log, { run: () => { throw boom; } });
  const store = storage(home, projectKey);
  const deps = {
    config: config({ [projectKey]: { enabled: true } }, { dailyBudget: 1 }),
    projectKey, paths: store.paths, stateStore: store.stateStore, cycleStore: store.cycleStore,
    leases: new leaseMod.ChannelLeaseRegistry(() => NOW), fire, uuid, now: () => NOW,
    logger: recordingLogger(log), isSessionInFlight: () => false,
  };
  const scheduler = new schedulerMod.DiscoveryScheduler(deps);
  let crash: any;
  try { await scheduler.runTickOnce(); } catch (error) { crash = { name: error.constructor.name, code: (error as any).code, message: (error as any).message }; }
  const state = JSON.parse(await readFile(deps.paths.stateFile, 'utf8'));
  const after = await scheduler.runTickOnce();
  emit('t11-fire-crash', { crash, state, lockAfter: existsSync(deps.paths.discoveryLockFile), nextTick: after, log });
}
{
  // t12: runTickOnce after stop answers the project_disabled shape.
  const home = await scratch('t12');
  const projectKey = join(home, 'proj');
  await mkdir(projectKey, { recursive: true });
  const log: string[] = [];
  const fire = fakeFire(log);
  const store = storage(home, projectKey);
  const deps = {
    config: config({ [projectKey]: { enabled: true } }),
    projectKey, paths: store.paths, stateStore: store.stateStore, cycleStore: store.cycleStore,
    leases: new leaseMod.ChannelLeaseRegistry(() => NOW), fire, uuid, now: () => NOW,
    logger: recordingLogger(log), isSessionInFlight: () => false,
  };
  const scheduler = new schedulerMod.DiscoveryScheduler(deps);
  await scheduler.start();
  await scheduler.stop();
  emit('t12-after-stop', await scheduler.runTickOnce());
}

// --- P9/P12: timer semantics under mock timers -------------------------------
// The tick's fs writes do not settle while node:test mock timers own the
// clock (observed: the tick stalls at its first threadpool op until the
// timers are reset), so the interval and stop-waits scenarios run against an
// in-memory state store + lock pair — the real bytes are pinned separately
// (s*/l*/t* scenarios). The lock fake keeps the wx contract: a second
// acquire while held returns false.
function inMemorySchedulerDeps(projectKey: string, minutes: number, fire: any, log: string[]) {
  const state: any = { schemaVersion: 1, todayKey: '2026-01-01', todayRunCount: 0, consecutiveFailures: 0 };
  const stateStore = {
    async read() { return { ...state }; },
    async markFireStarted(runId: string, now: Date) {
      state.lastFireStartedAt = now.toISOString();
      state.lastRunId = runId;
      state.todayKey = '2026-01-01';
      state.todayRunCount = state.todayRunCount + 1;
    },
    async clearDormant() { delete state.dormant; },
  };
  let lockHeld = false;
  const fakeFireModule = {
    DiscoveryFire: fireMod.DiscoveryFire,
    acquireDiscoveryLock: async () => { if (lockHeld) return false; lockHeld = true; log.push('lock.acquire'); return true; },
    releaseDiscoveryLock: async () => { lockHeld = false; log.push('lock.release'); },
  };
  return {
    state,
    lockHeld: () => lockHeld,
    fireModule: fakeFireModule,
    deps: {
      config: config({ [projectKey]: { enabled: true } }, { tickIntervalMinutes: minutes }),
      projectKey,
      paths: { projectKey, discoveryLockFile: '/tmp/probe-in-memory/discovery.lock' },
      stateStore,
      cycleStore: { async getRecord() { return undefined; } },
      leases: new leaseMod.ChannelLeaseRegistry(() => NOW),
      fire,
      uuid,
      now: () => NOW,
      logger: recordingLogger(log),
      isSessionInFlight: () => false,
    },
  };
}
async function loadSchedulerWithFire(fireModule: any) {
  return load('src/always-on/runtime/DiscoveryScheduler.ts', {
    '../config/parseAlwaysOnConfig.js': configMod,
    '../protocol/errors.js': errorsMod,
    './ChannelLeaseRegistry.js': leaseMod,
    './DiscoveryFire.js': fireModule,
    './DiscoveryGates.js': gatesMod,
    './SignalWatcher.js': signalWatcherMod,
  });
}
async function intervalScenario(id: string, minutes: number, firstTick: number): Promise<void> {
  const log: string[] = [];
  const fire = fakeFire(log);
  const home = await scratch(id);
  await mkdir(join(home, 'proj'), { recursive: true });
  const keyed = inMemorySchedulerDeps(join(home, 'proj'), minutes, fire, log);
  const schedulerMod2 = await loadSchedulerWithFire(keyed.fireModule);
  mock.timers.enable({ apis: ['setTimeout'] });
  const scheduler = new schedulerMod2.DiscoveryScheduler(keyed.deps);
  await scheduler.start();
  mock.timers.tick(firstTick);
  await settle(5);
  const before = fire.calls.length;
  mock.timers.tick(1);
  await settle(5);
  emit(id, { before, after: fire.calls.length, log });
  await scheduler.stop();
  mock.timers.reset();
}
await intervalScenario('i1-floor-1000ms', 0, 999);
await intervalScenario('i2-two-minutes', 2, 119_999);

// --- P12: stop waits for the in-flight tick, never aborts it -----------------
{
  const log: string[] = [];
  let releaseFire!: () => void;
  const gate = new Promise<void>((r) => { releaseFire = r; });
  const fire = fakeFire(log, { run: () => new Promise((res) => gate.then(() => res({ outcome: 'executed' }))) });
  const home = await scratch('w1');
  await mkdir(join(home, 'proj'), { recursive: true });
  const keyed = inMemorySchedulerDeps(join(home, 'proj'), 0, fire, log);
  const schedulerMod2 = await loadSchedulerWithFire(keyed.fireModule);
  mock.timers.enable({ apis: ['setTimeout'] });
  const scheduler = new schedulerMod2.DiscoveryScheduler(keyed.deps);
  await scheduler.start();
  mock.timers.tick(1000);
  await settle(5);
  let stopDone = false;
  const stopped = scheduler.stop().then(() => { stopDone = true; log.push('stop.done'); });
  await settle(5);
  const duringStop = {
    stopDone,
    fireStarted: fire.calls.length === 1,
    stateWritten: keyed.state.lastRunId,
    lockHeld: keyed.lockHeld(),
  };
  releaseFire();
  await settle(5);
  await stopped;
  emit('w1-stop-waits', { duringStop, afterStop: { stopDone, lockAfter: keyed.lockHeld(), log } });
  mock.timers.reset();
}

// --- P10: control-port envelopes (runtime + manager) -------------------------
{
  const home = await scratch('r1');
  const projectKey = join(home, 'proj');
  await mkdir(projectKey, { recursive: true });
  const gatewayCalls: any[] = [];
  const gateway = {
    submitTurn: async function* (input: any) { gatewayCalls.push({ op: 'submitTurn', input }); },
    abortTurn: async (input: any) => { gatewayCalls.push({ op: 'abortTurn', input }); },
    closeSession: async (input: any) => { gatewayCalls.push({ op: 'closeSession', input }); },
  };
  const runtime = new runtimeMod.AlwaysOnRuntime({
    config: config({ [projectKey]: { enabled: true } }),
    pilotHome: home,
    projectKey,
    now: () => NOW,
    uuid,
    logger,
    sessionCatalog: { kind: 'fake' },
    sessionTranscriptReader: { kind: 'fake' },
    skipToolCreation: true,
  });
  // r1: before bindAgentGateway every method answers not_ready.
  const before = {
    apply: await runtime.applyCycle({ projectKey, workCycleId: 'c0', projectName: 'P' }),
    abort: await runtime.abortRun({ projectKey, sessionKey: 'always-on/discovery:project=x:run=y' }),
    rerun: await runtime.rerunPlan({ projectKey, planId: 'p0' }),
  };
  runtime.bindAgentGateway(gateway, { isSessionInFlight: () => false });
  // r2: rerun against a missing plan.
  const rerunMissing = await runtime.rerunPlan({ projectKey, planId: 'p-missing' });
  // r3: plan record present, markdown missing -> plan_body_missing.
  const store = storage(home, projectKey);
  await writeJson(store.paths.planIndexFile, {
    schemaVersion: 1,
    plans: [{ id: 'p1', title: 'T', createdAt: '2026-01-01T00:00:00.000Z', status: 'ready', summary: 's', rationale: 'r', dedupeKey: 'd', sourceRunId: 'r0', planFilePath: 'plans/p1.md' }],
  });
  const rerunNoBody = await runtime.rerunPlan({ projectKey, planId: 'p1' });
  // r4: apply against a missing cycle.
  const applyMissing = await runtime.applyCycle({ projectKey, workCycleId: 'c-missing', projectName: 'P' });
  // r5: apply against a workspace-less cycle — the TS runtime path.
  await writeJson(store.paths.cycleIndexFile, {
    schemaVersion: 1,
    cycles: [{ id: 'c1', projectKey, status: 'active', planIds: [], createdAt: '2026-01-01T00:00:00.000Z', createdByRunId: 'r0' }],
  });
  let applyNoWorkspace: any;
  try { await runtime.applyCycle({ projectKey, workCycleId: 'c1', projectName: 'P' }); applyNoWorkspace = 'resolved'; }
  catch (error) { applyNoWorkspace = { name: (error as any).constructor.name, message: (error as any).message }; }
  // r6: abort for a session no run owns.
  const abortUnknown = await runtime.abortRun({ projectKey, sessionKey: 'always-on/discovery:project=elsewhere:run=z' });
  // r7: abort for a session this runtime's context registry owns.
  const ownedKey = 'always-on/discovery:project=x:run=owned';
  runtime.getRunContexts().register({ kind: 'discovery', sessionKey: ownedKey, runId: 'owned-run', projectKey, paths: store.paths, startedAt: NOW, planCallCount: 0 } as any);
  const abortOwned = await runtime.abortRun({ projectKey, sessionKey: ownedKey, reason: 'probe/abort' });
  // r8: once stop() begins every method answers not_ready.
  const stopping = runtime.stop();
  const afterStopAbort = await runtime.abortRun({ projectKey, sessionKey: ownedKey });
  await stopping;
  emit('r1-control-envelopes', { before, rerunMissing, rerunNoBody, applyMissing, applyNoWorkspace, abortUnknown, abortOwned, afterStopAbort, gatewayCalls });
}
{
  // r9: the multi-project manager answers project_not_found for unknown keys.
  const home = await scratch('r9');
  const projectKey = join(home, 'proj');
  await mkdir(projectKey, { recursive: true });
  const gateway = {
    submitTurn: async function* () {},
    abortTurn: async () => {},
    closeSession: async () => {},
  };
  const manager = new managerMod.AlwaysOnManager({
    config: config({ [projectKey]: { enabled: true } }),
    pilotHome: home,
    now: () => NOW,
    uuid,
    logger,
    sessionCatalog: { kind: 'fake' },
    sessionTranscriptReader: { kind: 'fake' },
  });
  const results = {
    apply: await manager.applyCycle({ projectKey: '/elsewhere', workCycleId: 'c0', projectName: 'P' }),
    abort: await manager.abortRun({ projectKey: '/elsewhere', sessionKey: 'k' }),
    rerun: await manager.rerunPlan({ projectKey: '/elsewhere', planId: 'p0' }),
  };
  await manager.stop();
  emit('r9-manager-project-not-found', results);
}

// --- P11: dormancy — signal clears dormant and re-arms a full interval ------
{
  const home = await scratch('d1');
  const projectKey = join(home, 'proj');
  await mkdir(projectKey, { recursive: true });
  const log: string[] = [];
  const fire = fakeFire(log);
  const store = storage(home, projectKey);
  const deps = {
    config: config({ [projectKey]: { enabled: true } }, { tickIntervalMinutes: 0 }),
    projectKey, paths: store.paths, stateStore: store.stateStore, cycleStore: store.cycleStore,
    leases: new leaseMod.ChannelLeaseRegistry(() => NOW), fire, uuid, now: () => NOW,
    logger: recordingLogger(log), isSessionInFlight: () => false,
  };
  await writeJson(deps.paths.stateFile, { schemaVersion: 1, todayKey: '2026-01-01', todayRunCount: 0, consecutiveFailures: 0, dormant: { since: '2026-01-01T00:00:00.000Z', lastBaselineAt: '2026-01-01T00:00:00.000Z' } });
  const scheduler = new schedulerMod.DiscoveryScheduler(deps);
  await scheduler.start();  // maybeRestoreDormancy attaches the watcher.
  // The dormant gate must block the 1s tick.
  await new Promise((r) => setTimeout(r, 1600));
  const firedWhileDormant = fire.calls.length;
  const blocked = log.some((line) => line.includes('always-on gate blocked') && line.includes('dormant_no_signal'));
  // A real change directly under the project root (Linux fs.watch is
  // root-only): debounce (2s) -> clearDormant -> a fresh full interval (1s)
  // -> fire.
  await writeFile(join(projectKey, 'signal.txt'), 'change', 'utf8');
  await new Promise((r) => setTimeout(r, 4500));
  const state = JSON.parse(await readFile(deps.paths.stateFile, 'utf8'));
  emit('d1-dormant-signal', { firedWhileDormant, blocked, dormantAfter: state.dormant ?? null, firedAfterSignal: fire.calls.length, log });
  await scheduler.stop();
}

// --- P2-P6: ProjectAutomationBundle semantics --------------------------------
type FakeProvider = any;
function makeProvider(name: string, log: string[], opts: { startThrows?: boolean; stopThrows?: boolean } = {}): FakeProvider {
  return {
    name,
    async start() { log.push(`${name}.start`); if (opts.startThrows) throw new Error(`${name} start boom`); },
    async stop() { log.push(`${name}.stop`); if (opts.stopThrows) throw new Error(`${name} stop boom`); },
    bindAgentGateway(gateway: unknown, hooks: unknown) { log.push(`${name}.bind:${hooks ? 'hooks' : 'none'}`); },
    getTools() { return [{ name: `${name}-tool` }]; },
  };
}
function makeBundle(log: string[], cfg: any, aoProviders: FakeProvider[], cronProviders: FakeProvider[], fallback: any) {
  const opts = { aoCount: 0, cronCount: 0, fallbackCount: 0 };
  const bundle = new bundleMod.ProjectAutomationBundle({
    config: cfg,
    pilotHome: '/tmp/probe-home',
    sessionOverrides: { set() {}, deletePrefix() {} } as any,
    createAlwaysOnManager: () => { opts.aoCount++; log.push(`ao-factory:${opts.aoCount}`); return aoProviders[opts.aoCount - 1] ?? makeProvider(`ao#${opts.aoCount}`, log); },
    createCronManager: () => { opts.cronCount++; log.push(`cron-factory:${opts.cronCount}`); return cronProviders[opts.cronCount - 1] ?? makeProvider(`cron#${opts.cronCount}`, log); },
    createStandaloneAlwaysOnControl: () => { opts.fallbackCount++; log.push('fallback-factory'); return fallback; },
  });
  return { bundle, opts };
}
{
  // b1: createAlwaysOn conditions (no config / no enabled project -> no
  // provider; an enabled project constructs exactly one).
  const a = makeBundle([], { alwaysOn: undefined, cron: undefined }, [], [], { kind: 'fallback' });
  const b = makeBundle([], { alwaysOn: { projects: { '/p': { enabled: false } } } }, [], [], { kind: 'fallback' });
  const c = makeBundle([], { alwaysOn: { projects: { '/p': { enabled: true } } }, cron: {} }, [makeProvider('ao#1', [])], [makeProvider('cron#1', [])], { kind: 'fallback' });
  emit('b1-create-conditions', { noConfig: a.opts, noEnabled: b.opts, enabled: c.opts });
}
{
  // b2/b3: start then stop — order and the published control.
  const log: string[] = [];
  const ao = makeProvider('ao', log), cron = makeProvider('cron', log);
  const { bundle } = makeBundle(log, { alwaysOn: { projects: { '/p': { enabled: true } } }, cron: {} }, [ao], [cron], { kind: 'fallback' });
  const published: any[] = [];
  bundle.attach({ agentGateway: {} as any, isProjectBusy: () => false, updateSubsystems: (u: any) => published.push(u) });
  const started = await bundle.start();
  await bundle.stop();
  emit('b2-start-stop-order', {
    started,
    log,
    published: published.map((u) => ({ tools: u.extraTools.map((t: any) => t.name), cronIsProvider: u.cron === cron, alwaysOnIsProvider: u.alwaysOnControl === ao, fallback: u.alwaysOnControl?.kind })),
  });
}
{
  // b3b: stop failures — a single error passes through, two become AggregateError.
  const single: string[] = [];
  const s1 = makeBundle(single, { alwaysOn: { projects: { '/p': { enabled: true } } }, cron: {} }, [makeProvider('ao', single, { stopThrows: true })], [makeProvider('cron', single)], {});
  s1.bundle.attach({ agentGateway: {}, updateSubsystems: () => {} });
  await s1.bundle.start();
  let singleErr: any; try { await s1.bundle.stop(); } catch (e) { singleErr = { name: (e as any).constructor.name, message: (e as any).message, errors: (e as any).errors?.map((x: any) => x.message) }; }
  const both: string[] = [];
  const s2 = makeBundle(both, { alwaysOn: { projects: { '/p': { enabled: true } } }, cron: {} }, [makeProvider('ao', both, { stopThrows: true })], [makeProvider('cron', both, { stopThrows: true })], {});
  s2.bundle.attach({ agentGateway: {}, updateSubsystems: () => {} });
  await s2.bundle.start();
  let bothErr: any; try { await s2.bundle.stop(); } catch (e) { bothErr = { name: (e as any).constructor.name, message: (e as any).message, errors: (e as any).errors?.map((x: any) => x.message) }; }
  emit('b3-stop-failures', { singleErr, bothErr, single, both });
}
{
  // b4: a start failure tears the generation back down.
  const log: string[] = [];
  const { bundle } = makeBundle(log, { alwaysOn: { projects: { '/p': { enabled: true } } } }, [makeProvider('ao', log, { startThrows: true })], [makeProvider('cron', log)], {});
  bundle.attach({ agentGateway: {}, updateSubsystems: () => {} });
  let err: any; try { await bundle.start(); } catch (e) { err = { name: (e as any).constructor.name, message: (e as any).message, errors: (e as any).errors?.map((x: any) => x.message) }; }
  emit('b4-start-failure', { err, log });
}
{
  // b5: reload with nothing changed touches no provider.
  const log: string[] = [];
  const ao = makeProvider('ao', log), cron = makeProvider('cron', log);
  const { bundle, opts } = makeBundle(log, { alwaysOn: { projects: { '/p': { enabled: true } } }, cron: {} }, [ao], [cron], {});
  bundle.attach({ agentGateway: {}, updateSubsystems: () => {} });
  await bundle.start();
  const logBefore = log.length, optsBefore = { ...opts };
  const status = await bundle.reload({ config: { alwaysOn: { projects: { '/p': { enabled: true } } }, cron: {} }, alwaysOnChanged: false, cronChanged: false });
  emit('b5-reload-no-change', { status, optsBefore, optsAfter: opts, logAdded: log.slice(logBefore) });
}
{
  // b6: reload with only cron changed reuses previous.alwaysOn.
  const log: string[] = [];
  const ao = makeProvider('ao', log), cron1 = makeProvider('cron#1', log), cron2 = makeProvider('cron#2', log);
  const published: any[] = [];
  const { bundle, opts } = makeBundle(log, { alwaysOn: { projects: { '/p': { enabled: true } } }, cron: {} }, [ao], [cron1, cron2], {});
  bundle.attach({ agentGateway: {}, updateSubsystems: (u: any) => published.push(u) });
  await bundle.start();
  const status = await bundle.reload({ config: { alwaysOn: { projects: { '/p': { enabled: true } } }, cron: { enabled: true } }, alwaysOnChanged: false, cronChanged: true });
  emit('b6-reload-cron-only', { status, opts, log, lastPublishAlwaysOnIsPrevious: published.at(-1).alwaysOnControl === ao });
}
{
  // b7: reload when the previous generation cannot stop.
  const log: string[] = [];
  const { bundle } = makeBundle(log, { alwaysOn: { projects: { '/p': { enabled: true } } } }, [makeProvider('ao', log, { stopThrows: true })], [makeProvider('cron', log)], {});
  bundle.attach({ agentGateway: {}, updateSubsystems: () => {} });
  await bundle.start();
  let err: any; try { await bundle.reload({ config: { alwaysOn: { projects: { '/p': { enabled: true } } } }, alwaysOnChanged: true, cronChanged: true }); } catch (e) { err = { name: (e as any).constructor.name, message: (e as any).message, errors: (e as any).errors?.map((x: any) => x.message) }; }
  emit('b7-reload-stop-failure', { err, log });
}
{
  // b8: candidate start failure restores the previous generation.
  const log: string[] = [];
  const ao1 = makeProvider('ao#1', log), ao2 = makeProvider('ao#2', log, { startThrows: true }), ao3 = makeProvider('ao#3', log);
  const { bundle, opts } = makeBundle(log, { alwaysOn: { projects: { '/p': { enabled: true } } } }, [ao1, ao2, ao3], [makeProvider('cron#1', log), makeProvider('cron#2', log), makeProvider('cron#3', log)], {});
  bundle.attach({ agentGateway: {}, updateSubsystems: () => {} });
  await bundle.start();
  let err: any; try { await bundle.reload({ config: { alwaysOn: { projects: { '/p': { enabled: true } } } }, alwaysOnChanged: true, cronChanged: true }); } catch (e) { err = { name: (e as any).constructor.name, message: (e as any).message, errors: (e as any).errors?.map((x: any) => x.message) }; }
  emit('b8-reload-restore', { err, opts, log });
}
{
  // b9: restore failure publishes a provider-less generation.
  const log: string[] = [];
  const ao1 = makeProvider('ao#1', log), ao2 = makeProvider('ao#2', log, { startThrows: true }), ao3 = makeProvider('ao#3', log, { startThrows: true });
  const published: any[] = [];
  const { bundle, opts } = makeBundle(log, { alwaysOn: { projects: { '/p': { enabled: true } } } }, [ao1, ao2, ao3], [makeProvider('cron#1', log), makeProvider('cron#2', log, { startThrows: true }), makeProvider('cron#3', log, { startThrows: true })], { kind: 'fallback' });
  bundle.attach({ agentGateway: {}, updateSubsystems: (u: any) => published.push(u) });
  await bundle.start();
  let err: any; try { await bundle.reload({ config: { alwaysOn: { projects: { '/p': { enabled: true } } } }, alwaysOnChanged: true, cronChanged: true }); } catch (e) { err = { name: (e as any).constructor.name, message: (e as any).message, errors: (e as any).errors?.map((x: any) => x.message) }; }
  const status = await bundle.reload({ config: {}, alwaysOnChanged: false, cronChanged: false });
  emit('b9-reload-restore-failure', { err, opts, log, lastPublishFallback: published.at(-1).alwaysOnControl?.kind, statusAfter: status });
}
{
  // b10: reload before start only stages (no provider lifecycle calls).
  const log: string[] = [];
  const { bundle, opts } = makeBundle(log, { alwaysOn: { projects: { '/p': { enabled: true } } } }, [makeProvider('ao#1', log), makeProvider('ao#2', log)], [makeProvider('cron#1', log), makeProvider('cron#2', log)], {});
  bundle.attach({ agentGateway: {}, updateSubsystems: () => {} });
  const status = await bundle.reload({ config: { alwaysOn: { projects: { '/p': { enabled: true } } } }, alwaysOnChanged: true, cronChanged: false });
  emit('b10-reload-before-start', { status, opts, log });
}
{
  // b11: without an Always-On provider the published control is the
  // standalone fallback (the TS shape this port deliberately deviates from).
  const log: string[] = [];
  const fallback = { kind: 'standalone-fallback' };
  const { bundle, opts } = makeBundle(log, { alwaysOn: undefined, cron: {} }, [], [makeProvider('cron', log)], fallback);
  const published: any[] = [];
  bundle.attach({ agentGateway: {}, updateSubsystems: (u: any) => published.push(u) });
  await bundle.start();
  // `status` is private; a no-change reload is the public route to it.
  const status = await bundle.reload({ config: { alwaysOn: undefined, cron: {} }, alwaysOnChanged: false, cronChanged: false });
  emit('b11-fallback-publish', { status, opts, alwaysOnControl: published.at(-1).alwaysOnControl, log });
}

// #2294 r3: `flag: 'wx'` and the recursive mkdir above follow the shared
// generator convention — the guard runs the probe into a fresh directory, and
// the write must fail rather than clobber if the oracle is already there.
await writeFile(join(out, 'oracle.json'), JSON.stringify(golden, null, 2), { flag: 'wx' });
process.stdout.write('probe complete\n');
