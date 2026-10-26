// All expected values and source hashes come from executing fixed real TS.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdirSync, realpathSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {collect} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE!);
const out = resolve(process.argv[2]!);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
const provenance = bindSourceTree(baseline, commit, git);
const root = 'src/context/memory/edgeclaw-memory-core/src';
const sourceSha256 = sourceClosureSha256(baseline, commit, git, [
  `${root}/core/retrieval/reasoning-loop.ts`,
]);
const {cases, clockMs, locale} = await collect(baseline);
assert.ok(cases.length > 80, `only ${cases.length} cases`);
for (const [fn, count] of Object.entries(cases.reduce((all: any, c: any) => (all[c.fn] = (all[c.fn] ?? 0) + 1, all), {}))) {
  assert.ok(count! > 0, fn);
}
const oracle = {
  sourceCommit: provenance.ref,
  sourceSha256,
  nodeVersion: process.version,
  locale,
  clockMs,
  cases,
};
const text = JSON.stringify(oracle, null, 2) + '\n';
for (const leak of [resolve(process.cwd()), process.cwd()]) {
  assert.ok(!text.includes(leak + '/'), `oracle.json leaks cwd ${leak}`);
}
mkdirSync(out, {recursive: true});
writeFileSync(join(out, 'oracle.json'), text, {flag: 'wx'});
console.log(`sha256 ${createHash('sha256').update(text).digest('hex')}; ${cases.length} cases`);
