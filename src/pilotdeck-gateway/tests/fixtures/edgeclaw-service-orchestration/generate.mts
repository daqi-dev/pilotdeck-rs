// All expected values and source hashes come from executing the fixed real TS.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdirSync, realpathSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {bindBaseline} from '../_shared/generator_guard.mjs';
import {ENTRY, collect} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.argv[2]);
assert.equal(process.versions.node.split('.')[0], '22');
const baseline = resolve(process.env.PDRS_BASELINE);
const out = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
const {sourceCommit, gitExecutable} = bindBaseline({baseline, commit, gitExecutable: git});
const root = 'src/context/memory/edgeclaw-memory-core/src';
const sources = [
  `${root}/service.ts`,
  `${root}/core/index.ts`,
  `${root}/core/storage/sqlite.ts`,
  `${root}/core/file-memory.ts`,
  `${root}/core/pipeline/heartbeat.ts`,
  `${root}/core/skills/llm-extraction.ts`,
  `${root}/core/review/dream-review.ts`,
  `${root}/core/retrieval/reasoning-loop.ts`,
  `${root}/message-utils.ts`,
];
const sourceSha256 = sourceClosureSha256(baseline, commit, gitExecutable, sources);
const observed = await collect(baseline);
assert.equal(observed.scenarios.length, 23, `expected 23 scenarios, got ${observed.scenarios.length}`);
const oracle = {
  sourceCommit,
  sourceSha256,
  nodeVersion: process.version,
  entry: ENTRY,
  randomValues: observed.randomValues,
  scenarios: observed.scenarios,
};
const text = JSON.stringify(oracle, null, 2) + '\n';
for (const leak of [resolve(process.cwd()), process.cwd()]) {
  assert.ok(!text.includes(leak + '/'), `oracle.json leaks cwd ${leak}`);
}
mkdirSync(out, {recursive: true});
writeFileSync(join(out, 'oracle.json'), text, {flag: 'wx'});
console.log(`sha256 ${createHash('sha256').update(text).digest('hex')}; ${observed.scenarios.length} scenarios`);
