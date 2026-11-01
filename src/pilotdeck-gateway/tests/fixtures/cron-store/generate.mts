// #1216 A8.3-pr1a: record the real TS cron storage + migration behavior.
//
// Loads the baseline sources (CronPaths / CronTaskStore / CronStoreMigration,
// plus pilot/paths.ts createProjectId) and drives them over synthetic trees,
// recording return values and the resulting file bytes as oracle.json. The
// clock is pinned by the shared installFixedClock so the migration marker's
// completedAt is byte-stable across regenerations.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, open, readFile, readdir, writeFile } from 'node:fs/promises';
import { utimesSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import { installFixedClock } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
// localeCompare order is part of the recorded bytes; the oracle is generated
// under (and replayed under) the same pinned locale as the edgeclaw fixtures.
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US',
  'run with LANG=en_US.UTF-8 and LC_ALL/LC_MESSAGES unset (got ' +
  `${new Intl.Collator().resolvedOptions().locale})`);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/pilot/paths.ts', 'src/cron/storage/CronPaths.ts',
  'src/cron/storage/CronTaskStore.ts', 'src/cron/storage/CronStoreMigration.ts'];
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
const cronPathsMod = await load(sources[1], { '../../pilot/paths.js': pilotPaths });
const storeMod = await load(sources[2], { './CronPaths.js': cronPathsMod });
const migrationMod = await load(sources[3], { './CronPaths.js': cronPathsMod });
const { resolveCronPaths, cronRunEventsPath } = cronPathsMod;
const { CronTaskStore } = storeMod;
const { migrateCronStores } = migrationMod;

const out = resolve(process.argv[2]);
assert.ok(!out.startsWith(baseline + '/'), 'never write into the baseline');

// -- helpers -----------------------------------------------------------------

const clone = (value: any) => value === undefined ? null : JSON.parse(JSON.stringify(value));
const compact = (value: any) => JSON.stringify(value);

async function writeInputs(root: string, files: { path: string, text?: string, bytes?: number[] }[]) {
  for (const file of files) {
    await mkdir(dirname(join(root, file.path)), { recursive: true });
    await writeFile(join(root, file.path), file.bytes ? Buffer.from(file.bytes) : file.text!, { flag: 'wx' });
  }
}

async function tree(root: string): Promise<{ files: Record<string, string>, dirs: string[] }> {
  const files: Record<string, string> = {}; const dirs: string[] = [];
  async function walk(dir: string) {
    const entries = await readdir(dir, { withFileTypes: true }).catch(() => []);
    for (const entry of entries) {
      const rel = relative(root, join(dir, entry.name));
      if (entry.isDirectory()) { dirs.push(rel); await walk(join(dir, entry.name)); continue; }
      assert.ok(entry.isFile(), `unexpected dirent ${rel}`);
      files[rel] = (await readFile(join(dir, entry.name))).toString('base64');
    }
  }
  await walk(root);
  return { files: Object.fromEntries(Object.entries(files).sort(([a], [b]) => (a < b ? -1 : 1))), dirs: dirs.sort() };
}

// The canonical CronTask key order is normalizeTask's return literal. putTask
// writes the caller's object verbatim, so the builder follows that order.
const TASK_KEYS = ['schemaVersion', 'taskId', 'message', 'schedule', 'status', 'sessionKey',
  'channelKey', 'projectKey', 'mode', 'timezone', 'createdAt', 'updatedAt', 'nextRunAt',
  'lastRunId', 'revision', 'scheduleComputationVersion', 'originSessionKey', 'originChannelKey'];
function task(fields: Record<string, any>) {
  const base: Record<string, any> = {
    schemaVersion: 1, taskId: 't1', message: 'hello', schedule: { type: 'cron', expression: '*/5 * * * *' },
    status: 'scheduled', sessionKey: 'sess-1', channelKey: 'im', createdAt: '2026-01-01T00:00:00.000Z',
    updatedAt: '2026-01-01T00:00:00.000Z',
  };
  const merged = { ...base, ...fields };
  const ordered: Record<string, any> = {};
  for (const key of TASK_KEYS) if (key in merged) ordered[key] = merged[key];
  return ordered;
}
function run(fields: Record<string, any>) {
  const base: Record<string, any> = {
    schemaVersion: 1, runId: 'r1', taskId: 't1', sessionKey: 'sess-1', startedAt: '2026-01-01T00:00:00.000Z',
  };
  const merged = { ...base, ...fields };
  const ordered: Record<string, any> = {};
  for (const key of ['schemaVersion', 'runId', 'taskId', 'sessionKey', 'projectKey', 'startedAt', 'finishedAt', 'outcome', 'error']) {
    if (key in merged) ordered[key] = merged[key];
  }
  return ordered;
}

