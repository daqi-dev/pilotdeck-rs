import assert from 'node:assert/strict';
import {pathToFileURL} from 'node:url';
export async function lifecycleCell(spec:any, root:string) {
  let timer:any;
  try{return await Promise.race([capture(spec,root),new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(new Error(`lifecycle deadline: ${spec.id}`)),3000);})]);}
  finally{clearTimeout(timer);}
}
async function capture(spec:any, root:string) {
  const {BackgroundSubagentRuntime} = await import(pathToFileURL(`${root}/src/agent/sub/BackgroundSubagentRuntime.ts`).href);
  const runtime=new BackgroundSubagentRuntime();
  const trace:string[]=[];
  let calls=0, release!:()=>void, started!:()=>void;
  const gate=new Promise<void>(resolve=>{release=resolve;});
  const running=new Promise<void>(resolve=>{started=resolve;});
  const task=runtime.start({sessionId:'session',parentTurnId:'turn',subagentId:'child',subagentType:'test',run:async()=>{
    calls++;trace.push('run');started();
    if(spec.event==='running-uncooperative')await gate;
    if(spec.event==='settled-rejected')throw new Error('fixture child failure');
  }});
  trace.push(`initial:${task.status}`);
  const done=(runtime as any).entries.get(task.taskId).done;
  let waited=false, stopped:boolean, pendingAfterStop=false, afterStop:any=null, endedAfterStop:any;
  if(spec.event==='pending-stop') {
    stopped=runtime.stop(task.taskId);trace.push('stop');await done;
  } else if(spec.event==='running-uncooperative') {
    await running;assert.equal(task.status,'running');
    const waiting=runtime.stopAndWait(task.taskId).then((value:boolean)=>{waited=true;return value;});trace.push('stop');
    await new Promise<void>(resolve=>setImmediate(resolve));
    pendingAfterStop=!waited;trace.push('wait-pending');
    afterStop={status:task.status,ended:!!task.endedAt,error:task.error??null};endedAfterStop=task.endedAt;
    release();trace.push('release');stopped=await waiting;
  } else {
    await done;trace.push('settled');stopped=runtime.stop(task.taskId);trace.push('stop');
  }
  assert.ok(task.endedAt,'done has endedAt');
  return {...spec,observable:{calls,status:task.status,error:task.error??null,ended:!!task.endedAt,stopped,pendingAfterStop,afterStop,endedUnchanged:spec.event==='running-uncooperative'?task.endedAt===endedAfterStop:null},trace};
}
