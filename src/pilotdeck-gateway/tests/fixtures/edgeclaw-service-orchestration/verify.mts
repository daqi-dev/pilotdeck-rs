// Re-run the pinned TS orchestration scenarios and require the Rust-produced
// observations to match both the committed oracle and a fresh TS run.
//
//   PDRS_BASELINE=/abs/PilotDeck node --import .../tsx/dist/loader.mjs \
//     verify.mts /abs/rust-observations.json
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFileSync, realpathSync} from 'node:fs';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {bindSourceTree, sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {ENTRY, collect} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.argv[2]);
const here = dirname(fileURLToPath(import.meta.url));
const oracle = JSON.parse(readFileSync(join(here, 'oracle.json'), 'utf8'));
const rust = JSON.parse(readFileSync(resolve(process.argv[2]), 'utf8'));
const baseline = resolve(process.env.PDRS_BASELINE);
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding: 'utf8'}).trim());
bindSourceTree(baseline, oracle.sourceCommit, git);
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
assert.deepEqual(
  sourceClosureSha256(baseline, oracle.sourceCommit, git, sources),
  oracle.sourceSha256,
  'baseline source closure differs from the oracle',
);
assert.equal(ENTRY, oracle.entry);

// A generic deepEqual failure reports the whole object and does not say which
// field carried the `-0` envelope, so the negative control could not tell an
// encoder regression from a temp-file or provenance error. Walk the two trees
// and fail with the exact path and both encodings instead.
function isNegativeZeroEnvelope(value: any): boolean {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).length === 2
    && value.$type === 'number' && value.value === '-0';
}
function negativeZeroMismatch(expected: any, actual: any, path: string): any {
  if (isNegativeZeroEnvelope(expected) || isNegativeZeroEnvelope(actual)) {
    const same = isNegativeZeroEnvelope(expected) && isNegativeZeroEnvelope(actual)
      && expected.$type === actual.$type && expected.value === actual.value;
    return same ? null : {path, expected, actual};
  }
  if (!expected || !actual || typeof expected !== 'object' || typeof actual !== 'object') {
    return null;
  }
  if (Array.isArray(expected) !== Array.isArray(actual)) return null;
  const keys = new Set([...Object.keys(expected), ...Object.keys(actual)]);
  for (const key of keys) {
    const child = Array.isArray(expected) ? `${path}[${key}]` : `${path}.${key}`;
    const found = negativeZeroMismatch(expected[key], actual[key], child);
    if (found) return found;
  }
  return null;
}
function requireSame(actual: any, expected: any, label: string) {
  const zero = negativeZeroMismatch(expected, actual, '$');
  if (zero) {
    throw new Error(
      `#776 negative-zero encoding mismatch at ${zero.path}: `
      + `expected ${JSON.stringify(zero.expected)} actual ${JSON.stringify(zero.actual)}`,
    );
  }
  assert.deepEqual(actual, expected, label);
}
const fresh = await collect(baseline);
requireSame(fresh.scenarios, oracle.scenarios, 'fresh TS run differs from the oracle');
requireSame(rust.scenarios, oracle.scenarios, 'Rust observations differ from real TS');
console.log(`real TS replayed ${fresh.scenarios.length} service orchestration scenarios; Rust matches`);
