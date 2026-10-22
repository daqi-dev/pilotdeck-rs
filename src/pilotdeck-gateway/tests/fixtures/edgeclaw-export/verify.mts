import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {readFileSync,realpathSync,writeFileSync,mkdirSync} from 'node:fs';
import {join,resolve,dirname} from 'node:path';
import {pathToFileURL,fileURLToPath} from 'node:url';
import {bindSourceTree,sourceClosureSha256} from '../../baseline_source_guard.mjs';
import {freezeClock,stepClock,observe} from './observe.mts';
assert.ok(process.env.PDRS_BASELINE);const root=resolve(process.argv[2]!);const output=resolve(process.argv[3]!);
const baseline=resolve(process.env.PDRS_BASELINE!);const git=realpathSync(process.env.PDRS_GIT||execFileSync('which',['git'],{encoding:'utf8'}).trim());
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';bindSourceTree(baseline,commit,git);
const source='src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const oracle=JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)),'oracle.json'),'utf8'));
assert.deepEqual(sourceClosureSha256(baseline,commit,git,[source]),oracle.sourceSha256);
process.env.HOME=join(root,'home');freezeClock();
const {MemoryRepository}=await import(pathToFileURL(join(baseline,source)).href);
// Mirror the generator: the observed phase runs on a stepping clock so the
// replay must make the same number of nowIso() reads in the same order.
stepClock();
const result=observe(MemoryRepository,root,oracle.operations);
mkdirSync(dirname(output),{recursive:true});writeFileSync(output,JSON.stringify(result,null,2)+'\n');