// -- A. resolveCronPaths / cronRunEventsPath (pure; fake absolute inputs) ----

const PROJECT_KEY = '/w/my-project';
const pathCases: { name: string, pilotHome: string, projectKey: string }[] = [
  { name: 'absolute', pilotHome: '/w/home', projectKey: '/w/projects/main' },
  { name: 'dot-segments', pilotHome: '/w/home/.', projectKey: '/w/projects/a/../b/./c' },
  { name: 'trailing-slash', pilotHome: '/w/home/', projectKey: '/w/projects/main/' },
  { name: 'repeated-separators', pilotHome: '/w//home', projectKey: '/w///projects/main' },
  { name: 'slug-fold', pilotHome: '/w/home', projectKey: '/w/My Project (2026)!!' },
  { name: 'slug-root-empty', pilotHome: '/w/home', projectKey: '/' },
  { name: 'backslash-and-dashes', pilotHome: '/w/home', projectKey: '/w/My\\Pr oject---x_1==' },
  { name: 'unicode-slug', pilotHome: '/w/home', projectKey: '/w/проект-中文/ÅÄÖ' },
  { name: 'collapse-runs', pilotHome: '/w/home', projectKey: '/w/~~~a***b' },
];
const pathsOracle = pathCases.map((entry) => ({
  name: entry.name, pilotHome: entry.pilotHome, projectKey: entry.projectKey,
  expect: resolveCronPaths({ pilotHome: entry.pilotHome, projectKey: entry.projectKey }),
}));

const eventsPaths = ['r-1', 'a b/c', '  ', '', '...', '中', '\ud800x', '-abc-', 'a--b??c', 'A.b_C-9']
  .map((runId) => ({ runId, expect: cronRunEventsPath(pathsOracle[0].expect as any, runId) }));

// -- B/C/D. CronTaskStore over real trees -------------------------------------

const TASKS = 'cron/projects/w-my-project/tasks.json';
const HISTORY = 'cron/projects/w-my-project/run-history.jsonl';
const EVENTS = (id: string) => `cron/projects/w-my-project/runs/${id}.events.jsonl`;

