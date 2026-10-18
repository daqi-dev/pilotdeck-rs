import { relative, isAbsolute, resolve } from 'node:path';
import { readFile, realpath, stat } from 'node:fs/promises';
import { staleEntry, failureSignature, matchesStaleFailure, behavioralCoverage, compareStaleRows, deriveStaleCascade, validateStaleCascade, oldRealGate, CASCADE_STATUS, staleMetadata, stalePairingStatus, HUMAN_CONFIRMATION } from './baseline-stale.mjs';
import { cellInventory, activeRegistration, deployOrigin, deployInstanceId, DEPLOY_CELLS, selectDeployCells } from './deploy-matrix.mjs';
export { CASCADE_STATUS } from './baseline-stale.mjs';

// Reasons belong to exact identities; there is no global reason whitelist.

export const caseId = row => JSON.stringify([row.file.replaceAll('\\', '/'), row.title]);
const allowlist = JSON.parse(await readFile(new URL('./skip-allowlist.json', import.meta.url), 'utf8'));
export const BASELINE_COMMIT = allowlist.baselineCommit;
export const SKIP_ALLOWLIST = Object.freeze(allowlist.tests.map(row => Object.freeze({ ...row,
  skipReasons: Object.freeze(row.skipReasons.map(reason => Object.freeze(reason))),
})));
const allowedSkipReasons = new Map(SKIP_ALLOWLIST.map(row => [caseId(row), row.skipReasons.map(reason => reason.description)]));
const allowedSkipIds = new Set(SKIP_ALLOWLIST.map(caseId));
if (allowedSkipIds.size !== SKIP_ALLOWLIST.length) throw new Error('duplicate skip allowlist identities');
// D is an execution gap, never an additional skip allowance. Bind diagnostics
// to reviewed identities rather than accepting every real/* filename/title.
const pendingCatalog = JSON.parse(await readFile(new URL('./pending-orchestration.json', import.meta.url), 'utf8'));
export const PENDING_ORCHESTRATION = Object.freeze(pendingCatalog.tests);
export const PENDING_STATUS = 'NOT_RUN_PENDING_ORCHESTRATION';
const pendingById = new Map(PENDING_ORCHESTRATION.map(row => [caseId(row), row]));
if (pendingCatalog.baselineCommit !== BASELINE_COMMIT || pendingById.size !== 28
  || PENDING_ORCHESTRATION.length !== 28 || [...pendingById.keys()].some(id => allowedSkipIds.has(id))) {
  throw new Error('Invalid D orchestration catalog (commit, identities or B overlap)');
}
// A rewrites are new owned identities keyed by the frozen (file, title), never
// by line (loop-expanded tests share one). They have their own account and
// never enter the baseline 72 reconciliation or the current 37 ruler.
const aCatalog = JSON.parse(await readFile(new URL('./a-identities.json', import.meta.url), 'utf8'));
export const A_IDENTITIES = Object.freeze(aCatalog.tests.map(row => Object.freeze({ ...row })));
export const aTitle = row => `[A:${row.file}:${row.title}]`;
// Any title segment carrying the A marker, catalogued or not.
const isARewrite = row => row.title.split(' › ').some(part => part.startsWith('[A:'));
const aIds = new Set(A_IDENTITIES.map(caseId));
if (aCatalog.baselineCommit !== BASELINE_COMMIT || aIds.size !== A_IDENTITIES.length || A_IDENTITIES.length !== 3
  || [...aIds].some(id => allowedSkipIds.has(id) || pendingById.has(id))) {
  throw new Error('Invalid A identity catalog (commit, duplicate (file, title), count or B/D overlap)');
}
export function checkAIdentities(rows, historical) {
  const errors = [], prior = new Set(historical.tests.map(caseId));
  for (const entry of A_IDENTITIES) if (!prior.has(caseId(entry))) errors.push(`A identity not in frozen inventory ${caseId(entry)}`);
  if (new Set(rows.map(row => row.title)).size !== rows.length) errors.push('duplicate A rewrite registrations');
  for (const entry of A_IDENTITIES) {
    const matches = rows.filter(row => row.title === aTitle(entry));
    if (matches.length !== 1) errors.push(`A identity ${caseId(entry)} registered ${matches.length} times`);
    else if (matches[0].file !== entry.spec) errors.push(`A identity ${caseId(entry)} registered in ${matches[0].file}, expected ${entry.spec}`);
  }
  for (const row of rows) if (!A_IDENTITIES.some(entry => aTitle(entry) === row.title)) errors.push(`unknown A rewrite ${caseId(row)}`);
  return errors;
}
// JSON reporter carries loop-expanded tests, unlike a source regex count.
export function reportCases(report, testDir) {
  const rows = [];
  function visit(suite, parents = []) {
    const titles = suite.title && !suite.file ? [...parents, suite.title] : parents;
    for (const spec of suite.specs || []) {
      // JSON reporter paths are relative to config.rootDir, including tests
      // declared by an imported frozen spec in an owned orchestration wrapper.
      const sourceFile = isAbsolute(spec.file) ? spec.file
        : report.config?.rootDir ? resolve(report.config.rootDir, spec.file) : spec.file;
      const file = isAbsolute(sourceFile) ? relative(testDir, sourceFile) : sourceFile;
      for (const test of spec.tests || []) rows.push({
        file, title: [...titles, spec.title].join(' › '), line: spec.line, column: spec.column,
        project: test.projectName || '', expectedStatus: test.expectedStatus,
        annotations: test.annotations || [], results: test.results || [],
      });
    }
    for (const child of suite.suites || []) visit(child, titles);
  }
  for (const suite of report.suites || []) visit(suite);
  return rows;
}
export function checkInventory(rows, historical) {
  const errors = [];
  const ids = rows.map(caseId), prior = historical.tests.map(caseId);
  if (rows.length !== historical.registered) errors.push(`registered ${rows.length} != exp007 ${historical.registered}`);
  if (new Set(ids).size !== ids.length) errors.push('duplicate inventory identities (multiple projects are not one registration)');
  if (new Set(prior).size !== prior.length) errors.push('duplicate historical identities');
  for (const id of prior) if (!ids.includes(id)) errors.push(`missing historical registration ${id}`);
  for (const id of ids) if (!prior.includes(id)) errors.push(`unknown registration ${id}`);
  for (const id of allowedSkipIds) if (!ids.includes(id)) errors.push(`skip allowlist identity not registered ${id}`);
  for (const id of pendingById.keys()) if (!ids.includes(id)) errors.push(`D orchestration identity not registered ${id}`);
  return errors;
}
function reconcileSingle(inventory, report, testDir, { staleCascade = false } = {}) {
  const received = reportCases(report, testDir), grouped = new Map();
  const errors = (report.errors || []).map(error => error.message || String(error));
  if (report.config?.forbidOnly !== true) errors.push('Report must attest config.forbidOnly=true');
  for (const row of received) {
    const id = caseId(row);
    if (!grouped.has(id)) grouped.set(id, []);
    grouped.get(id).push(row);
  }
  const expectedIds = new Set(inventory.map(caseId));
  for (const [id, [row]] of grouped) if (!expectedIds.has(id)) {
    errors.push(isARewrite(row) ? `A rewrite result never credits this ledger (A-class has its own account): ${id}` : `unknown result ${id}`);
  }
  const predecessors = inventory.filter(staleEntry).flatMap(registration => {
    const matches = grouped.get(caseId(registration));
    if (matches?.length !== 1 || matches[0].results.length !== 1) return [];
    const row = matches[0], result = row.results[0];
    if (result.retry !== undefined && result.retry !== 0) return [];
    return [{ ...registration, status: result.status === 'failed' ? 'FAIL' : 'NOT_RUN', expectedStatus: row.expectedStatus,
      failureSignature: failureSignature(result, testDir, report.config?.rootDir) }];
  });
  const cascade = staleCascade ? deriveStaleCascade(inventory, predecessors) : new Map();
  let executed = 0, skipped = 0;
  const rows = inventory.map(registration => {
    const matches = grouped.get(caseId(registration)) || [];
    const stale = staleEntry(registration);
    const base = { file: registration.file, title: registration.title,
      ...(stale ? { baselineStale: staleMetadata(registration), pairedConsistency: stalePairingStatus(registration) } : {}) };
    if (matches.length !== 1) {
      errors.push(`${matches.length ? 'duplicate' : 'missing'} result ${caseId(registration)}`);
      return { ...base, status: 'NOT_RUN', reason: matches.length ? 'duplicate results' : 'No execution result' };
    }
    const row = matches[0];
    if (row.results.length !== 1 || row.results[0]?.retry !== undefined && row.results[0].retry !== 0) {
      errors.push(`expected one attempt, got ${row.results.length}: ${caseId(row)}`);
      return { ...base, status: 'NOT_RUN', reason: 'No unique execution attempt (retries are not accepted)' };
    }
    const result = row.results[0];
    if (result.status === 'skipped') {
      const annotations = row.annotations.filter(a => a.type === 'skip');
      const reason = annotations.map(a => a.description || '').join('; ');
      const pending = pendingById.get(caseId(row));
      const derived = cascade.get(caseId(row));
      if (derived && annotations.length === 0 && row.expectedStatus === 'passed') return { ...base, status: CASCADE_STATUS,
        allowedSkip: false, rawStatus: result.status, rawSkipAnnotations: annotations, staleCascade: derived,
        source: pending.source, baselineSkipDescription: pending.baselineSkipDescription,
        reason: '冻结 serial 组内位于已匹配失效断言之后；原始 skipped；不计执行或通过' };
      if (pending) {
        errors.push(`${PENDING_STATUS} ${caseId(row)}: requires mock LLM/deployment orchestration`);
        // These frozen specs contain serial groups, but no test.skip calls.
        // Never invent a baseline reason for Playwright's unannotated skips.
        if (annotations.length) errors.push(`D skip annotation differs from frozen source ${caseId(row)}: ${reason}`);
        const predecessor = received.slice(0, received.indexOf(row)).find(candidate => candidate.file === row.file
          && candidate.results.some(attempt => ['failed', 'timedOut'].includes(attempt.status)));
        return { ...base, status: PENDING_STATUS, allowedSkip: false,
          reason: 'D 类待 mock LLM / 部署编排；原始状态 skipped；' + (annotations.length ? `报告 skip 原文：${reason}` : '原始 skip annotation 为空；基线无 test.skip 理由'),
          source: pending.source, baselineSkipDescription: pending.baselineSkipDescription,
          rawStatus: result.status, rawSkipAnnotations: annotations,
          ...(predecessor ? { serialPredecessor: { file: predecessor.file, title: predecessor.title, status: predecessor.results[0].status } } : {}),
        };
      }
      const allowedIdentity = allowedSkipIds.has(caseId(row));
      const allowedReason = annotations.length === 1 && typeof annotations[0].description === 'string' && allowedSkipReasons.get(caseId(row))?.includes(reason);
      if (!allowedIdentity) errors.push(`skip identity not in fixed allowlist ${caseId(row)}`);
      if (!allowedReason) errors.push(`skip reason not bound to identity ${caseId(row)}: ${reason}`);
      if (allowedIdentity && allowedReason) skipped++;
      return { ...base, status: 'SKIP', allowedSkip: Boolean(allowedIdentity && allowedReason), reason: reason || 'FAIL: skip has no source-bound reason' };
    }
    if (!['passed', 'failed', 'timedOut'].includes(result.status)) {
      errors.push(`unexecuted/interrupted status ${result.status}: ${caseId(row)}`);
      return { ...base, status: 'NOT_RUN', reason: result.status || 'unknown status' };
    }
    executed++;
    // Expected failures are still behavioral failures in parity; do not green an xfail.
    const status = result.status === 'passed' && row.expectedStatus === 'passed' ? 'PASS' : 'FAIL';
    if (status === 'FAIL') errors.push(`behavioral failure ${caseId(row)} (${result.status}, expected ${row.expectedStatus})`);
    return { ...base, status, reason: result.error?.message || '',
      ...(stale ? { failureSignature: failureSignature(result, testDir, report.config?.rootDir),
        expectedStatus: row.expectedStatus } : {}) };
  });
  const cascaded = rows.filter(row => row.status === CASCADE_STATUS).length;
  const expected = inventory.length - skipped - cascaded;
  // Only allowlisted identities actually registered in this suite reduce its floor.
  // In particular the three skeleton registrations have no skip allowance.
  const allowedSkips = [...expectedIds].filter(id => allowedSkipIds.has(id)).length;
  const minimumExecuted = inventory.length - allowedSkips - cascaded;
  if (executed < minimumExecuted) errors.push(`实际执行数 ${executed} < 最低执行数 ${minimumExecuted} (注册数 ${inventory.length} − 身份清单数 ${allowedSkips} − STALE_CASCADE ${cascaded})`);
  if (executed !== expected) errors.push(`应执行数 ${expected} vs 实际执行数 ${executed}`);
  const pendingOrchestration = rows.filter(row => row.status === PENDING_STATUS).length;
  return { denominator: inventory.length, allowedSkips, minimumExecuted, expected, executed, explicitSkips: skipped, pendingOrchestration,
    ...(staleCascade ? { staleCascade: cascaded } : {}),
    behavioralCoverage: behavioralCoverage(rows), baselineStale: rows.filter(row => row.baselineStale),
    status: errors.length ? 'FAIL' : 'PASS', rows, errors };
}

