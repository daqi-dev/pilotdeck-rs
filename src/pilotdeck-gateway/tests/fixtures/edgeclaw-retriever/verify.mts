// Replays every oracle input through fixed real TS and requires the same bytes.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFileSync, realpathSync} from 'node:fs';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {collect, LLM, REASONING, SERVICE, SQLITE} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE);
const baseline = resolve(process.env.PDRS_BASELINE!);
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(baseline, commit, git);
const oracle = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'oracle.json'), 'utf8'));
assert.deepEqual(sourceClosureSha256(baseline, commit, git, [REASONING, SQLITE, LLM, SERVICE]), oracle.sourceSha256);
const observed = await collect(baseline);
assert.deepEqual(
  {scenarios: observed.scenarios, matrix: observed.matrix, clockMs: observed.clockMs, randomToken: observed.randomToken},
  {scenarios: oracle.scenarios, matrix: oracle.matrix, clockMs: oracle.clockMs, randomToken: oracle.randomToken});
console.log(`verified ${oracle.scenarios.length} scenarios and ${oracle.matrix.length} matrix cells with ${process.version}`);
