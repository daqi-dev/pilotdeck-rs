// Replays every oracle input through fixed real TS and requires the same bytes.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFileSync, realpathSync} from 'node:fs';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {collect} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE);
const baseline = resolve(process.env.PDRS_BASELINE!);
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(baseline, commit, git);
const sourceSha256 = sourceClosureSha256(baseline, commit, git, [
  'src/context/memory/edgeclaw-memory-core/src/core/pipeline/heartbeat.ts',
]);
const oracle = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'oracle.json'), 'utf8'));
assert.deepEqual(sourceSha256, oracle.sourceSha256);
const observed = await collect(baseline);
assert.deepEqual(observed, {cases: oracle.cases, clockMs: oracle.clockMs});
if (process.argv[2]) {
  const {writeFileSync, mkdirSync} = await import('node:fs');
  mkdirSync(dirname(resolve(process.argv[2])), {recursive: true});
  writeFileSync(resolve(process.argv[2]), JSON.stringify(observed, null, 2) + '\n', {flag: 'wx'});
}
console.log(`verified ${oracle.cases.length} cases`);
