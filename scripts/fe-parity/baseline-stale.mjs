import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import { resolve, relative, isAbsolute, dirname } from 'node:path';
import { baseline } from './env.mjs';
import { serialSuccessors, serialLocation } from './serial-topology.mjs';

const digest = value => createHash('sha256').update(value).digest('hex');
// Reviewed manifest fingerprint. Changing a character/identity/evidence line
// requires an explicit new review in code, never an editable JSON allowance.
const REVIEWED_DIGEST = 'e9cb926026bb3cf6e03761e0048b789c705c6e7aba8b4ea915e292d48288a165';
const identity = row => JSON.stringify([row.file.replaceAll('\\', '/'), row.title]);
const deepFreeze = value => {
  if (value && typeof value === 'object') { Object.values(value).forEach(deepFreeze); Object.freeze(value); }
  return value;
};
export const BASELINE_STALE = deepFreeze(JSON.parse(await readFile(new URL('./baseline-stale.json', import.meta.url), 'utf8')));
export function validateStaleManifest(catalog = BASELINE_STALE) {
  validateOldSideNondeterminism(catalog);
  if (digest(JSON.stringify(catalog)) !== REVIEWED_DIGEST) throw new Error('Baseline stale manifest differs from reviewed exact identities/evidence');
  return catalog;
}
// This is reviewed identity-bound evidence, never a flag supplied by a report
// or borrowed from another registration. The digest pins all historical facts.
export function validateOldSideNondeterminism(catalog) {
  for (const row of catalog.tests) {
    const evidence = row.oldSideNondeterminism;
    if (row.oldSideNondeterministic === undefined && evidence === undefined) continue;
    const fail = () => { throw new Error(`Invalid old-side nondeterminism evidence: ${identity(row)}`); };
    if (row.oldSideNondeterministic !== true || !evidence || !evidence.reason || !evidence.proofScript
      || identity(evidence.identity ?? { file: '', title: '' }) !== identity(row)
      || !evidence.sourceAnchors?.length || !evidence.observations?.length) fail();
    if (!evidence.sourceAnchors.every(anchor => row.evidence?.some(source => source.file === anchor.file
      && source.line === anchor.line && source.text && /^[a-f0-9]{64}$/.test(source.sha256)))) fail();
    if (!evidence.observations.every(item => identity(item.identity ?? { file: '', title: '' }) === identity(row)
      && item.trace && /^[a-f0-9]{64}$/.test(item.traceSha256) && item.traceMember && item.networkMember
      && Number.isFinite(item.fill?.startMs) && Number.isFinite(item.fill?.endMs)
      && Number.isFinite(item.searchClick?.startMs) && item.runtimeRequests?.length)) fail();
    // The review must show both the pre-assertion timeout and a clean failure,
    // not merely speculate that an otherwise deterministic failure is flaky.
    if (!evidence.observations.some(item => item.rawStatus === 'timedOut' && item.searchClick.disabled
      && item.queries?.length === 0 && item.loadingSnapshotsAfterFillStarted?.length)
      || !evidence.observations.some(item => item.rawStatus === 'failed'
        && item.errorLocation?.file === row.failure.assertion.file
        && item.errorLocation?.line === row.failure.assertion.line
        && item.errorLocation?.column === row.failure.assertion.column
        && item.queries?.some(query => query.status === 501))) fail();
  }
}
export async function validateStaleEvidence(catalog = BASELINE_STALE, readSource = file => readFile(resolve(dirname(baseline), file), 'utf8')) {
  for (const row of catalog.tests) for (const evidence of row.evidence) {
    const source = await readSource(evidence.file);
    if (source.split(/\r?\n/)[evidence.line - 1] !== evidence.text || digest(source) !== evidence.sha256) {
      throw new Error(`Baseline stale evidence drift: ${evidence.file}:${evidence.line}`);
    }
  }
}
export async function validateBaselineStale(catalog = BASELINE_STALE) {
  validateStaleManifest(catalog);
  await validateStaleEvidence(catalog);
  return { status: 'PASS', identities: catalog.tests.length, behavioralCoverageCredit: 0 };
}
await validateBaselineStale();
const reviewedById = new Map(BASELINE_STALE.tests.map(row => [identity(row), row]));
export const staleEntry = row => reviewedById.get(identity(row));
export const HUMAN_CONFIRMATION = 'NEEDS_HUMAN_CONFIRMATION';
export const stalePairingStatus = row => staleEntry(row)?.oldSideNondeterministic ? HUMAN_CONFIRMATION : 'UNPAIRED';
export function staleMetadata(row) {
  const stale = staleEntry(row);
  if (!stale) return undefined;
  return { defectType: stale.defectType, proofScript: stale.proofScript, evidence: stale.evidence,
    ...(stale.oldSideNondeterministic ? { oldSideNondeterministic: true, oldSideNondeterminism: stale.oldSideNondeterminism } : {}),
    behavioralCoverageCredit: 0 };
}

