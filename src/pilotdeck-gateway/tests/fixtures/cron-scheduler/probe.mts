// #2295 A8.3: TS matrix probe — the S/T cells the golden oracle does not
// cover: the exact 60s boundary, the start/stop/poke gates, the S8/S10
// poke-during-tick timer-slot leak, and the direct CronFire classification
// ladder. Same loader, pinned clock, and locale as generate.mts; every
// scenario prints one `### <id> <json>` line so the raw Node output can be
// pasted into the issue evidence verbatim.
//
// The poke-during-tick scenarios reach into the runtime's private
// `scheduler`/`store` and slow `store.listTasks` by one setImmediate round —
// the only way to make a tick observably in-progress under mock timers. The
// poke is the real `CronScheduler.poke` (CronScheduler.ts:55-64) and the
// leak it demonstrates is the timer-slot overwrite in `scheduleNextTick`
// (CronScheduler.ts:71-86: a poke during a running tick arms a second
// 250ms timer; the tick's completion re-arm overwrites the slot without
// clearing it) — the behavior the port's serialized scheduler deliberately
// does not reproduce.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { mock } from 'node:test';
import { assertBaseline } from '../_shared/baseline.mts';
import { installFixedClock } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
assert.equal(process.version, 'v22.22.0', '#2295 pinned Node');
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US',
  'run with LANG=en_US.UTF-8 and LC_ALL/LC_MESSAGES unset (got ' +
  `${new Intl.Collator().resolvedOptions().locale})`);
const baseline = resolve(process.env.PDRS_BASELINE);
assertBaseline(baseline);
installFixedClock();

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

const pilotPaths = await load('src/pilot/paths.ts');
const timezoneMod = await load('src/cron/CronTimezone.ts');
const cronPathsMod = await load('src/cron/storage/CronPaths.ts', { '../../pilot/paths.js': pilotPaths });
const storeMod = await load('src/cron/storage/CronTaskStore.ts', { './CronPaths.js': cronPathsMod });
const migrationMod = await load('src/cron/storage/CronStoreMigration.ts', { './CronPaths.js': cronPathsMod });
const providerMod = await load('src/cron/runtime/CronProjectStorageProvider.ts',
  { '../storage/CronStoreMigration.js': migrationMod,
    '../storage/CronPaths.js': cronPathsMod, '../storage/CronTaskStore.js': storeMod });
const scheduleMod = await load('src/cron/runtime/CronSchedule.ts', { '../CronTimezone.js': timezoneMod });
const fireMod = await load('src/cron/runtime/CronFire.ts', { '../CronTimezone.js': timezoneMod, './CronSchedule.js': scheduleMod });
const schedulerMod = await load('src/cron/runtime/CronScheduler.ts', { '../CronTimezone.js': timezoneMod, './CronSchedule.js': scheduleMod });
// The manager constructor unconditionally builds the tool definitions
// (CronManager.ts:63), so the whole tool chain loads for real.
const schemasMod = await load('src/cron/tool/CronSchemas.ts');
const TOOLS = ['CronCreateTool', 'CronListTool', 'CronDeleteTool', 'CronStopTool',
  'CronRunNowTool', 'CronUpdateTool'];
const toolImports = () => ({ './CronSchemas.js': schemasMod });
const toolMod = await Promise.all(
  TOOLS.map((tool) => load(`src/cron/tool/${tool}.ts`, toolImports())));
const toolsIndex = await load('src/cron/tool/createCronToolDefinitions.ts', Object.fromEntries(
  toolMod.map((mod, index) => [`./${TOOLS[index]}.js`, mod])));
