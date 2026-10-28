// Real TS oracle for DefaultContextRuntime.captureTurn + MemoryResolver
// conversion + the context manifest method table.
//
//   PDRS_BASELINE=/abs/PilotDeck node --import \
//     /abs/PilotDeck/node_modules/tsx/dist/loader.mjs generate.mts /abs/output
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
import {
  conversions,
  runConversion,
  runManifest,
  runScenario,
  scenarios,
} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture output directory');
assert.equal(process.versions.node.split('.')[0], '22');
const COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const git = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const provenance = bindBaseline({ baseline: baseline, commit: COMMIT, gitExecutable: git }).provenance;
const sources = [
  'src/context/memory/MemoryResolver.ts',
  'src/context/DefaultContextRuntime.ts',
];
const sourceSha256 = sourceClosureSha256(baseline, provenance.ref, git, sources);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const { DefaultContextRuntime } = await import(
  pathToFileURL(join(baseline, sources[1])).href
);
const { canonicalMessagesToMemoryMessages } = await import(
  pathToFileURL(join(baseline, sources[0])).href
);
const { createSidecarDefaultModuleDispatcher } = await import(
  pathToFileURL(join(baseline, 'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts'))
    .href
);
const { HostToolCheckpoint } = await import(
  pathToFileURL(join(baseline, 'src/agent/modules/checkpoint/hostToolCheckpoint.ts')).href
);

const scenarioSpecs = scenarios();
const conversionSpecs = conversions();
const observed = [];
for (const spec of scenarioSpecs) {
  observed.push(
    await runScenario(DefaultContextRuntime, canonicalMessagesToMemoryMessages, spec),
  );
}
const converted = conversionSpecs.map((spec) => ({
  name: spec.name,
  includeForkCarryover: spec.includeForkCarryover ?? null,
  result: runConversion(canonicalMessagesToMemoryMessages, spec),
}));
const manifest = await runManifest(
  { DefaultContextRuntime, createSidecarDefaultModuleDispatcher, HostToolCheckpoint },
  canonicalMessagesToMemoryMessages,
);

const oracle = {
  sourceCommit: provenance.ref,
  sourceSha256,
  node: process.version,
  locale,
  scenarios: observed,
  scenarioSpecs,
  conversions: converted,
  conversionSpecs,
  manifest,
};
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), `${JSON.stringify(oracle, null, 2)}\n`, {
  flag: 'wx',
});
