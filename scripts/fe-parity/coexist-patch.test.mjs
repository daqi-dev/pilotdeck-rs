// #927 offline oracle for the Option A patch template and its strict segment
// boundary. Runs with `node --test scripts/fe-parity/coexist-patch.test.mjs`
// against the read-only baseline server source; no listen, no browser.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { applyOptionAPatch, isNewShellRequest, NEW_SHELL_PREFIX, optionAPatch } from './coexist-deploy.mjs';
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

test('the patched branch predicate is a strict segment boundary and honours the switch', () => {
  assert.equal(isNewShellRequest('/settings', true), true, 'exact prefix crosses');
  assert.equal(isNewShellRequest('/settings/models', true), true, 'deeper paths cross');
  assert.equal(isNewShellRequest('/settingsX', true), false, '/settingsX stays old');
  assert.equal(isNewShellRequest('/settingsx/models', true), false, 'case differs');
  assert.equal(isNewShellRequest('/pref/settings', true), false, 'suffix matches stay old');
  assert.equal(isNewShellRequest('/settings', false), false, 'switch off: byte-equivalent old behaviour');
});
