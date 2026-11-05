// #1325 A8.3-pr2: record the real TS cron management-method behavior.
//
// Loads the baseline sources (CronManager/CronRuntime plus the whole cron
// value-import chain) and drives the six CronControlPort methods over
// synthetic homes, recording each step's return value (JSON.stringify, so
// key order and undefined-dropping are the wire truth) or its error
// {name, code?, message} — the exact shapes the Rust port's methods.rs
// surface and CronError::as_json must reproduce. The clock is pinned by
// the shared installFixedClock, so every timestamp is byte-stable; the one
// real-clock quirk (runTaskNow's `new Date()` at CronRuntime.ts:396) is
// also pinned by that global patch on the TS side but cannot be replayed
// byte-identically by the Rust port, whose run-now cell stays shape-level.
//
// The unconfigured gate (requireCron, InProcessGateway.ts:3016-3022) is
// IPG-internal — loading the whole InProcessGateway is out of scope — so the
// unconfigured section instantiates the same DialogGatewayError the gate
// throws with its exact arguments; the requireCron source line is pinned by
// the recorded source hashes.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import { installFixedClock } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
assert.equal(process.version, 'v22.22.0', '#1325 pinned Node');
// localeCompare order is part of the recorded bytes (the global list merge);
// the oracle is generated under the same pinned locale as the cron-store
// fixture.
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
  'src/gateway/dialog/errors.ts',
  // CronRuntime constructs a SessionConfigOverrides per runtime (line 110),
  // so it loads for real too; its own imports are all type-only.
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
const overridesMod = await load(sources[20]);
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
const errorsMod = await load(sources[19]);
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
    sessionKey: 'sess-1', channelKey: 'cron',
    createdAt: '2025-12-31T00:00:00.000Z', updatedAt: '2025-12-31T00:00:00.000Z',
  };
  const merged = { ...base, ...fields };
  const ordered: Record<string, any> = {};
  for (const key of TASK_KEYS) if (key in merged) ordered[key] = merged[key];
  return ordered;
}
const tasksFile = (tasks: any[]) => JSON.stringify({ schemaVersion: 1, tasks }) + '\n';
const history = (...lines: string[]) => (lines.length ? lines.join('\n') + '\n' : '');

const M = 'cron/projects/w-my-project';
const A = 'cron/projects/w-alpha';
const B = 'cron/projects/w-beta';
const run = (fields: Record<string, any> = {}) => JSON.stringify({
  schemaVersion: 1, runId: 'r1', taskId: 't1', sessionKey: 'sess-1',
  startedAt: '2026-01-01T00:00:01.000Z', ...fields,
});

const CONFIG = { enabled: true, timezone: 'UTC', maxConcurrentRuns: 1, runTimeoutMinutes: 60 };
const cron = (expression: string, timezone?: string) =>
  ({ type: 'cron', expression, ...(timezone ? { timezone } : {}) });

// One cell: a fresh home seeded from `files`, one manager (pinned clock,
// counter taskIds), and the six methods driven step by step. Every step's
// outcome is recorded — a resolved value as JSON.stringify bytes, a thrown
// error as {name, code?, message}.
async function runCell(cell: any) {
  const home = join(out, 'cell-' + cell.name);
  await writeInputs(home, cell.files ?? []);
  let counter = 0;
  const manager = createCronManager({
    config: cell.config ?? CONFIG,
    pilotHome: home,
    now: () => new Date(),
    uuid: () => `task-${++counter}`,
  });
  const results: any[] = [];
  for (const step of cell.steps) {
    try {
      const result = await (manager as any)[step.method](step.input);
      results.push({ ok: true, json: JSON.stringify(result) });
    } catch (error: any) {
      results.push({ ok: false, error: {
        name: error?.name ?? 'Error',
        ...(typeof error?.code === 'string' ? { code: error.code } : {}),
        message: error?.message ?? String(error),
      } });
    }
  }
  return { name: cell.name, home: 'cell-' + cell.name,
    files: (cell.files ?? []).map((file: any) => ({ path: file.path, text: file.text })),
    config: cell.config ?? null, steps: cell.steps, results };
}

