// #2295 A8.3: record the real TS cron scheduler/fire behavior.
//
// Loads the baseline sources (CronManager/CronRuntime/CronFire/
// CronScheduler plus the whole cron value-import chain, the same loader as
// the native-cron fixture) and drives started managers over synthetic
// homes with `node:test` mock timers standing in for the timer half: every
// `advance` fast-forwards the mocked `setTimeout` clock by exactly its
// milliseconds and then pumps the event loop until the tick's promise chain
// (real fs IO) settles. The clock is pinned by the shared installFixedClock,
// so every recorded timestamp is byte-stable and the Rust replay reuses the
// same pinned epoch and uuid counters.
//
// The fake CronAgentGatewayPort yields no events, so a turn is event-free
// and the TS store writes no run-event lines — the exact shape the Rust
// port's synchronous submit boundary produces (the per-event appendRunEvent
// half is #2295's ledger entry, not a comparable artifact).
//
// Each cell's observations are: the ordered gateway call log (JSON.stringify
// of each port input), the final tasks.json bytes, the run-history.jsonl
// lines, and the distinct info/warn message texts. The EISDIR error metadata
// of the tick-failure cell is host-specific, so warns compare as a distinct
// message set, never counts.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile, rename, rm } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { mock } from 'node:test';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import { installFixedClock } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
assert.equal(process.version, 'v22.22.0', '#2295 pinned Node');
// localeCompare order is part of the recorded bytes (the store's task
// sort); the oracle is generated under the same pinned locale as the
// cron-store fixture.
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US',
  'run with LANG=en_US.UTF-8 and LC_ALL/LC_MESSAGES unset (got ' +
  `${new Intl.Collator().resolvedOptions().locale})`);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/pilot/paths.ts', 'src/cron/CronTimezone.ts',
  'src/cron/storage/CronPaths.ts', 'src/cron/storage/CronTaskStore.ts',
  'src/cron/storage/CronStoreMigration.ts',
  'src/cron/runtime/CronProjectStorageProvider.ts',
  'src/cron/runtime/CronSchedule.ts', 'src/cron/runtime/CronFire.ts',
  'src/cron/runtime/CronScheduler.ts', 'src/cron/tool/CronSchemas.ts',
  'src/cron/tool/CronCreateTool.ts', 'src/cron/tool/CronListTool.ts',
  'src/cron/tool/CronDeleteTool.ts', 'src/cron/tool/CronStopTool.ts',
  'src/cron/tool/CronRunNowTool.ts', 'src/cron/tool/CronUpdateTool.ts',
  'src/cron/tool/createCronToolDefinitions.ts',
  'src/cron/runtime/CronRuntime.ts', 'src/cron/runtime/CronManager.ts',
  // CronRuntime constructs a SessionConfigOverrides per runtime, so it loads
  // for real too; its own imports are all type-only.
  'src/always-on/runtime/SessionConfigOverrides.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
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

const pilotPaths = await load(sources[0]);
const timezoneMod = await load(sources[1]);
const cronPathsMod = await load(sources[2], { '../../pilot/paths.js': pilotPaths });
const storeMod = await load(sources[3], { './CronPaths.js': cronPathsMod });
const migrationMod = await load(sources[4], { './CronPaths.js': cronPathsMod });
const providerMod = await load(sources[5],
  { '../storage/CronStoreMigration.js': migrationMod,
    '../storage/CronPaths.js': cronPathsMod, '../storage/CronTaskStore.js': storeMod });
const scheduleMod = await load(sources[6], { '../CronTimezone.js': timezoneMod });
const fireMod = await load(sources[7], { '../CronTimezone.js': timezoneMod, './CronSchedule.js': scheduleMod });
const schedulerMod = await load(sources[8], { '../CronTimezone.js': timezoneMod, './CronSchedule.js': scheduleMod });
const schemasMod = await load(sources[9]);
const TOOLS = ['CronCreateTool', 'CronListTool', 'CronDeleteTool', 'CronStopTool',
  'CronRunNowTool', 'CronUpdateTool'];
const toolImports = () => ({ './CronSchemas.js': schemasMod });
const toolMod = await Promise.all(
  TOOLS.map((tool, index) => load(sources[10 + index], toolImports())));