// Merge complete profile runs only after validating every original report. A pass
// never erases a failed attempt, malformed skip, missing identity or bad provenance.
const reportList = reports => Array.isArray(reports) ? reports : [reports];
export function reconcile(inventory, reports, testDir, options) {
  const inputs = reportList(reports);
  if (inputs.length === 1) return reconcileSingle(inventory, inputs[0], testDir, options);
  const profiles = inputs.map(report => reconcileSingle(inventory, report, testDir, options));
  const errors = profiles.flatMap((profile, index) => profile.errors.map(error => `report ${index + 1}: ${error}`));
  if (!inputs.length) errors.push('No profile reports supplied');
  const rows = inventory.map((registration, index) => {
    const observations = profiles.map(profile => profile.rows[index]);
    const selected = observations.find(row => row.status === 'FAIL')
      || observations.find(row => row.status === 'PASS')
      || observations.find(row => row.status === 'NOT_RUN')
      || observations.find(row => row.status === PENDING_STATUS) || observations[0]
      || { file: registration.file, title: registration.title, status: 'NOT_RUN', reason: 'No profile reports' };
    return { ...selected, observations };
  });
  const executed = rows.filter(row => ['PASS', 'FAIL'].includes(row.status)).length;
  const skipped = rows.filter(row => row.status === 'SKIP' && row.observations.every(observation => observation.allowedSkip)).length;
  const allowedSkips = inventory.filter(row => allowedSkipIds.has(caseId(row))).length;
  const pendingOrchestration = rows.filter(row => row.observations.some(observation => observation.status === PENDING_STATUS)).length;
  return { denominator: inventory.length, allowedSkips, minimumExecuted: inventory.length - allowedSkips,
    expected: inventory.length - skipped, executed, explicitSkips: skipped, pendingOrchestration,
    behavioralCoverage: behavioralCoverage(rows), baselineStale: rows.filter(row => row.baselineStale),
    status: errors.length ? 'FAIL' : 'PASS', rows, errors, profiles };
}

