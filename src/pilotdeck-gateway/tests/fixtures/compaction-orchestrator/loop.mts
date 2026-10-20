// Drive the actual owning loop. The second provider call is made by AgentLoop.
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
export async function recordLoops(baseline:string,messages:any[],summary:string) {
  const load=(p:string)=>import(pathToFileURL(join(baseline,'src',p)).href);
  const {AgentLoop}=await load('agent/loop/AgentLoop.ts');
  const {DefaultContextRuntime}=await load('context/DefaultContextRuntime.ts');
  const {AutoCompactionPolicy}=await load('context/compaction/AutoCompactionPolicy.ts');
  const {ContextOverflowRecovery}=await load('context/recovery/ContextOverflowRecovery.ts');
  const {CompactionEngine,buildPostCompactMessages,truncateHeadPreservingCheckpoint}=await load('context/compaction/CompactionEngine.ts');
  const {TokenBudgetManager}=await load('context/budget/TokenBudgetManager.ts');
  const {ToolRegistry}=await load('tool/registry/ToolRegistry.ts');
  const {createDefaultPermissionContext}=await load('permission/index.ts');
  const cases=[];
  for(const failRetry of [false,true]) {
    let mainCalls=0,summaryCalls=0,ids=0;
    const trace:any[]=[],persisted:any[]=[],events:string[]=[];
    const snapshot=()=>{const ratio=mainCalls===0?.01:summaryCalls===0?.95:.5;return {tokens:ratio*10000,maxContextTokens:10000,warningRatio:.8,blockingRatio:.9,state:ratio>=.9?'blocking':ratio>=.8?'warning':'ok',ratio};};
    const native=new DefaultContextRuntime({compaction:{
      budget:{evaluate:snapshot,estimateMessagesTokens:(m:any[])=>new TokenBudgetManager().estimateMessagesTokens(m)},
      policy:new AutoCompactionPolicy(),recovery:new ContextOverflowRecovery(),
      summary:new CompactionEngine({provider:'fixture',model_:'summary',uuid:()=>`compact-${++ids}`,
        model:{async *stream(request:any){summaryCalls++;trace.push(['summary',request.messages.length]);yield {type:'text_delta',text:summary};yield {type:'message_end',finishReason:'stop'};}}}),
      buildPostCompactMessages,truncateHeadPreservingCheckpoint,
    }});
    const context={
      prepareForModel:async(i:any)=>({messages:i.messages,systemPromptParts:[],tools:i.tools,diagnostics:[],boundaries:[]}),
      applyToolResults:async(i:any)=>({messages:i.messages,diagnostics:[]}),
      recoverFromModelError:async(i:any)=>{const r=await native.recoverFromModelError(i);trace.push(['recovery',r]);return r;},
      tryAutoCompact:async(i:any)=>{trace.push(['compact',i.budgetStage]);return native.tryAutoCompact(i);},
      captureTurn:async()=>{},
    };
    const router={
      invalidateSticky:()=>({orchestrating:false}),
      decide:async({request}:any)=>({provider:request.provider,model:request.model,scenarioType:'default',isSubagent:false,orchestrating:false,resolvedFrom:'explicit',mutations:{}}),
      materializeRequest:(decision:any,request:any)=>({...request,provider:decision.provider,model:decision.model}),
      observeUsage:()=>{},stream:async function*(){},
      execute:async function*(_decision:any,request:any){
        mainCalls++;trace.push(['model',mainCalls,request.messages.length]);
        if(mainCalls===1){yield {type:'error',error:{provider:'fixture',protocol:'openai',code:'context_overflow',message:'context length exceeded',retryable:false,recoverableViaCompact:true,maxContextTokens:10000}};return;}
        if(failRetry){yield {type:'error',error:{provider:'fixture',protocol:'openai',code:'authentication',message:'retry denied',retryable:false}};return;}
        yield {type:'message_start',role:'assistant'};yield {type:'text_delta',text:'recovered'};yield {type:'message_end',finishReason:'stop'};
      },
    };
    const loop=AgentLoop.fromDependencies({
      provider:'fixture',model:'main',cwd:'/pdrs-fixture/project',maxContextTokens:10000,maxOutputTokens:1000,permissionMode:'bypassPermissions',
      permissionContext:createDefaultPermissionContext({cwd:'/pdrs-fixture/project',mode:'bypassPermissions',canPrompt:false,bypassAvailable:true}),
    },{
      router,context,tools:{registry:new ToolRegistry(),scheduler:{async executeAll(){return [];}}},
      tokenAccounting:{evaluateRequestBudget:async()=>snapshot()},
      now:()=>new Date('2026-10-19T00:00:00.000Z'),uuid:()=>`id-${++ids}`,
    });
    const generator=loop.run({sessionId:'s1',turnId:'t1',messages,maxTurns:2,onCompactPersisted:(r:any)=>persisted.push(r)});
    let result;
    for(;;){const step=await generator.next();if(step.done){result=step.value;break;}events.push(step.value.type);if(events.length>100)throw Error('unbounded loop');}
    native.dispose();
    cases.push({failRetry,messages,expected:{mainCalls,summaryCalls,trace,events,result:result.result.type,stopReason:result.result.stopReason,persisted:persisted.length}});
  }
  return cases;
}