const toolsIndex = await load(sources[16], Object.fromEntries(
  toolMod.map((mod, index) => [`./${TOOLS[index]}.js`, mod])));
const overridesMod = await load(sources[19]);
const runtimeImports = {
  '../../always-on/runtime/SessionConfigOverrides.js': overridesMod,
  '../storage/CronPaths.js': cronPathsMod, '../CronTimezone.js': timezoneMod,
  '../tool/createCronToolDefinitions.js': toolsIndex, './CronFire.js': fireMod,
  './CronSchedule.js': scheduleMod, './CronScheduler.js': schedulerMod,
  './CronProjectStorageProvider.js': providerMod,
};
const runtimeMod = await load(sources[17], runtimeImports);
const managerMod = await load(sources[18], {
  './CronRuntime.js': runtimeMod, '../tool/createCronToolDefinitions.js': toolsIndex,
  './CronProjectStorageProvider.js': providerMod,
});
const { createCronManager } = managerMod;

const out = resolve(process.argv[2]);
assert.ok(!out.startsWith(baseline + '/'), 'never write into the baseline');

// -- helpers -----------------------------------------------------------------

async function writeInputs(root: string, files: { path: string, text: string }[]) {
  for (const file of files) {
    await mkdir(dirname(join(root, file.path)), { recursive: true });
    await writeFile(join(root, file.path), file.text, { flag: 'wx' });
  }
}

// A stored task in the canonical normalizeTask literal order.
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

// The canonical project dir for key `/fire` — `createProjectId` strips the
// leading slash, so the dir is `fire` (the native-cron fixture's `w-` prefix
// comes from its `/w/...` keys). Seeding anywhere else makes the v1 store
// migration relocate the tasks before discovery sees them.
const M = 'cron/projects/fire';
const CONFIG = { enabled: true, timezone: 'UTC', maxConcurrentRuns: 1, runTimeoutMinutes: 60 };
const DUE = '2025-12-31T00:00:00.000Z';
const FAR = '2027-01-01T00:00:00.000Z';

// The event-free CronAgentGatewayPort: a turn is one async generator with no
// events, abort/close record their inputs, everything JSON.stringify-ed in
// the port's own literal key order (the bytes the Rust replay reproduces).
function makeGateway(log: string[]) {
  return {
    async *submitTurn(input: any) {
      log.push(`submitTurn ${JSON.stringify(input)}`);
    },
    async abortTurn(input: any) {
      log.push(`abortTurn ${JSON.stringify(input)}`);
    },
    async closeSession(input: any) {
      log.push(`closeSession ${JSON.stringify(input)}`);
    },
  };
}

// Pump the event loop until the started chains (real fs IO behind the
// scheduler/fire promises) have drained; every `advance` needs one.
async function settle(rounds = 800) {
  for (let i = 0; i < rounds; i++) {
    await new Promise<void>((resolve) => setImmediate(resolve));
  }
}

interface Observation {
  gatewayCalls: string[];
  tasksJson: string | null;
  runHistory: string[];
  infos: string[];
  warns: string[];
}

