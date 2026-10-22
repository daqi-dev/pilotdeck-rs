import assert from 'node:assert/strict';
import test from 'node:test';
import { assertPortablePaths } from './portable-paths.mjs';

test('reject case collisions in implicit directories before FS coalesces them', () => {
  assert.throws(() => assertPortablePaths(['plugins/Z/plugin.json', 'plugins/z/plugin.json']), /non-portable/);
});
test('reject canonical Unicode collisions', () => {
  assert.throws(() => assertPortablePaths(['plugins/Å/plugin.json', 'plugins/A\u030a/plugin.json']), /non-portable/);
  assert.throws(() => assertPortablePaths(['plugins/é/plugin.json', 'plugins/e\u0301/plugin.json']), /non-portable/);
});
test('accept distinct sorting names, exact repeats and separate parents', () => {
  assertPortablePaths(['plugins/Z-upper/plugin.json', 'plugins/z-lower/plugin.json', 'plugins/Å/plugin.json', 'plugins/2/plugin.json', 'plugins/10/plugin.json', 'skills/z-lower/SKILL.md', 'skills/z-lower/SKILL.md']);
});
