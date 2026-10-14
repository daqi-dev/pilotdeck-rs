import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve, dirname } from 'node:path';
import { baseline, historicalInventory } from './env.mjs';
import { PENDING_ORCHESTRATION, PENDING_STATUS, SKIP_ALLOWLIST, caseId, reconcile } from './accounting.mjs';

export async function orchestrationSelfCheck(check) {
  await check('D catalog is source-bound to all 28 registered identities, disjoint from B nine', async () => {
    const inventory = JSON.parse(await readFile(historicalInventory, 'utf8')).tests;
    const registeredD = inventory.filter(row => row.file.startsWith('real/'));
    assert.equal(registeredD.length, 28);
    assert.deepEqual(PENDING_ORCHESTRATION.map(caseId).sort(), registeredD.map(caseId).sort());
    assert.equal(SKIP_ALLOWLIST.length, 9);
    for (const row of PENDING_ORCHESTRATION) {
      assert(!SKIP_ALLOWLIST.some(allowed => caseId(allowed) === caseId(row)));
      assert.equal(row.baselineSkipDescription, null);
      const source = await readFile(resolve(baseline, 'e2e', row.file), 'utf8');
      assert(!source.includes('test.skip('), 'Do not invent a skip reason absent from the frozen spec');
      const line = ref => Number(ref.split(':').at(-1)) - 1;
      const registration = source.split('\n')[line(row.source.registration)];
      const title = row.title.startsWith('SES-COMPOSER-001 real ')
        ? 'SES-COMPOSER-001 real ${provider[0]} chat' : row.title;
      assert(registration.includes('test(') && registration.includes(title));
      assert.equal(row.source.registration.split(':')[0], `ui/e2e/${row.file}`);
      assert(source.split('\n')[line(row.source.serial)].includes("test.describe.configure({ mode: 'serial' })"));
      const [path] = row.source.orchestration.split(':');
      assert((await readFile(resolve(dirname(baseline), path), 'utf8')).split('\n')[line(row.source.orchestration)].includes('PILOTDECK_E2E_LLM_CENTER_URL'));
    }
  });
  await check('D runtime skips stay pending and FAIL, preserve raw reasons, never lower the execution floor', () => {
    const make = (row, status, annotations = []) => ({ config: { forbidOnly: true }, suites: [{ specs: [{ ...row,
      tests: [{ expectedStatus: 'passed', annotations, results: [{ status }] }],
    }] }] });
    for (const row of PENDING_ORCHESTRATION) {
      const skipped = make(row, 'skipped');
      const result = reconcile([row], skipped, '');
      assert.equal(result.rows[0].status, PENDING_STATUS);
      assert.deepEqual(result.rows[0].rawSkipAnnotations, []);
      assert.deepEqual(result.rows[0].source, row.source);
      assert.equal(result.rows[0].baselineSkipDescription, null);
      assert.equal(result.status, 'FAIL'); assert.equal(result.executed, 0);
      assert.equal(result.expected, 1); assert.equal(result.minimumExecuted, 1);
      assert.equal(result.explicitSkips, 0); assert.equal(result.allowedSkips, 0);
      assert.equal(result.pendingOrchestration, 1);
      assert(!result.errors.some(error => error.includes('skip identity not in fixed allowlist')));
      for (const reports of [[skipped, make(row, 'passed')], [make(row, 'passed'), skipped]]) {
        const merged = reconcile([row], reports, '');
        assert.equal(merged.status, 'FAIL'); assert.equal(merged.executed, 1);
        assert.equal(merged.pendingOrchestration, 1); assert.equal(merged.explicitSkips, 0);
        assert(merged.rows[0].observations.some(observation => observation.status === PENDING_STATUS));
      }
      const annotations = [{ type: 'skip', description: 'unexpected reason' }];
      const drift = reconcile([row], make(row, 'skipped', annotations), '');
      assert.deepEqual(drift.rows[0].rawSkipAnnotations, annotations);
      assert(drift.errors.some(error => error.startsWith('D skip annotation differs')));
      const outsider = { ...row, title: row.title + ' outsider' };
      assert.equal(reconcile([outsider], make(outsider, 'skipped'), '').rows[0].status, 'SKIP');
      for (const status of ['passed', 'failed', 'timedOut']) {
        const executed = reconcile([row], make(row, status), '');
        assert.equal(executed.pendingOrchestration, 0); assert.equal(executed.executed, 1);
        assert.equal(executed.rows[0].status, status === 'passed' ? 'PASS' : 'FAIL');
      }
      assert.equal(reconcile([row], { config: { forbidOnly: true }, suites: [] }, '').rows[0].status, 'NOT_RUN');
    }
  });
}