const normalizedInput = {
  schemaVersion: 1,
  tasks: [
    // Scrambled key order + unknown fields: normalizeTask must rebuild.
    { updatedAt: '2026-02-02T00:00:00.000Z', taskId: 'scrambled', message: 'm', schedule: { type: 'cron', expression: '* * * * *', timezone: 'Europe/Berlin' }, sessionKey: 's', channelKey: 'im', createdAt: '2026-01-02T00:00:00.000Z', schemaVersion: 1, extraTop: 1, status: 'running', projectKey: '/w/proj-a', nextRunAt: '2026-03-03T00:00:00.000Z', revision: 2, scheduleComputationVersion: 3, originSessionKey: 'os', originChannelKey: 'oc', mode: 'default', timezone: 'UTC', lastRunId: 'r-9' },
    { schemaVersion: 1, taskId: 'bad-status', message: 'm', schedule: { type: 'once', runAt: '2026-05-05T00:00:00.000Z' }, status: 7, sessionKey: 's', channelKey: 'im', createdAt: '2026-01-03T00:00:00.000Z', updatedAt: '2026-01-03T00:00:00.000Z' },
    { schemaVersion: 1, taskId: 'no-task-id', message: 'm', schedule: { type: 'once', runAt: '2026-05-05T00:00:00.000Z' }, sessionKey: 's', channelKey: 'im', createdAt: '2026-01-03T00:00:00.000Z', updatedAt: '2026-01-03T00:00:00.000Z' },
    { schemaVersion: 1, taskId: 'once-missing-runat', message: 'm', schedule: { type: 'once' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c' },
    { schemaVersion: 1, taskId: 'cron-missing-expression', message: 'm', schedule: { type: 'cron' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c' },
    { schemaVersion: 1, taskId: 'missing-schedule', message: 'm', sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c' },
    { schemaVersion: 1, taskId: 'revision-float', message: 'm', schedule: { type: 'cron', expression: 'x' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c', revision: 1.5 },
    { schemaVersion: 1, taskId: 'revision-negative', message: 'm', schedule: { type: 'cron', expression: 'x' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c', revision: -3 },
    { schemaVersion: 1, taskId: 'revision-unsafe', message: 'm', schedule: { type: 'cron', expression: 'x' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c', revision: 9007199254740993 },
    { schemaVersion: 1, taskId: 'revision-string', message: 'm', schedule: { type: 'cron', expression: 'x' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c', revision: '2' },
    { schemaVersion: 1, taskId: 'scv-string', message: 'm', schedule: { type: 'cron', expression: 'x' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c', scheduleComputationVersion: '2' },
    { schemaVersion: 1, taskId: 'scv-4', message: 'm', schedule: { type: 'cron', expression: 'x' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c', scheduleComputationVersion: 4 },
    { schemaVersion: 1, taskId: 'mode-number', message: 'm', schedule: { type: 'cron', expression: 'x' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c', mode: 5 },
    { schemaVersion: 1, taskId: 'next-run-at-number', message: 'm', schedule: { type: 'cron', expression: 'x' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c', nextRunAt: 12 },
    { schemaVersion: 0, taskId: 'schema-0', message: 'm', schedule: { type: 'cron', expression: 'x' }, sessionKey: 's', channelKey: 'im', createdAt: 'c', updatedAt: 'c' },
    'a plain string task',
    42,
  ],
};

const historyMixed = [
  compact(run({ runId: 'r1', startedAt: '2026-01-01T00:00:01.000Z' })),
  'not json at all',
  compact(run({ runId: 'r2', projectKey: '/w/proj-a', startedAt: '2026-01-01T00:00:02.000Z', finishedAt: '2026-01-01T00:00:09.000Z', outcome: 'completed' })),
  '[1, 2]',
  'null',
  compact({ schemaVersion: 1, runId: 'r3', taskId: 5, sessionKey: 's', startedAt: '2026-01-01T00:00:03.000Z' }),
  compact({ schemaVersion: 1, runId: 'r4', taskId: 't1', sessionKey: 's', startedAt: '2026-01-01T00:00:04.000Z', finishedAt: 'nope', outcome: 'failed', error: { code: 'x', message: 'boom' }, extra: true }),
  '',
  '   ',
].join('\n') + '\n';

const storeScenarios: any[] = [
  { name: 'list-missing', files: [], steps: [{ op: 'list' }] },
  { name: 'list-corrupt', files: [{ path: TASKS, text: '{"schemaVersion": 1, "tasks": [' }], steps: [{ op: 'list' }] },
  { name: 'list-bom', files: [{ path: TASKS, bytes: [0xEF, 0xBB, 0xBF, ...Buffer.from('{"schemaVersion":1,"tasks":[]}')] }], steps: [{ op: 'list' }] },
  { name: 'list-empty-object', files: [{ path: TASKS, text: '{}' }], steps: [{ op: 'list' }] },
  { name: 'list-tasks-not-array', files: [{ path: TASKS, text: '{"schemaVersion":1,"tasks":"nope"}' }], steps: [{ op: 'list' }] },
  { name: 'list-schema-version-2', files: [{ path: TASKS, text: compact({ schemaVersion: 2, tasks: [task({ taskId: 't9' })] }) }], steps: [{ op: 'list' }] },
  { name: 'list-normalize', files: [{ path: TASKS, text: JSON.stringify(normalizedInput, null, 2) }], steps: [{ op: 'list' }] },
  { name: 'get-found-and-missing', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't1' })] }) }], steps: [{ op: 'get', taskId: 't1' }, { op: 'get', taskId: 'zz' }] },
  { name: 'put-new', files: [], steps: [{ op: 'put', task: task({}) }] },
  { name: 'put-new-with-optionals', files: [], steps: [{ op: 'put', task: task({ projectKey: '/w/proj-a', mode: 'default', timezone: 'UTC', nextRunAt: '2026-04-04T00:00:00.000Z', lastRunId: 'r-7', revision: 4, scheduleComputationVersion: 2, originSessionKey: 'os', originChannelKey: 'oc' }) }] },
  { name: 'put-same-key-replaces-and-sorts', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't1', nextRunAt: '2026-06-01T00:00:00.000Z' }), task({ taskId: 't2', nextRunAt: '2026-02-01T00:00:00.000Z' })] }) }], steps: [{ op: 'put', task: task({ taskId: 't1', nextRunAt: '2026-03-01T00:00:00.000Z' }) }] },
  { name: 'put-sort-missing-and-collation', files: [], steps: [
    { op: 'put', task: task({ taskId: 't-iso', nextRunAt: '2026-1-3' }) },
    { op: 'put', task: task({ taskId: 't-Z', nextRunAt: 'Z' }) },
    { op: 'put', task: task({ taskId: 't-a-acute', nextRunAt: 'ä' }) },
    { op: 'put', task: task({ taskId: 't-z', nextRunAt: 'z' }) },
    { op: 'put', task: task({ taskId: 't-empty', nextRunAt: '' }) },
    { op: 'put', task: task({ taskId: 't-none' }) },
    { op: 'put', task: task({ taskId: 't-cjk', nextRunAt: '中' }) },
    { op: 'put', task: task({ taskId: 't-digit', nextRunAt: '10' }) },
    { op: 'put', task: task({ taskId: 't-digit2', nextRunAt: '9' }) },
  ] },
  { name: 'put-after-corrupt-starts-empty', files: [{ path: TASKS, text: '{{{' }], steps: [{ op: 'put', task: task({ taskId: 't1' }) }] },
  { name: 'replace-missing-writes-nothing', files: [], steps: [{ op: 'replace', task: task({}) }] },
  { name: 'replace-found', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't1', message: 'old' })] }) }], steps: [{ op: 'replace', task: task({ taskId: 't1', message: 'new' }) }] },
  { name: 'update-modify', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't1', message: 'old' }), task({ taskId: 't2' })] }) }], steps: [{ op: 'update', taskId: 't1', mode: 'modify' }] },
  { name: 'update-remove', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't1' }), task({ taskId: 't2' })] }) }], steps: [{ op: 'update', taskId: 't1', mode: 'remove' }] },
  { name: 'update-missing', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't2' })] }) }], steps: [{ op: 'update', taskId: 't1', mode: 'modify' }] },
  { name: 'delete-found', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't1' }), task({ taskId: 't2' })] }) }], steps: [{ op: 'delete', taskId: 't1' }] },
  { name: 'delete-missing', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't2' })] }) }], steps: [{ op: 'delete', taskId: 't1' }] },
  { name: 'concurrent-puts-serialize', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't1', nextRunAt: '2026-06-01T00:00:00.000Z' })] }) }], steps: [
    { op: 'put-concurrent', tasks: [task({ taskId: 't1', nextRunAt: '2026-05-01T00:00:00.000Z' }), task({ taskId: 't2', nextRunAt: '2026-03-01T00:00:00.000Z' })] },
  ] },
  { name: 'atomic-replace-keeps-old-inode-bytes', files: [{ path: TASKS, text: compact({ schemaVersion: 1, tasks: [task({ taskId: 't1', message: 'old' })] }) }], steps: [
    { op: 'put', task: task({ taskId: 't1', message: 'old' }) },
    { op: 'open-fd' },
    { op: 'put', task: task({ taskId: 't1', message: 'new' }) },
    { op: 'read-fd' },
  ] },
  { name: 'append-run-new', files: [], steps: [{ op: 'append-run', record: run({}) }] },
  { name: 'append-run-twice', files: [], steps: [{ op: 'append-run', record: run({ runId: 'r1' }) }, { op: 'append-run', record: run({ runId: 'r2', taskId: 't2' }) }] },
  { name: 'list-runs-missing', files: [], steps: [{ op: 'list-runs' }] },
  { name: 'list-runs-mixed', files: [{ path: HISTORY, text: historyMixed }], steps: [{ op: 'list-runs' }] },
  { name: 'list-runs-limit-0', files: [{ path: HISTORY, text: historyMixed }], steps: [{ op: 'list-runs', limit: 0 }] },
  { name: 'list-runs-limit-negative', files: [{ path: HISTORY, text: historyMixed }], steps: [{ op: 'list-runs', limit: -1 }] },
  { name: 'list-runs-limit-2', files: [{ path: HISTORY, text: historyMixed }], steps: [{ op: 'list-runs', limit: 2 }] },
  { name: 'list-runs-limit-large', files: [{ path: HISTORY, text: historyMixed }], steps: [{ op: 'list-runs', limit: 60 }] },
  { name: 'list-runs-invalid-utf8', files: [{ path: HISTORY, bytes: [...Buffer.from('{"schemaVersion":1,"runId":"r1","taskId":"t1","sessionKey":"s","startedAt":"x"}\n'), 0xFF, 0x0A] }], steps: [{ op: 'list-runs' }] },
  { name: 'append-run-event', files: [], steps: [
    { op: 'append-run-event', runId: 'r-1', event: { type: 'run_started', runId: 'r-1', taskId: 't1' } },
    { op: 'append-run-event', runId: 'r-1', event: { type: 'run_completed', runId: 'r-1', nested: { a: [1, null, 'x'] } } },
  ] },
  { name: 'append-run-event-sanitizes', files: [], steps: [
    { op: 'append-run-event', runId: 'a b/c 2000!!', event: { type: 'e' } },
    { op: 'append-run-event', runId: '\ud800x', event: { type: 'e' } },
    { op: 'append-run-event', runId: '', event: { type: 'e' } },
  ] },
];