// -- cells --------------------------------------------------------------------

const cells = [
  // The happy-path chain over one project: create (once, future), list with
  // history, update (once -> cron, revision+1), stop idle, delete twice.
  { name: 'create-lifecycle', files: [], steps: [
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'm1',
      sessionKey: 'origin-session', channelKey: 'origin-channel', mode: 'default',
      schedule: { type: 'once', runAt: '2027-01-01T00:00:00.000Z' } } },
    { method: 'listTasks', input: { projectKey: '/w/my-project', includeHistory: true } },
    { method: 'updateTask', input: { taskId: 'task-1', projectKey: '/w/my-project',
      expectedRevision: 0, message: 'm2',
      schedule: { type: 'cron', expression: '*/5 * * * *', timezone: 'UTC' } } },
    { method: 'stopTask', input: { taskId: 'task-1', runId: 'run-x' } },
    { method: 'deleteTask', input: { taskId: 'task-1', projectKey: '/w/my-project' } },
    { method: 'deleteTask', input: { taskId: 'task-1', projectKey: '/w/my-project' } },
  ] },
  // A delay schedule folds into once with a computed runAt. An unknown unit
  // (or a missing one) never reaches the "positive finite" gate:
  // DELAY_UNIT_MS has no such key, so the multiply is NaN, the Invalid Date
  // is not nullish, and its toISOString throws RangeError.
  { name: 'create-delay-fold', files: [], steps: [
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'd1',
      schedule: { type: 'delay', amount: 90, unit: 'minute' } } },
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'd2',
      schedule: { type: 'delay', amount: 90, unit: 'minutes' } } },
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'd3',
      schedule: { type: 'delay', amount: 5 } } },
  ] },
  // A cron schedule with an explicit timezone: the task carries it and the
  // next run is computed in that zone.
  { name: 'create-cron-berlin', files: [], steps: [
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'c1',
      schedule: { type: 'cron', expression: '30 9 * * *', timezone: 'Europe/Berlin' } } },
  ] },
  // The create-side validation ladder, in its exact order.
  { name: 'create-errors', files: [], steps: [
    { method: 'createTask', input: {} },
    { method: 'createTask', input: { projectKey: '   ', message: 'm', schedule: cron('* * * * *') } },
    { method: 'createTask', input: { projectKey: 5, message: 'm', schedule: cron('* * * * *') } },
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'm' } },
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'm',
      schedule: { type: 'once', runAt: '2020-01-01T00:00:00.000Z' } } },
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'm',
      schedule: { type: 'delay', amount: -5, unit: 'minutes' } } },
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'm',
      schedule: cron('* * * * *'), timezone: 'Mars/Phobos' } },
    { method: 'createTask', input: { projectKey: '/w/my-project', message: 'm',
      schedule: cron('not a cron') } },
  ] },
  // The update validation ladder plus the closure outcomes (running,
  // conflict, success, then a stale revision).
  { name: 'update-cases', files: [
    { path: `${M}/tasks.json`, text: tasksFile([
      task({ taskId: 't-scheduled', schedule: cron('*/5 * * * *'), projectKey: '/w/my-project' }),
      task({ taskId: 't-running', schedule: cron('*/5 * * * *'), projectKey: '/w/my-project',
        status: 'running', lastRunId: 'run-9' }),
    ]) },
  ], steps: [
    { method: 'updateTask', input: { taskId: 't-scheduled', projectKey: '/w/other',
      expectedRevision: 0, message: 'm', schedule: cron('*/5 * * * *') } },
    { method: 'updateTask', input: { taskId: 't-scheduled', projectKey: '/w/my-project',
      expectedRevision: 1.5, message: 'm', schedule: cron('*/5 * * * *') } },
    { method: 'updateTask', input: { taskId: 't-scheduled', projectKey: '/w/my-project',
      expectedRevision: -1, message: 'm', schedule: cron('*/5 * * * *') } },
    { method: 'updateTask', input: { taskId: 't-scheduled', projectKey: '/w/my-project',
      expectedRevision: 0, message: '   ', schedule: cron('*/5 * * * *') } },
    { method: 'updateTask', input: { taskId: 't-scheduled', projectKey: '/w/my-project',
      expectedRevision: 0, message: 'm', schedule: cron('*/5 * * * *'),
      timezone: 'Mars/Phobos' } },
    { method: 'updateTask', input: { taskId: 't-running', projectKey: '/w/my-project',
      expectedRevision: 0, message: 'm', schedule: cron('*/5 * * * *') } },
    { method: 'updateTask', input: { taskId: 't-scheduled', projectKey: '/w/my-project',
      expectedRevision: 5, message: 'm', schedule: cron('*/5 * * * *') } },
    { method: 'updateTask', input: { taskId: 't-scheduled', projectKey: '/w/my-project',
      expectedRevision: 0, message: 'm2', schedule: cron('*/10 * * * *') } },
    { method: 'updateTask', input: { taskId: 't-scheduled', projectKey: '/w/my-project',
      expectedRevision: 0, message: 'm3', schedule: cron('*/10 * * * *') } },
  ] },
  // ProjectKey-less routing over loaded runtimes: not_found, the two
  // cross-project ambiguity errors, and the idle stop shape.
  { name: 'update-scan-not-found-and-ambiguous', files: [
    { path: `${A}/tasks.json`, text: tasksFile([
      task({ taskId: 'shared', schedule: cron('*/5 * * * *'), projectKey: '/w/alpha',
        lastRunId: 'run-shared' }),
    ]) },
    { path: `${B}/tasks.json`, text: tasksFile([
      task({ taskId: 'shared', schedule: cron('*/5 * * * *'), projectKey: '/w/beta',
        lastRunId: 'run-shared' }),
    ]) },
  ], steps: [
    { method: 'listTasks', input: { projectKey: '/w/alpha' } },
    { method: 'listTasks', input: { projectKey: '/w/beta' } },
    { method: 'updateTask', input: { taskId: 'shared', expectedRevision: 0, message: 'm',
      schedule: cron('*/5 * * * *') } },
    { method: 'deleteTask', input: { taskId: 'shared' } },
    { method: 'runTaskNow', input: { taskId: 'shared' } },
    { method: 'stopTask', input: { runId: 'run-shared' } },
    { method: 'updateTask', input: { taskId: 'missing', expectedRevision: 0, message: 'm',
      schedule: cron('*/5 * * * *') } },
    { method: 'stopTask', input: { taskId: 'missing' } },
  ] },
  // runTaskNow: not_found and already_running are byte cells; the success
  // step is the real-clock quirk cell (shape-level in the Rust replay).
  { name: 'run-now-shape', files: [
    { path: `${M}/tasks.json`, text: tasksFile([
      task({ taskId: 't-running', schedule: cron('*/5 * * * *'), projectKey: '/w/my-project',
        status: 'running', lastRunId: 'run-1' }),
      task({ taskId: 't-scheduled', schedule: cron('*/5 * * * *'), projectKey: '/w/my-project' }),
    ]) },
  ], steps: [
    { method: 'runTaskNow', input: { taskId: 'missing', projectKey: '/w/my-project' } },
    { method: 'runTaskNow', input: { taskId: 't-running', projectKey: '/w/my-project' } },
    { method: 'runTaskNow', input: { taskId: 't-scheduled', projectKey: '/w/my-project' } },
  ] },
  // The global list merge: runtimes loaded first, then the projectKey-less
  // list sorts by (nextRunAt ?? "").localeCompare and slices the runs by
  // startedAt (limit 0 keeps everything: slice(-0)).
  { name: 'list-global-merge', files: [
    { path: `${A}/tasks.json`, text: tasksFile([
      task({ taskId: 't-a1', schedule: cron('*/5 * * * *'), projectKey: '/w/alpha',
        nextRunAt: '2026-02-01T00:00:00.000Z' }),
      task({ taskId: 't-a2', schedule: cron('*/5 * * * *'), projectKey: '/w/alpha' }),
    ]) },
    { path: `${A}/run-history.jsonl`, text: history(
      run({ runId: 'r-a1', projectKey: '/w/alpha' }),
      run({ runId: 'r-a2', projectKey: '/w/alpha', startedAt: '2026-01-01T00:00:02.000Z' })) },
    { path: `${B}/tasks.json`, text: tasksFile([
      task({ taskId: 't-b1', schedule: cron('*/5 * * * *'), projectKey: '/w/beta',
        nextRunAt: '2026-03-01T00:00:00.000Z' }),
    ]) },
    { path: `${B}/run-history.jsonl`, text: history(
      run({ runId: 'r-b1', projectKey: '/w/beta', startedAt: '2026-01-01T00:00:03.000Z' }),
      run({ runId: 'r-b2', projectKey: '/w/beta', startedAt: '2026-01-01T00:00:04.000Z' })) },
  ], steps: [
    { method: 'listTasks', input: { projectKey: '/w/alpha' } },
    { method: 'listTasks', input: { projectKey: '/w/beta' } },
    { method: 'listTasks', input: {} },
    { method: 'listTasks', input: { includeHistory: true, limit: 2 } },
    { method: 'listTasks', input: { includeHistory: true, limit: 0 } },
  ] },
  // The disabled config: create/update refuse; routing methods still answer.
  { name: 'disabled-config', config: { enabled: false, timezone: 'UTC',
    maxConcurrentRuns: 1, runTimeoutMinutes: 60 }, files: [], steps: [
    { method: 'createTask', input: { projectKey: '/w/disabled', message: 'm',
      schedule: cron('*/5 * * * *') } },
    { method: 'updateTask', input: { taskId: 't', projectKey: '/w/disabled',
      expectedRevision: 0, message: 'm', schedule: cron('*/5 * * * *') } },
    { method: 'listTasks', input: {} },
    { method: 'deleteTask', input: { taskId: 't', projectKey: '/w/disabled' } },
    { method: 'runTaskNow', input: { taskId: 't', projectKey: '/w/disabled' } },
  ] },
  // stopRunning:true routes through stopTask first; with no active run the
  // result drops the undefined stoppedRunId.
  { name: 'delete-stop-running-idle', files: [
    { path: `${M}/tasks.json`, text: tasksFile([
      task({ taskId: 't1', schedule: cron('*/5 * * * *'), projectKey: '/w/my-project' }),
    ]) },
  ], steps: [
    { method: 'deleteTask', input: { taskId: 't1', projectKey: '/w/my-project',
      stopRunning: true } },
  ] },
];

