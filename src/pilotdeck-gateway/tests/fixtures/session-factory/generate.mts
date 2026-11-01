import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
import {normalize} from './normalize.mts';
import {observations} from './projections.mts';
import {fileStateTiming} from './file-state.mts';
import {followups,cancellationDisposal} from './followups.mts';
const here=path.dirname(fileURLToPath(import.meta.url));
const output=path.resolve(process.argv[2]);
const base=path.resolve(process.env.PDRS_BASELINE!);
assert.equal(process.version,'v22.22.0');
const sourceCommit=assertBaseline(base);
const sourceSha256=await baselineSourceHashes(base,JSON.parse(fs.readFileSync(path.join(here,'sources.json'),'utf8')));
fs.mkdirSync(output,{recursive:true});
fs.writeFileSync(path.join(output,'.generation-owner'),'session-factory\n',{flag:'wx'});
// Keep probe files through check.py's physical inventory/byte audit. Each
// root is allocated here before launching its child, so even child timeouts
// leave an owned root that this parent's finally can remove precisely.
const ownedProbeRoots:string[]=[];
function probeEnv(extra:Record<string,string>={}) {
 const work=fs.mkdtempSync(path.join(output,'run-'));ownedProbeRoots.push(work);
 return {...process.env,PDRS_BASELINE:base,MATRIX_ROOT:output,TSX_DISABLE_CACHE:'1',...extra,MATRIX_WORK_ROOT:work};
}
try {
process.stdout.write(execFileSync(process.execPath,['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(here,'file-snapshot-test.mts'),output],{env:{...process.env,PDRS_BASELINE:base,TSX_DISABLE_CACHE:'1'},encoding:'utf8',timeout:15000}));
const root=output;
 process.stdout.write(execFileSync(process.execPath,['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(here,'probe.mts')],{env:probeEnv(),encoding:'utf8',timeout:120000}));
process.stdout.write(execFileSync('python3',[path.join(here,'check.py'),root],{encoding:'utf8',timeout:30000}));
const cases=JSON.parse(fs.readFileSync(path.join(root,'raw/index.json'),'utf8')).results.map((row:any)=>normalize(JSON.parse(fs.readFileSync(path.join(root,'raw',row.name+'.json'),'utf8')),root)).sort((a:any,b:any)=>a.name.localeCompare(b.name,'en'));
assert.equal(cases.length,20);
const extra=await observations(base);
const fileTiming=await fileStateTiming(base,output);
const followupCases=await followups(base,output);
const supplemental=(name:string,env:Record<string,string>,cell:string)=>{
 process.stdout.write(execFileSync(process.execPath,['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(here,'probe.mts'),cell],{env:probeEnv({MATRIX_OUTPUT:name,...env}),encoding:'utf8',timeout:120000}));
 const report=JSON.parse(fs.readFileSync(path.join(output,name,cell+'.json'),'utf8'));
 assert.equal(report.status,'OBSERVED');return report;
};
const cancelledDisposal={
 router:['recreate','shutdown'].map(event=>{
  const r=supplemental('dispose-reason-'+event,{MATRIX_DISPOSE_REASON:'1'},`fence-idle-${event}-beforeload`);
  const result=r.snapshots.find((s:any)=>s.label==='result');
  const candidateReasons=r.timeline.filter((e:any)=>e.type==='handle.dispose-start'&&e.data.id===2&&e.ordinal<result.ordinal).map((e:any)=>e.data.reason);
  const candidateState=result.handles[1].state;
  assert.deepEqual(candidateReasons,event==='shutdown'?['session_router_shutdown']:[],'#1122 router candidate dispose reason');
  assert.equal(candidateState,event==='shutdown'?'disposed':'active','#1122 router candidate state');
  const row={event,candidateReasons,candidateState};console.log('PASS: #1122 real TS router candidate',JSON.stringify(row));return row;
 }),
 maintenance:await cancellationDisposal(base),
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
process.stdout.write(execFileSync(process.execPath,['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(here,'probe.mts'),...fenceNames],{env:probeEnv({MATRIX_OUTPUT:'fence-matrix'}),encoding:'utf8',timeout:120000}));
const fenceMatrix=fenceNames.map(name=>{
 const r=JSON.parse(fs.readFileSync(path.join(output,'fence-matrix',name+'.json'),'utf8'));
 assert.equal(r.status,'OBSERVED');assert.deepEqual(r.forbidden,[]);
 if(name.includes('-twice-'))assert.equal(r.timeline.find((e:any)=>e.type==='fence.old-dispose-return')?.data.publications,2,'#745 twice disposal follows final publication');
 if(name.includes('-close-'))assert(r.timeline.find((e:any)=>e.type==='fence.old-dispose-return')?.ordinal>r.timeline.find((e:any)=>e.type==='fence.first-before-close-return')?.ordinal,'#745 close disposal follows first rejection');
 const at=(label:string)=>r.snapshots.find((s:any)=>s.label===label);
 const boundary=at('fence-boundary'),event=at('fence-event');
 return {name,oldState:boundary.handles[0].state,oldInFlight:boundary.handles[0].inFlight,
  registryAtBoundary:boundary.registry.handle,registryAfterEvent:event.registry.handle,
  results:at('result').results.map((r:any)=>({label:r.label,status:r.status,...(r.error?{error:r.error}:{}),...(r.handle?{handle:r.handle}:{})})),
  oldPendingAtBoundary:boundary.results.some((r:any)=>r.label==='fence-old-turn'&&r.status==='pending')};
});
const twiceReturnOrders=['early','late'].map(order=>{
 const r=supplemental('twice-'+order,{MATRIX_TWICE_RETURN:order},'fence-idle-twice-beforeload');
 const results=r.snapshots.find((s:any)=>s.label==='result').results;
 const first=results.find((v:any)=>v.label==='fence-first'),second=results.find((v:any)=>v.label==='fence-second');
 assert.equal(first.status,'fulfilled');assert.equal(second.status,'fulfilled');
 assert.equal(first.handle,3,'#745 twice first final handle');
 assert.equal(second.handle,order==='early'?2:3,'#745 twice return order observed');
 return {order,firstHandle:first.handle,secondHandle:second.handle,returnsSameHandle:first.handle===second.handle};
});
const closeReturnOrders=['early','late'].map(order=>{
 const r=supplemental('close-'+order,{MATRIX_CLOSE_RETURN:order},'fence-idle-close-beforeload');
 const first=r.snapshots.find((s:any)=>s.label==='result').results.find((v:any)=>v.label==='fence-first');
 assert.equal(first.status,'rejected');
 assert.deepEqual(first.error,{name:'Error',message:order==='early'?'Agent is not registered: session-745':'Scoped service is not registered in gateway-agents: agent:session-745'},'#745 close return order observed');
 return {order,error:first.error};
});
const capture=supplemental('capture-after-prepare',{MATRIX_DISPOSE_PREPARE:'1'},'recreate-idle');
const captureResult=capture.results.find((r:any)=>r.label==='recreate');
assert.equal(captureResult.status,'rejected','#745 prepare disposal rejects TS recreate');
assert.deepEqual(captureResult.error,{name:'Error',message:'Session projection driver is disposed.'},'#745 prepare disposal TS error');
const prepareDisposal={status:captureResult.status,error:captureResult.error};
const memory=supplemental('memory-only',{MATRIX_MEMORY_ONLY:'1'},'recreate-idle');
const memoryOnly={injected:memory.timeline.find((e:any)=>e.type==='injected.reload').data.state,
 actual:memory.snapshots.find((s:any)=>s.label==='result').handles[1].session};
assert.notDeepEqual(memoryOnly.actual.messages,memoryOnly.injected.messages);
fs.writeFileSync(path.join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,cases,...extra,fileTiming,writerInterleavings,fenceMatrix,twiceReturnOrders,closeReturnOrders,memoryOnly,prepareDisposal,followups:followupCases,cancelledDisposal},null,2)+'\n',{flag:'wx'});
console.log('PASS: factory fixture', cases.length, 'cells and',extra.projections.length,'projection probes');

} finally {
 for(const work of ownedProbeRoots)fs.rmSync(work,{recursive:true,force:true});
}