const storeOracle = [];
for (const scenario of storeScenarios) {
  const home = join(out, 'store-' + scenario.name);
  await writeInputs(home, scenario.files);
  const paths = resolveCronPaths({ pilotHome: home, projectKey: PROJECT_KEY });
  const store = new CronTaskStore(paths);
  let handle: any = null;
  const results = [];
  for (const step of scenario.steps) {
    let result;
    switch (step.op) {
      case 'list': result = await store.listTasks(); break;
      case 'get': result = await store.getTask(step.taskId); break;
      case 'put': await store.putTask(step.task); break;
      case 'put-concurrent': await Promise.all(step.tasks.map((entry: any) => store.putTask(entry))); break;
      case 'replace': result = await store.replaceTask(step.task); break;
      case 'update': result = await store.updateTask(step.taskId, (existing: any) =>
        step.mode === 'remove' ? undefined : ({ ...existing, message: 'edited', updatedAt: '2026-09-09T00:00:00.000Z' })); break;
      case 'delete': result = await store.deleteTask(step.taskId); break;
      case 'append-run': await store.appendRun(step.record); break;
      case 'list-runs': result = await store.listRuns(step.limit); break;
      case 'append-run-event': await store.appendRunEvent(step.runId, step.event); break;
      case 'open-fd': handle = await open(paths.tasksFile, 'r'); break;
      case 'read-fd': result = await handle.readFile('utf8'); await handle.close(); handle = null; break;
      default: throw new Error(`unknown op ${step.op}`);
    }
    results.push(clone(result));
  }
  if (handle) await handle.close();
  storeOracle.push({ name: scenario.name, home: 'store-' + scenario.name, projectKey: PROJECT_KEY,
    initial: scenario.files.map((file: any) => ({ path: file.path, text: file.text ?? null, bytes: file.bytes ?? null })),
    steps: scenario.steps, results, final: await tree(home) });
}

