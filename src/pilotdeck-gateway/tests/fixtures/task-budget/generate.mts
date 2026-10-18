// Real baseline methods, compiled in memory; no baseline source is copied or changed.
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const source = 'src/cli/ProjectRuntimeRegistry.ts';
const sourceCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
assert.equal(execFileSync('git', ['-C', baseline, 'diff', 'HEAD', '--', source], { encoding: 'utf8' }), '');
const sourcePath = join(baseline, source);
const original = await readFile(sourcePath, 'utf8');
const requireBaseline = createRequire(join(baseline, 'package.json'));
const requireTsx = createRequire(requireBaseline.resolve('tsx'));
const { transformSync } = requireTsx('esbuild');
const compiled = transformSync(original + '\nexport { taskBudgetLedgerKey };\n', {
  loader: 'ts', format: 'esm', target: 'es2022', sourcefile: sourcePath,
});
const code = compiled.code.replace(/from (["'])(\.[^"']+)\1/g, (_: string, quote: string, specifier: string) => {
  let dependency = resolve(dirname(sourcePath), specifier);
  if (dependency.endsWith('.js') && existsSync(dependency.slice(0, -3) + '.ts')) dependency = dependency.slice(0, -3) + '.ts';
  return 'from ' + quote + pathToFileURL(dependency).href + quote;
});
const { ProjectRuntimeRegistry, taskBudgetLedgerKey } = await import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));
const ROOT = '/pdrs-fixture/project';
const NOW = 1792281600000;
const ledgerPath = (home: string) => join(home, 'gateway/sdk-task-budget-ledger.jsonl');
const bytes = async (home: string) => readFile(ledgerPath(home), 'utf8').catch(e => { if (e.code === 'ENOENT') return null; throw e; });
function registry(home: string, threshold: number) {
  // Only persistence methods are exercised. Avoid constructing real project
  // resources, watchers or sessions; the resolve seam supplies a root only.
  const r: any = Object.create(ProjectRuntimeRegistry.prototype);
  Object.assign(r, {
    options: { pilotHome: home, now: () => new Date(r.fixtureNow) }, fixtureNow: NOW,
    sdkTaskBudgetLedgers: new Map(), sdkSessionConfigs: new Map(), sdkSessionProjects: new Map(),
    taskBudgetLedgerRecordCount: 0, taskBudgetLedgerCompactAfterRecords: threshold,
    resolve: (root: string) => ({ projectRoot: root }),
  });
  return r;
}
const clone = (value: any) => JSON.parse(JSON.stringify(value));
function state(r: any) {
  return clone([...r.sdkTaskBudgetLedgers.entries()].map(([key, ledger]: any) => ({ key,
    ...ledger, settledRunIds: [...ledger.settledRunIds],
  })));
}
async function run(r: any, step: any) {
  r.fixtureNow = step.now ?? NOW;
  const root = step.root ?? ROOT;
  const session = step.session ?? 'session';
  try {
    switch (step.op) {
      case 'load': await r.ensureTaskBudgetLedgerLoaded(); break;
      case 'configure': await r.configureProjectTaskBudget(root, session, { scope: step.scope ?? 'project', total: step.total, ...(step.retention === undefined ? {} : { projectRetentionMs: step.retention }) }); break;
      case 'settle':
        if (step.budget !== false) r.sdkSessionConfigs.set(session, { taskBudget: { scope: step.scope ?? 'session', total: 10 } });
        else r.sdkSessionConfigs.delete(session);
        await r.recordTaskBudgetSpendForSdk({ sessionKey: session, projectKey: root, runId: step.run, turnSpentUsd: step.special ? Number(step.special) : step.spend }); break;
      case 'clear': r.clearTaskBudgetLedger(root, session); break;
      case 'get': {
        await r.ensureTaskBudgetLedgerLoaded();
        const ledger = r.sdkTaskBudgetLedgers.get(taskBudgetLedgerKey(root, step.scope ?? 'session', session));
        return ledger ? clone({ ...ledger, settledRunIds: [...ledger.settledRunIds] }) : null;
      }
      default: throw new Error(`unknown step ${step.op}`);
    }
    return { ok: true };
  } catch (error: any) { return { error: { name: error.name, code: error.code ?? null, message: error.message } }; }
}
const crossStep = { op: 'settle', session: 'cross-ts', run: 'from-ts', spend: 0.125 };
if (process.argv[3] === '--verify-rust') {
  const oracle = JSON.parse(await readFile(new URL('./oracle.json', import.meta.url), 'utf8'));
  for (const scenario of oracle.scenarios) {
    const home = join(output, scenario.name);
    const r = registry(home, scenario.threshold);
    assert.deepEqual(await run(r, { op: 'load' }), scenario.reloadError ? { error: scenario.reloadError } : { ok: true });
    assert.deepEqual(state(r), scenario.reloaded, `${scenario.name}: TS reads Rust state`);
    assert.equal(r.taskBudgetLedgerRecordCount, scenario.reloadCount);
    assert.deepEqual(await run(r, crossStep), scenario.reloadError ? { error: scenario.reloadError } : { ok: true });
    assert.equal(await bytes(home), scenario.crossFile, `${scenario.name}: TS continues Rust bytes`);
  }
  const rejected = oracle.scenarios.filter((s: any) => s.reloadError).length;
  console.log(`PASS: TS checked ${oracle.scenarios.length} Rust ledgers (${oracle.scenarios.length - rejected} continued, ${rejected} cached-load rejection)`);
  process.exit(0);
}
await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
const scenarios: any[] = [];
async function scenario(name: string, steps: any[], threshold = 512, initial: string | null = null) {
  const home = join(scratch, name);
  if (initial !== null) { await mkdir(dirname(ledgerPath(home)), { recursive: true }); await writeFile(ledgerPath(home), initial); }
  let r = registry(home, threshold);
  const results = [];
  for (const step of steps) {
    let result;
    if (step.op === 'reopen') { r = registry(home, threshold); result = { ok: true }; }
    else result = await run(r, step);
    results.push({ ...step, result, count: r.taskBudgetLedgerRecordCount, file: await bytes(home) });
    if (existsSync(dirname(ledgerPath(home)))) assert.deepEqual((await readdir(dirname(ledgerPath(home)))).filter(s => s.endsWith('.tmp')), []);
  }
  const final = await bytes(home);
  const reopened = registry(home, threshold);
  const reloadResult = await run(reopened, { op: 'load' });
  const reloadError = reloadResult?.error ?? null;
  const reloaded = state(reopened), reloadCount = reopened.taskBudgetLedgerRecordCount;
  assert.deepEqual(await run(reopened, crossStep), reloadError ? { error: reloadError } : { ok: true });
  const item = { name, threshold, initial, steps: results, reloaded, reloadCount, reloadError, crossFile: await bytes(home) };
  scenarios.push(item);
  console.log(`${name}: ${steps.length} steps, final=${final?.length ?? 'absent'} bytes, reload=${reloadCount}`);
  return item;
}
const configure = (total = 10, retention?: number, extra = {}) => ({ op: 'configure', total, ...(retention === undefined ? {} : { retention }), ...extra });
const settle = (run: string, spend = 0.1, extra = {}) => ({ op: 'settle', run, spend, ...extra });
const get = (extra = {}) => ({ op: 'get', ...extra });
const load = { op: 'load' }, reopen = { op: 'reopen' };
try {
  const normal = await scenario('normal', [load, configure(10, undefined, { scope: 'session' }),
    settle('run-z'), settle('run-a', 0.2), settle('run-z', 99), get(),
    settle('bad', -1), settle('bad', 0, { special: 'NaN' }), settle('bad', 0, { special: 'Infinity' }),
    settle('disabled', 1, { budget: false }), settle('zero', -0), get(),
    { op: 'clear' }, get(), { op: 'clear' }, settle('new', 1e-7), reopen, get()]);
  assert.equal(normal.steps[5].result.spentUsd, 0.30000000000000004);
  assert.equal(normal.steps[10].count, 3);
  const project = await scenario('project', [configure(10, 100),
    configure(10, 100, { session: 'other', now: NOW + 90 }),
    configure(11, 100), configure(10, 200),
    settle('p-run', 2, { scope: 'project', now: NOW + 95 }),
    settle('p-run', 9, { scope: 'project', session: 'other', now: NOW + 180 }),
    configure(10, 100, { now: NOW + 194 }), configure(20, 100, { now: NOW + 195 }),
    get({ scope: 'project' }), { op: 'clear' }, get({ scope: 'project' }),
    settle('late', 1, { scope: 'project', now: NOW + 1000 }), configure(20, 100, { now: NOW + 1099 }),
    get({ scope: 'project' }), configure(30, undefined, { now: NOW + 1100 }), get({ scope: 'project' })]);
  assert.equal(project.steps[2].result.error.code, 'SDK_PROJECT_TASK_BUDGET_TOTAL_CONFLICT');
  assert.equal(project.steps[3].result.error.code, 'SDK_PROJECT_TASK_BUDGET_RETENTION_CONFLICT');
  assert.equal(project.steps[8].result.spentUsd, 0);
  assert.equal(project.steps[13].result.spentUsd, 1);
  const compact = await scenario('compact', [configure(10, 100), settle('z', 0.25, { scope: 'project' }),
    settle('\ue000'), settle('😀'), settle('\ud800'), get(),
    { op: 'clear' }, settle('again'), reopen, get({ scope: 'project' }), get()], 5);
  const seed = compact.steps[4].file;
  assert.ok(seed.split('\n').filter(Boolean).every((s: string) => JSON.parse(s).kind === 'snapshot'));
  assert.deepEqual(JSON.parse(seed.split('\n')[1]).settledRunIds, ['\ud800', '😀', '\ue000']);
  await scenario('empty-compact', [load, settle('x'), { op: 'clear' }], 1);
  await scenario('clear-before-load', [{ op: 'clear', session: 'missing' }, load, get()], 512, normal.steps[3].file);
  const early = await scenario('clear-before-load-compact', [{ op: 'clear' }, load], 1, seed);
  assert.equal(early.steps[0].file, '');
  await scenario('deferred-load-compact', [load, get(), settle('next'), reopen, get()], 2, normal.steps[3].file);
  const duplicate = normal.steps[1 + 1].file;
  const dup = await scenario('replay-duplicate', [load, get(), settle('new'), reopen, get()], 3, duplicate + duplicate);
  assert.equal(dup.steps[0].count, 2);
  assert.equal(dup.steps[1].result.spentUsd, 0.1);
  await scenario('no-final-newline', [load, get(), settle('lost'), get(), reopen, get(), settle('survives'), reopen, get()], 512, duplicate.trimEnd());
  const malformed = '\n \t\r\n{bad}\nnull\n[]\n' + duplicate + '{"version":1,"kind":"future","projectRoot":"/pdrs-fixture/project","sessionKey":"session"}\n' + duplicate.slice(0, -8);
  const bad = await scenario('bad-lines', [load, get(), settle('glued'), reopen, get(), settle('valid'), reopen, get()], 512, malformed);
  assert.equal(bad.steps[0].count, 1);
  // Inputs below are mutations of genuine TS snapshots/records, never expected
  // output fabrication. They probe exactly the loader's loose validation.
  const snapshot = JSON.parse(seed.split('\n')[1]);
  const configured = JSON.parse(project.steps[0].file);
  const settlement = JSON.parse(duplicate);
  const lines = (...rows: any[]) => rows.map(row => JSON.stringify(row) + '\n').join('');
  const merged = await scenario('snapshot-merge', [load, get({ scope: 'project' }), settle('max', 0.5, { scope: 'project', now: NOW + 10 }),
    configure(10, 100, { now: NOW + 109 }), get({ scope: 'project' }), configure(20, 100, { now: NOW + 110 })], 5,
    lines(configured, { ...settlement, scope: 'project', lastActivityAtMs: NOW + 50 },
      { ...settlement, scope: 'project', turnSpentUsd: 999, lastActivityAtMs: NOW + 9999 },
      { ...snapshot, scope: 'project', spentUsd: 4, settledRunIds: ['a', 'a', 'b'] }, configured,
      { ...configured, totalUsd: 99, lastActivityAtMs: NOW + 99 }));
  assert.equal(merged.steps[1].result.spentUsd, 4);
  assert.deepEqual(merged.steps[1].result.settledRunIds, ['a', 'b']);
  await scenario('max-activity', [load, get({ scope: 'project' }), configure(10, 100, { now: NOW + 149 }), configure(20, 100, { now: NOW + 150 })], 512,
    lines(configured, { ...settlement, scope: 'project', runId: 'later', lastActivityAtMs: NOW + 50 },
      { ...settlement, scope: 'project', runId: 'older', lastActivityAtMs: NOW + 10 }));
  await scenario('snapshot-clears-metadata', [load, get({ scope: 'project' }), settle('next', 1, { scope: 'project' })], 3,
    lines(configured, { ...snapshot, scope: 'project' }));
  await scenario('optional-metadata', [load, get(), settle('next'), reopen, get()], 2,
    lines({ ...snapshot, totalUsd: null, projectRetentionMs: '100', lastActivityAtMs: '123', settledRunIds: ['a', 'a'], spentUsd: -2 }));
  await scenario('nullish-configure', [load, get({ scope: 'project' }), configure(10, 100, { now: NOW + 100 })], 2,
    lines({ ...snapshot, scope: 'project', totalUsd: null, projectRetentionMs: null, lastActivityAtMs: null }, configured));
  await scenario('invalid-metadata-expiry', [load, configure(20, 100), get({ scope: 'project' })], 512,
    lines({ ...snapshot, scope: 'project', totalUsd: 10, projectRetentionMs: {}, lastActivityAtMs: NOW }));
  for (const [name, retention, activity] of [
    ['retention', { toString: 0 }, NOW], ['activity', 100, { toString: 0 }],
    ['nested', 100, [[{ toString: null }]]],
  ]) {
    const invalid = await scenario('primitive-' + name, [load, configure(20, 100), get({ scope: 'project' })], 512,
      lines({ ...snapshot, scope: 'project', totalUsd: 10, projectRetentionMs: retention, lastActivityAtMs: activity }));
    assert.equal(invalid.steps[1].result.error.message, 'Cannot convert object to primitive value');
  }
  const failedLoad = await scenario('load-primitive-error', [load, load, get({ scope: 'project' }), settle('never'), configure(20)], 512,
    lines(configured, { ...settlement, scope: 'project', lastActivityAtMs: { toString: 0 } }));
  assert.equal(failedLoad.steps[0].result.error.message, 'Cannot convert object to primitive value');
  assert.ok(failedLoad.steps.every((step: any) => step.count === 2 && step.result.error));
  await scenario('invalid-total', [load, configure(20)], 512,
    lines({ ...snapshot, scope: 'project', totalUsd: 'bad' }));
  await scenario('orphan-activity', [load, get(), settle('next')], 2,
    lines({ ...snapshot, lastActivityAtMs: 17 }));
  const invalids = [null, [], 1, {}, { ...settlement, version: 2 }, { ...settlement, scope: null },
    { ...settlement, runId: 1 }, { ...settlement, turnSpentUsd: '1' }, { ...snapshot, settledRunIds: ['x', 1] },
    { ...configured, scope: 'session' }, { ...configured, totalUsd: null }, { ...settlement, projectRoot: null }];
  const validation = await scenario('validation', [load, get(), settle('next')], 2,
    lines(...invalids, { ...settlement, turnSpentUsd: -1 }, { ...snapshot, settledRunIds: [], spentUsd: -3 }));
  assert.equal(validation.steps[0].count, 2);
  await scenario('path-and-unicode', [settle('一', 1, { root: '/pdrs-fixture/a/../project//./', session: '中\ud800' }),
    settle('😀', 1, { root: '/pdrs-fixture/project', session: '中\ud800' }),
    get({ session: '中\ud800' }), { op: 'clear', session: '中\ud800' }, settle('reinsert', 0.25), reopen, get()], 2);
  await scenario('numbers', [settle('negative-zero', -0), settle('tiny', 1e-7), settle('huge', 1e21),
    settle('finite-max', 1.7976931348623157e308), settle('overflow', 1.7976931348623157e308), reopen, get()], 4);
  const defaultLimit = await scenario('default-threshold', [load, settle('threshold'), get()], 512, duplicate.repeat(511));
  assert.equal(defaultLimit.steps[0].count, 511);
  assert.equal(defaultLimit.steps[1].count, 1);
  assert.equal(JSON.parse(defaultLimit.steps[1].file).kind, 'snapshot');
  await scenario('fixed-rounding', [configure(0.0078125), configure(0.0234375), configure(1e21)]);
  const oracle = JSON.stringify({ sourceCommit, sourceSha256: { [source]: createHash('sha256').update(original).digest('hex') }, scenarios }, null, 2) + '\n';
  for (const path of [baseline, output, scratch]) assert.ok(!oracle.includes(path), `oracle leaks ${path}`);
  await writeFile(join(output, 'oracle.json'), oracle);
  console.log(`PASS: ${scenarios.length} real TS task-budget scenarios generated`);
} finally { await rm(scratch, { recursive: true, force: true }); }
