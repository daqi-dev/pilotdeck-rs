import { relative, isAbsolute } from 'node:path';

export const caseId = row => JSON.stringify([row.file.replaceAll('\\', '/'), row.title]);
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
  return errors;
}
export function reconcile(inventory, report, testDir) {
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
      return { ...base, status: 'NOT_RUN', reason: matches.length ? 'duplicate results' : 'No execution result; F2 skeleton does not run the baseline suite' };
    }
    const row = matches[0];
    if (row.results.length !== 1) {
      errors.push(`expected one attempt, got ${row.results.length}: ${caseId(row)}`);
      return { ...base, status: 'NOT_RUN', reason: 'No unique execution attempt (retries are not accepted)' };
    }
    const result = row.results[0];
    if (result.status === 'skipped') {
      const reason = row.annotations.filter(a => a.type === 'skip' && typeof a.description === 'string' && a.description.trim()).map(a => a.description.trim()).join('; ');
      if (!reason) errors.push(`skip without explicit reason ${caseId(row)}`);
      else skipped++;
      return { ...base, status: 'SKIP', reason: reason || 'FAIL: skip has no explicit reason' };
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
  if (executed !== expected) errors.push(`应执行数 ${expected} vs 实际执行数 ${executed}`);
  return { denominator: inventory.length, expected, executed, explicitSkips: skipped, status: errors.length ? 'FAIL' : 'PASS', rows, errors };
}