// -- E. migrateCronStores ------------------------------------------------------

function t(id: string, fields: Record<string, any> = {}) {
  return task({ taskId: id, createdAt: '2026-01-01T00:00:00.000Z', ...fields });
}
function r(id: string, fields: Record<string, any> = {}) {
  return run({ runId: id, startedAt: '2026-01-01T00:00:00.000Z', ...fields });
}
const tasksFile = (tasks: any[]) => JSON.stringify({ schemaVersion: 1, tasks }, null, 2) + '\n';
const jsonl = (...lines: string[]) => (lines.length ? lines.join('\n') + '\n' : '');

const A = 'cron/projects/alpha', B = 'cron/projects/beta', PA = 'cron/projects/w-proj-a', PB = 'cron/projects/w-proj-b';

const migrationScenarios: any[] = [
  { name: 'no-cron-dir', files: [] },
  { name: 'empty-projects-dir', files: [{ path: 'cron/projects/.keep', text: '' }] },
  { name: 'tasks-without-projectkey-stay', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('a1', { nextRunAt: '2026-03-03T00:00:00.000Z' }), t('a2', { updatedAt: '2026-02-01T00:00:00.000Z' })]) },
    { path: `${A}/run-history.jsonl`, text: jsonl(compact(r('r1')), compact(r('r2', { projectKey: '/w/proj-a', startedAt: '2026-01-02T00:00:00.000Z' }))) },
  ] },
  { name: 'tasks-move-to-projectkey-dir', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('a1', { projectKey: '/w/proj-a' }), t('a2', { projectKey: '/w/proj a' })]) },
  ] },
  { name: 'runs-by-explicit-projectkey', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('a1')]) },
    { path: `${A}/run-history.jsonl`, text: jsonl(compact(r('r1', { projectKey: '/w/proj-a' })), compact(r('r2')), compact(r('r3', { projectKey: ' ' }))) },
  ] },
  { name: 'runs-by-unique-task-project', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('t1', { projectKey: '/w/proj-a' }), t('t2', { projectKey: '/w/proj-b' })]) },
    { path: `${A}/run-history.jsonl`, text: jsonl(compact(r('r1', { taskId: 't1' })), compact(r('r2', { taskId: 't2' })), compact(r('r3', { taskId: 'unknown' })), compact(r('r4', { taskId: 't1', projectKey: '/w/proj-a' }))) },
  ] },
  { name: 'runs-ambiguous-task-stay', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('t1', { projectKey: '/w/proj-a' }), t('t1', { projectKey: '/w/proj-b', updatedAt: '2026-05-05T00:00:00.000Z' })]) },
    { path: `${A}/run-history.jsonl`, text: jsonl(compact(r('r1', { taskId: 't1' }))) },
  ] },
  { name: 'blocked-dir-keeps-source-tasks', files: [
    { path: `${B}/tasks.json`, text: '{"schemaVersion": 1, "tasks": [' },
    { path: `${B}/run-history.jsonl`, text: jsonl(compact(r('rb'))) },
    { path: `${A}/tasks.json`, text: tasksFile([t('a1', { projectKey: '/w/proj-b' }), t('a2')]) },
    { path: `${A}/run-history.jsonl`, text: jsonl(compact(r('r1', { projectKey: '/w/proj-b' })), compact(r('r2'))) },
  ] },
  { name: 'blocked-dir-tasks-not-array', files: [
    { path: `${B}/tasks.json`, text: '{"tasks": 5}' },
    { path: `${A}/tasks.json`, text: tasksFile([t('a1', { projectKey: '/w/proj-b' })]) },
  ] },
  { name: 'invalid-run-lines-preserved', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('a1')]) },
    { path: `${A}/run-history.jsonl`, text: 'good-but-invalid\n' + jsonl(compact(r('r1')), 'not json', compact(r('r1', { startedAt: '2026-01-02T00:00:00.000Z' })), '[1]', 'null', 'duplicate invalid') },
  ] },
  { name: 'run-events-merge-and-move', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('t1', { projectKey: '/w/proj-a' })]) },
    { path: `${A}/run-history.jsonl`, text: jsonl(compact(r('r1', { taskId: 't1' }))) },
    { path: `${A}/runs/r1.events.jsonl`, text: '{"schemaVersion":1,"runId":"r1","event":{"n":1}}\n{"schemaVersion":1,"runId":"r1","event":{"n":2}}\n' },
    { path: `${PA}/runs/r1.events.jsonl`, text: '{"schemaVersion":1,"runId":"r1","event":{"n":2}}\n{"schemaVersion":1,"runId":"r1","event":{"n":3}}\n' },
    { path: `${A}/runs/other.events.jsonl`, text: '{"schemaVersion":1,"runId":"other","event":{}}\n' },
  ] },
  { name: 'duplicate-tasks-dedupe', files: [
    { path: `${A}/tasks.json`, text: tasksFile([
      t('dup', { updatedAt: '2026-01-01T00:00:00.000Z', message: 'older' }),
      t('dup', { updatedAt: '2026-06-01T00:00:00.000Z', message: 'newer' }),
      t('tie', { updatedAt: '2026-02-01T00:00:00.000Z', message: 'first' }),
      t('tie', { updatedAt: '2026-02-01T00:00:00.000Z', message: 'second' }),
      'anonymous string',
      { noTaskId: true },
    ]) },
  ] },
  { name: 'duplicate-runs-dedupe', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('t1')]) },
    { path: `${A}/run-history.jsonl`, text: jsonl(
      compact(r('dup', { startedAt: '2026-01-01T00:00:00.000Z' })),
      compact(r('dup', { startedAt: '2026-01-01T00:00:00.000Z', finishedAt: '2026-01-01T00:05:00.000Z', outcome: 'completed' })),
      compact(r('dup2', { startedAt: '2026-01-01T00:00:00.000Z', finishedAt: '2026-01-03T00:00:00.000Z' })),
      compact(r('dup2', { startedAt: '2026-01-01T00:00:00.000Z', finishedAt: '2026-01-02T00:00:00.000Z' })),
      compact({ runId: 5, startedAt: 'x' }),
      compact(r('sorted-z', { startedAt: '2026-12-01T00:00:00.000Z' })),
      compact(r('sorted-a', { startedAt: '2026-02-01T00:00:00.000Z' })),
    ) },
  ] },
  { name: 'missing-tasks-file-is-writable', files: [
    { path: `${A}/run-history.jsonl`, text: jsonl(compact(r('r1'))) },
    { path: `${B}/tasks.json`, text: tasksFile([t('b1', { projectKey: '/w/proj-a' })]) },
  ] },
  { name: 'non-directory-entries-skipped', files: [
    { path: 'cron/projects/plain-file', text: 'not a dir' },
    { path: `${A}/tasks.json`, text: tasksFile([t('a1')]) },
  ] },
  { name: 'migration-twice-is-idempotent', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('a1', { projectKey: '/w/proj-a' })]) },
    { path: `${A}/run-history.jsonl`, text: jsonl(compact(r('r1', { projectKey: '/w/proj-a' }))) },
  ], twice: true },
  { name: 'stale-lock-removed', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('a1')]) },
  ], staleLockMs: 11 * 60_000 },
  { name: 'fresh-lock-times-out', files: [
    { path: `${A}/tasks.json`, text: tasksFile([t('a1')]) },
  ], freshLockMs: 60_000 },
];

