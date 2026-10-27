import assert from 'node:assert/strict';
import { realpathSync } from 'node:fs';
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
import { entries, record } from './record.mts';

assert.equal(process.versions.node.split('.')[0],'22');
assert.equal(new Intl.Collator().resolvedOptions().locale,'en-US');
assert.ok(process.env.PDRS_BASELINE);
assert.ok(process.env.PDRS_GIT);
assert.ok(process.argv[2]);
const baseline=realpathSync(process.env.PDRS_BASELINE);
const git=realpathSync(process.env.PDRS_GIT);
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const binding=bindBaseline({ baseline, commit, gitExecutable: git }).provenance;
const sourceSha256=sourceClosureSha256(baseline,commit,git,entries);
const output=resolve(process.argv[2]);await mkdir(output,{recursive:true});
// Node never deletes. The enclosing Rust Scratch owns cleanup.
const run=await mkdtemp(join(output,'.run-'));
const cases=await record(baseline,run);
assert.deepEqual(bindBaseline({ baseline, commit, gitExecutable: git }).provenance,binding);
await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit:binding.ref,sourceSha256,...cases},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({orchestration:cases.orchestrationCases.length,recovery:cases.recoveryCases.length}));