const cellOracle = [];
for (const cell of cells) {
  cellOracle.push(await runCell(cell));
}

// -- unconfigured gate ---------------------------------------------------------

// requireCron (InProcessGateway.ts:3016-3022) throws before any adapter runs;
// the six adapters share one gate, so one instantiation covers the error
// shape every method surfaces unconfigured.
const gate = new errorsMod.DialogGatewayError(
  'CAPABILITY_UNAVAILABLE', 'Cron runtime is not configured on this gateway.');
const unconfigured = ['createTask', 'listTasks', 'updateTask', 'deleteTask', 'stopTask',
  'runTaskNow'].map((method) => ({ method, error: {
    name: gate.name, code: gate.code, message: gate.message } }));

// -- publish -------------------------------------------------------------------

// The matrix shape is frozen: the Rust replay asserts the same counts before
// replaying, so adding or removing a cell must update both sides.
assert.equal(cellOracle.length, 10, 'cell count drifted');
assert.equal(unconfigured.length, 6, 'unconfigured count drifted');

const oracle = { sourceCommit, sourceSha256, node: process.version,
  collator: new Intl.Collator().resolvedOptions().locale,
  cells: cellOracle, unconfigured };
await writeFile(join(out, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(`recorded ${cellOracle.length} method cells, ${unconfigured.length} unconfigured entries`);