// Metadata is emitted by the executing Playwright config, never assigned from CLI labels.
export function reconcilePair(inventory, reports, testDir, origins) {
  const accounts = {}, errors = [];
  for (const frontend of ['old', 'new']) {
    accounts[frontend] = reconcile(inventory, reports[frontend], testDir);
    for (const [index, report] of reportList(reports[frontend]).entries()) {
      const metadata = report.config?.metadata;
      if (metadata?.frontend !== frontend || metadata?.baseURL !== origins[frontend]) {
        errors.push(`${frontend} report ${index + 1}: report config.metadata must attest frontend=${frontend}, baseURL=${origins[frontend]}`);
      }
    }
  }
  const skipIds = frontend => new Set(accounts[frontend].rows.filter(row => row.status === 'SKIP').map(caseId));
  const oldSkips = skipIds('old'), newSkips = skipIds('new');
  for (const id of oldSkips) if (!newSkips.has(id)) errors.push(`skip identity only in old: ${id}`);
  for (const id of newSkips) if (!oldSkips.has(id)) errors.push(`skip identity only in new: ${id}`);
  const stale = compareStaleRows(inventory, accounts, !errors.some(error => error.includes('report config.metadata')));
  errors.push(...stale.errors);
  return { ...accounts, errors, baselineStale: stale.rows, needsHumanConfirmation: stale.needsHumanConfirmation,
    status: errors.length ? 'FAIL' : 'PASS' };
}

