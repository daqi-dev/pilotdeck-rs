// For future real TS captures: preserve raw timer observations and emit shared provenance.
// Absolute timers[*].start values vary between captures; do not normalize them.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/model/streaming/streamModel.ts',
  'src/context/budget/TokenAccountingRuntime.ts',
  'src/network/fetch.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { stdout, stderr } = await promisify(execFile)(process.execPath, [
  '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'),
  fileURLToPath(new URL('./probe.mts', import.meta.url)),
], { env: { ...process.env, PDRS_BASELINE: baseline }, maxBuffer: 16 * 1024 * 1024 });
process.stderr.write(stderr);
const observations = stdout.trim().split('\n').map(line => JSON.parse(line));
const provenance = observations.filter(row => Object.hasOwn(row, 'source'));
assert.equal(provenance.length, sources.length, 'probe source coverage');
for (const [index, row] of provenance.entries()) {
  assert.equal(`src/${row.source}`, sources[index], 'probe source order');
  assert.equal(row.sha256, sourceSha256[sources[index]], 'probe source hash');
  assert.equal(row.node, process.version, 'probe Node version');
}
const cases = observations.filter(row => Object.hasOwn(row, 'id'));
assert.equal(observations.length, provenance.length + cases.length, 'unexpected probe output');
assert.equal(cases.length, 196, 'probe must record the full timing matrix');
assert.equal(new Set(cases.map(row => row.id)).size, 196, 'probe case IDs must be unique');
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
const output = resolve(process.argv[2]);
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'),
  JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 1) + '\n');
console.log(`recorded ${cases.length} timing cases from ${sourceCommit}`);