const overridesMod = await load('src/always-on/runtime/SessionConfigOverrides.ts');
const runtimeImports = {
  '../../always-on/runtime/SessionConfigOverrides.js': overridesMod,
  '../storage/CronPaths.js': cronPathsMod, '../CronTimezone.js': timezoneMod,
  '../tool/createCronToolDefinitions.js': toolsIndex, './CronFire.js': fireMod,
  './CronSchedule.js': scheduleMod, './CronScheduler.js': schedulerMod,
  // The probe constructs runtimes directly (no manager-supplied provider), so
  // CronRuntime's own storage-provider import must resolve too.
  './CronProjectStorageProvider.js': providerMod,
  '../storage/CronProjectStorageProvider.js': providerMod,
};
const runtimeMod = await load('src/cron/runtime/CronRuntime.ts', runtimeImports);
const managerMod = await load('src/cron/runtime/CronManager.ts',
  { './CronRuntime.js': runtimeMod, '../tool/createCronToolDefinitions.js': toolsIndex,
    './CronProjectStorageProvider.js': providerMod });
const { createCronManager } = managerMod;

const out = resolve(process.argv[2]);
assert.ok(!out.startsWith(baseline + '/'), 'never write into the baseline');

// -- shared helpers -----------------------------------------------------------

async function writeInputs(root: string, files: { path: string, text: string }[]) {
  for (const file of files) {
    await mkdir(dirname(join(root, file.path)), { recursive: true });
    await writeFile(join(root, file.path), file.text, { flag: 'wx' });
  }
}

const TASK_KEYS = ['schemaVersion', 'taskId', 'message', 'schedule', 'status', 'sessionKey',
  'channelKey', 'projectKey', 'mode', 'timezone', 'createdAt', 'updatedAt', 'nextRunAt',
  'lastRunId', 'revision', 'scheduleComputationVersion', 'originSessionKey', 'originChannelKey'];
function task(fields: Record<string, any>) {
  const base: Record<string, any> = {
    schemaVersion: 1, taskId: 't1', message: 'seeded', status: 'scheduled',
    sessionKey: 'cron:t1', channelKey: 'cron', projectKey: '/fire', mode: 'default',
    timezone: 'UTC', createdAt: '2025-12-31T00:00:00.000Z',
    updatedAt: '2025-12-31T00:00:00.000Z',
  };
  const merged = { ...base, ...fields };
  const ordered: Record<string, any> = {};
  for (const key of TASK_KEYS) if (key in merged) ordered[key] = merged[key];
  return ordered;
}
const tasksFile = (tasks: any[]) => JSON.stringify({ schemaVersion: 1, tasks }) + '\n';
const once = (overrides: Record<string, any> = {}) =>
  task({ schedule: { type: 'once', runAt: DUE }, nextRunAt: DUE, ...overrides });
const cron5 = (overrides: Record<string, any> = {}) =>
  task({ schedule: { type: 'cron', expression: '*/5 * * * *', timezone: 'UTC' },
    nextRunAt: DUE, scheduleComputationVersion: 3, ...overrides });

const M = 'cron/projects/fire';
const CONFIG = { enabled: true, timezone: 'UTC', maxConcurrentRuns: 1, runTimeoutMinutes: 60 };
const DUE = '2025-12-31T00:00:00.000Z';
const FAR = '2027-01-01T00:00:00.000Z';
const PINNED = new Date('2026-01-01T00:00:00.000Z').getTime();