export async function loadReportPair(paths) {
  // Check every original across and within sides, including symlinks/hardlinks.
  const seen = [], reports = {};
  for (const frontend of ['old', 'new']) {
    const inputs = paths[frontend] ? reportList(paths[frontend]) : [];
    const loaded = [];
    for (const path of inputs) {
      const canonical = await realpath(path), info = await stat(canonical);
      if (seen.some(prior => prior.path === canonical || (prior.dev === info.dev && prior.ino === info.ino))) {
        throw new Error('old/new profile reports must be different files (including symlink/hardlink aliases)');
      }
      seen.push({ path: canonical, dev: info.dev, ino: info.ino });
      loaded.push(JSON.parse(await readFile(canonical, 'utf8')));
    }
    reports[frontend] = Array.isArray(paths[frontend]) ? loaded : loaded[0] || { suites: [] };
  }
  return reports;
}

// Real uses two disjoint native deployments, not overlapping profile reports.
// Reconcile each original against its fixed partition before assembling D's 28 rows.
export function reconcileReal(inventory, reports, testDir, origins, modes = ['ready', 'onboarding'], frontend = 'old') {
  const errors = [], partitions = {}, byId = new Map();
  const expectedIds = PENDING_ORCHESTRATION.map(caseId).sort();
  if (JSON.stringify(inventory.map(caseId).sort()) !== JSON.stringify(expectedIds)) errors.push('Real inventory must match the fixed D 28 identities');
  if (!modes.length || new Set(modes).size !== modes.length || modes.some(mode => !['ready', 'onboarding'].includes(mode))) errors.push('Invalid real deployment modes');
  for (const mode of modes) {
    const subset = inventory.filter(row => (row.file === 'real/onboarding.spec.mjs') === (mode === 'onboarding'));
    const report = reports[mode] || { suites: [] };
    const metadata = report.config?.metadata;
    if (!['old', 'new'].includes(frontend) || metadata?.frontend !== frontend || metadata?.baseURL !== origins[mode] || metadata?.mode !== mode
      || metadata?.suite !== 'real-D' || metadata?.backend !== 'native-with-mock-llm') errors.push(`${mode}: invalid real provenance`);
    partitions[mode] = reconcile(subset, report, testDir, { staleCascade: true });
    errors.push(...partitions[mode].errors.map(error => `${mode}: ${error}`));
    for (const row of partitions[mode].rows) {
      if (byId.has(caseId(row))) errors.push(`duplicate real partition ${caseId(row)}`);
      byId.set(caseId(row), row);
    }
  }
  const rows = inventory.map(row => byId.get(caseId(row)) || { file: row.file, title: row.title, status: 'NOT_RUN', reason: 'Deployment mode not requested' });
  const executed = rows.filter(row => ['PASS', 'FAIL'].includes(row.status)).length;
  const pendingOrchestration = rows.filter(row => row.status === PENDING_STATUS).length;
  const staleCascade = rows.filter(row => row.status === CASCADE_STATUS).length;
  const full = modes.length === 2;
  if (full && executed !== 28 - staleCascade) errors.push(`D expected ${28 - staleCascade} executions (28 minus ${staleCascade} STALE_CASCADE), received ${executed}`);
  const requested = rows.filter(row => byId.has(caseId(row)));
  const account = { denominator: 28, expected: (full ? 28 : requested.length) - staleCascade, minimumExecuted: (full ? 28 : requested.length) - staleCascade,
    executed, explicitSkips: 0, allowedSkips: 0, pendingOrchestration, staleCascade, rows, partitions,
    behavioralCoverage: behavioralCoverage(rows), baselineStale: rows.filter(row => row.baselineStale),
    coverage: full ? 'D_ALL_28' : 'D_PARTIAL_MODE_ONLY', status: errors.length ? 'FAIL' : 'PASS', errors };
  errors.push(...validateStaleCascade(inventory, account).errors);
  account.status = errors.length ? 'FAIL' : 'PASS';
  if (frontend === 'old') account.oldGate = oldRealGate(inventory, account);
  return account;
}

