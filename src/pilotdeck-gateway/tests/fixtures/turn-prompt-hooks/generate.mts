// Real TurnRunner + LifecycleRuntime + HookRuntime; shell timeout is a deterministic port result.
import assert from 'node:assert/strict';
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
const root=resolve(process.env.PDRS_BASELINE!), out=resolve(process.argv[2]);
assert.equal(process.version,'v22.22.0');
const sourceCommit=assertBaseline(root);
const entries=['agent/turn/TurnRunner.ts','agent/session/AgentSessionEventRecorder.ts',
  'session/transcript/JsonlTranscriptWriter.ts','session/events/SessionRuntime.ts',
  'session/persistence/JsonlSessionPersistence.ts','extension/hooks/execution/HookRuntime.ts',
  'extension/hooks/execution/CommandHookExecutor.ts','lifecycle/runtime/LifecycleRuntime.ts'];
const old=JSON.parse(readFileSync(fileURLToPath(new URL('../turn-runner/oracle.json',import.meta.url)),'utf8'));
const sourceSha256=await baselineSourceHashes(root,[...new Set([...Object.keys(old.sourceSha256),...entries.map(p=>'src/'+p)])].sort());
const [{TurnRunner},{AgentSessionEventRecorder},{JsonlTranscriptWriter},{SessionRuntime},
  {JsonlSessionPersistence},{HookRuntime},{CommandHookExecutor},{LifecycleRuntime}]=await Promise.all(
  entries.map(p=>import(pathToFileURL(join(root,'src',p)).href)));
