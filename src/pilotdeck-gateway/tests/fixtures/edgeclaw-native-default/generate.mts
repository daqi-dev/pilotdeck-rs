import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdirSync,writeFileSync} from 'node:fs';
import {join,resolve} from 'node:path';
import {bindBaseline} from '../_shared/generator_guard.mjs';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {ENTRY,SOURCES,NOW,RANDOM,M21,configSpecs,collect} from './observe.mts';
assert.ok(process.env.PDRS_BASELINE,'#778 PDRS_BASELINE is required');
assert.ok(process.argv[2],'#778 output directory is required');
assert.equal(process.version,'v22.22.0','#778 exact Node baseline');
const baseline=resolve(process.env.PDRS_BASELINE),commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const binding=bindBaseline({baseline,commit});
const sourceSha256=sourceClosureSha256(baseline,commit,binding.gitExecutable,SOURCES);
const out=resolve(process.argv[2]);mkdirSync(out,{recursive:true});
const observed=await collect(baseline,{exportRoot:join(out,'ts-store')});
assert.deepEqual(observed.scenarios.map(c=>c.name),M21,'#778 complete M21 denominator');
assert.deepEqual(observed.configurations.map(c=>c.name),configSpecs().map(c=>c.name),'#778 complete config denominator');
assert.deepEqual(bindBaseline({baseline,commit,gitExecutable:binding.gitExecutable}).provenance,binding.provenance,'#778 source unchanged');
const oracle={sourceCommit:binding.sourceCommit,sourceSha256,nodeVersion:process.version,entry:ENTRY,now:NOW,random:RANDOM,...observed};
const bytes=JSON.stringify(oracle,null,2)+'\n';
writeFileSync(join(out,'oracle.json'),bytes,{flag:'wx'});
console.log(`Node ${process.version}; sha256 ${createHash('sha256').update(bytes).digest('hex')}; ${observed.scenarios.length} M21 + ${observed.configurations.length} config cells`);
for(const cell of observed.scenarios)console.log(JSON.stringify(cell));
for(const cell of observed.configurations)console.log(JSON.stringify(cell));

console.log(JSON.stringify({endToEnd:observed.endToEnd}));

console.log(JSON.stringify({maxLines:observed.maxLines}));
