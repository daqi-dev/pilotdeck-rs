// All golden observations and SQLite bytes are produced by fixed real TS.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {mkdtempSync,mkdirSync,readFileSync,realpathSync,rmSync,writeFileSync,chmodSync,readdirSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree,sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {runConcurrent} from './concurrency.mts';
import {source,scenarios,freezeClock,open,seed,disk,observe} from './observe.mts';
assert.equal(process.version,'v22.22.0');assert.equal(Intl.DateTimeFormat().resolvedOptions().locale,'en-US');assert.ok(process.env.PDRS_BASELINE);assert.ok(process.argv[2]);
const baseline=resolve(process.env.PDRS_BASELINE!);const out=resolve(process.argv[2]!);
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git=realpathSync(process.env.PDRS_GIT||execFileSync('which',['git'],{encoding:'utf8'}).trim());
const provenance=bindSourceTree(baseline,commit,git);const sourceSha256=sourceClosureSha256(baseline,commit,git,[source]);
const scratch=mkdtempSync(join(tmpdir(),'pdrs-snapshots-'));process.env.HOME=join(scratch,'home');freezeClock();
const {MemoryRepository}=await import(pathToFileURL(join(baseline,source)).href);
function makeWritable(p:string){chmodSync(p,0o700);for(const e of readdirSync(p,{withFileTypes:true}))if(e.isDirectory())makeWritable(join(p,e.name));}
const cases=[];
for(const scenario of scenarios){const root=join(scratch,scenario.name);mkdirSync(root,{recursive:true});const r=open(MemoryRepository,root);seed(r);r.close();const seedDisk=disk(root);const database=readFileSync(join(root,'control.sqlite')).toString('base64');const observations=observe(MemoryRepository,root,scenario.ops);for(let i=0;i<scenario.ops.length;i++){const expected=(scenario.ops[i].faults??[]).length;assert.equal(observations[i].fired.length,expected,`${scenario.name} ${i} fault not hit`);}cases.push({...scenario,seedDisk,database,observations});makeWritable(root);}
const concurrent=await runConcurrent(MemoryRepository,baseline,join(scratch,'concurrent'));
const oracle={concurrent,sourceCommit:provenance.ref,sourceSha256,nodeVersion:process.version,cases};const text=JSON.stringify(oracle,null,2)+'\n';mkdirSync(out,{recursive:true});writeFileSync(join(out,'oracle.json'),text);console.log(`sha256 ${createHash('sha256').update(text).digest('hex')}; ${cases.length} scenarios; ${cases.reduce((n,c)=>n+c.ops.length,0)} observations`);rmSync(scratch,{recursive:true,force:true});
