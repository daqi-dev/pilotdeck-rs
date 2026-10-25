// Re-run the pinned TS capture-turn scenarios and require the Rust-produced
// observations to match both the committed oracle and a fresh TS run.
//
//   PDRS_BASELINE=/abs/PilotDeck node --import .../tsx/dist/loader.mjs \
//     verify.mts /abs/rust-observations.json
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { realpathSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import {
  conversions,
  runConversion,
  runManifest,
  runScenario,
  scenarios,
} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the Rust observations JSON');
const here = dirname(fileURLToPath(import.meta.url));
const oracle = JSON.parse(readFileSync(join(here, 'oracle.json'), 'utf8'));
const rust = JSON.parse(readFileSync(resolve(process.argv[2]), 'utf8'));
const baseline = resolve(process.env.PDRS_BASELINE);
const git = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
bindSourceTree(baseline, oracle.sourceCommit, git);
assert.deepEqual(
  sourceClosureSha256(baseline, oracle.sourceCommit, git, [
    'src/context/memory/MemoryResolver.ts',
    'src/context/DefaultContextRuntime.ts',
  ]),
  oracle.sourceSha256,
);

const { DefaultContextRuntime } = await import(
  pathToFileURL(join(baseline, 'src/context/DefaultContextRuntime.ts')).href
);
const { canonicalMessagesToMemoryMessages } = await import(
  pathToFileURL(join(baseline, 'src/context/memory/MemoryResolver.ts')).href
);
const { createSidecarDefaultModuleDispatcher } = await import(
  pathToFileURL(join(baseline, 'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts'))
    .href
);
const { HostToolCheckpoint } = await import(
  pathToFileURL(join(baseline, 'src/agent/modules/checkpoint/hostToolCheckpoint.ts')).href
);

const observed = [];
for (const spec of scenarios()) {
  observed.push(
    await runScenario(DefaultContextRuntime, canonicalMessagesToMemoryMessages, spec),
  );
}
const converted = conversions().map((spec) => ({
  name: spec.name,
  includeForkCarryover: spec.includeForkCarryover ?? null,
  result: runConversion(canonicalMessagesToMemoryMessages, spec),
}));
const manifest = await runManifest(
  { DefaultContextRuntime, createSidecarDefaultModuleDispatcher, HostToolCheckpoint },
  canonicalMessagesToMemoryMessages,
);

assert.deepEqual(observed, oracle.scenarios, 'fresh TS run differs from oracle scenarios');
assert.deepEqual(converted, oracle.conversions, 'fresh TS run differs from oracle conversions');
assert.deepEqual(manifest, oracle.manifest, 'fresh TS run differs from oracle manifest');
assert.deepEqual(rust.scenarios, observed, 'Rust scenarios differ from real TS');
assert.deepEqual(rust.conversions, converted, 'Rust conversions differ from real TS');
assert.deepEqual(rust.manifest, manifest, 'Rust manifest differs from real TS');
console.log(
  `real TS replayed ${observed.length} capture scenarios, ${converted.length} conversions and the manifest; Rust matches`,
);
