import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
const base=path.resolve(process.env.PDRS_BASELINE!);
const output=path.resolve(process.argv[2]);
assert.equal(process.version,'v22.22.0','#1022 r2 Node version');
const sourceCommit=assertBaseline(base);
const sourceSha256=await baselineSourceHashes(base,['src/agent/scope/AgentHandle.ts']);
const {AgentHandle}=await import(`${base}/src/agent/scope/AgentHandle.ts`);
function deferred(){let resolve!:()=>void;const promise=new Promise<void>(r=>{resolve=r});return {promise,resolve};}
async function bounded<T>(promise:Promise<T>):Promise<T>{
 let timer:ReturnType<typeof setTimeout>;
 try{return await Promise.race([promise,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(Error('#1022 r2 TS gate timeout')),5000)})]);}
 finally{clearTimeout(timer!);}
}
const rows=[];
for(const name of ['success','sync-throw','async-reject','unobserved-error']){
 const entered=deferred(),release=deferred();let calls=0;
 const handle=new AgentHandle({sessionId:'session-1022'} as any,{onDispose:()=>{
  calls++;entered.resolve();
  if(name==='sync-throw')throw Error('DISPOSE_1022');
  return release.promise.then(()=>{if(name!=='success')throw Error('DISPOSE_1022')});
 }});
 const disposing=handle.dispose('first');
 // Attach a rejection observer without awaiting the original caller. A JS
 // Promise has no Rust drop/cancellation; the repeated-dispose result is real TS.
 const observe=(p:Promise<void>)=>p.then(()=>null,(e:any)=>({name:e.name,message:e.message,errors:e.errors.map((x:any)=>x.message)}));
 const first=observe(disposing);
 try{
  await bounded(entered.promise);
  const repeat=handle.dispose('retry');
  assert.strictEqual(repeat,disposing,'#1022 r2 TS disposal promise retained');
  release.resolve();
  const error=await bounded(observe(repeat));
  assert.deepEqual(error,await bounded(first),'#1022 r2 TS retry error retained');
  assert.equal(calls,1,'#1022 r2 TS disposer called once');
  assert.equal(handle.state,'disposed','#1022 r2 TS terminal state');
  if(name==='success')assert.equal(error,null,'#1022 r2 TS normal drain');
  else assert.deepEqual(error,{name:'AggregateError',message:'Failed to dispose agent handle.',errors:['DISPOSE_1022']},'#1022 r2 TS disposal error');
  rows.push({name,state:handle.state,inFlight:handle.inFlight,calls,samePromise:repeat===disposing,error});
 }finally{release.resolve();await bounded(first);}
}
fs.mkdirSync(output,{recursive:true});
fs.writeFileSync(path.join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,rows},null,2)+'\n',{flag:'wx'});
console.log('PASS: #1022 r2 real AgentHandle disposal/retry',JSON.stringify(rows));
