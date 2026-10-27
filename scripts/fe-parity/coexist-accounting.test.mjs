// #927 offline oracle for the coexist accounting verdict: the three-state
// status (PASS / FAIL / ERROR) must fold in every execution-level error and
// never credit a suite whose evidence is invalid. Runs with
// `node --test scripts/fe-parity/coexist-accounting.test.mjs` on synthetic
// Playwright JSON reports (the same shape reportCases parses); no listen, no
// browser, no real report files.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { coexistAccounting, newSideAccounting, COEXIST_CASES, coexistTestDir } from './coexist-runner.mjs';

const runs = Object.keys(COEXIST_CASES).map(phase =>
  ({ phase, label: `coexist-${phase}`, frontend: 'old', testDir: coexistTestDir }));

// A Playwright JSON report for one phase: every registered title executed and
// passed, unless overridden. Extra specs (duplicates) can be appended.
function phaseReport(phase, { errors = [], status = {}, drop = [] } = {}) {
  const titles = COEXIST_CASES[phase].filter(title => !drop.includes(title));
  return { suites: [{ specs: titles.map(title => ({ title, file: 'coexist.spec.mjs', line: 1, column: 1,
    tests: [{ results: [{ status: status[title] ?? 'passed' }] }] })) }], errors };
}
function reportsWith(patch = {}) {
  const reports = {};
  for (const run of runs) reports[run.label] = phaseReport(run.phase, patch[run.phase] ?? {});
  return reports;
}

test('a clean 14-case run accounts PASS', () => {
  const accounting = coexistAccounting(reportsWith(), runs);
  assert.equal(accounting.status, 'PASS', '正控：14 例全绿且无执行错误时必须 PASS');
  assert.equal(accounting.expected, 14, '正控：应执行数必须是登记的 14');
  assert.equal(accounting.executed, 14, '正控：实际通过数必须是 14');
  assert.equal(accounting.errors.length, 0, '正控：无错误');
});

test('a report execution error must not stay PASS (P0)', () => {
  const reports = reportsWith({ valid: { errors: [{ message: 'Playwright exit 2, signal none inconsistent with original results' }] } });
  const accounting = coexistAccounting(reports, runs);
  assert.notEqual(accounting.status, 'PASS', 'P0：报告含执行错误时总状态不得 PASS（14 例全绿也不行）');
  assert.equal(accounting.status, 'ERROR', 'P0：执行错误必须记 ERROR（证据无效），不得与行为 FAIL 混同');
  assert.ok(accounting.errors.some(error => error.includes('execution error')),
    'P0：report.errors 必须并入账目错误（照 accounting.mjs 的兄弟写法）');
});

test('a duplicate failed identity behind a passing first match must not stay PASS (P0)', () => {
  const reports = reportsWith();
  // Append a same-named failed instance; the first match passes, so only the
  // accounting's identity/multiset discipline can surface it.
  const specs = reports['coexist-valid'].suites[0].specs;
  specs.push(structuredClone(specs[0]));
  specs.at(-1).tests = [{ results: [{ status: 'failed' }] }];
  const accounting = coexistAccounting(reports, runs);
  assert.notEqual(accounting.status, 'PASS', 'P0：重复失败身份不得被首条 PASS 遮蔽');
  assert.equal(accounting.status, 'ERROR', 'P0：身份重复必须记 ERROR（注册漂移）');
});

test('a same-named skipped or empty-results duplicate must not stay PASS (P0-1 fix-r2)', () => {
  // The pre-fix filter dropped all-skipped and empty-results instances before
  // the identity discipline ran, so a duplicate in those forms was invisible
  // to the executed-title multiset and the first match kept the suite green.
  for (const kind of ['skipped', 'empty']) {
    const reports = reportsWith();
    const specs = reports['coexist-valid'].suites[0].specs;
    const extra = structuredClone(specs[0]);
    extra.tests = [kind === 'empty' ? { results: [] } : { results: [{ status: 'skipped' }] }];
    specs.push(extra);
    const accounting = coexistAccounting(reports, runs);
    assert.notEqual(accounting.status, 'PASS',
      'P0-1（fix-r2）：同名 skipped/空结果重复实例不得被过滤成 PASS（登记身份的实例匹配数必须恰为 1）');
    assert.equal(accounting.status, 'ERROR',
      'P0-1（fix-r2）：重复身份（任何形态，含 skipped/空结果实例）= 注册漂移记 ERROR');
    assert.ok(accounting.errors.some(error => error.includes('expected exactly 1')),
      'P0-1（fix-r2）：重复实例必须以实例数≠1 的漂移错误入账，不得只在行级遮蔽');
  }
});