// Pump the event loop until the started chains (real fs IO behind the
// scheduler/fire promises) have drained; mock timers never advance here.
// Unlike generate.mts's fixed 800 rounds, this pump is time-bounded: the
// poke/stop scenarios stack several sequential fire chains between steps,
// and on a slow filesystem those need real milliseconds a fixed round
// count cannot guarantee (a fire left mid-chain would bleed into the next
// step and read as a missing or spurious tick).
async function settle(ms = 300) {
  const end = performance.now() + ms;
  let rounds = 0;
  while (performance.now() < end && rounds < 500_000) {
    rounds++;
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
}

function makeLogger(logs: string[]) {
  return {
    info: (message: string) => logs.push(`info ${message}`),
    warn: (message: string) => logs.push(`warn ${message}`),
  };
}

// The fake CronAgentGatewayPort: submitTurn is an async generator whose
// events the caller configures (default: none — the event-free turn). The
// submit generator receives the scenario context via closure — the port
// itself only ever passes the single turn input — so mid-turn cells can flip
// stopRequested or mutate the store.
function makeGateway(log: string[], submit?: (input: any, ctx: any) => AsyncGenerator<any>, ctx?: any) {
  return {
    async *submitTurn(input: any) {
      log.push(`submitTurn ${JSON.stringify(input)}`);
      if (submit) yield* submit(input, ctx);
    },
    async abortTurn(input: any) {
      log.push(`abortTurn ${JSON.stringify(input)}`);
    },
    async closeSession(input: any) {
      log.push(`closeSession ${JSON.stringify(input)}`);
    },
  };
}

function observe(home: string, log: string[], logs: string[]) {
  let tasksJson: string | null = null;
  try { tasksJson = readFileSync(join(home, M, 'tasks.json'), 'utf-8'); } catch { /* absent */ }
  let runHistory: string[] = [];
  try {
    runHistory = readFileSync(join(home, M, 'run-history.jsonl'), 'utf-8')
      .split('\n').filter((line) => line.trim().length > 0);
  } catch { /* absent */ }
  return {
    gatewayCalls: [...log],
    tasksJson, runHistory,
    infos: [...new Set(logs.filter((line) => line.startsWith('info ')))].sort(),
    warns: [...new Set(logs.filter((line) => line.startsWith('warn ')))].sort(),
  };
}

async function scenario(id: string, body: () => Promise<unknown>) {
  mock.timers.enable({ apis: ['setTimeout'] });
  try {
    const result = await body();
    console.log(`### ${id} ${JSON.stringify(result)}`);
  } catch (error: any) {
    console.log(`### ${id} ERROR ${JSON.stringify({
      name: error?.name ?? 'Error',
      ...(typeof error?.code === 'string' ? { code: error.code } : {}),
      message: error?.message ?? String(error),
    })}`);
  } finally {
    mock.timers.reset();
  }
}

const home = (name: string) => join(out, name);

// -- S1: the exact 60s idle-poll boundary ---------------------------------------

await scenario('S1-exact-60s-boundary', async () => {
  const root = home('s1');
  await writeInputs(root, [{ path: `${M}/tasks.json`, text: tasksFile([once()]) }]);
  const log: string[] = [];
  const logs: string[] = [];
  const manager = createCronManager({
    config: CONFIG, pilotHome: root, now: () => new Date(), uuid: () => 'id-1',
    logger: makeLogger(logs),
  });
  manager.bindAgentGateway(makeGateway(log));
  await manager.start();
  mock.timers.tick(59_999);
  await settle();
  const before = observe(root, log, logs);
  mock.timers.tick(1);
  await settle();
  const after = observe(root, log, logs);
  return { before, after };
});

// -- S2: a second runtime start recalculates nothing ----------------------------

await scenario('S2-second-start-no-recalc', async () => {
  const root = home('s2');
  // A once task without nextRunAt is the one shape the recalculation writes.
  const seeded = once({ schedule: { type: 'once', runAt: FAR } });
  delete seeded.nextRunAt;
  await writeInputs(root, [{ path: `${M}/tasks.json`, text: tasksFile([seeded]) }]);
  const runtime = new runtimeMod.CronRuntime({
    config: CONFIG, pilotHome: root, projectKey: '/fire', skipToolCreation: true,
    now: () => new Date(), uuid: () => 'id-1', logger: makeLogger([]),
  });
  runtime.bindAgentGateway(makeGateway([]));
  await runtime.start();
  const afterStart = readFileSync(join(root, M, 'tasks.json'), 'utf-8');
  await runtime.start();
  const afterSecond = readFileSync(join(root, M, 'tasks.json'), 'utf-8');
  return { firstStartWroteNextRunAt: afterStart.includes(FAR),
    secondStartRewrote: afterSecond !== afterStart };
});

// -- S3: a start after stop does not revive the scheduler -----------------------

await scenario('S3-start-after-stop-no-revive', async () => {
  const root = home('s3');
  await writeInputs(root, [{ path: `${M}/tasks.json`, text: tasksFile([once()]) }]);
  const log: string[] = [];
  const runtime = new runtimeMod.CronRuntime({
    config: CONFIG, pilotHome: root, projectKey: '/fire', skipToolCreation: true,
    now: () => new Date(), uuid: () => 'id-1', logger: makeLogger([]),
  });
  runtime.bindAgentGateway(makeGateway(log));
  await runtime.start();
  await runtime.stop();
  await runtime.start();
  mock.timers.tick(600_000);
  await settle();
  return { submits: log.filter((line) => line.startsWith('submitTurn')).length };
});

// -- S4: a disabled config never arms -------------------------------------------

await scenario('S4-disabled-never-arms', async () => {
  const root = home('s4');
  await writeInputs(root, [{ path: `${M}/tasks.json`, text: tasksFile([once()]) }]);
  const log: string[] = [];
  const logs: string[] = [];
  const runtime = new runtimeMod.CronRuntime({
    config: { ...CONFIG, enabled: false }, pilotHome: root, projectKey: '/fire',
    skipToolCreation: true, now: () => new Date(), uuid: () => 'id-1',
    logger: makeLogger(logs),
  });
  runtime.bindAgentGateway(makeGateway(log));
  await runtime.start();
  mock.timers.tick(600_000);
  await settle();
  return { gatewayCalls: [...log], infos: [...new Set(logs)].sort() };
});

// -- S5: start before bind throws ------------------------------------------------

await scenario('S5-start-before-bind', async () => {
  const managerRoot = home('s5-manager');
  await mkdir(managerRoot, { recursive: true });
  const manager = createCronManager({
    config: CONFIG, pilotHome: managerRoot, now: () => new Date(), uuid: () => 'id-1',
  });
  try {
    await manager.start();
    return { managerThrew: false };
  } catch (error: any) {
    const runtimeRoot = home('s5-runtime');
    await mkdir(runtimeRoot, { recursive: true });
    const runtime = new runtimeMod.CronRuntime({
      config: CONFIG, pilotHome: runtimeRoot, projectKey: '/fire', skipToolCreation: true,
      now: () => new Date(), uuid: () => 'id-1',
    });
    try {
      await runtime.start();
      return { managerThrew: true, managerMessage: error?.message, runtimeThrew: false };
    } catch (runtimeError: any) {
      return { managerThrew: true, managerMessage: error?.message,
        runtimeThrew: true, runtimeMessage: runtimeError?.message };
    }
  }
});

// -- S7: poke's three gates -------------------------------------------------------

await scenario('S7-poke-gates', async () => {
  const root = home('s7');
  await writeInputs(root, [{ path: `${M}/tasks.json`, text: tasksFile([once()]) }]);
  const log: string[] = [];
  const runtime = new runtimeMod.CronRuntime({
    config: CONFIG, pilotHome: root, projectKey: '/fire', skipToolCreation: true,
    now: () => new Date(), uuid: () => 'id-1', logger: makeLogger([]),
  });
  runtime.bindAgentGateway(makeGateway(log));
  // Poke before start: the scheduler exists (bind created it) but is not
  // running, so the poke is a no-op.
  (runtime as any).scheduler.poke();
  mock.timers.tick(600_000);
  await settle();
  const beforeStart = log.filter((line) => line.startsWith('submitTurn')).length;
  await runtime.start();
  mock.timers.tick(600_000);
  await settle();
  const afterStart = log.filter((line) => line.startsWith('submitTurn')).length;
  await runtime.stop();
  (runtime as any).scheduler.poke();
  mock.timers.tick(600_000);
  await settle();
  const afterStop = log.filter((line) => line.startsWith('submitTurn')).length;
  return { beforeStart, afterStart, afterStop };
});

// -- S8/S10: a poke during a running tick leaks a second timer ---------------------

async function pokeDuringTickScenario(stoppedAfterTick: boolean) {
  const root = home(stoppedAfterTick ? 's10' : 's8');
  await writeInputs(root, [{ path: `${M}/tasks.json`, text: tasksFile([once()]) }]);
  const log: string[] = [];
  const runtime = new runtimeMod.CronRuntime({
    config: CONFIG, pilotHome: root, projectKey: '/fire', skipToolCreation: true,
    now: () => new Date(), uuid: () => 'id-1', logger: makeLogger([]),
  });
  runtime.bindAgentGateway(makeGateway(log));
  await runtime.start();
  // Hold the next tick open for one macrotask: the store's listTasks resolves
  // one setImmediate round late (the tick's real fs IO stays behind it, and
  // mock timers never advance during the settle pump).
  const store = (runtime as any).store;
  const realList = store.listTasks.bind(store);
  let slowed = true;
  store.listTasks = async () => {
    if (slowed) { slowed = false; await new Promise<void>((r) => setImmediate(r)); }
    return realList();
  };
  mock.timers.tick(60_000);
  // The timer callback ran into the tick's first await (this.timer is
  // undefined at that point), so this poke lands with the tick in progress
  // and arms a second 250ms timer the tick's own re-arm never clears.
  (runtime as any).scheduler.poke();
  await settle();
  // The tick completed and its re-arm overwrote the timer slot; the poked
  // 250ms timer is now unreferenced but still pending.
  if (stoppedAfterTick) {
    await runtime.stop();
  }
  // A fresh due task behind the store's back: only the leaked 250ms timer
  // can fire it inside the 60s idle window (stop() clears the slot, not it).
  await writeFile(join(root, M, 'tasks.json'),
    tasksFile([once({ taskId: 't3', sessionKey: 'cron:t3' })]));
  mock.timers.tick(250);
  await settle();
  return {
    firstTickFired: log.some((line) => line.startsWith('submitTurn')
      && line.includes('"runId":"id-1"') && line.includes('"sessionKey":"cron:t1"')),
    leakedTimerFired: log.some((line) => line.startsWith('submitTurn')
      && line.includes('"sessionKey":"cron:t3"')),
    gatewayCalls: [...log],
  };
}

await scenario('S8-poke-during-tick-leaks-second-timer', () => pokeDuringTickScenario(false));
await scenario('S10-post-stop-tick-leak', () => pokeDuringTickScenario(true));

// -- T5 + T8-T16: the direct CronFire ladder ----------------------------------------

// One direct-CronFire scratch: the real store, a Map registry, recorded
// release/delivery seams, and a configurable submit generator. The submit
// generator receives the registry so the mid-turn scenarios can flip
// stopRequested (the runtime's stop() does exactly that, CronRuntime.ts:202)
// or mutate the store the way a concurrent writer would.
async function fireScenario(
  id: string,
  seed: any,
  submit?: (input: any, ctx: any) => AsyncGenerator<any>,
  overrides: {
    delivery?: (delivery: any) => Promise<void>;
    breakHistory?: boolean;
    mutate?: () => Promise<void>;
  } = {},
) {
  await scenario(id, async () => {
    const root = home(id);
    await writeInputs(root, [{ path: `${M}/tasks.json`, text: tasksFile([seed]) }]);
    if (overrides.breakHistory) {
      await mkdir(join(root, M, 'run-history.jsonl'));
    }
    const store = new storeMod.CronTaskStore(
      cronPathsMod.resolveCronPaths({ pilotHome: root, projectKey: '/fire' }));
    const log: string[] = [];
    const logs: string[] = [];
    const releases: string[] = [];
    const deliveries: any[] = [];
    const activeRuns = new Map<string, any>();
    const ctx = { activeRuns, root };
    const fire = new fireMod.CronFire({
      gateway: makeGateway(log, submit, ctx),
      store,
      now: () => new Date(PINNED),
      registerActiveRun: (run: any) => void activeRuns.set(run.runId, run),
      unregisterActiveRun: (runId: string) => activeRuns.get(runId),
      getActiveRun: (runId: string) => activeRuns.get(runId),
      runTimeoutMs: 3_600_000,
      defaultTimezone: 'UTC',
      releaseTaskSession: async (task: any) => { releases.push(task.sessionKey); },
      onResultDelivery: overrides.delivery
        ? async (delivery: any) => overrides.delivery!(delivery)
        : async (delivery: any) => { deliveries.push(delivery); },
      logger: { warn: (message: string) => logs.push(`warn ${message}`) },
    });
    const snapshot = (await store.listTasks())[0];
    await overrides.mutate?.();
    await fire.runTask(snapshot, 'run-1');
    await settle();
    let taskStatus: string | null = null;
    let taskNextRunAt: string | null = null;
    let taskRevision: number | null = null;
    let taskCount = 0;
    try {
      const tasks = await store.listTasks();
      taskCount = tasks.length;
      taskStatus = tasks[0]?.status ?? null;
      taskNextRunAt = tasks[0]?.nextRunAt ?? null;
      taskRevision = tasks[0]?.revision ?? null;
    } catch {
      // T15: the store itself is broken; the observation below still shows
      // the artifacts the fire managed to write.
    }
    // The per-event appendRunEvent artifacts: every run-event file the turn
    // produced (the half the port's synchronous boundary folds away).
    const runEvents: string[] = [];
    try {
      for (const file of readdirSync(join(root, M), { recursive: true })) {
        if (typeof file !== 'string' || !file.endsWith('.jsonl')
          || file === 'run-history.jsonl') continue;
        runEvents.push(...readFileSync(join(root, M, file), 'utf-8')
          .split('\n').filter((line) => line.trim().length > 0));
      }
    } catch { /* absent */ }
    return {
      ...observe(root, log, logs),
      releases, deliveries, runEvents,
      taskStatus, taskNextRunAt, taskRevision, taskCount,
    };
  });
}

// T5: the snapshot is stale by the time the fire claims it (the revision was
// bumped behind the store's back after the snapshot, before runTask).
await fireScenario('T5-stale-claim-aborts', cron5(), undefined, {
  mutate: async () => {
    await new storeMod.CronTaskStore(cronPathsMod.resolveCronPaths({
      pilotHome: home('T5-stale-claim-aborts'), projectKey: '/fire',
    })).updateTask('t1', (current: any) => ({ ...current, revision: 5 }));
  },
});

// The error-event ladder: one terminal error event per turn.
const errorEvent = (code: string, message: string) =>
  async function* () { yield { type: 'error', code, message }; };

await fireScenario('T8-turn-timeout', cron5(), errorEvent('turn_timeout', 'the turn timed out'));
await fireScenario('T9-agent-aborted', cron5(), errorEvent('agent_aborted', 'the agent aborted'));
await fireScenario('T10-session-busy', cron5(), errorEvent('session_busy', 'the session is busy'));
// T11: the seam throws instead of yielding an event.
await fireScenario('T11-seam-throws', cron5(), async function* () {
  throw new Error('the seam threw');
});

// T12: stopRequested flips during the turn — the finally overrides the
// classified outcome (CronFire.ts:151-154).
await fireScenario('T12-stop-requested', once(), async function* (input: any, ctx: any) {
  ctx.activeRuns.get(input.runId).stopRequested = true;
  yield { type: 'error', code: 'agent_aborted', message: 'the agent aborted' };
});

// T13: the terminal record sink is a directory.
await fireScenario('T13-append-run-fails', once(), undefined, { breakHistory: true });

// T14: the delivery handler rejects.
await fireScenario('T14-delivery-fails', once(), undefined, {
  delivery: async () => { throw new Error('delivery refused'); },
});

// T15: the task store breaks mid-turn (before the post-run update).
await fireScenario('T15-post-run-update-fails', once(), async function* (_input: any, ctx: any) {
  await rename(join(ctx.root, M, 'tasks.json'), join(ctx.root, M, 'tasks.json.bak'));
  await mkdir(join(ctx.root, M, 'tasks.json'));
});

// T16: a concurrent revision change between the claim and the post-run
// update keeps the task.
await fireScenario('T16-stale-post-run-keeps-task', once(), async function* (_input: any, ctx: any) {
  await new storeMod.CronTaskStore(cronPathsMod.resolveCronPaths({
    pilotHome: ctx.root, projectKey: '/fire',
  })).updateTask('t1', (current: any) => ({ ...current, revision: (current.revision ?? 0) + 1 }));
});

console.log('probe complete');