// One cell: a fresh home seeded from `files`, one manager over the fake
// gateway (pinned clock, counter ids), the actions driven in order. Every
// manager-call action records its resolved value (JSON.stringify bytes) or
// its thrown error; every `observe` snapshots the comparable artifacts.
async function runCell(cell: any) {
  const home = join(out, 'cell-' + cell.name);
  await writeInputs(home, cell.files ?? []);
  const tasksPath = join(home, M, 'tasks.json');
  const historyPath = join(home, M, 'run-history.jsonl');
  const backupPath = tasksPath + '.bak';
  mock.timers.enable({ apis: ['setTimeout'] });
  let counter = 0;
  const gatewayLog: string[] = [];
  const logs: string[] = [];
  const logger = {
    info: (message: string) => logs.push(`info ${message}`),
    warn: (message: string) => logs.push(`warn ${message}`),
  };
  const manager = createCronManager({
    config: cell.config ?? CONFIG,
    pilotHome: home,
    now: () => new Date(),
    uuid: () => `id-${++counter}`,
    logger,
  });
  manager.bindAgentGateway(makeGateway(gatewayLog));

  const observe = (): Observation => {
    let tasksJson: string | null = null;
    try {
      tasksJson = readFileSync(tasksPath, 'utf-8');
    } catch {
      tasksJson = null;
    }
    let runHistory: string[] = [];
    try {
      runHistory = readFileSync(historyPath, 'utf-8')
        .split('\n').filter((line) => line.trim().length > 0);
    } catch {
      runHistory = [];
    }
    return {
      gatewayCalls: [...gatewayLog],
      tasksJson,
      runHistory,
      infos: [...new Set(logs.filter((line) => line.startsWith('info ')))].sort(),
      warns: [...new Set(logs.filter((line) => line.startsWith('warn ')))].sort(),
    };
  };

  const results: any[] = [];
  const observations: Record<string, Observation> = {};
  try {
    for (const action of cell.actions) {
      let result: any;
      try {
        switch (action.t) {
          case 'start':
            await manager.start();
            break;
          case 'list':
            result = await (manager as any).listTasks(action.input);
            break;
          case 'create':
            result = await (manager as any).createTask(action.input);
            break;
          case 'stop':
            await manager.stop();
            break;
          case 'tickOnce':
            await manager.runTickOnce(action.projectKey);
            break;
          case 'advance':
            mock.timers.tick(action.ms);
            await settle();
            break;
          case 'breakTasks':
            await rename(tasksPath, backupPath);
            await mkdir(tasksPath);
            break;
          case 'restoreTasks':
            await rm(tasksPath, { recursive: true });
            await rename(backupPath, tasksPath);
            break;
          case 'writeTasks':
            await writeFile(tasksPath, action.text, 'utf-8');
            break;
          case 'observe':
            observations[action.as] = observe();
            break;
          default:
            throw new Error(`unknown action ${action.t}`);
        }
        // The tick's fires are `void`-ed promise chains over real fs IO; every
        // non-observe action drains them before the next one runs (settle
        // never advances the mocked timer clock).
        if (action.t !== 'observe') await settle();
        results.push({ ok: true, json: JSON.stringify(result) ?? null });
      } catch (error: any) {
        results.push({ ok: false, error: {
          name: error?.name ?? 'Error',
          ...(typeof error?.code === 'string' ? { code: error.code } : {}),
          message: error?.message ?? String(error),
        } });
      }
    }
  } finally {
    mock.timers.reset();
  }
  return { name: cell.name, home: 'cell-' + cell.name,
    files: (cell.files ?? []).map((file: any) => ({ path: file.path, text: file.text })),
    config: cell.config ?? null, actions: cell.actions, results, observations };
}

// -- cells --------------------------------------------------------------------

// Golden 1 + the stop cell's control: a due once task fires on the first
// 60s idle poll, the run completes, and the once task is deleted with its
// session released.
const dueOnce = () => tasksFile([task({
  schedule: { type: 'once', runAt: DUE }, nextRunAt: DUE,
})]);

