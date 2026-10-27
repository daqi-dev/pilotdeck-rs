// Real TurnRunner: an explicit recorder gate observes its first yield/return.
import assert from 'node:assert/strict';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
const root=resolve(process.env.PDRS_BASELINE!), out=resolve(process.argv[2]);
assert.equal(process.version,'v22.22.0');
const sourceCommit=assertBaseline(root);
const sources=['agent/turn/TurnRunner.ts','agent/turn/TurnInputProcessor.ts',
  'agent/session/AgentSessionEventRecorder.ts','session/transcript/InMemoryTranscriptWriter.ts'];
const sourceSha256=await baselineSourceHashes(root,sources.map(p=>`src/${p}`));
const [{TurnRunner},,{AgentSessionEventRecorder},{InMemoryTranscriptWriter}]=await Promise.all(
  sources.map(p=>import(pathToFileURL(join(root,'src',p)).href)));
const rows=[];
for(const name of ['start_success','start_failure','start_pending','start_drop']) {
  const timer=setTimeout(()=>{console.error(`#741 ${name} exceeded 5s`);process.exit(1);},5000);
  try {
    let release!:()=>void, entered!:()=>void;
    const gate=new Promise<void>(r=>release=r), ready=new Promise<void>(r=>entered=r);
    const writer=new InMemoryTranscriptWriter(), recorder=new AgentSessionEventRecorder(writer);
    const start=recorder.startTurn.bind(recorder);
    let starts=0, loopCalls=0, accepts=0, settled=false, returned=false;
    recorder.startTurn=async(...args:any[])=>{starts++;entered();
      if(name==='start_failure')throw new Error('injected start');
      if(name==='start_pending'||name==='start_drop')await gate;
      await start(...args);};
    const accept=writer.recordAcceptedInput.bind(writer);
    writer.recordAcceptedInput=async(...args:any[])=>{accepts++;return accept(...args);};
    const loop={snapshotFileState:()=>({}),async *run(){loopCalls++;throw new Error('unexpected loop');}};
    const runner=new TurnRunner(loop,writer,undefined,()=>new Date('2026-10-26T00:00:00Z'),undefined,
      {cwd:out,transcriptPath:'',collectFileArtifacts:false},{eventRecorder:recorder});
    const generator=runner.run({sessionId:'s',turnId:'t',messages:[],input:{type:'text',text:'probe'}});
    const first=generator.next().then(v=>{settled=true;return {event:v.value?.type,error:null};},
      e=>{settled=true;return {event:null,error:e.message};});
    await ready;
    let pendingReturn:Promise<unknown>|undefined;
    if(name==='start_drop')pendingReturn=generator.return(undefined).then(()=>{returned=true;});
    await new Promise(r=>setImmediate(r));
    const beforeRelease={settled,returned,accepts,loopCalls};
    release();
    const observed=await first;
    await (pendingReturn??generator.return(undefined));
    rows.push({name,beforeRelease,...observed,starts,accepts,loopCalls});
  } finally {clearTimeout(timer);}
}
assertBaseline(root);
mkdirSync(out,{recursive:true});
const bytes=JSON.stringify({sourceCommit,sourceSha256,rows},null,2)+'\n';
writeFileSync(join(out,'oracle.json'),bytes,{flag:'wx'});
console.log(bytes);
