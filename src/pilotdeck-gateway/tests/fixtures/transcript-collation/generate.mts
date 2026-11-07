// #132: real `readTranscript` order for equal-sequence `createdAt` ties that
// carry isolated UTF-16 surrogates. Nothing is re-implemented here: the
// generator imports the pinned baseline entry point and records its output.
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#132 Node pin');
assert.equal(process.versions.icu, '78.2', '#132 ICU pin');
assert.ok(process.env.PDRS_BASELINE, '#132 baseline required');
assert.ok(process.argv[2], '#132 fresh output directory required');
const output = resolve(process.argv[2]);
mkdirSync(output, { recursive: true }); // oracle.json uses flag 'wx'
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sourceSha256 = await baselineSourceHashes(baseline, [
  'src/session/transcript/TranscriptReader.ts',
  'src/session/transcript/TranscriptEntry.ts',
]);
const { readTranscript } = await import(
  pathToFileURL(join(baseline, 'src/session/transcript/TranscriptReader.ts')).href
);
const cases = [
  // Replacement, PUA, one lone high surrogate and one valid pair.
  { id: 'lone-high-among-valid-code-points', values: ['�', '', '\ud800', '𐀀'] },
  // Distinct lone code units, which ICU4X would all fold to U+FFFD.
  { id: 'distinct-lone-code-units', values: ['\ud800', '\udc00', '\ud801'] },
];
const scratch = mkdtempSync(join(tmpdir(), 'pdrs-transcript-collation-'));
const inputs = [];
const results = [];
for (const [index, testCase] of cases.entries()) {
  const lines = testCase.values.map((createdAt, position) =>
    JSON.stringify({
      type: 'user',
      sessionId: 's',
      turnId: `t${position}`,
      sequence: 1,
      createdAt,
    }),
  );
  const file = join(scratch, `${index}.jsonl`);
  writeFileSync(file, lines.join('\n') + '\n');
  inputs.push({ id: testCase.id, lines });
  const read = await readTranscript(file);
  results.push({ id: testCase.id, entries: read.entries, diagnostics: read.diagnostics });
}
const oracle = { node: process.version, icu: process.versions.icu, sourceCommit, sourceSha256, inputs, results };
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle) + '\n', { flag: 'wx' });
process.stdout.write(JSON.stringify({ node: process.version, icu: process.versions.icu, cases: results.length }) + '\n');
