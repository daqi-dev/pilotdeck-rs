import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFileSync, realpathSync} from 'node:fs';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {collect, ENTRY} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE);
const base = resolve(process.env.PDRS_BASELINE);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
bindSourceTree(base, commit, git);
const oracle = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'oracle.json'), 'utf8'));
const root = 'src/context/memory/edgeclaw-memory-core/src';
const sources = [
  `${root}/core/pipeline/heartbeat.ts`,
  `${root}/service.ts`,
  `${root}/core/file-memory.ts`,
  `${root}/core/storage/sqlite.ts`,
  `${root}/core/skills/llm-extraction.ts`,
];
assert.deepEqual(sourceClosureSha256(base, commit, git, sources), oracle.sourceSha256);
assert.deepEqual(await collect(base), {cases: oracle.cases});
console.log(`verified ${oracle.cases.length} cases with ${process.version} (${ENTRY})`);