// Keep both the helper assertion and the spec callsite: reviewed helper failures share the
// same helper, so comparing helper line alone would accept the wrong defect.
// Direct spec assertions bind the first source stack frame to the same spec.
export function failureSignature(result, testDir, rootDir = testDir) {
  if (result.status !== 'failed' || !result.error || result.errors?.length > 1) return null;
  const clean = value => String(value || '').replace(/\x1b\[[0-9;]*m/g, '');
  const error = result.error, message = clean(error.message), stack = clean(error.stack);
  const loc = error.location;
  if (!loc || typeof loc.file !== 'string' || !Number.isInteger(loc.line) || !Number.isInteger(loc.column)) return null;
  const normalizedFile = file => relative(testDir, isAbsolute(file) ? file : resolve(rootDir, file)).replaceAll('\\', '/');
  const assertion = { file: normalizedFile(loc.file), line: loc.line, column: loc.column };
  for (const other of [result.errorLocation, ...(result.errors || []).map(item => item.location)].filter(Boolean)) {
    if (typeof other.file !== 'string') return null;
    if (JSON.stringify({ file: normalizedFile(other.file), line: other.line, column: other.column }) !== JSON.stringify(assertion)) return null;
  }
  const locators = [...message.matchAll(/^Locator: (.+)$/gm)];
  const matchers = [...message.matchAll(/expect\(locator\)\.(\w+)\((?:expected)?\) failed/g)];
  if (locators.length !== 1 || matchers.length !== 1) return null;
  const locator = locators[0][1], matcher = matchers[0][1];
  const locations = [...stack.matchAll(/(?:\(|at )(\/[^\n]*?):(\d+):(\d+)\)?(?=\n|$)/g)]
    .map(match => ({ file: normalizedFile(match[1]), line: Number(match[2]), column: Number(match[3]) }));
  const callsite = locations.find(item => item.file !== assertion.file)
    ?? locations[0];
  if (!locator || !matcher || !callsite) return null;
  return { assertion, callsite, matcher, locator };
}
export function matchesStaleFailure(row, signature) {
  const reviewed = staleEntry(row);
  return Boolean(reviewed && signature && JSON.stringify(signature) === JSON.stringify(reviewed.failure));
}
export function behavioralCoverage(rows) {
  const eligible = rows.filter(row => !staleEntry(row) && row.status !== 'STALE_CASCADE');
  const acceptedSkips = eligible.filter(row => row.status === 'SKIP' && row.allowedSkip).length;
  return { eligible: eligible.length - acceptedSkips, executed: eligible.filter(row => ['PASS', 'FAIL'].includes(row.status)).length,
    passed: eligible.filter(row => row.status === 'PASS').length, excludedBaselineStale: rows.filter(staleEntry).length,
    ...(rows.some(row => row.status === 'STALE_CASCADE') ? { excludedStaleCascade: rows.filter(row => row.status === 'STALE_CASCADE').length } : {}) };
}

export const CASCADE_STATUS = 'STALE_CASCADE';
export function deriveStaleCascade(inventory, observations) {
  const successors = new Map();
  for (const predecessor of observations.filter(row => staleEntry(row) && row.status === 'FAIL'
    && row.expectedStatus === 'passed' && matchesStaleFailure(row, row.failureSignature))) {
    for (const row of serialSuccessors(inventory, predecessor)) if (!successors.has(identity(row))) {
      successors.set(identity(row), { predecessor: { file: predecessor.file, title: predecessor.title }, ...serialLocation(row) });
    }
  }
  return successors;
}
export function validateStaleCascade(inventory, account) {
  const errors = [], expected = deriveStaleCascade(inventory, account.rows), actual = new Set();
  for (const row of account.rows) {
    const derived = expected.get(identity(row));
    if (row.status === CASCADE_STATUS) {
      actual.add(identity(row));
      if (!derived || row.rawStatus !== 'skipped' || row.rawSkipAnnotations?.length !== 0
        || JSON.stringify(row.staleCascade) !== JSON.stringify(derived)) errors.push(`Invalid ${CASCADE_STATUS} derivation ${identity(row)}`);
    }
    if (derived && row.status !== CASCADE_STATUS) errors.push(`Missing ${CASCADE_STATUS} successor ${identity(row)}`);
  }
  if (account.staleCascade !== undefined && account.staleCascade !== actual.size) errors.push('STALE_CASCADE count differs from derived row identities');
  return { ids: [...actual].sort(), expectedIds: [...expected.keys()].sort(), errors };
}

// Old-only calibration is explicit; it cannot certify a MATCHED_FAIL pair.
export function oldRealGate(inventory, account) {
  const errors = [...account.errors], stale = inventory.filter(staleEntry);
  const realStale = BASELINE_STALE.tests.filter(row => row.file.startsWith('real/'));
  const sourcesValid = !errors.some(error => error.includes('invalid real provenance'));
  const matched = stale.filter(registration => {
    const row = account.rows.find(item => identity(item) === identity(registration));
    return sourcesValid && row?.status === 'FAIL' && row.expectedStatus === 'passed' && matchesStaleFailure(row, row.failureSignature);
  });
  const quota = new Map(matched.map(row => [`behavioral failure ${identity(row)} (failed, expected passed)`, 1]));
  const remaining = errors.filter(error => {
    const normalized = error.replace(/^(?:ready|onboarding): /, '');
    if ((quota.get(normalized) ?? 0) > 0) { quota.set(normalized, quota.get(normalized) - 1); return false; }
    return true;
  });
  if (stale.length !== realStale.length || !realStale.every(entry => stale.some(row => identity(row) === identity(entry)))
    || matched.length !== stale.length) remaining.push(`Old calibration requires all ${realStale.length} reviewed D stale failures`);
  const cascade = validateStaleCascade(inventory, account);
  remaining.push(...cascade.errors);
  for (const row of account.rows) if (!staleEntry(row) && row.status !== CASCADE_STATUS && row.status !== 'PASS') remaining.push(`Old calibration requires PASS ${identity(row)}`);
  if (account.coverage !== 'D_ALL_28') remaining.push('Old calibration requires both real deployments');
  return { status: remaining.length ? 'FAIL' : 'PASS', scope: 'OLD_REAL_CALIBRATION_ONLY',
    matchedFailConditions: matched.length, expectedStaleFailures: realStale.length, pairedConsistency: 'UNPAIRED', staleCascade: cascade.ids.length,
    pendingOrchestration: account.pendingOrchestration, passed: account.rows.filter(row => row.status === 'PASS').length,
    executed: account.executed, errors: remaining };
}

export function compareStaleRows(inventory, accounts, sourcesValid = true) {
  const rows = [], errors = [], matchingIds = new Set();
  sourcesValid &&= !['old', 'new'].some(side => accounts[side]?.errors.some(error => error.includes('invalid real provenance')));
  if (['old', 'new'].some(side => accounts[side]?.staleCascade !== undefined)) {
    const cascades = Object.fromEntries(['old', 'new'].map(side => [side, validateStaleCascade(inventory, accounts[side])]));
    for (const side of ['old', 'new']) errors.push(...cascades[side].errors.map(error => `${side}: ${error}`));
    if (JSON.stringify(cascades.old.ids) !== JSON.stringify(cascades.new.ids)) errors.push('STALE_CASCADE sets differ between old and new');
  }
  for (const registration of inventory.filter(staleEntry)) {
    const observations = {};
    for (const side of ['old', 'new']) {
      const row = accounts[side]?.rows.find(item => identity(item) === identity(registration));
      const attempts = row?.observations || (row ? [row] : []);
      observations[side] = attempts.map(item => ({ status: item.status, expectedStatus: item.expectedStatus,
        failureSignature: item.failureSignature ?? null }));
    }
    const consistent = sourcesValid && ['old', 'new'].every(side => observations[side].length > 0
      && observations[side].every(row => row.status === 'FAIL' && row.expectedStatus === 'passed'
        && matchesStaleFailure(registration, row.failureSignature)));
    const nondeterministic = staleEntry(registration).oldSideNondeterministic === true;
    if (nondeterministic) errors.push(`${HUMAN_CONFIRMATION}: evidence-bound old-side nondeterminism ${identity(registration)}`);
    else if (consistent) matchingIds.add(identity(registration));
    if (!consistent) errors.push(`Baseline stale must be FAIL at reviewed assertion/callsite on both sides: ${identity(registration)}`);
    rows.push({ file: registration.file, title: registration.title, defectType: staleEntry(registration).defectType,
      ...(nondeterministic ? { oldSideNondeterministic: true, oldSideNondeterminism: staleEntry(registration).oldSideNondeterminism } : {}),
      observations, consistency: nondeterministic ? HUMAN_CONFIRMATION : consistent ? 'MATCHED_FAIL' : 'FAIL', behavioralCoverageCredit: 0 });
  }
  // Only remove exact behavioral-failure diagnostics for validated pairs. All
  // registration, provenance, skip, serial pending and retry errors survive.
  for (const side of ['old', 'new']) {
    // One generated failure diagnostic per accepted observation, no more.
    // An identical report.errors message cannot consume a second allowance.
    const quota = new Map([...matchingIds].map(id => {
      const row = accounts[side].rows.find(item => identity(item) === id);
      return [`behavioral failure ${id} (failed, expected passed)`, row.observations?.length ?? 1];
    }));
    for (const error of accounts[side]?.errors || []) {
      const normalized = error.replace(/^(?:report \d+|ready|onboarding): /, '');
      if ((quota.get(normalized) ?? 0) > 0) quota.set(normalized, quota.get(normalized) - 1);
      else errors.push(`${side}: ${error}`);
    }
  }
  return { rows, errors, matchingIds: [...matchingIds], needsHumanConfirmation: rows.filter(row => row.consistency === HUMAN_CONFIRMATION).length };
}
