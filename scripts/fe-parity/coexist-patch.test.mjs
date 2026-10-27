// #927 offline oracle for the Option A patch template and its strict segment
// boundary. Runs with `node --test scripts/fe-parity/coexist-patch.test.mjs`
// against the read-only baseline server source; no listen, no browser. The
// behavioural test EXECUTES the actual deployed template string
// (optionAPatch.prefixBranch) against a fake req/res/process — the same code
// the patched ui/server runs — instead of a hand-copied mirror of its
// predicate, so a drifted or mutated template cannot pass by looking right.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { applyOptionAPatch, executeOptionAPatchBranch, NEW_SHELL_PREFIX, NEW_SHELL_ENTRY_MARKER, optionAPatch } from './coexist-deploy.mjs';
import { baseline } from './env.mjs';

test('the A patch applies to the pristine baseline server exactly once per anchor', async () => {
  const source = await readFile(resolve(baseline, 'server/index.js'), 'utf8');
  const patched = applyOptionAPatch(source);
  assert.ok(patched.includes(optionAPatch.staticMount), 'static mount must land in the patched copy');
  assert.ok(patched.includes(optionAPatch.prefixBranch), 'prefix branch must land in the patched copy');
  // The branch's guarded prefix must be tied to the shared constant with the
  // strict segment boundary; a typo'd template prefix fails here.
  assert.ok(optionAPatch.prefixBranch.includes(
    `req.path === '${NEW_SHELL_PREFIX}' || req.path.startsWith('${NEW_SHELL_PREFIX}/')`),
  'prefix branch must guard exactly the migrated prefix');
  // Applying twice must fail (either guard: unique anchors, or the switch).
  assert.throws(() => applyOptionAPatch(patched), /anchor must occur exactly once|already carries the new-shell switch/);
});

test('the deployed branch template serves the new shell only with the switch on', () => {
  // Real execution of the deployed branch, the way the patched ui/server
  // would evaluate it: switch on serves the new-shell entrypoint document,
  // switch unset falls through to the old catchall (the rollback contract).
  assert.equal(executeOptionAPatchBranch('/settings', { PILOTDECK_NEW_SHELL: '1' }),
    '/copy/ui/server/../new-shell/index.html', '开关开：/settings 必须实际服务新壳入口');
  assert.equal(executeOptionAPatchBranch('/settings/models', { PILOTDECK_NEW_SHELL: '1' }),
    '/copy/ui/server/../new-shell/index.html', '开关开：/settings 深层路径必须服务新壳入口');
  assert.equal(executeOptionAPatchBranch('/settings', {}), null,
    '开关 unset：/settings 必须回落旧 bundle（回退开关，字节等价旧行为）');
  assert.equal(executeOptionAPatchBranch('/settingsX', { PILOTDECK_NEW_SHELL: '1' }), null,
    '严格段边界：/settingsX 必须保持旧 bundle');
  assert.equal(executeOptionAPatchBranch('/settingsx/models', { PILOTDECK_NEW_SHELL: '1' }), null,
    '严格段边界：大小写不同必须保持旧 bundle');
  assert.equal(executeOptionAPatchBranch('/pref/settings', { PILOTDECK_NEW_SHELL: '1' }), null,
    '严格段边界：后缀匹配必须保持旧 bundle');
});

test('the new bundle is discriminated by its own entrypoint marker', async t => {
  // The marker S1 asserts on must be unique to the new shell's document:
  // boundary-boot.js is injected into both shells and cannot discriminate.
  // Verified against the trusted build outputs when they exist (they are the
  // exact bytes the deployment copies); explicitly skipped otherwise, never
  // faked.
  const { existsSync } = await import('node:fs');
  const { oldDist, newDist } = await import('./env.mjs');
  if (!existsSync(resolve(newDist, 'index.html')) || !existsSync(resolve(oldDist, 'index.html'))) {
    return t.skip('trusted build outputs not present; the deployment assembly enforces the marker at build time');
  }
  const [oldHtml, newHtml] = await Promise.all([
    readFile(resolve(oldDist, 'index.html'), 'utf8'),
    readFile(resolve(newDist, 'index.html'), 'utf8'),
  ]);
  assert.ok(newHtml.includes(NEW_SHELL_ENTRY_MARKER), '新壳构建产物必须引用探针入口模块（S1 判据的存在性前提）');
  assert.ok(!oldHtml.includes(NEW_SHELL_ENTRY_MARKER), '旧 bundle 不得引用探针入口模块（S1 判据的区分性前提）');
});
