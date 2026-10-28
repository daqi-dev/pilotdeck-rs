import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
import {normalize} from './normalize.mts';
import {observations} from './projections.mts';
import {fileStateTiming} from './file-state.mts';
const here=path.dirname(fileURLToPath(import.meta.url));
const output=path.resolve(process.argv[2]);
const base=path.resolve(process.env.PDRS_BASELINE!);
assert.equal(process.version,'v22.22.0');
const sourceCommit=assertBaseline(base);
const sourceSha256=await baselineSourceHashes(base,JSON.parse(fs.readFileSync(path.join(here,'sources.json'),'utf8')));
fs.mkdirSync(output,{recursive:true});
fs.writeFileSync(path.join(output,'.generation-owner'),'session-factory\n',{flag:'wx'});
const root=output;
 process.stdout.write(execFileSync(process.execPath,['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(here,'probe.mts')],{env:{...process.env,PDRS_BASELINE:base,MATRIX_ROOT:output,TSX_DISABLE_CACHE:'1'},encoding:'utf8',timeout:120000}));
process.stdout.write(execFileSync('python3',[path.join(here,'check.py'),root],{encoding:'utf8',timeout:30000}));
const cases=JSON.parse(fs.readFileSync(path.join(root,'raw/index.json'),'utf8')).results.map((row:any)=>normalize(JSON.parse(fs.readFileSync(path.join(root,'raw',row.name+'.json'),'utf8')),root)).sort((a:any,b:any)=>a.name.localeCompare(b.name,'en'));
assert.equal(cases.length,20);
const extra=await observations(base);
const fileTiming=await fileStateTiming(base,output);
const supplemental=(name:string,env:Record<string,string>,cell:string)=>{
 process.stdout.write(execFileSync(process.execPath,['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(here,'probe.mts'),cell],{env:{...process.env,PDRS_BASELINE:base,MATRIX_ROOT:output,MATRIX_OUTPUT:name,TSX_DISABLE_CACHE:'1',...env},encoding:'utf8',timeout:120000}));
 const report=JSON.parse(fs.readFileSync(path.join(output,name,cell+'.json'),'utf8'));
 assert.equal(report.status,'OBSERVED');return report;
};
const writerInterleavings=['after-publish','before-read'].map(name=>{
 const report=supplemental('writer-'+name,{MATRIX_WRITER_TIMING:name},'recreate-running');
 const error=report.results.find((r:any)=>r.label==='resume-after-both-writers').error;
 assert.equal(error.code,'sequence_not_increasing');
 return {name,tsDefect:true,error,sequences:report.commits.map((r:any)=>({storage:r.storage,sequence:r.entry.sequence})),
  replacementWriter:report.snapshots.find((s:any)=>s.label==='replacement-ready').storages[1].writer,
  entriesAtCheckpoint:report.snapshots.find((s:any)=>s.label==='replacement-ready').storages[1].entriesAtCheckpoint};
});
const fenceNames=['idle','quick','never','disposing'].flatMap(state=>['recreate','twice','close','shutdown'].flatMap(event=>['beforeload','afterload','publish'].map(timing=>`fence-${state}-${event}-${timing}`)));
process.stdout.write(execFileSync(process.execPath,['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(here,'probe.mts'),...fenceNames],{env:{...process.env,PDRS_BASELINE:base,MATRIX_ROOT:output,MATRIX_OUTPUT:'fence-matrix',TSX_DISABLE_CACHE:'1'},encoding:'utf8',timeout:120000}));
const fenceMatrix=fenceNames.map(name=>{
 const r=JSON.parse(fs.readFileSync(path.join(output,'fence-matrix',name+'.json'),'utf8'));
 assert.equal(r.status,'OBSERVED');assert.deepEqual(r.forbidden,[]);
 const at=(label:string)=>r.snapshots.find((s:any)=>s.label===label);
 const boundary=at('fence-boundary'),event=at('fence-event');
 return {name,oldState:boundary.handles[0].state,oldInFlight:boundary.handles[0].inFlight,
  registryAtBoundary:boundary.registry.handle,registryAfterEvent:event.registry.handle,
  results:at('result').results.map((r:any)=>({label:r.label,status:r.status,...(r.error?{error:r.error}:{}),...(r.handle?{handle:r.handle}:{})})),
  oldPendingAtBoundary:boundary.results.some((r:any)=>r.label==='fence-old-turn'&&r.status==='pending')};
});
const memory=supplemental('memory-only',{MATRIX_MEMORY_ONLY:'1'},'recreate-idle');
const memoryOnly={injected:memory.timeline.find((e:any)=>e.type==='injected.reload').data.state,
 actual:memory.snapshots.find((s:any)=>s.label==='result').handles[1].session};
assert.notDeepEqual(memoryOnly.actual.messages,memoryOnly.injected.messages);
fs.writeFileSync(path.join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,cases,...extra,fileTiming,writerInterleavings,fenceMatrix,memoryOnly},null,2)+'\n',{flag:'wx'});
console.log('PASS: factory fixture', cases.length, 'cells and',extra.projections.length,'projection probes');