mkdirSync(out,{recursive:true});
writeFileSync(join(out,'.generation-owner'),'turn-prompt-hooks\n',{flag:'wx'});
const now=()=>new Date('2026-10-26T00:00:00.000Z');
const [{AgentLoop},{createSidecarAgentTurnCapabilities}]=await Promise.all(['agent/loop/AgentLoop.ts','agent/loop/AgentTurnCapabilities.ts'].map(p=>import(pathToFileURL(join(root,'src',p)).href)));
const rows=[];
for(const name of ['none','pass','blocks','context','block','throw','timeout','reject','accepted-fails','block-complete-fails','pending','abort-pending','drop-pending','abort-after-submitted','blocks-coerce','blocks-null','blocks-empty']) {
  const timer=setTimeout(()=>{console.error(`#741 prompt ${name} exceeded 5s`);process.exit(1);},5000);
  try {
    const path=join(out,name+'.jsonl');writeFileSync(path,'',{flag:'wx'});
    let ids=0, calls=0, loopCalls=0, completes=0, closes=0, returned=false, settled=false;
    let release!:()=>void, entered!:()=>void;
    const gate=new Promise<void>(r=>release=r), ready=new Promise<void>(r=>entered=r);
    const store=new SessionRuntime({now,uuid:()=>`id-${++ids}`});
    const persistence=new JsonlSessionPersistence({path});
    store.subscribe(entry=>{
      if(name==='accepted-fails'&&entry.type==='accepted_input')throw Error('injected write failure');
      if(name==='block-complete-fails'&&entry.type==='turn_result')throw Error('injected write failure');
      return persistence.append(entry);
    },{failureMode:'propagate',flush:()=>persistence.flush()});
    const writer=new JsonlTranscriptWriter({path,eventStore:store}), recorder=new AgentSessionEventRecorder(writer);
    const complete=recorder.completeTurn.bind(recorder);
    recorder.completeTurn=async r=>{completes++;await complete(r);};
    const hookInput:any[]=[], dispatches:any[]=[], hookResults:any[]=[];
    const hooks=new HookRuntime({UserPromptSubmit:[{hooks:[name==='timeout'?{type:'command',command:'probe'}:{type:'callback',name:'probe'}]}]},
      new CommandHookExecutor({async execute(input){hookInput.push(JSON.parse(input.stdin));return {stdout:'',stderr:'',exitCode:null,timedOut:true};}}));
    hooks.getCallbackExecutor().register('probe',async input=>{
      hookInput.push(input.hookInput);entered();
      if(name.endsWith('pending'))await gate;
      if(name==='throw')throw Error('injected callback');
      if(name==='block'||name==='block-complete-fails')return {type:'sync',continue:false,stopReason:'blocked'};
      if(name==='context'||name==='abort-after-submitted')return {type:'sync',specific:{hookEventName:'UserPromptSubmit',additionalContext:'extra'}};
      return {type:'sync'};
    });
    const runtime=new LifecycleRuntime(hooks), dispatch=runtime.dispatch.bind(runtime);
    runtime.dispatch=async input=>{calls++;dispatches.push({...input,signal:input.signal?'<signal>':undefined});
      if(name==='reject')throw Error('injected dispatch');
      const result=await dispatch(input);hookResults.push(result);return result;};
    let loopMessages:any[]=[], beforeRelease:any;
    const config={cwd:out,runMode:'agent'};
    const native=new AgentLoop(config,createSidecarAgentTurnCapabilities(config,{ports:{model:{async prepare(){throw Error('unexpected model');},async *stream(){throw Error('unexpected stream');}},toolExecution:{list:()=>[],executeAll:async()=>[]}},now}));
    const result={type:'success',sessionId:'s',turnId:'t',stopReason:'completed',usage:{},permissionDenials:[],turns:1,startedAt:now().toISOString(),completedAt:now().toISOString()};
    const loop={snapshotFileState:()=>({}),async *run(input){loopCalls++;loopMessages=input.messages;
      if(name==='abort-after-submitted')return yield* native.run(input);
      yield {type:'model_event',sessionId:'s',turnId:'t',event:{type:'message_start',role:'assistant'}};
      yield {type:'turn_completed',sessionId:'s',turnId:'t',result};return {result,messages:input.messages};}};
    const runner=new TurnRunner(loop,writer,undefined,now,name==='none'?undefined:runtime,
      {cwd:'/fixture',transcriptPath:'/fixture/s.jsonl',collectFileArtifacts:false},{eventRecorder:recorder});
    const rawInput=name==='blocks-empty'?{type:'blocks',content:[]}:name==='blocks-coerce'?{type:'blocks',content:[{type:'text',text:42},{type:'text',text:false},{type:'text',text:null},{type:'text',text:{}}]}:name==='blocks-null'?{type:'blocks',content:[null]}:name==='blocks'?{type:'blocks',content:[{type:'text',text:'first\ud800'},{type:'image',source:{type:'url',url:'https://example.invalid/image'}},{type:'text',text:'second'}]}:{type:'text',text:'probe\ud800'};
    const controller=new AbortController();
    const history=[{role:'assistant',content:[{type:'text',text:'history'}]}];
    const synthetic=[{role:'user',content:[{type:'text',text:'synthetic'}],metadata:{synthetic:true}}];
    const generator=runner.run({sessionId:'s',turnId:'t',input:rawInput,messages:history,syntheticMessages:synthetic,
      abortSignal:controller.signal,closeSteerMailbox:()=>{closes++;return [];}});
    const events:any[]=[];let error=null;
    try {
      if(name.endsWith('pending')) {
        events.push((await generator.next()).value,(await generator.next()).value);
        const next=generator.next().then(v=>{settled=true;events.push(v.value);});
        await ready;
        if(name==='abort-pending')controller.abort('probe');
        const stop=name==='drop-pending'?generator.return(undefined).then(()=>{returned=true;}):undefined;
        await new Promise(r=>setImmediate(r));
        beforeRelease={settled,returned,calls,loopCalls,completes};
        release();await next;await (stop??generator.return(undefined));
      } else for await(const event of generator) {events.push(event);if(name==='abort-after-submitted'&&event.type==='user_prompt_submitted')controller.abort('probe');}
    } catch(e){error=e.message;}
    if(name!=='accepted-fails')assert.equal(events[1]?.type,'input_accepted',`${name}: accepted gate`);
    if(!['reject','accepted-fails','blocks-null'].includes(name))assert.equal(events[2]?.type,'user_prompt_submitted',`${name}: submitted gate`);
    await writer.close();await runtime.dispose();
    rows.push({name,rawInput,history,synthetic,events,error,calls,loopCalls,completes,closes,loopMessages,
      dispatches,hookInput,hookResults,...(beforeRelease?{beforeRelease}:{}),transcript:readFileSync(path,'utf8')});
  } finally {clearTimeout(timer);}
}
assertBaseline(root);
const bytes=JSON.stringify({sourceCommit,sourceSha256,rows},null,2)+'\n';
writeFileSync(join(out,'oracle.json'),bytes,{flag:'wx'});console.log(bytes);