const cells = [
  { name: 'due-once-fires', files: [{ path: `${M}/tasks.json`, text: dueOnce() }], actions: [
    { t: 'start' },
    { t: 'advance', ms: 60_000 },
    { t: 'observe', as: 'final' },
  ] },
  // Golden 1b: the cron variant reschedules from the pinned finishedAt
  // instead of deleting.
  { name: 'due-cron-reschedules', files: [{ path: `${M}/tasks.json`, text: tasksFile([
    task({ schedule: { type: 'cron', expression: '*/5 * * * *', timezone: 'UTC' },
      nextRunAt: DUE, scheduleComputationVersion: 3 }),
  ]) }], actions: [
    { t: 'start' },
    { t: 'advance', ms: 60_000 },
    { t: 'observe', as: 'final' },
  ] },
  // Golden 2: a not-due task never fires and its stored bytes are untouched.
  { name: 'not-due-no-fire', files: [{ path: `${M}/tasks.json`, text: tasksFile([
    task({ schedule: { type: 'once', runAt: FAR }, nextRunAt: FAR }),
  ]) }], actions: [
    { t: 'start' },
    { t: 'advance', ms: 60_000 },
    { t: 'observe', as: 'final' },
  ] },
  // Golden 3: createTask pokes the timer to 250ms, so the due seed fires
  // without any idle poll elapsing.
  { name: 'poke-fires-early', files: [{ path: `${M}/tasks.json`, text: dueOnce() }], actions: [
    { t: 'start' },
    { t: 'create', input: { projectKey: '/fire', message: 'poked',
      schedule: { type: 'once', runAt: FAR } } },
    { t: 'advance', ms: 250 },
    { t: 'observe', as: 'final' },
  ] },
  // Golden 3's negative half: without the poke, 250ms of fast-forward is
  // not a tick; the full 60s window is.
  { name: 'no-poke-no-early-fire', files: [{ path: `${M}/tasks.json`, text: dueOnce() }], actions: [
    { t: 'start' },
    { t: 'advance', ms: 250 },
    { t: 'observe', as: 'mid' },
    { t: 'advance', ms: 59_750 },
    { t: 'observe', as: 'final' },
  ] },
  // Golden 4: an unreadable task store fails the tick (warn, no crash) and
  // the next tick after the restore fires normally.
  { name: 'tick-failure-recovers', files: [{ path: `${M}/tasks.json`, text: dueOnce() }], actions: [
    { t: 'start' },
    { t: 'breakTasks' },
    { t: 'advance', ms: 60_000 },
    { t: 'restoreTasks' },
    { t: 'advance', ms: 60_000 },
    { t: 'observe', as: 'final' },
  ] },
  // Golden 5: stop() parks the scheduler, so no amount of fast-forward
  // fires the due seed; the session still releases as task-finished.
  { name: 'stop-no-fire', files: [{ path: `${M}/tasks.json`, text: dueOnce() }], actions: [
    { t: 'start' },
    { t: 'stop' },
    { t: 'advance', ms: 600_000 },
    { t: 'observe', as: 'final' },
  ] },
  // Golden 6: after the first tick the scheduler re-arms, so a second due
  // task written behind the store's back still fires.
  { name: 'rearms-after-tick', files: [{ path: `${M}/tasks.json`, text: dueOnce() }], actions: [
    { t: 'start' },
    { t: 'advance', ms: 60_000 },
    { t: 'writeTasks', text: tasksFile([task({
      taskId: 't2', sessionKey: 'cron:t2',
      schedule: { type: 'once', runAt: DUE }, nextRunAt: DUE,
    })]) },
    { t: 'advance', ms: 60_000 },
    { t: 'observe', as: 'final' },
  ] },
  // T4: with maxConcurrentRuns 1 the second due task is delayed, never
  // fired, with the constant 60s defer (delayTask, not the idle poll).
  { name: 'capacity-delays-second-task', files: [{ path: `${M}/tasks.json`, text: tasksFile([
    task({ taskId: 't1', sessionKey: 'cron:t1',
      schedule: { type: 'once', runAt: DUE }, nextRunAt: DUE }),
    task({ taskId: 't2', sessionKey: 'cron:t2',
      schedule: { type: 'once', runAt: DUE }, nextRunAt: DUE }),
  ]) }], actions: [
    { t: 'start' },
    { t: 'advance', ms: 60_000 },
    { t: 'advance', ms: 60_000 },
    { t: 'observe', as: 'final' },
  ] },
  // S12: the manager-level runTickOnce loads and ticks the runtime without
  // any timer elapsing.
  { name: 'tick-once-manager-level', files: [{ path: `${M}/tasks.json`, text: dueOnce() }], actions: [
    { t: 'start' },
    { t: 'tickOnce', projectKey: '/fire' },
    { t: 'observe', as: 'final' },
  ] },
];

const cellOracle = [];
for (const cell of cells) {
  cellOracle.push(await runCell(cell));
}

// -- publish ------------------------------------------------------------------

// The matrix shape is frozen: the Rust replay asserts the same counts before
// replaying, so adding or removing a cell must update both sides.
assert.equal(cellOracle.length, 10, 'cell count drifted');
for (const cell of cellOracle) {
  assert.ok(cell.observations.final, `cell ${cell.name} must observe its final state`);
}

const oracle = { sourceCommit, sourceSha256, node: process.version,
  collator: new Intl.Collator().resolvedOptions().locale,
  cells: cellOracle };
await writeFile(join(out, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(`recorded ${cellOracle.length} scheduler cells`);
