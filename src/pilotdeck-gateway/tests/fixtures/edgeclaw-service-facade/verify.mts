// Re-run the pinned TS facade scenarios and require the Rust-produced
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
  `${root}/core/retrieval/reasoning-loop.ts`,
  `${root}/message-utils.ts`,
];
assert.deepEqual(
  sourceClosureSha256(baseline, oracle.sourceCommit, git, sources),
  oracle.sourceSha256,
  'baseline source closure differs from the oracle',
);
assert.equal(ENTRY, oracle.entry);
const fresh = await collect(baseline);
assert.deepEqual(fresh.scenarios, oracle.scenarios, 'fresh TS run differs from the oracle');
assert.deepEqual(rust.scenarios, oracle.scenarios, 'Rust observations differ from real TS');
console.log(`real TS replayed ${fresh.scenarios.length} service scenarios; Rust matches`);