const migrationOracle = [];
for (const scenario of migrationScenarios) {
  const home = join(out, 'migration-' + scenario.name);
  await writeInputs(home, scenario.files);
  const warnings: any[] = [];
  const logger = { warn(message: string, meta: any) { warnings.push({ message, meta }); } };
  if (scenario.staleLockMs !== undefined || scenario.freshLockMs !== undefined) {
    // Fixed-clock-relative mtime so staleness is decided by the pinned epoch.
    const age = scenario.staleLockMs ?? scenario.freshLockMs!;
    const stamp = new Date(Date.now() - age);
    await mkdir(join(home, 'cron'), { recursive: true });
    const lock = join(home, 'cron', '.store-migration.lock');
    await writeFile(lock, 'held\n', { flag: 'wx' });
    utimesSync(lock, stamp, stamp);
  }
  let error: string | null = null;
  try {
    await migrateCronStores({ pilotHome: home, logger });
  } catch (caught) {
    error = String(caught);
  }
  const once = await tree(home);
  let twice = null;
  if (scenario.twice && !error) {
    await migrateCronStores({ pilotHome: home, logger });
    twice = await tree(home);
  }
  // Warn metadata and error text carry the scenario's absolute home; mask it
  // so the committed oracle stays byte-identical when OUT differs.
  const masked = clone(warnings.map((entry) => ({ ...entry,
    meta: JSON.parse(JSON.stringify(entry.meta).split(home).join('{HOME}')) })));
  migrationOracle.push({ name: scenario.name, home: 'migration-' + scenario.name,
    input: scenario.files.map((file: any) => ({ path: file.path, text: file.text ?? null, bytes: file.bytes ?? null })),
    warnings: masked, error: error && error.split(home).join('{HOME}'), once, twice });
}

// -- publish -------------------------------------------------------------------

// The matrix shape is frozen: the Rust replay asserts the same four counts
// before replaying, so adding or removing a cell must update both sides.
for (const [label, actual, expected] of [
  ['paths', pathsOracle.length, 9],
  ['eventsPaths', eventsPaths.length, 10],
  ['store', storeOracle.length, 33],
  ['migration', migrationOracle.length, 18],
] as const) {
  assert.equal(actual, expected, `${label} cell count drifted`);
}

const oracle = {
  sourceCommit, sourceSha256, node: process.version,
  collator: new Intl.Collator().resolvedOptions().locale,
  paths: pathsOracle, eventsPaths,
  store: storeOracle, migration: migrationOracle,
};
await writeFile(join(out, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(`recorded ${pathsOracle.length} path cells, ${eventsPaths.length} event-path cells, ` +
  `${storeOracle.length} store cells, ${migrationOracle.length} migration cells`);