test('a behavioural failure is FAIL, a missing identity is ERROR', () => {
  const failed = reportsWith({ valid: { status: { [COEXIST_CASES.valid[0]]: 'failed' } } });
  assert.equal(coexistAccounting(failed, runs).status, 'FAIL', '行为失败记 FAIL（不是 ERROR，不是 PASS）');
  const missing = reportsWith({ valid: { drop: [COEXIST_CASES.valid[0]] } });
  assert.equal(coexistAccounting(missing, runs).status, 'ERROR', '缺失身份=注册漂移记 ERROR（不是 PASS）');
});

test('the native pairing run keeps its own denominator', () => {
  const nativeRuns = [{ phase: 'valid', label: 'native-auth-new', frontend: 'new', testDir: coexistTestDir }];
  const report = { suites: [{ specs: [{ title: 'native Settings reads, saves, reloads, and authenticates runtime/config calls',
    file: 'real-auth-native.spec.mjs', line: 6, column: 1, tests: [{ results: [{ status: 'failed' }] }] }] }], errors: [] };
  const accounting = coexistAccounting({ 'native-auth-new': report }, nativeRuns);
  assert.equal(accounting.status, 'FAIL', '新侧 native 原件按原结果记账（诚实 FAIL）');
  assert.equal(accounting.expected, 1, '新侧分母是 native 原件数，不并入 coexist 14');
});

test('newSideAccounting consumes the pairing accounting three-state (P0-2 fix-r2)', () => {
  const nativeCell = rows => rows.find(row => row.cell === 'native-auth');
  const nativeRow = { phase: 'new-native-auth',
    title: 'real-auth-native.spec.mjs :: native Settings reads, saves, reloads, and authenticates runtime/config calls',
    status: 'PASS', reason: null };
  // The review's injected scenario: rows all PASS while the report carried an
  // execution error, so the original accounting is ERROR — the new-side cell
  // must consume that verdict instead of re-deriving PASS from the rows.
  const invalid = { rows: [nativeRow], status: 'ERROR',
    errors: ['coexist valid execution error: Playwright exit 2'], executed: 1, expected: 1 };
  const side = newSideAccounting(invalid, true);
  assert.equal(nativeCell(side.rows).status, 'ERROR',
    'P0-2（fix-r2）：原账目 ERROR 时新侧 native-auth 格不得记 PASS（执行错误优先于行级 PASS）');
  assert.equal(side.status, 'ERROR',
    'P0-2（fix-r2）：新侧汇总必须消费原账目三态，B 总判分据此不得 PASS');
  assert.ok(side.rows.filter(row => row.cell !== 'native-auth').every(row => row.status === 'NOT_RUN_NEW_SIDE'),
    'P0-2（fix-r2）：其余格必须保持 NOT_RUN_NEW_SIDE（未迁移路由不得被汇总改判）');
  // A fatal run has no accounting at all: invalid evidence, not a behavioural
  // verdict.
  const fatal = newSideAccounting(null, false);
  assert.equal(nativeCell(fatal.rows).status, 'ERROR',
    'P0-2（fix-r2）：fatal（无原账目）必须记 ERROR，不得退回 FAIL/PASS');
  assert.equal(fatal.status, 'ERROR', 'P0-2（fix-r2）：fatal 新侧汇总必须记 ERROR');
  // Positive controls: the honest shapes stay honest.
  const failed = { rows: [{ ...nativeRow, status: 'FAIL' }], status: 'FAIL', errors: [], executed: 0, expected: 1 };
  const honest = newSideAccounting(failed, true);
  assert.equal(nativeCell(honest.rows).status, 'FAIL',
    'P0-2（fix-r2）：行为失败仍诚实记 FAIL（F3+ 迁移完成前的预期形态）');
  assert.equal(honest.status, 'FAIL', 'P0-2（fix-r2）：行为失败的新侧汇总记 FAIL');
  const passed = { rows: [nativeRow], status: 'PASS', errors: [], executed: 1, expected: 1 };
  assert.equal(nativeCell(newSideAccounting(passed, true).rows).status, 'PASS',
    'P0-2（fix-r2）：原账目 PASS 且行级 PASS 才记 PASS（正控）');
});
