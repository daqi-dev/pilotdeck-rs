import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
import {normalize} from './normalize.mts';
import {observations} from './projections.mts';
const here=path.dirname(fileURLToPath(import.meta.url));
const output=path.resolve(process.argv[2]);
const base=path.resolve(process.env.PDRS_BASELINE!);
assert.equal(process.version,'v22.22.0');
const sourceCommit=assertBaseline(base);
const sourceSha256=await baselineSourceHashes(base,JSON.parse(fs.readFileSync(path.join(here,'sources.json'),'utf8')));
fs.mkdirSync(output,{recursive:true});
fs.writeFileSync(path.join(output,'.generation-owner'),'session-factory\n',{flag:'wx'});
const root=process.env.MATRIX_IMPORT_ROOT?path.resolve(process.env.MATRIX_IMPORT_ROOT):output;
if(!process.env.MATRIX_IMPORT_ROOT){
 process.stdout.write(execFileSync(process.execPath,['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(here,'probe.mts')],{env:{...process.env,PDRS_BASELINE:base,MATRIX_ROOT:output,TSX_DISABLE_CACHE:'1'},encoding:'utf8',timeout:120000}));
}
process.stdout.write(execFileSync('python3',[path.join(here,'check.py'),root],{encoding:'utf8',timeout:30000}));
const cases=JSON.parse(fs.readFileSync(path.join(root,'raw/index.json'),'utf8')).results.map((row:any)=>normalize(JSON.parse(fs.readFileSync(path.join(root,'raw',row.name+'.json'),'utf8')),root)).sort((a:any,b:any)=>a.name.localeCompare(b.name,'en'));
assert.equal(cases.length,20);
const extra=await observations(base);
fs.writeFileSync(path.join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,cases,...extra},null,2)+'\n',{flag:'wx'});
console.log('PASS: factory fixture', cases.length, 'cells and',extra.projections.length,'projection probes');
