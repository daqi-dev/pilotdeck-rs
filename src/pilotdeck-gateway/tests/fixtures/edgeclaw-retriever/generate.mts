// All expected values and source hashes come from executing fixed real TS.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdirSync, realpathSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {collect, LLM, REASONING, SERVICE, SQLITE} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE!);
const out = resolve(process.argv[2]!);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
const provenance = bindSourceTree(baseline, commit, git);
const sourceSha256 = sourceClosureSha256(baseline, commit, git, [REASONING, SQLITE, LLM, SERVICE]);
const {scenarios, matrix, clockMs, randomToken} = await collect(baseline);
assert.ok(scenarios.length >= 4, `only ${scenarios.length} scenarios`);
assert.ok(matrix.length >= 8, `only ${matrix.length} matrix cells`);
for (const scenario of scenarios) {
  assert.ok(scenario.operations.some((op: any) => op.result.debug?.cacheHit === false), `${scenario.name}: no miss`);
}
const oracle = {
  sourceCommit: provenance.ref,
  sourceSha256,
  nodeVersion: process.version,
  clockMs,
  randomToken,
  scenarios,
  matrix,
};
const text = JSON.stringify(oracle, null, 2) + '\n';
for (const leak of [resolve(process.cwd()), process.cwd()]) {
  assert.ok(!text.includes(leak + '/'), `oracle.json leaks cwd ${leak}`);
}
mkdirSync(out, {recursive: true});
writeFileSync(join(out, 'oracle.json'), text, {flag: 'wx'});
console.log(`sha256 ${createHash('sha256').update(text).digest('hex')}; ${scenarios.length} scenarios; ${matrix.length} matrix cells`);
