// Node 22, LANG=en_US.UTF-8, PDRS_BASELINE=fixed checkout, PDRS_GIT=absolute git.
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { record } from './record.mts';

assert.equal(process.versions.node.split('.')[0],'22');
assert.equal(new Intl.Collator().resolvedOptions().locale,'en-US');
assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.env.PDRS_GIT);
assert.ok(process.argv[2]);
const baseline=realpathSync(process.env.PDRS_BASELINE);
const git=realpathSync(process.env.PDRS_GIT);
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const entries=[
  'src/context/compaction/CompactionEngine.ts',
  'src/context/compaction/CompactionPort.ts',
  'src/context/compaction/stripMultimedia.ts',
  'src/context/DefaultContextRuntime.ts',
  'src/agent/modules/context/durableContextRuntime.ts',
  'src/agent/session/AgentSessionEventRecorder.ts',
  'src/session/transcript/JsonlTranscriptWriter.ts',
  'src/session/events/SessionRuntime.ts',
  'src/session/persistence/JsonlSessionPersistence.ts',
];
// This guard compares every src byte to its commit blob (including ignored
// or assume-unchanged files), not merely Git's tracked-dirty status.
const binding=bindSourceTree(baseline,commit,git);
const sourceSha256=sourceClosureSha256(baseline,commit,git,entries);
const output=resolve(process.argv[2]);
await mkdir(output,{recursive:true});
// No removal is performed by Node. The Rust regeneration test owns the
// enclosing Scratch and checks ownership before deleting its run directory.
const run=await mkdtemp(join(output,'.run-'));
const cases=await record(baseline,run);
assert.deepEqual(bindSourceTree(baseline,commit,git),binding,'baseline changed during recording');
await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit:binding.ref,sourceSha256,...cases},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({engine:cases.engineCases.length,strip:cases.stripCases.length,truncate:cases.truncateCases.length,projection:cases.projectionCases.length,manual:cases.manualCases.length}));
