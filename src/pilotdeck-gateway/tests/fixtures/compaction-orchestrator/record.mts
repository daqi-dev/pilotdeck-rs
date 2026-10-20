// Real production modules with deterministic auxiliary-model and budget ports.
import assert from 'node:assert/strict';
import { mkdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { recordLoops } from './loop.mts';

export const entries = [
  'src/context/compaction/AutoCompactionPolicy.ts',
  'src/context/compaction/CompactionOrchestrator.ts',
  'src/context/recovery/ContextOverflowRecovery.ts',
  'src/context/DefaultContextRuntime.ts',
  'src/agent/modules/context/durableContextRuntime.ts',
  'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
  'src/agent/scope/AgentHandle.ts',
  'src/session/persistence/JsonlSessionPersistence.ts',
];
export async function record(baseline: string, run: string) {
  const load = (p: string) => import(pathToFileURL(join(baseline,p)).href);
  const { AutoCompactionPolicy } = await load(entries[0]);
  const { createCompactionOrchestrator } = await load(entries[1]);
  const { ContextOverflowRecovery } = await load(entries[2]);
  const { DefaultContextRuntime } = await load(entries[3]);
  const { createDurableContextRuntime } = await load(entries[4]);
  const { AgentHandle } = await load(entries[6]);
  const { createSidecarDefaultModuleDispatcher } = await load(entries[5]);
  const { createSidecarContextRequestIdentityServices } = await load('src/agent/modules/transport/sidecarToolContext.ts');
  const { JsonlSessionPersistence } = await load(entries[7]);
  const { CompactionEngine, buildPostCompactMessages, truncateHeadPreservingCheckpoint } = await load('src/context/compaction/CompactionEngine.ts');
  const { MicroCompactionEngine } = await load('src/context/compaction/MicroCompactionEngine.ts');
  const { SnipEngine } = await load('src/context/compaction/SnipEngine.ts');
  const { TokenBudgetManager } = await load('src/context/budget/TokenBudgetManager.ts');
  const { AgentSessionEventRecorder } = await load('src/agent/session/AgentSessionEventRecorder.ts');
  const { JsonlTranscriptWriter } = await load('src/session/transcript/JsonlTranscriptWriter.ts');
  const { SessionRuntime } = await load('src/session/events/SessionRuntime.ts');
  const user = (text: string) => ({role:'user',content:[{type:'text',text}]});
  const assistant = (text: string) => ({role:'assistant',content:[{type:'text',text}]});
  const turns = Array.from({length:12},(_,i)=>[user(`task ${i} ${'evidence '.repeat(25)}`),assistant(`done ${i} ${'finding '.repeat(25)}`)]).flat();
  const tools = [
    user('inspect files'),
    ...Array.from({length:6},(_,i)=>[
      {role:'assistant',content:[{type:'tool_call',id:`c${i}`,name:'bash',input:{command:'read'}}]},
      {role:'user',content:[{type:'tool_result',toolCallId:`c${i}`,content:[{type:'text',text:`result ${i} ${'evidence '.repeat(1200)}`}]}]},
    ]).flat(),user('continue'),
  ];
  const summary = '## Objective\nImplement the task.\n## Current State\nSource checked.\n## Remaining\nRun tests.\n## Files And Artifacts\nsrc/main.rs';
  const policy = new AutoCompactionPolicy();
  const snapshot = (ratio: number) => ({tokens:ratio*10000,maxContextTokens:10000,warningRatio:.8,blockingRatio:.9,state:ratio>=.9?'blocking':ratio>=.8?'warning':'ok',ratio});
  const policyCases = ['ok','warning','blocking','unknown'].map(state=>({
    snapshot:{...snapshot(.95),state},expected:policy.evaluateSnapshot({...snapshot(.95),state}),
  }));
  const policyBudgetCases = [0,1,100,10000].map(context=>({messages:[user('hello')],context,reserved:1,
    expected:policy.evaluate([user('hello')],context,{reservedOutputTokens:1})}));
  const errors = [
    {},{code:'other'},{code:'image_too_large'},{code:'other',recoverableViaImageStrip:'yes',availableOutputTokens:1},
    {code:'prompt_too_long'},{code:'context_overflow'},{code:'other',recoverableViaCompact:true},
    {code:'other',recoverableViaCompact:1},{code:'prompt_too_long',maxContextTokens:100.9},
    {code:'prompt_too_long',maxContextTokens:.5},{code:'other',maxOutputTokens:.5},
    {code:'other',maxOutputTokens:0},{code:'other',maxOutputTokens:25.9},
    {code:'other',availableOutputTokens:.1,maxContextTokens:.5},
    {code:'other',availableOutputTokens:4095.99,maxContextTokens:8000.9},
    {code:'other',availableOutputTokens:4096.99,maxOutputTokens:20},
    {code:'other',availableOutputTokens:0,maxOutputTokens:20},
    {code:['x',null,2]},{code:'\ud800'}, {code:'prompt_too_long',maxContextTokens:'20.9'},
  ];
  const recoveryCases = [];
  for (const error of errors) for (const attempted of [false,true]) {
    const input={error,hasAttemptedCompact:attempted};
    recoveryCases.push({input,expected:new ContextOverflowRecovery().decide(input)});
  }
  recoveryCases.push({input:{error:{code:'context_overflow'},hasAttemptedCompact:true},
    options:{truncateFirstKeepRatio:.3,truncateSecondKeepRatio:.1},
    expected:new ContextOverflowRecovery({truncateFirstKeepRatio:.3,truncateSecondKeepRatio:.1}).decide({error:{code:'context_overflow'},hasAttemptedCompact:true})});
  const fallbackCases = [];
  const fallback = new DefaultContextRuntime();
  for (const input of recoveryCases.slice(0,20).map(c=>c.input)) {
    fallbackCases.push({input,expected:await fallback.recoverFromModelError(input)});
  }
  fallback.dispose();
  const malformedRecoveryCases=[];
  for(const input of [{},{error:null},null]) {
    try {new ContextOverflowRecovery().decide(input);assert.fail('expected TypeError');}
    catch(error:any){malformedRecoveryCases.push({input,expected:error.message});}
  }
  const definitions:any[] = [
    {name:'disabled',noBudget:true},
    {name:'missing-policy',noPolicy:true},
    {name:'policy-ok',ratios:[.79]},
    {name:'warning-no-micro',ratios:[.8]},
    {name:'warning-micro-no-change',ratios:[.8,.7],micro:true},
    {name:'warning-micro-rewrite',ratios:[.8,.7],micro:true,tools:true},
    {name:'blocking-full',ratios:[.9,.5]},
    {name:'blocking-micro-full',ratios:[.95,.91,.5],micro:true,tools:true},
    {name:'no-summary',ratios:[.95],noSummary:true},
    {name:'failed-summary-skips',ratios:[.95,.65],modes:['error']},
    {name:'target-not-reached',ratios:[.95,.65]},
    {name:'target-fixed-tie',ratios:[.95,.8125]},
    {name:'post-summary-snip',ratios:[.95,.7,.55],snip:true,builderMany:true},
    {name:'post-summary-snip-non-message-budget',ratios:[.95,.7,.65],snip:true,estimate:2000},
    {name:'exact-target-no-snip',ratios:[.95,.6],snip:true},
    {name:'emergency-summary',ratios:[.95,.9,.9,.5]},
    {name:'emergency-entry-recheck',ratios:[.95,.95,.5]},
    {name:'emergency-micro',ratios:[.95,.95,.95,.95,.95,.5],micro:true,tools:true},
    {name:'emergency-truncate',ratios:[.95,.95,.95,.95,.7]},
    {name:'emergency-overflow',ratios:[.95,1.05,1.05,1.05,1.05]},
    {name:'failed-emergency-overflow',ratios:[.95,1.05,1.05,1.05],modes:['error','error']},
    {name:'manual-policy-bypass',ratios:[.1,.65],manual:true,noPolicy:true,micro:true},
    {name:'manual-post-snip',ratios:[.1,.7,.55],manual:true,snip:true,builderMany:true},
    {name:'manual-emergency',ratios:[.1,.95,.95,.95,.95],manual:true},
    {name:'manual-overflow',ratios:[.1,1.05,1.05,1.05,1.05],manual:true},
    {name:'manual-not-forced',ratios:[.1],input:{trigger:'manual',manualForce:false}},
    {name:'evaluator-overrides-local-budget',ratios:[.95,.5],evaluator:true},
    {name:'provider-throws',ratios:[.95],throwSummary:true},
    {name:'emergency-nan-continues',ratios:[.95,.95,'NaN',.5],modes:['error','error']},
    {name:'emergency-custom-clone',ratios:[.95,.95,.95,.5],modes:['error','error'],cloneTruncate:true},
    {name:'recovery-then-compaction',ratios:[.95,.5],retry:true},
    {name:'recovery-second-decision',ratios:[.95,1.05,1.05,1.05,1.05],retry:true,retryFails:true},
  ];
  const orchestrationCases = [];
  for (const def of definitions) {
    const messages = def.tools ? tools : turns;
    const snapshots = (def.ratios??[.95]).map((r:any)=>snapshot(Number(r)));
    const input={sessionId:'s1',turnId:'t1',messages,maxContextTokens:10000,...(def.manual?{trigger:'manual',manualForce:true}:{}),...def.input};
    let evaluations=0,modelCalls=0,ids=0,entries=0,operations=0;
    const trace:any[] = [], requests:any[] = [], recovery:any[] = [];
    const evaluate = (messages:any[]) => {
      const result=snapshots[Math.min(evaluations++,snapshots.length-1)];
      trace.push({stage:'budget',messages:structuredClone(messages),snapshot:result});
      return structuredClone(result);
    };
    const engine = new CompactionEngine({
      provider:'fixture',model_:'summary-model',uuid:()=>`compact-${++ids}`,
      model:{async *stream(request:any) {
        requests.push(structuredClone(request));
        if ((def.modes??[])[modelCalls++] === 'error') {yield {type:'error',error:{message:'fixture summary failure'}};return;}
        yield {type:'text_delta',text:summary};yield {type:'message_end',finishReason:'stop'};
      }},
    });
    const micro=new MicroCompactionEngine(),snip=new SnipEngine();
    const port:any={
      ...(!def.noBudget?{budget:{evaluate:def.evaluator?()=>{throw Error('local budget must not run');}:evaluate,
        estimateMessagesTokens:(m:any[])=>def.estimate??new TokenBudgetManager().estimateMessagesTokens(m)}}:{}),
      ...(!def.noPolicy?{policy}:{}),
      ...(!def.noSummary?{summary:{run:async(i:any)=>{
        const {signal,...fields}=i;trace.push({stage:'summary',input:structuredClone(fields)});
        if(def.throwSummary)throw Error('fixture summary port failure');
        return engine.run(i);
      }}}:{}),
      ...(def.micro?{micro:{apply:(i:any)=>{trace.push({stage:'micro',trimToTokens:i.trimToTokens,...(i.keepLatest!==undefined?{keepLatest:i.keepLatest}:{}),...(i.protectedToolNames!==undefined?{protectedToolNames:i.protectedToolNames}:{})});return micro.apply(i);}}}:{}),
      ...(def.snip?{snip:{snip:(m:any[],o:any)=>{trace.push({stage:'snip',options:o});return snip.snip(m,o);}}}:{}),
      recovery:new ContextOverflowRecovery(),
      buildPostCompactMessages:(r:any)=>def.builderMany?[...buildPostCompactMessages(r),...turns]:buildPostCompactMessages(r),
      truncateHeadPreservingCheckpoint:(m:any[],r:number)=>{trace.push({stage:'truncate',keepRatio:r});return def.cloneTruncate?structuredClone(m):truncateHeadPreservingCheckpoint(m,r);},
    };
    const runtime=new DefaultContextRuntime({compaction:port});
    const dir=join(run,def.name);await mkdir(dir);
    const path=join(dir,'s1.jsonl');
    const events=new SessionRuntime({now:()=>new Date('2026-10-19T00:00:00.000Z'),uuid:()=>{const n=++entries;return `${n.toString(16).padStart(8,'0')}-0000-4000-8000-${n.toString(16).padStart(12,'0')}`;}});
    const persistence=new JsonlSessionPersistence({path});
    events.subscribe((entry:any)=>persistence.append(entry),{failureMode:'propagate',flush:()=>persistence.flush()});
    const writer=new JsonlTranscriptWriter({path,eventStore:events});
    const recorder=new AgentSessionEventRecorder(writer,{uuid:()=>`op-${++operations}`});
    const durable=createDurableContextRuntime(runtime,recorder);
    await recorder.startTurn('s1','t1');
    if(def.retry)recovery.push(await durable.recoverFromModelError({error:{code:'context_overflow',maxContextTokens:10000},hasAttemptedCompact:false}));
    let response:any;
    try {
      const result=await durable.tryAutoCompact({...input,...(def.evaluator?{budgetEvaluator:evaluate}:{})});
      response={result};
      await persistence.flush();
      response.beforePersist=await readFile(path,'utf8');
      if(result.type==='compacted'&&result.result){
        const r=result.result;
        const boundary={kind:'compact',subtype:'compact_boundary',compactMetadata:{compactionId:r.compactionId,trigger:input.trigger??'auto',preTokens:r.preTokens,postTokens:r.postTokens,messagesSummarized:r.messagesSummarized}};
        await writer.recordCompactionReplacement('s1','t1',boundary,result.messages);
        await recorder.commitDeferredCompaction('s1','t1');
      }
      if(def.retryFails)recovery.push(await durable.recoverFromModelError({error:{code:'context_overflow'},hasAttemptedCompact:true}));
    } catch(error:any) {response={error:error.message};await persistence.flush();response.beforePersist=await readFile(path,'utf8');}
    await persistence.flush();
    orchestrationCases.push({definition:def,input,snapshots,expected:{response,trace,requests,recovery,transcript:await readFile(path,'utf8')}});
    runtime.dispose();
  }
  // AgentHandle owns admission; the stage orchestrator has no reentry lock.
  let finish!:()=>void;
  const waiting=new Promise<void>(resolve=>{finish=resolve;});
  const handle=new AgentHandle({sessionId:'s1',pendingTurns:()=>[],compact:async()=>{await waiting;return {type:'skipped',turnId:'id',reason:'no_compactable_history',usage:{}};}} as any,{uuid:()=> 'id'});
  const first=handle.compact();let handleSecond;
  try {await handle.compact();}catch(error:any){handleSecond=error.message;}
  finish();
  const handleFirst=await first;
  const noProvider=new DefaultContextRuntime();
  const noProviderResult=await noProvider.tryAutoCompact({messages:[],maxContextTokens:12});
  noProvider.dispose();
  const identityCases=[];
  for(const nan of [false,true]) {
    const messages=[{...user('one'),metadata:{value:nan?NaN:1}}];
    const output=truncateHeadPreservingCheckpoint(messages,.1);
    identityCases.push({nan,messages,expected:{messages:output,changed:messages.length!==output.length||messages.some((m,i)=>m!==output[i])}});
  }
  const hostCases=[];
  for (const mode of ['pre_route','routed','recovery','without-budget','invalid-budget','provider-overflow']) {
    const calls:any[]=[];let pass=0,ids=0;
    const native=new DefaultContextRuntime({compaction:{
      budget:{evaluate:()=>snapshot(.01),estimateMessagesTokens:(m:any[])=>new TokenBudgetManager().estimateMessagesTokens(m)},
      policy,summary:new CompactionEngine({provider:'fixture',model_:'summary-model',uuid:()=>`compact-${++ids}`,
        model:{async *stream(){yield {type:'text_delta',text:summary};yield {type:'message_end',finishReason:'stop'};}}}),
      recovery:new ContextOverflowRecovery(),buildPostCompactMessages,truncateHeadPreservingCheckpoint,
    }});
    const context={prepareForModel:async(i:any)=>({messages:i.messages,systemPrompt:'preview',tools:[]}),
      tryAutoCompact:native.tryAutoCompact.bind(native),recoverFromModelError:native.recoverFromModelError.bind(native)};
    const request={provider:'p',model:'m',messages:[],systemPrompt:'base',tools:[]};
    const input={messages:turns,budgetProjection:{stage:['pre_route','routed','recovery'].includes(mode)?mode:'routed'},
      budgetRequest:request,budgetPreparation:{provider:'p',model:'m',tools:[]},reservedOutputTokens:0};
    const dispatcher=createSidecarDefaultModuleDispatcher({
      config:{cwd:'/pdrs-fixture/project',provider:'p',model:'m',maxContextTokens:10000},
      input:{sessionId:'s1',turnId:'t1',messages:[]},checkpoint:{},capabilityResultObserver:{},planTodoHandler:async()=>({}),
      modules:{model:{execution:{}},context:{execution:context,requestIdentity:createSidecarContextRequestIdentityServices({})},
        ...(mode==='without-budget'?{}:{budget:{evaluateRequestBudget:async(request:any,options:any)=>{
          calls.push({request,options});return mode==='invalid-budget'?{...snapshot(.95),ratio:-1}:snapshot(mode==='provider-overflow'?1.05:pass++===0?.95:.5);
        }}}),capability:{execution:{list:()=>[]},runtimeContext:{bindTurn:()=>({permissionContext:()=>({})})}}},
    } as any);
    let response;
    try {response=await dispatcher.handlers.context({payload:{operation:'try_auto_compact',input}} as any);}
    catch(error:any){response={error:error.message,code:error.code};}
    hostCases.push({mode,input,expected:{response,calls}});
    await dispatcher.dispose();native.dispose();
  }
  assert.equal(orchestrationCases.length,32);
  assert.equal(recoveryCases.length,41);
  assert.equal(fallbackCases.length,20);
  const loopCases=await recordLoops(baseline,turns,summary);
  return {policyCases,policyBudgetCases,recoveryCases,fallbackCases,malformedRecoveryCases,orchestrationCases,identityCases,hostCases,loopCases,admissionProbe:{handleSecond,handleFirst},noProviderResult};
}
