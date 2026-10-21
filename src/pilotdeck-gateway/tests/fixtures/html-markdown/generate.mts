import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { record } from './record.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide a fresh output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const ref = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const binding = bindSourceTree(baseline, ref, git);
// Match web-tools' closure so that offline capture can compare to its existing
// recorded hashes; a full clean-tree attestation still happens here.
const hashes = sourceClosureSha256(baseline, ref, git,
  ['src/tool/builtin/webFetch.ts', 'src/tool/builtin/webSearch.ts']);
const oracle = await record(baseline, binding.ref, hashes);
const output = join(resolve(process.argv[2]), 'oracle.json');
writeFileSync(output, JSON.stringify(oracle, null, 1) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ output, cases: oracle.cases.length, gaps: oracle.parserGaps.length }));