// Offline double-side D comparison. Every side supplies two original disjoint
// reports; this cannot borrow profile executions. Only source-derived stale
// cascades are excluded from the execution floor and never become PASS.
export function reconcileRealPair(inventory, reports, testDir, origins) {
  const accounts = {}, errors = [];
  for (const side of ['old', 'new']) {
    const partitions = {};
    const inputs = reportList(reports[side]);
    if (inputs.length !== 2) errors.push(`${side}: real pair requires exactly ready/onboarding originals`);
    for (const report of inputs) {
      const mode = report?.config?.metadata?.mode;
      if (!['ready', 'onboarding'].includes(mode) || partitions[mode]) {
        errors.push(`${side}: duplicate/unknown real report partition ${mode}`);
      } else partitions[mode] = report;
    }
    accounts[side] = reconcileReal(inventory, partitions, testDir, origins[side], ['ready', 'onboarding'], side);
  }
  const stale = compareStaleRows(inventory, accounts);
  errors.push(...stale.errors);
  return { ...accounts, errors, baselineStale: stale.rows, needsHumanConfirmation: stale.needsHumanConfirmation,
    scope: 'D_28_PAIR_ONLY', status: errors.length ? 'FAIL' : 'PASS' };
}

// B uses an execution floor per deployment. The generic historical B skip
// allowlist remains unchanged; URL-gated skips never reduce this suite's floor.
export function validateDeployRegistration(rows, inventory, cell) {
  const expected = cellInventory(inventory, cell);
  if (JSON.stringify(rows.map(deployInstanceId).sort()) !== JSON.stringify(expected.map(deployInstanceId).sort())) throw new Error(`${cell.id}: B registration identity drift`);
  for (const row of rows) {
    const original = expected.find(item => deployInstanceId(item) === deployInstanceId(row));
    if (row.line !== original.line || row.column !== original.column) throw new Error(`${cell.id}: frozen B registration location/project drift`);
  }
  return expected;
}
export function reconcileDeployCell(inventory, report, testDir, cell) {
  const registrations = cellInventory(inventory, cell), expected = registrations.filter(row => activeRegistration(row, cell)).length;
  const errors = (report?.errors || []).map(error => error.message || String(error));
  const meta = report?.config?.metadata;
  if (report?.config?.forbidOnly !== true || meta?.frontend !== 'old' || meta?.baseURL !== deployOrigin
    || meta?.suite !== 'blackbox-deploy-B' || meta?.backend !== 'native-with-mock-llm'
    || meta?.cell !== cell.id || meta?.profile !== cell.profile) errors.push(`${cell.id}: invalid B report provenance`);
  const received = reportCases(report || {}, testDir);
  try { validateDeployRegistration(received, inventory, cell); } catch (error) { errors.push(error.message); }
  const validOriginalReport = errors.length === 0;
  const rows = registrations.map(registration => {
    const active = activeRegistration(registration, cell);
    const matches = received.filter(row => deployInstanceId(row) === deployInstanceId(registration));
    const base = { ...registration, cell: cell.id, required: active, rawSkipAnnotations: matches[0]?.annotations.filter(a => a.type === 'skip') || [] };
    if (matches.length !== 1 || matches[0].results.length !== 1 || (matches[0].results[0].retry ?? 0) !== 0) {
      errors.push(`${cell.id}: no unique original attempt for ${caseId(registration)}`);
      return { ...base, status: 'NOT_RUN', reason: 'missing/duplicate/retried attempt' };
    }
    const row = matches[0], attempt = row.results[0];
    if (!active) {
      const binding = SKIP_ALLOWLIST.find(item => item.file === registration.file && item.title === registration.canonicalTitle)?.skipReasons[1];
      if (attempt.status !== 'skipped' || base.rawSkipAnnotations.length !== 1
        || base.rawSkipAnnotations[0].description !== binding?.description) {
        errors.push(`${cell.id}: profile exclusion differs from frozen source ${caseId(registration)}`);
        return { ...base, status: 'FAIL', reason: 'Invalid profile exclusion', rawStatus: attempt.status };
      }
      return { ...base, status: 'PROFILE_EXCLUDED', reason: binding.description, source: binding.source, rawStatus: 'skipped' };
    }
    if (!['passed', 'failed', 'timedOut'].includes(attempt.status)) {
      errors.push(`${cell.id}: required B case did not execute (${attempt.status}) ${caseId(registration)}`);
      return { ...base, status: attempt.status === 'skipped' ? 'SKIP' : 'NOT_RUN', reason: 'Required B execution; env gates never authorize skip', rawStatus: attempt.status };
    }
    const status = attempt.status === 'passed' && row.expectedStatus === 'passed' && !base.rawSkipAnnotations.length ? 'PASS' : 'FAIL';
    if (status === 'FAIL') errors.push(`${cell.id}: B behavioral failure ${caseId(registration)}`);
    const stale = staleEntry(registration);
    if (stale?.oldSideNondeterministic) errors.push(`${cell.id}: ${HUMAN_CONFIRMATION} ${caseId(registration)}`);
    const signature = stale ? failureSignature(attempt, testDir, report.config?.rootDir) : undefined;
    const matched = Boolean(stale && validOriginalReport
      && row.expectedStatus === 'passed' && !base.rawSkipAnnotations.length && matchesStaleFailure(registration, signature));
    return { ...base, status, executed: true, rawStatus: attempt.status, reason: attempt.error?.message || '',
      ...(stale ? { expectedStatus: row.expectedStatus, failureSignature: signature,
        baselineStale: staleMetadata(registration), matchedFailCondition: matched, pairedConsistency: stalePairingStatus(registration) } : {}) };
  });
  const executed = rows.filter(row => row.executed).length;
  if (expected !== executed) errors.push(`${cell.id}: 应执行数 ${expected} vs 实际执行数 ${executed}`);
  return { cell: cell.id, profile: cell.profile, denominator: registrations.length, expected, executed, minimumExecuted: expected,
    explicitSkips: 0, profileExclusions: rows.filter(row => row.status === 'PROFILE_EXCLUDED').length,
    pendingOrchestration: 0, rows, errors, status: errors.length ? 'FAIL' : 'PASS' };
}
export function reconcileDeploy(inventory, reports, testDir, cells) {
  const partitions = {}, errors = [];
  const catalog = inventory.filter(row => SKIP_ALLOWLIST.some(item => caseId(item) === caseId(row)));
  if (catalog.length !== 9 || new Set(catalog.map(caseId)).size !== 9) errors.push('B requires the original nine source registrations');
  for (const row of catalog) {
    const source = SKIP_ALLOWLIST.find(item => caseId(item) === caseId(row)).source[0];
    if (row.line !== Number(source.split(':').at(-1)) || !Number.isInteger(row.column) || row.column < 1) errors.push(`B inventory location drift ${caseId(row)}`);
  }
  try {
    const requested = [...new Set(cells.map(cell => cell.profile).filter(profile => profile !== 'native'))];
    const canonical = selectDeployCells(requested);
    if (cells.length !== canonical.length || canonical.some(cell => cells.filter(item => JSON.stringify(item) === JSON.stringify(cell)).length !== 1)) {
      errors.push('B requires exact requested profile cells and both native auxiliaries');
    }
  } catch (error) { errors.push(error.message); }
  for (const cell of cells) {
    if (cell.blocked) {
      const rows = cellInventory(inventory, cell).filter(row => activeRegistration(row, cell)).map(row => ({ ...row, cell: cell.id, required: true, status: 'BLOCKED', reason: cell.blocked }));
      partitions[cell.id] = { cell: cell.id, profile: cell.profile, expected: rows.length, minimumExecuted: rows.length,
        executed: 0, status: 'BLOCKED', errors: [cell.blocked], rows, profileExclusions: 0 };
    } else partitions[cell.id] = reconcileDeployCell(inventory, reports[cell.id], testDir, cell);
    errors.push(...partitions[cell.id].errors.map(error => `${cell.id}: ${error}`));
  }
  for (const id of Object.keys(reports)) if (!cells.some(cell => cell.id === id && !cell.blocked)) errors.push(`Unknown/blocked B report partition ${id}`);
  const b = catalog;
  const observations = Object.values(partitions).flatMap(part => part.rows.filter(row => row.required));
  const rows = b.map(registration => {
    const matches = observations.filter(row => row.file === registration.file && row.canonicalTitle === registration.title);
    const status = !matches.length ? 'NOT_RUN' : matches.some(row => row.status === 'BLOCKED') ? 'BLOCKED'
      : matches.some(row => row.status !== 'PASS') ? 'FAIL' : 'PASS';
    const stale = staleEntry(registration);
    return { file: registration.file, title: registration.title, status, reason: matches.map(row => row.reason).filter(Boolean).join('; '), observations: matches,
      ...(stale ? { baselineStale: staleMetadata(registration), matchedFailCondition: matches.length > 0 && matches.every(row => row.matchedFailCondition === true),
        pairedConsistency: matches.length ? stalePairingStatus(registration) : 'UNPAIRED' } : {}) };
  });
  const expected = rows.filter(row => row.observations.length).length;
  const executed = rows.filter(row => row.observations.some(item => item.executed)).length;
  const instanceExpected = Object.values(partitions).reduce((n, p) => n + p.expected, 0);
  const instanceExecuted = Object.values(partitions).reduce((n, p) => n + p.executed, 0);
  if (expected !== executed) errors.push(`B unique 应执行数 ${expected} vs 实际执行数 ${executed}; BLOCKED 保留分母`);
  return { denominator: 9, expected, executed, minimumExecuted: expected, instanceExpected, instanceExecuted,
    explicitSkips: 0, profileExclusions: Object.values(partitions).reduce((n, p) => n + p.profileExclusions, 0), pendingOrchestration: 0,
    blocked: rows.filter(row => row.status === 'BLOCKED').length, rows, partitions,
    behavioralCoverage: { eligible: rows.filter(row => row.observations.length && !staleEntry(row)).length,
      passed: rows.filter(row => row.status === 'PASS' && !staleEntry(row)).length,
      excludedBaselineStale: rows.filter(row => row.observations.length && staleEntry(row)).length },
    baselineStale: rows.filter(row => row.observations.length && staleEntry(row)),
    needsHumanConfirmation: rows.filter(row => row.pairedConsistency === HUMAN_CONFIRMATION).length,
    scope: cells.length === DEPLOY_CELLS.length ? 'B_ALL_9_OLD_ONLY' : 'B_PARTIAL_PROFILES_OLD_ONLY', status: errors.length ? 'FAIL' : 'PASS', errors };
}
