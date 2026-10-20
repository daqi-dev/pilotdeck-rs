// Executable cases only. All expected values are obtained from loaded TS
// production modules; generate.mts owns their provenance and output.
import assert from 'node:assert/strict';
import { mkdir, readFile } from 'node:fs/promises';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';

export async function record(baseline: string, run: string) {
  const load = (p: string) => import(pathToFileURL(join(baseline, p)).href);
  const { CompactionEngine, buildPostCompactMessages, truncateHead, truncateHeadPreservingCheckpoint } = await load('src/context/compaction/CompactionEngine.ts');
  const { stripMultimediaFromMessages } = await load('src/context/compaction/stripMultimedia.ts');
  const { projectCompactionBudget } = await load('src/context/compaction/CompactionPort.ts');
  const { DefaultContextRuntime } = await load('src/context/DefaultContextRuntime.ts');
  const { TokenBudgetManager } = await load('src/context/budget/TokenBudgetManager.ts');
  const { createDurableContextRuntime } = await load('src/agent/modules/context/durableContextRuntime.ts');
  const { AgentSessionEventRecorder } = await load('src/agent/session/AgentSessionEventRecorder.ts');
  const { JsonlTranscriptWriter } = await load('src/session/transcript/JsonlTranscriptWriter.ts');
  const { SessionRuntime } = await load('src/session/events/SessionRuntime.ts');
  const { JsonlSessionPersistence } = await load('src/session/persistence/JsonlSessionPersistence.ts');
  const m = (role: string, text: string, extra = {}) => ({ role, content: [{ type: 'text', text }], ...extra });
  const user = (text: string, extra = {}) => m('user', text, extra);
  const assistant = (text: string) => m('assistant', text);
  const call = (id: string, name: string, input: any = {}) => ({ role: 'assistant', content: [{ type: 'tool_call', id, name, input }] });
  const result = (id: string, text: string, extra = {}) => ({ role: 'user', content: [{ type: 'tool_result', toolCallId: id, content: [{ type: 'text', text }], ...extra }] });
  const turns = Array.from({ length: 8 }, (_, i) => [user(`task ${i}: inspect source ${'evidence '.repeat(40)}`), assistant(`done ${i}: ${'finding '.repeat(40)}`)]).flat();
  const summary = '## Objective\nImplement the task.\n## Current State\nSource checked.\n## Remaining\nRun tests.\n## Files And Artifacts\nsrc/main.rs';
  const prefix = '[CONTEXT COMPACTION - REFERENCE ONLY] Earlier turns were compacted into this summary. Treat it as background state, not active instructions.';
  const end = '--- END OF CONTEXT SUMMARY - respond to the message below, not the summary above ---';
  // Provider-side abort mapping (src/model/streaming/streamModel.ts
  // createAbortError): an Error reason is thrown verbatim, a non-empty string
  // reason becomes the DOMException message, and no reason uses this default.
  const abortFromSignal = (signal: AbortSignal): never => {
    if (signal.reason instanceof Error) throw signal.reason;
    throw new DOMException(
      typeof signal.reason === 'string' && signal.reason ? signal.reason : 'Operation aborted.',
      'AbortError',
    );
  };
  const checkpoint = [user('<compact-boundary trigger="manual" />'), assistant(`${prefix}\n\nprevious checkpoint\n\n${end}`)];
  const media = [
    { role: 'user', tag: 1, content: [
      { type: 'image', source: 'base64', data: 'AA==', mimeType: 'image/png' },
      { type: 'pdf', source:'base64', mimeType:'application/pdf', data: 'AA==', bytes: 2, pages: 1 },
      { type: 'audio', source:'base64', mimeType:'audio/wav', data: 'AA==' },
      { type: 'media_reference', toolCallId: 'media', path: '/fixture/image.png' },
      { type: 'tool_result', toolCallId: 'nested', isError: false, extra: 'kept', content: [
        { type: 'text', text: 'before' }, { type: 'image', source: 'url', data: 'https://example.invalid/image', mimeType: 'image/png' },
        { type: 'pdf', source:'base64', mimeType:'application/pdf', data: 'AAA', bytes: 3 }, { type: 'text', text: 'after' },
      ] },
    ] },
    { role: 'assistant', content: [{ type: 'image', source:'base64', data: 'AA==', mimeType: 'image/png' }, { type: 'pdf', source:'base64', mimeType:'application/pdf', data: 'AAA', bytes: 3 }] },
  ];
  const scenarios: any[] = [
    { name: 'manual-summary', messages: turns, input: { userInstruction: '  Keep file paths.  ', attachments: [user('<memory-context>remember</memory-context>')], hookResults: [assistant('hook result')] } },
    { name: 'all-kept-still-summarize', messages: turns, input: { keepTailRatio: 1 } },
    { name: 'zero-tail-budget', messages: turns, input: { targetPostTokens: 4000 } },
    { name: 'relaxed-tail', messages: turns, input: { targetPostTokens: 1, protectedToolNames: null } },
    { name: 'skip-empty', messages: [] },
    { name: 'skip-short', messages: [user('hello')] },
    { name: 'rolling-checkpoints', messages: [...checkpoint, ...checkpoint, ...turns], input: { cacheReset: false } },
    { name: 'checkpoint-only', messages: checkpoint },
    { name: 'non-prefix-checkpoint', messages: [...turns, ...checkpoint] },
    { name: 'media-projection', messages: [...turns, ...media] },
    { name: 'protected-boundary', messages: [...turns.slice(0,4), user('read protected skill'), call('p','read_skill',{path:'/fixture/skill.md'}), result('p','skill facts'), ...turns.slice(4)], input: { targetPostTokens: 4000 } },
    { name: 'protected-earliest', messages: [user('read protected skill'),call('p','read_skill'),result('p','facts'), ...turns], input: { targetPostTokens: 4000 } },
    { name: 'memory-boundary-null-tools', messages: [...turns.slice(0,4), user('<memory-context>keep</memory-context>'), ...turns.slice(4)], input: { targetPostTokens: 4000, protectedToolNames: null } },
    { name: 'tool-pair-tail', messages: [...turns, call('unpaired','exec'), result('orphan','discard'), call('ok','exec'),result('ok','keep')], input: { targetPostTokens: 4000 } },
    { name: 'request-anchor', messages: [user('single task'), ...Array.from({length:8},(_,i)=>[call(`c${i}`,'exec'),result(`c${i}`,`result ${i} ${'text '.repeat(90)}`)]).flat()], input: { targetPostTokens: 4000 } },
    { name: 'large-duplicate-results', messages: [...turns,call('a','exec',{long:'🦀'.repeat(1500),deep:{a:{b:{c:{d:'stop'}}}},array:Array.from({length:25},(_,i)=>i)}),result('a','large '.repeat(700)),call('b','exec'),result('b','large '.repeat(700),{isError:true})], input: { targetPostTokens: 4000 } },
    { name: 'large-whitespace-result', messages: [...turns,call('a','exec'),result('a','x'+ ' '.repeat(3000)+'y')], input: { targetPostTokens: 4000 } },
    { name: 'protected-anchors-disabled', messages: [...turns,user('read skill r12-plan and kdoc_help'),call('p','read_skill',{path:'skill-guide',long:'x'.repeat(2500)}),result('p','read kchunk_body and document-guide')], input: { protectedToolNames:null,targetPostTokens:4000 } },
    { name: 'reference-anchors', messages: [...turns,user('file ref'),call('f','read_file',{path:'file-anchor'}),{role:'user',content:[{type:'tool_result_reference',toolCallId:'f',path:'/p/'+ 'x'.repeat(800),readFilePath:'/read/'+ 'r'.repeat(800),originalBytes:50000,hasMore:true,mimeType:'text/plain',reason:'why '.repeat(400),preview:'kdoc_body '+ 'p'.repeat(1600),isError:false}]}], input:{protectedToolNames:null,targetPostTokens:4000} },
    { name: 'anchor-limit', messages: [...turns,...Array.from({length:15},(_,i)=>[user(`skill ${i}`),call(`p${i}`,'read_skill',{path:`skill-item-${i}`}),result(`p${i}`,'facts')]).flat()],input:{protectedToolNames:null,targetPostTokens:4000} },
    { name: 'anchor-character-limit', messages: [...turns,...Array.from({length:12},(_,i)=>[user('q'.repeat(1200)),call(`p${i}`,'read_skill',{long:'v'.repeat(2100)}),result(`p${i}`,'r'.repeat(1400))]).flat()],input:{protectedToolNames:null,targetPostTokens:4000} },
    { name: 'weak-summary', messages: turns, reply:'bare summary' },
    { name: 'redaction-envelope', messages: turns, reply:`${prefix}\n\n## Objective\nsecret=abc; ghp_abcdefghijk sk-abcdefgh xoxb-secret-value password:hello\n## Current State\nok\n## Remaining\nok\n## Files And Artifacts\nok\n${end}` },
    { name: 'utf16-summary', messages: turns, reply:'\uFEFF## Objective\n🦀\ud800\n## Current State\nok\n## Remaining\nok\n## Files And Artifacts\nok\uFEFF' },
    { name: 'custom-prompt-limit',messages:turns,options:{systemPrompt:'  Custom base.  ',maxOutputTokens:50},input:{maxOutputTokens:77.5,targetPostTokens:500.2} },
    { name: 'empty-summary', messages: turns, reply:' \uFEFF ' },
    { name: 'length-failure', messages: turns, finishReason:'length' },
    { name: 'filter-failure', messages: turns, finishReason:'content_filter' },
    { name: 'error-event', messages: turns, mode:'error' },
    { name: 'iterator-failure', messages: turns, mode:'throw' },
    { name: 'timeout', messages: turns, mode:'timeout' },
    { name: 'cooldown-and-expiry', messages: turns, modes:['error','ok','ok'], offsets:[0,1000,60001] },
    { name: 'abort-default', messages: turns, abort: { when: 'before' } },
    { name: 'abort-explicit-reason', messages: turns, abort: { when: 'before', reason: 'custom abort reason' } },
    { name: 'abort-empty-reason', messages: turns, abort: { when: 'before', reason: '' } },
    { name: 'abort-mid-stream', messages: turns, abort: { when: 'during' } },
    { name: 'abort-cooldown', messages: turns, abort: [{ when: 'before' }, { when: 'before', reason: 'second abort reason' }], offsets: [0, 1000] },
    { name: 'empty-protection',messages: [...turns, user('skill'),call('p','read_skill'),result('p','facts')],options:{protectedToolNames:[]},input:{protectedToolNames:null,targetPostTokens:4000} },
    { name: 'heading-newlines',messages:turns,reply:'## Objective\rA\u2028## Current State\u2029B\r\n## Remaining\nC\n## Files And Artifacts\nD' },
    { name: 'surrogate-dedup',messages:[...turns,call('a','exec'),result('a','x'.repeat(2001)+'\ud800'),call('b','exec'),result('b','x'.repeat(2001)+'\udc00')] },
    { name: 'ordered-input-keys',messages:[...turns,call('a','exec',Object.fromEntries([...Array.from({length:20},(_,i)=>[`key${i}`,i]),['0',20]])),result('a','done')], orderedInputEntries:[...Array.from({length:20},(_,i)=>[`key${i}`,i]),['0',20]] },
    { name: 'prototype-setter',messages:[...turns,call('a','exec',JSON.parse('{"__proto__":{"marker":"prototype"},"safe":2}')),result('a','done')] },
    { name: 'ascii-regex',messages:[...turns,call('a','ſkill',{ref:'r١٢-foo'}),result('a','kdoc_abc'),call('b','read_file',{ref:'r١٢-bar'}),result('b','none')],reply:summary+'\napi_Key=abcd' },
    { name: 'source-heading-body',messages:[...turns,call('a','read_file',{ref:'file-anchor'}),result('a','done')],reply:summary+'\n\n## Source Anchors\n\nKeep this spacing.' },
    { name: 'empty-error-string',messages:turns,mode:'error',errorMessage:'' },
    { name: 'injected-fractional-accounting',messages:[...turns,user('new task')],estimateScale:2.5 },
  ];
  assert.equal(scenarios.length, 46);
  const engineCases = [];
  let now = 1_000_000;
  let activeAbort: any;
  let controller: AbortController | undefined;
  const realNow = Date.now;
  Date.now = () => now;
  try {
    for (const scenario of scenarios) {
      let callIndex = 0, pass = 0;
      const requests: any[] = [], events: any[] = [], lifecycle: any[] = [];
      const model = { async *stream(request: any, signal?: AbortSignal) {
        if (activeAbort?.when === 'before') abortFromSignal(signal!);
        requests.push(structuredClone(request));
        const mode = scenario.modes?.[pass] ?? scenario.mode ?? 'ok';
        if (mode === 'throw') throw new Error('fixture iterator failure');
        if (mode === 'timeout') {
          await new Promise((_, reject) => setTimeout(() => reject(new Error('fixture timeout')),1));
        }
        if (activeAbort?.when === 'during') {
          yield { type:'thinking_delta',text:'ignored' };
          yield { type:'text_delta',text:'partial' };
          controller!.abort(activeAbort.reason);
          abortFromSignal(signal!);
        }
        if (mode === 'error') { yield { type:'error',error:{message:scenario.errorMessage ?? 'fixture model failure'} }; return; }
        const value = scenario.reply ?? summary;
        yield { type:'thinking_delta',text:'ignored' };
        yield { type:'text_delta',text:value.slice(0,15) };
        yield { type:'text_delta',text:value.slice(15) };
        yield { type:'usage',usage:{inputTokens:101,outputTokens:22,cacheReadTokens:3,cacheWriteTokens:4} };
        yield { type:'message_end',finishReason:scenario.finishReason ?? 'stop' };
      }};
      const engine = new CompactionEngine({
        model,provider:'fixture',model_:'summary-model',...scenario.options,
        tokenAccounting:scenario.estimateScale===undefined?undefined:{estimateMessages:(messages:any[])=>messages.length*scenario.estimateScale},
        uuid:()=>`compact-${++callIndex}`,
        lifecycle:{dispatch:async (event:any)=>{lifecycle.push(structuredClone(event));}},
        eventEmitter:(event:any)=>events.push(structuredClone(event)),
      });
      const passes = [];
      for (const [index, offset] of (scenario.offsets ?? [0]).entries()) {
        now = 1_000_000 + offset;
        activeAbort = Array.isArray(scenario.abort) ? scenario.abort[index] : scenario.abort;
        controller = activeAbort ? new AbortController() : undefined;
        if (activeAbort?.when === 'before') controller!.abort(activeAbort.reason);
        const input: any = {trigger:'manual',messages:scenario.messages,sessionId:'s1',turnId:'t1',...scenario.input};
        if (controller) input.signal = controller.signal;
        const output = await engine.run(input);
        passes.push({result:output,postMessages:buildPostCompactMessages(output)});
        pass++;
      }
      engineCases.push({ ...scenario, expected:{passes,requests,lifecycle,events} });
    }
  } finally { Date.now = realNow; }
  const truncateCases = [
    {messages:[],ratio:0.1},{messages:turns,ratio:0},{messages:turns,ratio:0.25},{messages:turns,ratio:1},
    {messages:[...checkpoint,...turns],ratio:0.05},
    {messages:[user('task'),call('x','exec'),result('x','fact'),assistant('next')],ratio:0.25},
    {messages:[...checkpoint],ratio:0.1},
  ].map(input=>({...input,expected:{raw:truncateHead(input.messages,input.ratio),checkpoint:truncateHeadPreservingCheckpoint(input.messages,input.ratio)}}));
  const projectionCases = [
    {},{manualForce:false,reservedOutputTokens:0,maxContextTokens:0},
    {trigger:'manual',manualForce:true,budgetStage:'routed',budgetRequest:{model:'x'},budgetPreparation:{tools:[]},budgetCalibration:{actualInputTokens:10}},
    {allowFallbackOnFailure:true}, {maxContextTokens:100,trigger:'reactive'}, {budgetStage:null,allowFallbackOnFailure:true},
  ].map(input=>({input,expected:projectCompactionBudget(input)}));
  const manualCases = [];
  for (const mode of ['ok','error','timeout','lifecycle-throw','no-summary','invalid-result','provider-throw','missing-messages','no-result','builder-assistant','builder-empty','abort-throw']) {
    const dir = join(run,mode); await mkdir(dir);
    const path = join(dir,'s1.jsonl');
    let ids=0, operations=0, compactIds=0;
    const runtime = new SessionRuntime({
      now:()=>new Date('2026-10-19T00:00:00.000Z'),
      uuid:()=>{const n=++ids;return `${n.toString(16).padStart(8,'0')}-0000-4000-8000-${n.toString(16).padStart(12,'0')}`;},
    });
    const persistence = new JsonlSessionPersistence({path});
    runtime.subscribe((entry:any)=>persistence.append(entry),{failureMode:'propagate',flush:()=>persistence.flush()});
    const writer = new JsonlTranscriptWriter({path,eventStore:runtime});
    const recorder = new AgentSessionEventRecorder(writer,{uuid:()=>`op-${++operations}`});
    const requests: any[]=[];
    const engine = new CompactionEngine({
      provider:'fixture',model_:'summary-model',uuid:()=>`compact-${++compactIds}`,
      lifecycle:mode==='lifecycle-throw'?{dispatch:async()=>{throw new Error('fixture lifecycle failure');}}:undefined,
      model:{async *stream(request:any){
        requests.push(structuredClone(request));
        if(mode==='timeout') await new Promise((_,reject)=>setTimeout(()=>reject(new Error('fixture timeout')),1));
        if(mode==='error'){yield {type:'error',error:{message:'fixture model failure'}};return;}
        yield {type:'text_delta',text:summary};yield {type:'message_end',finishReason:'stop'};
      }},
    });
    let port:any = {
      budget:new TokenBudgetManager(),summary:mode==='no-summary'?undefined:engine,
      buildPostCompactMessages,truncateHeadPreservingCheckpoint,
    };
    if(mode==='builder-assistant')port.buildPostCompactMessages=()=>[assistant('custom replacement')];
    if(mode==='builder-empty')port.buildPostCompactMessages=()=>[];
    if(['invalid-result','provider-throw','missing-messages','no-result'].includes(mode))port.autoCompact=async()=>{
      if(mode==='provider-throw')throw new Error('fixture provider failure');
      if(mode==='missing-messages')return {type:'compacted',tier:'full',snapshot:{},result:{summaryMessage:assistant(summary)}};
      if(mode==='no-result')return {type:'compacted',messages:turns,tier:'full',snapshot:{}};
      return {type:'compacted',messages:turns,tier:'full',snapshot:{},result:{error:'broken'}};
    };
    const abortController = new AbortController();
    if(mode==='abort-throw'){
      abortController.abort();
      port.autoCompact=async()=>{ throw abortFromSignal(abortController.signal); };
    }
    const delegate = new DefaultContextRuntime({compaction:port});
    const durable = createDurableContextRuntime(delegate,recorder);
    await recorder.startTurn('s1','t1');
    let response:any;
    const input={sessionId:'s1',turnId:'t1',messages:turns,trigger:'manual',manualForce:true,maxContextTokens:20000};
    const callInput = mode==='abort-throw'?{...input,signal:abortController.signal}:input;
    let beforePersist:string;
    try {
      const result = await durable.tryAutoCompact(callInput);
      response={result};
      await persistence.flush();
      beforePersist=await readFile(path,'utf8');
      if(result.type==='compacted'){
        const boundary={kind:'compact',subtype:'compact_boundary',compactMetadata:{compactionId:result.result.compactionId,trigger:'manual',preTokens:result.result.preTokens,postTokens:result.result.postTokens,messagesSummarized:result.result.messagesSummarized}};
        await writer.recordCompactionReplacement('s1','t1',boundary,result.messages);
        await recorder.commitDeferredCompaction('s1','t1');
        response.boundary=boundary;
      }
    } catch(error:any){response={error:error.message};await persistence.flush();beforePersist=await readFile(path,'utf8');}
    await persistence.flush();
    manualCases.push({name:mode,input,expected:{response,requests,beforePersist,transcript:await readFile(path,'utf8')}});
    delegate.dispose();
  }
  assert.equal(manualCases.length,12);
  const budgetCases = [
    {options:{},messages:media,context:20000,reserved:2000},
    {options:{perMessageOverhead:100.5,multimediaTokens:77.25,warningRatio:0.4,blockingRatio:0.5},messages:media,context:1000,reserved:100},
    {options:{perMessageOverhead:0,multimediaTokens:0},messages:turns,context:3000,reserved:0},
  ].map(input=>({...input,expected:new TokenBudgetManager(input.options).evaluate(input.messages,input.context,{reservedOutputTokens:input.reserved})}));
  return {
    engineCases, stripCases:[{input:media,expected:stripMultimediaFromMessages(media)}],
    truncateCases,projectionCases,manualCases,budgetCases,
  };
}
