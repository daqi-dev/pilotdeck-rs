import { relative, isAbsolute } from 'node:path';
import { readFile, realpath, stat } from 'node:fs/promises';

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
// JSON reporter carries loop-expanded tests, unlike a source regex count.
export function reportCases(report, testDir) {
  const rows = [];
  function visit(suite, parents = []) {
    const titles = suite.title && !suite.file ? [...parents, suite.title] : parents;
    for (const spec of suite.specs || []) {
      const file = isAbsolute(spec.file) ? relative(testDir, spec.file) : spec.file;
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
function reconcileSingle(inventory, report, testDir) {
  const received = reportCases(report, testDir), grouped = new Map();
  const errors = (report.errors || []).map(error => error.message || String(error));
  if (report.config?.forbidOnly !== true) errors.push('Report must attest config.forbidOnly=true');
  for (const row of received) {
    const id = caseId(row);
    if (!grouped.has(id)) grouped.set(id, []);
    grouped.get(id).push(row);
  }
  const expectedIds = new Set(inventory.map(caseId));
  for (const id of grouped.keys()) if (!expectedIds.has(id)) errors.push(`unknown result ${id}`);
  let executed = 0, skipped = 0;
  const rows = inventory.map(registration => {
    const matches = grouped.get(caseId(registration)) || [];
    const base = { file: registration.file, title: registration.title };
    if (matches.length !== 1) {
      errors.push(`${matches.length ? 'duplicate' : 'missing'} result ${caseId(registration)}`);
      return { ...base, status: 'NOT_RUN', reason: matches.length ? 'duplicate results' : 'No execution result' };
    }
    const row = matches[0];
    if (row.results.length !== 1) {
      errors.push(`expected one attempt, got ${row.results.length}: ${caseId(row)}`);
      return { ...base, status: 'NOT_RUN', reason: 'No unique execution attempt (retries are not accepted)' };
    }
    const result = row.results[0];
    if (result.status === 'skipped') {
      const annotations = row.annotations.filter(a => a.type === 'skip');
      const reason = annotations.map(a => a.description || '').join('; ');
      const pending = pendingById.get(caseId(row));
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
    return { ...base, status, reason: result.error?.message || '' };
  });
  const expected = inventory.length - skipped;
  // Only allowlisted identities actually registered in this suite reduce its floor.
  // In particular the three skeleton registrations have no skip allowance.
  const allowedSkips = [...expectedIds].filter(id => allowedSkipIds.has(id)).length;
  const minimumExecuted = inventory.length - allowedSkips;
  if (executed < minimumExecuted) errors.push(`实际执行数 ${executed} < 最低执行数 ${minimumExecuted} (注册数 ${inventory.length} − 身份清单数 ${allowedSkips})`);
  if (executed !== expected) errors.push(`应执行数 ${expected} vs 实际执行数 ${executed}`);
  const pendingOrchestration = rows.filter(row => row.status === PENDING_STATUS).length;
  return { denominator: inventory.length, allowedSkips, minimumExecuted, expected, executed, explicitSkips: skipped, pendingOrchestration, status: errors.length ? 'FAIL' : 'PASS', rows, errors };
}

// Merge complete profile runs only after validating every original report. A pass
// never erases a failed attempt, malformed skip, missing identity or bad provenance.
const reportList = reports => Array.isArray(reports) ? reports : [reports];
export function reconcile(inventory, reports, testDir) {
  const inputs = reportList(reports);
  if (inputs.length === 1) return reconcileSingle(inventory, inputs[0], testDir);
  const profiles = inputs.map(report => reconcileSingle(inventory, report, testDir));
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
  return { ...accounts, errors, status: errors.length || Object.values(accounts).some(a => a.status !== 'PASS') ? 'FAIL' : 'PASS' };
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
