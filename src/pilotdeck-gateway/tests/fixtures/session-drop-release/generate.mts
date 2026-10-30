// #1010 TS goldens: the drop side of the AgentHandle lifecycle, using the
// real AgentHandle class with a deterministic session seam (only lifecycle
// paths are touched). The cases pin the TS side of the state x event x
// timing matrix in _tmp/20261026-issue-1010/plan.md:
//   handle-drop-gc  cells 1/10  dropping every reference (GC forced through
//                              --expose-gc) runs no onDispose — AgentHandle
//                              has no deterministic destructor
//   abandoned-waiter cell 4     a dropped dispose waiter does not cancel the
//                              in-flight finishDispose
//   process-exit    cells 3/8   the TS equivalent of a vanished runtime:
//                              process.exit() skips pending async cleanup
// Claim OUT before baseline loading or child work. Existing directories are
// rejected untouched; missing ancestors are created recursively. Oracle is
// separately protected with wx against a late competing file creation.
// Only this invocation's mkdtemp roots are removed, including on failure.
import fs from 'node:fs';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import assert from 'node:assert/strict';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
const base=path.resolve(process.env.PDRS_BASELINE!);
const output=path.resolve(process.argv[2]);
assert.equal(process.version,'v22.22.0','#1010 Node version');
if(!process.argv[3]?.startsWith('--child-process-exit')){
 fs.mkdirSync(path.dirname(output),{recursive:true});
 fs.mkdirSync(output);
}
const sourceCommit=assertBaseline(base);
const {AgentHandle}=await import(`${base}/src/agent/scope/AgentHandle.ts`);
const {PluginRegistry}=await import(`${base}/src/extension/plugins/runtime/PluginRegistry.ts`);
async function bounded<T>(promise:Promise<T>,label:string):Promise<T>{
 let timer:any;
 try{return await Promise.race([promise,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(Error('#1010 TS gate timeout: '+label)),5000)})]);}
 finally{clearTimeout(timer!);}
}
const sessionStub=()=>({sessionId:'session-1010'}) as any;

// Child stages use the real AgentHandle; the onDispose seam pins a release
// or close that never resolves. The parent owns argv[4]'s unique directory.
if(process.argv[3]?.startsWith('--child-process-exit')){
 const mode=process.argv[3];
 const marker=path.join(process.argv[4]!,'marker.json');
 let onDisposeRan=false,releaseCalls=0,closeCalls=0;
 let entered!:()=>void;
 const atGate=new Promise<void>(resolve=>{entered=resolve;});
 const never=new Promise<void>(()=>{});
 const handle=new AgentHandle(sessionStub(),{onDispose:async()=>{
  onDisposeRan=true;
  if(mode==='--child-process-exit-release-pending'){entered();await never;}
  releaseCalls++;
  if(mode==='--child-process-exit-close-pending'){entered();await never;}
  closeCalls++;
 }});
 void handle.dispose('#1010 process-exit probe');
 if(mode!=='--child-process-exit')await bounded(atGate,'cleanup stage entered');
 const observations:any={stateAtExit:handle.state,onDisposeRan};
 if(mode!=='--child-process-exit')Object.assign(observations,{releaseCalls,closeCalls});
 const temporary=marker+'.tmp';
 fs.writeFileSync(temporary,JSON.stringify(observations),{flag:'wx'});
 fs.renameSync(temporary,marker);
 process.exit(7);
}

// Children still verify the entire pinned source tree via assertBaseline;
// only the parent needs to compute the recorded closure (650 git reads).
const sourceSha256=await baselineSourceHashes(base,['src/agent/scope/AgentHandle.ts','src/extension/plugins/runtime/PluginRegistry.ts']);
const workRoots:string[]=[];
const cases:any[]=[];
function record(name:string,observations:any){cases.push({name,node:process.version,observations});}

