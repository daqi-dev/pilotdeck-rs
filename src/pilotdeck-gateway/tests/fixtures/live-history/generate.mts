// Real fixed-baseline AgentHandle admission + AgentSession history projection.
import assert from 'node:assert/strict';
import {mkdirSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
const root=resolve(process.env.PDRS_BASELINE!), output=resolve(process.argv[2]);
const sourceCommit=assertBaseline(root);
const sources=['src/agent/scope/AgentHandle.ts','src/agent/session/AgentSession.ts'];
const sourceSha256=await baselineSourceHashes(root,sources);
const [{AgentHandle},{AgentSession}]=await Promise.all(sources.map(p=>import(pathToFileURL(join(root,p)).href)));
mkdirSync(output,{recursive:true});
async function bounded<T>(p:Promise<T>):Promise<T>{
  let timer:ReturnType<typeof setTimeout>;
  try{return await Promise.race([p,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(new Error('probe timeout')),5000);})]);}
  finally{clearTimeout(timer!);}
}
let reads=0, nested:Promise<any>|undefined, loopMessages:any[]=[];
const runner={sessionEventRecorder:{},async *run(input:any){loopMessages=input.messages;yield {type:'model_event',sessionId:'s',turnId:input.turnId};},snapshotFileState:()=>({})};
const session=new AgentSession({sessionId:'s',turnRunner:runner as any});
const handle=new AgentHandle(session);
const original=(session as any).projectedMessages.bind(session);
const matrix:any[]=[];
const rejection=async(turn:string)=>{
  try{await handle.submit({type:'text',text:'probe'},{turnId:turn}).next();return null;}
  catch(error:any){return error.message;}
};
(session as any).projectedMessages=()=>{
  reads++;
  if(reads===1){
    assert.equal(handle.inFlight,1);
    nested=rejection('nested');
    matrix.push({name:'history-owned',active:handle.inFlight,reads});
  }
  return original();
};
const first=handle.submit({type:'text',text:'first'},{turnId:'first'});
for(let i=0;i<10;i++){const event=await bounded(first.next());if(event.value?.type==='model_event')break;assert.ok(i<9 && !event.done,'first reaches runner');}
assert.equal(await bounded(nested!),'Agent handle already has an active turn.');
for(const name of ['pending','aborted']){
  if(name==='aborted')session.abort('cancel');
  assert.equal(await bounded(rejection(name)),'Agent handle already has an active turn.');
  matrix.push({name,active:handle.inFlight,reads});
}
// The predecessor never finishes naturally. return() deterministically releases it.
await bounded(first.return());
const message={role:'assistant',content:[{type:'text',text:'fresh'}]};
(session as any).state.messages=[message];
const second=handle.submit({type:'text',text:'second'},{turnId:'second'});
for(let i=0;i<10;i++){const event=await bounded(second.next());if(event.value?.type==='model_event')break;assert.ok(i<9 && !event.done,'second reaches runner');}
matrix.push({name:'released',active:handle.inFlight,reads,messages:loopMessages});
await bounded(second.return());
await bounded(handle.dispose());
assert.equal(await bounded(rejection('disposed')),'Cannot submit a turn; agent handle is disposed.');
matrix.push({name:'disposed',active:handle.inFlight,reads});
assert.deepEqual(matrix.map(r=>r.reads),[1,1,1,2,2]);
assertBaseline(root);
writeFileSync(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,matrix},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify(matrix,null,2));
