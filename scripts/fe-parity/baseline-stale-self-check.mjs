import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { baseline, realOrigin, origin } from './env.mjs';
import { BASELINE_STALE, validateStaleManifest, validateStaleEvidence, validateBaselineStale, validateStaleCascade, oldRealGate, compareStaleRows } from './baseline-stale.mjs';
import { serialSuccessors, SERIAL_TOPOLOGY, readSerialTopology } from './serial-topology.mjs';
import { proveBaselineStale } from './baseline-stale-proof.mjs';
import { PENDING_ORCHESTRATION, reconcileRealPair, reconcilePair, reconcileReal, caseId } from './accounting.mjs';

const testDir = resolve(baseline, 'e2e');
const origins = { ready: realOrigin('ready'), onboarding: realOrigin('onboarding') };
// Synthetic control reports, never written as runtime evidence.
export function staleControlAttempt(entry) {
  const { assertion, callsite, matcher, locator } = entry.failure;
  const location = { ...assertion, file: resolve(testDir, assertion.file) };
  const message = `Error: expect(locator).${matcher}(${matcher === 'toContainText' ? 'expected' : ''}) failed\n\nLocator: ${locator}\nExpected: visible\nError: element(s) not found`;
  const stack = assertion.file === callsite.file
    ? `${message}\n    at ${resolve(testDir, callsite.file)}:${callsite.line}:${callsite.column}`
    : `${message}\n    at clickButton (${location.file}:${location.line}:${location.column})\n    at ${resolve(testDir, callsite.file)}:${callsite.line}:${callsite.column}`;
  const error = { location, message, stack };
  return { status: 'failed', retry: 0, error, errors: [error], errorLocation: location };
}
function controls() {
  const cascadeIds = new Set(BASELINE_STALE.tests.flatMap(row => serialSuccessors(PENDING_ORCHESTRATION, row)).map(caseId));
  return Object.fromEntries(['old', 'new'].map(side => [side, ['ready', 'onboarding'].map(mode => ({
    config: { forbidOnly: true, rootDir: testDir, metadata: { frontend: side, mode, baseURL: origins[mode], suite: 'real-D', backend: 'native-with-mock-llm' } },
    errors: [], suites: [{ specs: PENDING_ORCHESTRATION.filter(row => (row.file === 'real/onboarding.spec.mjs') === (mode === 'onboarding')).map(row => {
      const entry = BASELINE_STALE.tests.find(item => caseId(item) === caseId(row));
      return { file: row.file, title: row.title, tests: [{ expectedStatus: 'passed', annotations: [], results: [entry ? staleControlAttempt(entry) : { status: cascadeIds.has(caseId(row)) ? 'skipped' : 'passed', retry: 0 }] }] };
    }) }],
  }))]));
}
const compare = reports => reconcileRealPair(PENDING_ORCHESTRATION, reports, testDir, { old: origins, new: origins });
const staleSpec = (reports, side = 'new') => reports[side][0].suites[0].specs.find(row => row.title === BASELINE_STALE.tests[0].title);
export async function baselineStaleSelfCheck(check) {
  await check('baseline stale: reviewed exact manifest, evidence lines/hashes and executable offline proof', async () => {
    assert.equal((await validateBaselineStale()).identities, 4);
    assert.equal((await proveBaselineStale(BASELINE_STALE)).proofs.length, 4);
    for (const mutate of [
      catalog => { catalog.tests[0].title += 'x'; },
      catalog => { catalog.tests[0].file = 'real/files.spec.mjs'; },
      catalog => { catalog.tests[0].evidence[0].line++; },
      catalog => { catalog.tests[0].defectType += 'x'; },
      catalog => { catalog.tests.push({ ...catalog.tests[0] }); },
      catalog => { catalog.tests.pop(); },
    ]) {
      const changed = structuredClone(BASELINE_STALE); mutate(changed);
      assert.throws(() => validateStaleManifest(changed), /differs from reviewed/);
    }
    const evidence = BASELINE_STALE.tests[0].evidence[0];
    await assert.rejects(() => validateStaleEvidence(BASELINE_STALE, async file => {
      const source = await readFile(resolve(dirname(baseline), file), 'utf8');
      return file === evidence.file ? '\n' + source : source;
    }), /evidence drift/);
  });
  await check('baseline stale: matching FAIL pairs retain FAIL, 17 executions / 11 cascades / 13 PASS; assertion/identity/skip/serial/provenance fail', () => {
    const reports = controls(), result = compare(reports);
    assert.equal(result.status, 'PASS');
    assert.equal(result.scope, 'D_28_PAIR_ONLY');
    for (const side of ['old', 'new']) {
      assert.equal(result[side].executed, 17); assert.equal(result[side].expected, 17);
      assert.equal(result[side].minimumExecuted, 17); assert.equal(result[side].status, 'FAIL');
      assert.equal(result[side].staleCascade, 11); assert.equal(result[side].pendingOrchestration, 0);
      assert.deepEqual(result[side].behavioralCoverage, { eligible: 13, executed: 13, passed: 13, excludedBaselineStale: 4, excludedStaleCascade: 11 });
      assert.equal(result[side].rows.filter(row => row.baselineStale && row.status === 'FAIL').length, 4);
    }
    assert(result.baselineStale.every(row => row.consistency === 'MATCHED_FAIL' && row.behavioralCoverageCredit === 0));
    assert.equal(reconcileReal(PENDING_ORCHESTRATION, { ready: reports.old[0], onboarding: reports.old[1] }, testDir, origins).status, 'FAIL');
    assert.equal(result.old.oldGate.status, 'PASS');
    assert.equal(result.old.oldGate.matchedFailConditions, 4);
    // Every reviewed predecessor, including Files, must match its own signature.
    for (const entry of BASELINE_STALE.tests) for (const mutate of [
      attempt => { attempt.error.location.line++; },
      attempt => { attempt.error.stack = attempt.error.stack.replace(`${entry.failure.callsite.file}:${entry.failure.callsite.line}:${entry.failure.callsite.column}`, `${entry.failure.callsite.file}:1:1`); },
      attempt => { attempt.error.message = attempt.error.message.replace(entry.failure.locator, 'borrowed locator'); },
      attempt => { attempt.status = 'passed'; },
    ]) {
      const changed = controls();
      const spec = changed.new[0].suites[0].specs.find(row => row.title === entry.title);
      mutate(spec.tests[0].results[0]);
      assert.equal(compare(changed).status, 'FAIL', `${entry.title}: ${mutate}`);
    }
    const wrongSource = controls(); wrongSource.new[0].config.metadata.frontend = 'old';
    assert(compare(wrongSource).baselineStale.every(row => row.consistency === 'FAIL'));
    for (const mutate of [
      reports => { staleSpec(reports).title += 'x'; },
      reports => { staleSpec(reports).file = 'real/files.spec.mjs'; },
      reports => { staleSpec(reports).tests[0].results = [{ status: 'passed' }]; },
      reports => { for (const side of ['old', 'new']) staleSpec(reports, side).tests[0].results = [{ status: 'passed' }]; },
      reports => { staleSpec(reports).tests[0].results = [{ status: 'skipped' }]; },
      reports => { staleSpec(reports).tests[0].results = []; },
      reports => { staleSpec(reports).tests[0].results.push({ status: 'passed' }); },
      reports => { staleSpec(reports).tests[0].results[0].retry = 1; },
      reports => { staleSpec(reports).tests[0].expectedStatus = 'failed'; },
      reports => { staleSpec(reports).tests[0].results[0].error.location.line++; },
      reports => { for (const side of ['old', 'new']) staleSpec(reports, side).tests[0].results[0].error.location.line++; },
      reports => { staleSpec(reports).tests[0].results[0].error.stack = staleSpec(reports).tests[0].results[0].error.stack.replace('session.spec.mjs:19:9', 'session.spec.mjs:18:9'); },
      reports => { staleSpec(reports).tests[0].results[0].error.message += '\nLocator: borrowed'; },
      reports => { staleSpec(reports).tests[0].results[0].error.message = 'Error: unrelated failure'; },
      reports => { const entry = BASELINE_STALE.tests[0]; reports.new[0].suites[0].specs[0].tests[0].results = [staleControlAttempt(entry)]; },
      reports => { reports.new[0].suites[0].specs.find(row => row.title.startsWith('SKL-EDIT')).tests[0].results = [{ status: 'passed' }]; },
      reports => { reports.new[0].suites[0].specs.find(row => row.title.startsWith('SKL-EDIT')).tests[0].annotations = [{ type: 'skip', description: 'manual cascade' }]; },
      reports => { reports.new[0].suites[0].specs[0].tests[0].results = [{ status: 'STALE_CASCADE' }]; },
      reports => { reports.new[0].suites[0].specs.pop(); },
      reports => { reports.new[0].config.metadata.frontend = 'old'; },
      reports => { reports.new[0].config.metadata.baseURL = 'http://127.0.0.1:3001'; },
      reports => { reports.new.push(reports.new[0]); },
      reports => { reports.new[0].errors.push({ message: 'orchestration failure' }); },
      reports => { reports.new[0].errors.push({ message: `behavioral failure ${caseId(BASELINE_STALE.tests[0])} (failed, expected passed)` }); },
    ]) {
      const changed = controls(); mutate(changed);
      assert.equal(compare(changed).status, 'FAIL', String(mutate));
    }
    // The existing full-baseline pair path obeys the same rule without using
    // real provenance or partition logic. Covers repeated profile observations.
    const inventory = BASELINE_STALE.tests;
    const pairReports = Object.fromEntries(['old', 'new'].map(side => [side, [{
      config: { forbidOnly: true, rootDir: testDir, metadata: { frontend: side, baseURL: origin(side) } },
      suites: [{ specs: inventory.map(row => ({ file: row.file, title: row.title,
        tests: [{ expectedStatus: 'passed', results: [staleControlAttempt(row)] }] })) }],
    }]]));
    assert.equal(reconcilePair(inventory, pairReports, testDir, { old: origin('old'), new: origin('new') }).status, 'PASS');
    pairReports.new.push(structuredClone(pairReports.new[0]));
    pairReports.new[1].suites[0].specs[0].tests[0].results = [{ status: 'passed' }];
    assert.equal(reconcilePair(inventory, pairReports, testDir, { old: origin('old'), new: origin('new') }).status, 'FAIL');
  });
  await check('STALE_CASCADE: frozen AST topology, manual non-successor labels, asymmetric sets and PASS predecessor are rejected', async () => {
    assert.equal(SERIAL_TOPOLOGY.length, 28);
    assert.deepEqual(BASELINE_STALE.tests.map(row => serialSuccessors(PENDING_ORCHESTRATION, row).length), [6, 0, 2, 3]);
    const catalog = JSON.parse(await readFile(new URL('./pending-orchestration.json', import.meta.url), 'utf8'));
    catalog.tests[0].source.serial = catalog.tests[8].source.serial;
    await assert.rejects(() => readSerialTopology(catalog), /catalog drift/);
    await assert.rejects(() => readSerialTopology(undefined, async file => '\n' + await readFile(resolve(testDir, file), 'utf8')), /source drift/);
    const good = compare(controls());
    assert.deepEqual(validateStaleCascade(PENDING_ORCHESTRATION, good.old).errors, []);
    const forged = structuredClone(good);
    const outsider = forged.old.rows.find(row => row.title.startsWith('FIL-TREE-001'));
    outsider.status = 'STALE_CASCADE'; outsider.rawStatus = 'skipped'; outsider.rawSkipAnnotations = [];
    outsider.staleCascade = forged.old.rows.find(row => row.status === 'STALE_CASCADE' && row !== outsider).staleCascade;
    assert(validateStaleCascade(PENDING_ORCHESTRATION, forged.old).errors.some(error => error.includes('Invalid STALE_CASCADE')));
    assert.equal(oldRealGate(PENDING_ORCHESTRATION, forged.old).status, 'FAIL');
    assert(compareStaleRows(PENDING_ORCHESTRATION, forged).errors.some(error => error.includes('sets differ')));
    const mismatch = structuredClone(good);
    mismatch.new.rows.find(row => row.status === 'STALE_CASCADE').status = 'PASS';
    assert(compareStaleRows(PENDING_ORCHESTRATION, mismatch).errors.some(error => error.includes('sets differ')));
    const passed = structuredClone(good);
    for (const side of ['old', 'new']) passed[side].rows.find(row => row.title === BASELINE_STALE.tests[0].title).status = 'PASS';
    assert(validateStaleCascade(PENDING_ORCHESTRATION, passed.old).errors.some(error => error.includes('Invalid STALE_CASCADE')));
    assert.equal(oldRealGate(PENDING_ORCHESTRATION, passed.old).status, 'FAIL');
    assert(compareStaleRows(PENDING_ORCHESTRATION, passed).errors.some(error => error.includes('Invalid STALE_CASCADE')));
    // A Files failure at an unreviewed location cannot exempt its successors.
    const files = controls(); files.old[0].suites[0].specs[0].tests[0].results = [{ status: 'failed' }];
    for (const row of files.old[0].suites[0].specs.slice(1, 4)) row.tests[0].results = [{ status: 'skipped' }];
    const account = compare(files).old;
    assert.equal(account.staleCascade, 8); assert.equal(account.pendingOrchestration, 3);
    assert.equal(account.oldGate.status, 'FAIL');
  });
}