try{
// cells 1/10: drop every reference. TS has no deterministic drop cleanup.
{
 const gc=(globalThis as any).gc;
 assert.equal(typeof gc,'function','#1010 --expose-gc is required for handle-drop-gc');
 let onDisposeCalls=0;
 const stateBefore='active';
 const weak=(()=>{
  const handle=new AgentHandle(sessionStub(),{onDispose:()=>{onDisposeCalls++;}});
  assert.equal(handle.state,'active','#1010 fresh handle state');
  return new WeakRef(handle);
 })();
 // No deref inside the loop: a deref keeps its target alive to the end of
 // the current job, so a same-job gc() could never observe collection. GC
 // rounds run first; one final deref reads the outcome.
 for(let round=0;round<5;round++){
  gc();
  gc({type:'major',execution:'sync'});
  await new Promise(resolve=>setTimeout(resolve,10));
 }
 const collected=weak.deref()===undefined;
 assert.equal(collected,true,'#1010 the handle was collected after losing its reference');
 record('handle-drop-gc',{stateBefore,collected,onDisposeCalls});
 assert.equal(onDisposeCalls,0,'#1010 collection runs no onDispose — no deterministic destructor');
}

// cell 4: the dispose waiter is dropped; the in-flight finishDispose still
// completes (a JS promise is not cancelled by losing its waiter).
{
 let onDisposeCalls=0;
 const handle=new AgentHandle(sessionStub(),{onDispose:()=>{onDisposeCalls++;}});
 const stateBefore=handle.state;
 void handle.dispose('#1010 abandoned-waiter probe');
 await bounded(new Promise(resolve=>setTimeout(resolve,20)),'abandoned-waiter settle');
 record('abandoned-waiter',{stateBefore,stateAfter:handle.state,onDisposeCalls});
}

// fix-r3: real TS registry waits for the last lease; cancelling a caller's
// interest does not cancel its memoized release. The peer stays held while
// retirement enters its actual waitForLeaseChange seam.
{
 const registry=new PluginRegistry();
 let calls=0,atWait!:()=>void,atHook!:()=>void,finish!:()=>void;
 const waiting=new Promise<void>(resolve=>{atWait=resolve;});
 const hook=new Promise<void>(resolve=>{atHook=resolve;});
 const gate=new Promise<void>(resolve=>{finish=resolve;});
 const plugin={name:'lease-1010',source:'builtin',dispose:async()=>{calls++;atHook();await gate;}} as any;
 registry.replaceAll([plugin]);
 const first=registry.acquire(),last=registry.acquire();
 const original=(registry as any).waitForLeaseChange.bind(registry);
 (registry as any).waitForLeaseChange=()=>{const result=original();atWait();return result;};
 let settled=false;
 const disposal=registry.dispose().then(()=>{settled=true;});
 await bounded(waiting,'registry lease wait');
 const firstRelease=first.release();
 assert.equal(first.release(),firstRelease,'#1010 TS release promise memoized');
 assert.equal((registry as any).pluginLeaseCounts.get(plugin),1,'#1010 TS last peer remains held');
 const held={state:registry.state,settled,disposeCalls:calls};
 assert.deepEqual(held,{state:'draining',settled:false,disposeCalls:0});
 // Abandon the first waiter. Only the last release can reach the hook.
 const lastRelease=last.release();
 await bounded(hook,'last lease callback');
 const pending={settled,disposeCalls:calls};
 assert.deepEqual(pending,{settled:false,disposeCalls:1});
 finish();
 await bounded(Promise.all([disposal,firstRelease,lastRelease]),'registry released');
 assert.equal(first.release(),firstRelease,'#1010 TS completed release promise memoized');
 record('plugin-lease-last-release',{held,pending,stateAfter:registry.state,disposeCalls:calls});
}

// fix-r3: hook the real waitForNoActiveRun, then enter its ordinary waiter.
// A permanently held run keeps onDispose pending; releasing it completes once.
{
 let atWait!:()=>void,atRun!:()=>void,finishRun!:()=>void,onDisposeCalls=0;
 const waiting=new Promise<void>(resolve=>{atWait=resolve;});
 const running=new Promise<void>(resolve=>{atRun=resolve;});
 const runGate=new Promise<void>(resolve=>{finishRun=resolve;});
 const session={...sessionStub(),abort:()=>{},async *submit(){atRun();await runGate;}};
 const handle=new AgentHandle(session,{onDispose:()=>{onDisposeCalls++;}});
 const stream=handle.submit({} as any);
 const next=stream.next();
 await bounded(running,'session submit entered');
 const original=(handle as any).waitForNoActiveRun.bind(handle);
 (handle as any).waitForNoActiveRun=()=>{const result=original();atWait();return result;};
 const disposal=handle.dispose('#1010 activity gate');
 await bounded(waiting,'handle activity wait');
 assert.equal(handle.inFlight,1,'#1010 TS activity remains held at wait');
 const pending={state:handle.state,onDisposeCalls};
 assert.deepEqual(pending,{state:'draining',onDisposeCalls:0});
 finishRun();
 await bounded(next,'session submit finished');
 await bounded(disposal,'handle activity released');
 record('handle-activity-wait',{pending,stateAfter:handle.state,onDisposeCalls});
}

// A process cannot execute JS after exit; these are async cutoff probes,
// not claims that TS provides Rust's deterministic Drop or writer fence.
for(const name of ['process-exit','process-exit-release-pending','process-exit-close-pending']){
 const workRoot=fs.mkdtempSync(path.join(output,'work-'));
 workRoots.push(workRoot);
 let childError:any=null;
 try{
  execFileSync(process.execPath,[
   '--disable-warning=ExperimentalWarning',
   '--import',`${base}/node_modules/tsx/dist/loader.mjs`,
   process.argv[1]!,output,'--child-'+name,workRoot,
  ],{env:process.env,stdio:['ignore','pipe','pipe'],timeout:15000});
 }catch(error){childError=error;}
 assert.equal(childError?.status,7,'#1010 process-exit child exit code: '+JSON.stringify({
  code:childError?.code,signal:childError?.signal,status:childError?.status,
  stderr:childError?.stderr?.toString(),
 }));
 const marker=JSON.parse(fs.readFileSync(path.join(workRoot,'marker.json'),'utf8'));
 record(name,{childExit:7,...marker});
}

fs.writeFileSync(path.join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,cases},null,2)+'\n',{flag:'wx'});
}finally{
 for(const workRoot of workRoots)fs.rmSync(workRoot,{recursive:true,force:true,maxRetries:5});
}
console.log('PASS: #1010 real TS AgentHandle drop-side probes',cases.length,'cases');
console.log(JSON.stringify(cases,null,2));
