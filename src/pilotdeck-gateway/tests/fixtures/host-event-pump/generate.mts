// Execute the unchanged baseline runner/projector; only the protocol iterator is controlled.
import assert from 'node:assert/strict';
import {mock} from 'node:test';
import {createRequire,syncBuiltinESMExports} from 'node:module';
import {readFileSync,existsSync,mkdirSync,writeFileSync} from 'node:fs';
import {dirname,join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version,'v22.22.0');
const root=resolve(process.env.PDRS_BASELINE!), output=resolve(process.argv[2]);
const sourceCommit=assertBaseline(root), file='src/agent/modules/transport/agentLoopSidecarClient.ts';
const sourceSha256=await baselineSourceHashes(root,[file,'src/agent/protocol/events.ts','src/agent/stream/TurnTimeline.ts']);
const req=createRequire(join(root,'package.json')), {transformSync}=createRequire(req.resolve('tsx'))('esbuild');
const path=join(root,file);
let code=transformSync(readFileSync(path,'utf8')+'\nexport {AgentLoopSidecarRunner,SidecarTurnProtocol};', {loader:'ts',format:'esm',target:'es2022'}).code;
code=code.replace(/from (["'])(\.[^"']+)\1/g,(_:string,q:string,s:string)=>{
  let p=resolve(dirname(path),s); if(p.endsWith('.js')&&existsSync(p.slice(0,-3)+'.ts'))p=p.slice(0,-3)+'.ts';
  return `from ${q}${pathToFileURL(p).href}${q}`;
});
const {AgentLoopSidecarRunner,SidecarTurnProtocol}=await import('data:text/javascript;base64,'+Buffer.from(code).toString('base64'));
const {createAgentEventBuffer}=await import(pathToFileURL(join(root,'src/agent/protocol/events.ts')).href);
const flush=async()=>{for(let i=0;i<100;i++)await Promise.resolve();};
function gate(){let resolve!:(v?:any)=>void;const promise=new Promise<any>(r=>resolve=r);return {promise,resolve};}
const event=(type:string,extra:any={})=>({type,sessionId:'session',turnId:'turn',...extra});
const clean=(e:any)=>{const {timeline,streamBoundary,...rest}=e;return JSON.parse(JSON.stringify(rest));};
const cases:any[]=[];
for(const name of ['idle','callback','reset','heartbeat','interleave','wire','predecessor-cancel','finish','cancel','drop']) {
  mock.timers.enable({apis:['Date','setTimeout'],now:0});syncBuiltinESMExports();
  const buffer=createAgentEventBuffer(), incoming:any[]=[], trace:any[]=[], blocked=gate();
  let waiter:any, drains=0, closed=0, cancels=0, pending:any, settled=false;
  const send=(value:any,done=false)=>{const n={value,done};if(waiter){const w=waiter;waiter=undefined;w(n);}else incoming.push(n);};
  SidecarTurnProtocol.prototype.execute=function(){
    this.connection={send:(m:any)=>{if(m.method==='cancel')cancels++;},close:()=>{remove();closed++;}};
    this.streamId='stream';this.supportsCancel=true;const remove=this.linkCancellation();
    if(name==='callback')void this.moduleResponseFor({kind:'request',method:'module_call',messageId:'call',runId:'run',operationId:'operation',module:'model',payload:{}});
    return {next:()=>incoming.length?Promise.resolve(incoming.shift()):new Promise(r=>waiter=r)};
  };
  const context={permissionContext:()=>({}),toolRuntimeContext:()=>({}),executionContext:()=>({})};
  const abort=new AbortController();
  const runner=new AgentLoopSidecarRunner({config:{cwd:'/fixture',permissionMode:'default'},uuid:()=> 'id',connect:async()=>({}),
    turnComposition:()=>({forTurn:()=>({moduleHandlers:{model:async()=>{buffer.emitter(event('background'));await blocked.promise;return {};}}})}),modules:{model:{execution:{}},
      capability:{runtimeContext:{bindTurn:()=>context},execution:{list:()=>[]}},permission:{requestContext:{bindTurn:()=>context}},
      context:{requestIdentity:{bindTurn:()=>({contextIdentity:()=>({})})}},event:{emit:buffer.emitter,drain:()=>{drains++;return buffer.drain();}}}});
  const iterator=runner.run({sessionId:'session',turnId:'turn',messages:[],execution:{runId:'run',operationId:'operation'},abortSignal:abort.signal,
    onAgentStatusMessage:()=>blocked.promise});
  const next=async()=>{settled=false;pending=iterator.next().then((n:any)=>{settled=true;if(!n.done)trace.push({at:Date.now(),event:clean(n.value)});return n;});await flush();};
  const tick=async(ms:number)=>{while(ms>0){const step=Math.min(500,ms);mock.timers.tick(step);ms-=step;await flush();}};
  const emit=(type:string,extra:any={})=>buffer.emitter(event(type,extra));
  await next();
  if(name==='idle'||name==='callback') {
    if(name==='idle')emit('background');await tick(499);assert.equal(settled,false);if(name==='callback')emit('during_callback');await tick(1);assert.equal(settled,true);
    if(name==='callback'){await next();assert.equal(settled,true);await next();emit('after_callback');await tick(500);assert.equal(settled,true);}
  } else if(name==='reset') {
    await tick(499);send(event('turn_started'));await flush();assert.equal(settled,true);await next();emit('background');
    await tick(1);assert.equal(settled,false);await tick(499);assert.equal(settled,true);
  } else if(name==='heartbeat'||name==='interleave') {
    emit('subagent_started',{subagentId:'child',subagentType:'task'});await tick(500);await next();
    await tick(1999);assert.equal(settled,false);await tick(1);
    if(name==='heartbeat'){assert.equal(settled,true);await next();await tick(2000);assert.equal(settled,true);}
    else {
      await next();await tick(1500);emit('pre_tool_execute',{sessionId:'session::sub::child',toolCallId:'tool',toolName:'read'});
      await tick(500);await next();assert.equal(settled,true);await next();await tick(2000);assert.equal(settled,true);
      await next();await tick(1500);emit('post_tool_execute',{sessionId:'session::sub::child',toolCallId:'tool',toolName:'read',success:true});
      await tick(500);await next();await next();await tick(1500);emit('subagent_completed',{subagentId:'child'});await tick(500);
      await next();await tick(2000);assert.equal(settled,false);
    }
  } else if(name==='wire'||name==='predecessor-cancel') {
    send(event('agent_status',{status:'thinking',message:'busy'}));await flush();emit('background');await tick(1000);assert.equal(settled,false);
    if(name==='predecessor-cancel') {
      let returned=false;const returning=iterator.return(undefined).then(()=>returned=true);await flush();assert.equal(returned,false);
      blocked.resolve();await pending;await returning;trace.push({returnWaited:true});
    } else {blocked.resolve();await flush();await next();await tick(500);assert.equal(settled,true);}
  }
  if(name!=='predecessor-cancel') {
    // return() is queued behind pending next; make that next complete first.
    if(name==='cancel'){abort.abort();await flush();assert.equal(cancels,1);emit('during_cancel');await tick(500);assert.equal(settled,true);await next();}
    if(name==='drop'){emit('before_drop');await tick(500);assert.equal(settled,true);}
    if(name==='finish'){emit('final_buffer');send({result:{type:'success'},messages:[]},true);await flush();assert.equal(settled,true);await next();}
    if(!settled){send({result:{type:name==='cancel'?'aborted':'success'},messages:[]},true);await pending;}
    await iterator.return(undefined);
  }
  const before=drains;await tick(10000);assert.equal(drains,before);assert.equal(closed,1);
  cases.push({name,trace,closed,drainsAfterClose:drains-before});mock.timers.reset();
}
mkdirSync(output,{recursive:true});
writeFileSync(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,cases})+'\n',{flag:'wx'});
console.log(JSON.stringify(cases,null,2));
