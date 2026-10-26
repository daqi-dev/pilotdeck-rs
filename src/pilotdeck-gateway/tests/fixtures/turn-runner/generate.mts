// Real cc7c4e6 TurnRunner + recorder; deterministic writer failure seams only.
import assert from 'node:assert/strict';
import {probeSidecar} from './sidecar.mts';
import {verifyConsumers} from './consumers.mjs';
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, join, relative, resolve} from 'node:path';
import {createRequire} from 'node:module';
import {existsSync} from 'node:fs';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
const root = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(root);
const sources = ['agent/turn/TurnRunner.ts', 'agent/turn/TurnInputProcessor.ts',
  'agent/session/AgentSessionEventRecorder.ts', 'session/transcript/JsonlTranscriptWriter.ts',
  'session/events/SessionRuntime.ts', 'session/persistence/JsonlSessionPersistence.ts',
  'agent/loop/AgentLoop.ts', 'agent/loop/AgentTurnCapabilities.ts'];
// Hash the complete local runtime import closure, including the spawned CLI.
// Transpilation removes type-only imports; package/node imports are pinned by
// the baseline installation. The CLI's env-selected factory is unset by sidecar.mts.
const ts = createRequire(join(root, 'package.json'))('typescript');
function runtimeSources(entries: string[]): string[] {
  const seen = new Set<string>();
  const visit = (path: string) => {
    if (seen.has(path)) return;
    assert.ok(path.startsWith('src/'), `source outside baseline src: ${path}`);
    seen.add(path);
    const code = ts.transpileModule(readFileSync(join(root,path),'utf8'), {
      compilerOptions: {target:ts.ScriptTarget.ESNext,module:ts.ModuleKind.ESNext}
    }).outputText;
    const tree = ts.createSourceFile(path,code,ts.ScriptTarget.Latest,true);
    const scan = (node: any) => {
      const specifier = (ts.isImportDeclaration(node) || ts.isExportDeclaration(node)) ? node.moduleSpecifier
        : ts.isCallExpression(node) && (node.expression.kind === ts.SyntaxKind.ImportKeyword || node.expression.getText(tree) === 'require')
          ? node.arguments[0] : undefined;
      if (specifier && ts.isStringLiteral(specifier) && specifier.text.startsWith('.')) {
        const base = resolve(root,dirname(path),specifier.text);
        const candidates = [base.replace(/\.js$/,'.ts'),base,`${base}.ts`,join(base,'index.ts')];
        const found = candidates.find(p=>existsSync(p) && !p.endsWith('/'));
        assert.ok(found, `unresolved import ${path}: ${specifier.text}`);
        visit(relative(root,found));
      }
      ts.forEachChild(node,scan);
    };
    scan(tree);
  };
  entries.forEach(visit);
  return [...seen].sort();
}
const sourceSha256 = await baselineSourceHashes(root, runtimeSources([...sources,
  'agent/modules/transport/agentLoopSidecarClient.ts', 'agent/modules/transport/agentLoopSidecarServer.ts',
  'agent/modules/transport/stdioAgentLoopSidecarConnection.ts', 'cli/pilotdeck-agent-loop-default-factory.ts',
  'cli/pilotdeck-agent-loop-sidecar.ts', 'permission/index.ts',
  'agent/session/createAgentSession.ts', 'session/transcript/InMemoryTranscriptWriter.ts',
  'session/events/SessionDomainValidation.ts', 'agent/protocol/errors.ts', 'status/agentStatus.ts'
].map(p => `src/${p}`)));
const load = (p: string) => import(pathToFileURL(join(root, 'src', p)).href);
const [{TurnRunner}, , {AgentSessionEventRecorder}, {JsonlTranscriptWriter}, {SessionRuntime},
  {JsonlSessionPersistence}, {AgentLoop}, {createSidecarAgentTurnCapabilities}] = await Promise.all(sources.map(load));
const now = () => new Date('2026-10-26T00:00:00.000Z');
mkdirSync(output, {recursive:true});
writeFileSync(join(output, '.generation-owner'), 'turn-runner\n', {flag:'wx'});
function writerFor(name: string, fault = '', failures = 0, errorMessage = 'injected write failure') {
  const path = join(output, `${name}.jsonl`);
  writeFileSync(path, '', {flag:'wx'});
  let ids = 0, attempts = 0;
  const runtime = new SessionRuntime({now, uuid:() => `id-${++ids === 2 && name === 'accepted-duplicate' ? 1 : ids}`});
  const persistence = new JsonlSessionPersistence({path});
  runtime.subscribe(entry => {if (entry.type === fault && ++attempts <= failures) throw new Error(errorMessage);
    return persistence.append(entry);}, {failureMode:'propagate', flush:() => persistence.flush()});
  const writer = new JsonlTranscriptWriter({path, eventStore:runtime});
  return {writer, attempts:()=>attempts, bytes:()=>{try{return readFileSync(path,'utf8');}catch{return '';}}};
}
const result = (type = 'success', turnId = 't') => ({type,sessionId:'s',turnId,stopReason:'completed',
  usage:{},permissionDenials:[],turns:1,startedAt:now().toISOString(),completedAt:now().toISOString()});
const matrix:any[] = [];
const completionCases = ['turn', 'session', 'number', 'array', 'null', 'undefined',
  'missing-session', 'missing-turn', 'turn-number', 'turn-null', 'turn-bool', 'turn-array', 'turn-object', 'turn-coercion', 'turn-undefined', 'turn-surrogate',
  'session-number', 'session-null', 'session-bool', 'session-array', 'session-object'].flatMap(kind =>
  ['idle', 'step'].map(state => `invalid-result-${kind}-${state}`));
const malformedResult = (name: string): any => {
  const kind = name.replace(/^invalid-result-/, '').replace(/-(idle|step)$/, '');
  const values: Record<string, any> = {number:42, array:[], null:null, undefined};
  if (Object.hasOwn(values, kind)) return values[kind];
  const r: any = result();
  if (kind === 'missing-session') delete r.sessionId;
  else if (kind === 'missing-turn') delete r.turnId;
  else if (kind === 'session') r.sessionId = 'other';
  else if (kind === 'turn') r.turnId = 'other';
  else {
    const [field, type] = kind.split('-');
    r[field === 'turn' ? 'turnId' : 'sessionId'] = {number:42, null:null, bool:false, array:['x',null,42], object:{key:'x'}, coercion:{toString:null}, undefined, surrogate:'wrong\ud800'}[type];
  }
  return r;
};
const catchCases = ['loop-failed-complete-once', 'loop-failed-invalid-idle', 'loop-failed-invalid-step'];
for (const name of ['success','start-fails','accepted-fails','abort-before-execute','abort-after-accepted',
  'loop-fails-twice','loop-throws','early-completed','consumer-drop','complete-once','complete-always','input-text','input-blocks','accepted-duplicate','loop-aborted-error','status-fails','loop-null-hint','input-metadata-all','input-metadata-false','input-metadata-null','input-metadata-absent','input-metadata-array','input-metadata-string','input-metadata-number','input-text-original','input-synthetic-null',...completionCases,...catchCases,'loop-missing-message','loop-undefined-message','loop-null-message']) {
  const fault = name === 'status-fails' ? 'agent_status_message' : name === 'start-fails' ? 'turn_started' : name === 'accepted-fails' ? 'accepted_input'
    : (name.startsWith('complete-') || name==='loop-failed-complete-once') ? 'turn_result' : '';
  const ctx = writerFor(name, fault, name.endsWith('complete-once') ? 1 : 99);
  const controller = new AbortController();
  let loopCalls=0, modelCalls=0, returns=0, completes=0, closes=0;
  let loopMessages:any[]=[];
  const inputCase=name.startsWith('input-');
  const invalidResult=completionCases.includes(name);
  const messageCase=name.endsWith('-message');
  const failureError=messageCase ? {code:'agent_invalid_state',...(name==='loop-missing-message'?{}:{message:name==='loop-null-message'?null:undefined})} : undefined;
  const history=inputCase?[{role:'assistant',content:[{type:'text',text:'history'}]}]:[];
  const synthetic=name==='input-synthetic-null'?null:inputCase?[{role:'user',content:[{type:'text',text:'synthetic'}],metadata:{synthetic:true}}]:[];
  const submitOptions: any = name==='input-metadata-all' ? {modelSelection:{provider:'test',model:'chosen'},permissionMode:'default',runMode:'agent',basePermissionMode:'plan',allowPlanModeTools:true,ignored:'never persisted'}
    : name==='input-metadata-false' ? {modelSelection:false,permissionMode:'',runMode:false,basePermissionMode:0,allowPlanModeTools:false}
    : name==='input-metadata-null' ? {modelSelection:null,permissionMode:null,runMode:null,basePermissionMode:null,allowPlanModeTools:null}
    : name==='input-metadata-array' ? {modelSelection:[true,null]} : name==='input-metadata-string' ? {modelSelection:'a\ud800😀'} : name==='input-metadata-number' ? {modelSelection:1}
    : name==='input-metadata-absent' ? {} : inputCase ? {allowPlanModeTools:false} : {};
  const rawInput=name==='input-blocks'?{type:'blocks',content:[{type:'text',text:'raw\ud800'},{type:'text',text:'second'}]}:{type:'text',text:name==='input-text-original'?'raw\ud800':'probe',...(name==='input-text-original'?{metadata:{permissionMode:'ignored'}}:{})};
  const recorder = new AgentSessionEventRecorder(ctx.writer);
  const complete = recorder.completeTurn.bind(recorder);
  recorder.completeTurn = async (value:any) => {completes++; await complete(value);};
  const config={cwd:output,runMode:'agent'};
  const model={async prepare({request}:any){modelCalls++; return {request};},async *stream(){modelCalls++; throw new Error('unexpected model');}};
  const native = new AgentLoop(config, createSidecarAgentTurnCapabilities(config,
    {ports:{model,toolExecution:{list:()=>[],executeAll:async()=>[]}},now}));
  const loop={snapshotFileState:()=>({}), async *run(input:any) {
    loopCalls++;loopMessages=input.messages;
    try {
      if(name.startsWith('abort-')) return yield* native.run(input);
      if((invalidResult || catchCases.includes(name)) && name.endsWith('-step')) await recorder.recordModelRequest('s','t',{request:{messages:[]}});
      if(catchCases.includes(name)) yield {type:'turn_failed',sessionId:'s',turnId:'t',error:{code:'agent_invalid_state',message:'loop failed'}};
      if(messageCase) yield {type:'turn_failed',sessionId:'s',turnId:'t',error:failureError};
      if(name==='loop-aborted-error') {const {AgentRuntimeError}=await load('agent/protocol/errors.ts');throw new AgentRuntimeError('agent_aborted','injected abort');}
      if(name==='loop-throws') throw new Error('injected loop failure');
      if(['loop-fails-twice','status-fails','loop-null-hint'].includes(name)) for(let i=0;i<2;i++) yield {type:'turn_failed',sessionId:'s',turnId:'t',error:{code:'agent_invalid_state',message:'loop failed',...(name==='loop-null-hint'?{userHint:null}:{})}};
      const r=name.startsWith('loop-failed-invalid-') ? result('error','other') : invalidResult ? malformedResult(name) : result(catchCases.includes(name) || messageCase || ['loop-fails-twice','status-fails','loop-null-hint'].includes(name)?'error':'success');
      if(name==='early-completed') yield {type:'turn_completed',sessionId:'s',turnId:'t',result:r};
      yield {type:'model_event',sessionId:'s',turnId:'t',event:{type:'message_start',role:'assistant'}};
      yield {type:'turn_completed',sessionId:'s',turnId:'t',result:r};
      return {result:r,messages:input.messages};
    } finally {returns++;}
  }};
  const runner = new TurnRunner(loop,ctx.writer,undefined,now,undefined,
    {cwd:output,transcriptPath:'',collectFileArtifacts:false},{eventRecorder:recorder});
  const events:any[]=[];
  let error: string | null=null;
  if(name==='abort-before-execute')controller.abort('probe');
  try {for await(const e of runner.run({sessionId:'s',turnId:'t',messages:history,input:rawInput,syntheticMessages:synthetic,
    ...submitOptions,
    abortSignal:controller.signal,closeSteerMailbox:()=>{closes++;return [];}})) {
      events.push(e);
      if(name==='abort-after-accepted' && e.type==='input_accepted')controller.abort('probe');
      if(name==='consumer-drop' && e.type==='model_event')break;
  }}catch(e:any){error=e.message;}
  matrix.push({name,...(invalidResult?{rawResult:malformedResult(name),...(malformedResult(name)===undefined?{rawResultUndefined:true}:{}),...(name.startsWith('invalid-result-turn-undefined-')?{rawTurnUndefined:true}:{}),openStep:name.endsWith('-step')}:{}),...(catchCases.includes(name)?{rawResult:result('error',name.startsWith('loop-failed-invalid-')?'other':'t'),openStep:name.endsWith('-step')}:{}),...(messageCase?{failureError,failureMessageUndefined:Object.hasOwn(failureError,'message') && failureError.message===undefined,statusTextOwn:events.filter(e=>e.type==='agent_status').map(e=>Object.hasOwn(e,'text')),statusTextUndefined:events.filter(e=>e.type==='agent_status').map(e=>e.text===undefined)}:{}),events,error,loopCalls,modelCalls,returns,completes,closes,writeAttempts:ctx.attempts(),transcript:ctx.bytes(),...(inputCase?{rawInput,history,synthetic,loopMessages}: {}),...(inputCase?{submitOptions}: {})});
}
// Execute the baseline's exact status normalizer; do not reimplement the oracle.
const sidecarSource=readFileSync(join(root,'src/agent/modules/transport/agentLoopSidecarClient.ts'),'utf8');
const statusSource=sidecarSource.slice(sidecarSource.indexOf('function sidecarStatusMessage('),sidecarSource.indexOf('function serializeTransportError('));
const {sidecarStatusMessage}=await import('data:text/javascript;base64,'+Buffer.from(ts.transpileModule(statusSource+'\nexport {sidecarStatusMessage};', {compilerOptions:{target:ts.ScriptTarget.ESNext,module:ts.ModuleKind.ESNext}}).outputText).toString('base64'));
const forward:any[]=[];
const agentEventSource=readFileSync(join(root,'src/agent/protocol/events.ts'),'utf8').split('export type AgentEventEmitter')[0];
const eventTypes=[...agentEventSource.matchAll(/type: "([a-z_]+)"/g)].map(m=>m[1]);
const ordinary=eventTypes.filter(t=>!['turn_started','input_accepted','turn_completed','turn_failed','file_artifacts','agent_status'].includes(t));
const ev=(type:string,extra:any={})=>({type,sessionId:'s',turnId:'t',moduleId:'loop',timeline:{epoch:0,order:7},streamBoundary:{marker:'raw'},extra:{keep:[null,'x'],startedAt:'keep',nested:{type:'turn_completed',messages:['keep'],completedAt:'keep'}},...extra});
const failure=()=>ev('turn_failed',{error:{code:'agent_invalid_state',message:'loop failed'}});
const status=(extra:any={})=>ev('agent_status',{event:'custom',...extra});
const definitions:any[]=[...ordinary.map(t=>({name:`event_${t}`,inputs:[{event:ev(t,{phase:"first"}),wire:true},{event:ev(t,{phase:"following"}),wire:true},{event:failure(),wire:true},{event:ev(t,{phase:"after_failure"}),wire:true}]})),
  {name:'artifacts',inputs:[{event:ev('file_artifacts',{artifacts:[{path:'ignored'}]}),wire:true}]},
  {name:'completed',inputs:[{event:ev('turn_completed',{result:result()}),wire:true},{event:ev('warning'),wire:true}]},
  {name:'failed_twice',inputs:[{event:failure(),wire:true},{event:failure(),wire:true}]},
  ...Object.entries({explicit:{kind:'error'},normal:{kind:'status'},turn_failed:{kind:'error',event:'turn_failed'},hidden:{kind:'error',detail:{visible:false}},
    severity:{detail:{severity:'error',message:'detail text'}},nullish:{kind:null,text:null,event:'model_request_failed',detail:{message:'detail text'}},
    text:{kind:'error',text:'explicit',detail:{message:'unused'}},text_empty:{text:''},text_false:{text:false},text_zero:{text:0},kind_false:{kind:false,event:'model_request_failed'},kind_zero:{kind:0,event:'model_request_failed'},detail_null:{detail:null},empty_kind:{kind:'',event:'model_request_failed'},numeric_message:{detail:{message:3}},
    ...Object.fromEntries(['max_budget_reached','task_budget_reached','model_request_failed','tool_call_recovery_exhausted','max_turns_reached','max_output_recovery_exhausted','empty_response','content_filter','unknown_finish_reason'].map(event=>[`implicit_${event}`,{event}]))})
    .map(([name,extra])=>({name:`status_${name}`,inputs:[{event:status(extra),wire:true},{event:failure(),wire:true}]})),
  {name:'host_status',inputs:[{event:status({kind:'error'}),wire:false},{event:failure(),wire:true}]},
  ...['host','pump'].map(source=>({name:`malformed_${source}`,source,inputs:[{event:ev('turn_failed',{error:null}),wire:false}]})),
  {name:'status_after_failed',inputs:[{event:failure(),wire:true},{event:status({kind:'error'}),wire:true},{event:failure(),wire:true}]},
  {name:'status_write_error',fault:'agent_status_message',inputs:[{event:status({kind:'error'}),wire:true}]},
  ...['null','missing'].flatMap(kind=>[false,true].map(visible=>({name:`malformed_${kind}_${visible?'visible':'first'}`,inputs:[...(visible?[{event:status({kind:'error'}),wire:true}]:[]),{event:ev('turn_failed',kind==='null'?{error:null}:{}),wire:true}]}))),
  ...['complete_once','invalid_idle','invalid_step'].map(name=>({name:`catch_${name}`,fault:name==='complete_once'?'turn_result':'',inputs:[{event:failure(),wire:true}]}))];
for(const def of definitions) {
  const ctx=writerFor(`forward-${def.name}`,def.fault??'',1,'recording write failed'), recorder=new AgentSessionEventRecorder(ctx.writer);
  const r=result(def.inputs.some((i:any)=>i.event.type==='turn_failed')?'error':'success',def.name.startsWith('catch_invalid')?'other':'t');
  const loop={async *run(input:any){
    if(def.name==='catch_invalid_step')await recorder.recordModelRequest('s','t',{request:{messages:[]}});
    for(const item of def.inputs) {
      if(item.wire && item.event.type==='agent_status')await input.onAgentStatusMessage(sidecarStatusMessage(item.event));
      yield item.event;
    }
    if(def.name!=='completed')yield {type:'turn_completed',sessionId:'s',turnId:'t',result:r};
    return {result:r,messages:input.messages};
  }};
  const runner=new TurnRunner(loop,ctx.writer,undefined,now,undefined,{cwd:output,transcriptPath:'',collectFileArtifacts:false},{eventRecorder:recorder});
  const events:any[]=[];
  for await(const event of runner.run({sessionId:'s',turnId:'t',messages:[],input:{type:'text',text:'probe'}}))events.push(event);
  forward.push({...def,result:r,events,transcript:ctx.bytes(),consumption:{test:`forward_${def.name}`,scope:def.name==='completed'?'strict guard retained; end-to-end pending pr5d':'real TurnRunner + sidecarStatusMessage, StreamHandler/LiveTurnStream replay'}});
}
// A status writer is an explicit predecessor: neither next nor return overtakes it.
for(const name of ['status_cancel','status_drop']) {
  const ctx=writerFor(`forward-${name}`), before=ctx.writer.recordAgentStatusMessage.bind(ctx.writer);
  let enter!:()=>void, release!:()=>void;
  const entered=new Promise<void>(r=>enter=r), gate=new Promise<void>(r=>release=r);
  let writes=0,closed=false;
  ctx.writer.recordAgentStatusMessage=async(...args:any[])=>{writes++;enter();await gate;return before(...args);};
  const loop={async *run(input:any){try{const event=status({kind:'error'});await input.onAgentStatusMessage(sidecarStatusMessage(event));yield event;return {result:result(),messages:input.messages};}finally{closed=true;}}};
  const runner=new TurnRunner(loop,ctx.writer,undefined,now,undefined,{cwd:output,transcriptPath:'',collectFileArtifacts:false});
  const it=runner.run({sessionId:'s',turnId:'t',messages:[],input:{type:'text',text:'probe'}});
  const timer=setTimeout(()=>{console.error(`forward ${name}: gate timeout`);process.exit(1);},20000);
  try {
    for(let i=0;i<3;i++)await it.next();
    let first=false,second=false;
    const pending=it.next().then(n=>{first=true;return n;});await entered;
    const next=(name==='status_drop'?it.return(undefined):it.next()).then(n=>{second=true;return n;});
    for(let i=0;i<100;i++)await Promise.resolve();
    assert.equal(first,false);assert.equal(second,false);assert.equal(writes,1);assert.equal(closed,false);
    release();const delivered=await pending;await next;await it.return(undefined);
    assert.equal(delivered.value.type,'agent_status');assert.equal(writes,1);assert.equal(closed,true);
    forward.push({name,pending:{first:false,second:false,writes:1,closed:false},after:{writes,closed,type:delivered.value.type},consumption:{test:`forward_${name}`,scope:'TS queued next/return; Rust cancelled borrow/drop retain blocking owner; Drop difference inherited from pr5b'}});
  }finally{clearTimeout(timer);}
}
// The real client owns callback/yield/ACK ordering; the connection only supplies frames.
const {createAgentLoopSidecarRuntimeFactory}=await load('agent/modules/transport/agentLoopSidecarClient.ts');
function ackGate(){let resolve!:(v?:any)=>void;const promise=new Promise<any>(r=>resolve=r);return {promise,resolve};}
for(const reject of [false,true]) {
  const name=reject?'status_receipt_failure':'status_receipt_success';
  const entered=ackGate(),release=ackGate(),sent:any[]=[],incoming:any[]=[];let waiter:any,closed=0,callbacks=0;
  const hostEvents=[status({event:'host-probe',kind:'error'})];
  const push=(value:any)=>{if(waiter){const r=waiter;waiter=undefined;r({value,done:false});}else incoming.push(value);};
  const binding={protocolVersion:'2.0',moduleId:'fixture',moduleInstanceId:'instance',connectionGeneration:'current',capabilitiesVersion:'v1'};
  const connection={send(m:any){sent.push(m);const reply={kind:'response',messageId:'response-'+m.messageId,inReplyTo:m.messageId,ok:true};
    if(m.method==='hello')push({...reply,...binding});
    if(m.method==='capabilities')push({...reply,...binding,payload:{capabilitiesVersion:'v1',methods:[{name:'execute',profiles:['streaming'],resumeSupport:'streaming'},{name:'resume'},{name:'ack'}]}});
    if(m.method==='execute'){push({...reply,requestId:m.requestId,streamId:'stream'});const ids={runId:m.runId,operationId:m.operationId,requestId:m.requestId};
      push({kind:'event',eventType:'agent_event',streamId:'stream',...ids,sequence:0,final:false,payload:{type:'agent_status',sessionId:'s',turnId:'t',event:'custom',kind:'error',text:'failure'}});
      push({kind:'event',eventType:'agent_event',streamId:'stream',...ids,sequence:1,final:true,outcome:'completed',payload:{result:result(),messages:[]}});
    }
  },receive(){return {[Symbol.asyncIterator](){return this;},next():Promise<any>{return incoming.length?Promise.resolve({value:incoming.shift(),done:false}):new Promise(r=>waiter=r);}};},close(){closed++;}};
  const context={permissionContext:()=>({}),toolRuntimeContext:()=>({}),executionContext:()=>({})};
  const modules={event:{drain:()=>hostEvents.splice(0)},model:{execution:{}},capability:{runtimeContext:{bindTurn:()=>context},execution:{list:()=>[]}},permission:{requestContext:{bindTurn:()=>context}},context:{requestIdentity:{bindTurn:()=>({contextIdentity:()=>({})})}}};
  const factory=createAgentLoopSidecarRuntimeFactory({connect:async()=>connection,uuid:()=> 'id',turnComposition:()=>({forTurn:()=>({moduleHandlers:{}})})});
  const runner=factory({config:{cwd:output,permissionMode:'default'},capabilities:{transport:{}},sidecarModules:modules} as any);
  const iterator=runner.run({sessionId:'s',turnId:'t',messages:[],execution:{runId:'run',operationId:'operation'},onAgentStatusMessage:async()=>{callbacks++;entered.resolve();await release.promise;if(reject)throw new Error('status write failed');}});
  const acks=()=>sent.filter(x=>x.method==='ack');
  const timer=setTimeout(()=>{console.error(`${name}: gate timeout`);process.exit(1);},5000);
  try {
    const host=(await iterator.next()).value;assert.equal(host.type,'agent_status');assert.equal(host.event,'host-probe');assert.equal(callbacks,0);
    const first=iterator.next();await entered.promise;assert.equal(callbacks,1);assert.equal(acks().length,0);release.resolve();
    if(reject){await assert.rejects(first,/status write failed/);assert.equal(acks().length,0);}
    else {assert.equal((await first).value.type,'agent_status');assert.equal(acks().length,0);assert.equal((await iterator.next()).value.type,'turn_completed');await iterator.next();assert.equal(acks().length,1);}
    writeFileSync(join(output,`${name}-raw.json`),JSON.stringify({sent,closed,hostStatusCallbacks:0,totalStatusCallbacks:callbacks},null,2)+'\n',{flag:'wx'});
    forward.push({name,reject,pendingAcks:0,acks:acks().map(a=>a.lastAppliedSequence),closed,consumption:{test:`forward_${name}`,scope:'real TS client ACK; Rust real HostClient callback/applied cursor with existing peer'}});
  }finally{clearTimeout(timer);}
}
// An iterator's asynchronous finally must finish before catch completes the turn.
{
  const name='pump_error_cleanup',ctx=writerFor(name),entered=ackGate(),release=ackGate();let closed=false,completes=0;
  const recorder=new AgentSessionEventRecorder(ctx.writer);const complete=recorder.completeTurn.bind(recorder);recorder.completeTurn=async(...args:any[])=>{assert.equal(closed,true);completes++;return complete(...args);};
  const loop={async *run(){try{throw new Error('pump failed');}finally{entered.resolve();await release.promise;closed=true;}}};
  const runner=new TurnRunner(loop,ctx.writer,undefined,now,undefined,{cwd:output,transcriptPath:'',collectFileArtifacts:false},{eventRecorder:recorder});
  const iterator=runner.run({sessionId:'s',turnId:'t',messages:[],input:{type:'text',text:'probe'}});
  const timer=setTimeout(()=>{console.error(`${name}: cleanup timeout`);process.exit(1);},5000);
  try{
    for(let i=0;i<3;i++)await iterator.next();
    let resolved=false;const pending=iterator.next().then(n=>{resolved=true;return n;});await entered.promise;
    assert.equal(resolved,false);assert.equal(completes,0);release.resolve();await pending;while(!(await iterator.next()).done){}
    assert.equal(completes,1);
    forward.push({name,pendingCompletes:0,closed,completes,consumption:{test:`forward_${name}`,scope:'real TurnRunner awaits loop finally; Rust pump joins owned execute cleanup'}});
  }finally{clearTimeout(timer);}
}
// Recorder completion is independently replayable in Rust, including retry state.
const recorderCases:any[]=[];
for(const name of ['success','error','aborted','max_turns','no-step','unstarted','step-once','result-once','result-always','new-turn','restore','restart-fails',
  ...['null','undefined','number'].flatMap(kind=>['fresh','started','completed'].map(phase=>`invalid-${kind}-${phase}`))]) {
  const fault=name==='step-once'?'step_completed':name.startsWith('result-')?'turn_result':'';
  const ctx=writerFor(`recorder-${name}`,fault,name==='result-always'?99:1);
  let recorder=new AgentSessionEventRecorder(ctx.writer);
  const operations:any[]=[];
  if(name!=='unstarted' && !name.endsWith('-fresh'))operations.push({op:'start',turn:'t'});
  if(!['no-step','unstarted'].includes(name) && !name.endsWith('-fresh'))operations.push({op:'request',turn:'t'});
  const value=result(['error','aborted','max_turns'].includes(name)?name:'success');
  if(name.startsWith('invalid-')) {
    if(name.endsWith('-completed'))operations.push({op:'complete',result:value});
    const kind=name.split('-')[1];
    operations.push({op:'invalid-complete',kind});
    // A failed/no-op malformed call must not close the step or poison retry.
    if(name.endsWith('-started'))operations.push({op:'complete',result:value});
  } else operations.push({op:'complete',result:value},{op:'complete',result:value});
  if(name==='new-turn')operations.push({op:'start',turn:'other'},{op:'complete',result:value},{op:'complete',result:result('success','other')});
  if(name==='restart-fails')operations.push({op:'start',turn:'t'},{op:'complete',result:value});
  if(name==='restore')operations.push({op:'restore'},{op:'complete',result:value});
  const outcomes:any[]=[];
  for(const op of operations) {try {
    if(op.op==='start')await recorder.startTurn('s',op.turn);
    if(op.op==='request')await recorder.recordModelRequest('s',op.turn,{request:{messages:[]}});
    if(op.op==='complete')await recorder.completeTurn(op.result);
    if(op.op==='invalid-complete')await recorder.completeTurn(op.kind==='null'?null:op.kind==='undefined'?undefined:42);
    if(op.op==='restore')recorder=new AgentSessionEventRecorder(ctx.writer);
    outcomes.push('ok');
  }catch(e:any){outcomes.push(e.message);}}
  recorderCases.push({name,fault,failures:name==='result-always'?99:1,operations,outcomes,transcript:ctx.bytes()});
}
const sidecar = await probeSidecar(root, output);
const matrixConsumers: Record<string,string> = {
  "input-text-original": "recording_original_text",
  "input-metadata-array": "recording_metadata_array",
  "input-metadata-string": "recording_metadata_string",
  "input-metadata-number": "recording_metadata_number",
  "input-metadata-all": "recording_metadata_all",
  "input-metadata-false": "recording_metadata_false",
  "input-metadata-null": "recording_metadata_null",
  "input-metadata-absent": "recording_metadata_absent",
  "success": "runner_success",
  "start-fails": "runner_start_write_failure",
  "accepted-fails": "runner_accepted_write_failure",
  "loop-fails-twice": "runner_repeated_loop_failure",
  "loop-throws": "runner_loop_exception",
  "consumer-drop": "runner_consumer_drop",
  "complete-once": "runner_completion_retry",
  "complete-always": "runner_completion_persistent_failure",
  "input-text": "runner_input_text_boundaries",
  "input-blocks": "runner_input_blocks_boundaries",
  "accepted-duplicate": "runner_accepted_validation_error",
  "loop-aborted-error": "runner_aborted_exception",
  "status-fails": "runner_best_effort_status",
  "loop-null-hint": "runner_nullish_hint"
};
for(const name of [...completionCases,...catchCases,'loop-missing-message','loop-undefined-message','loop-null-message']) matrixConsumers[name]=`runner_${name.replaceAll('-','_')}`;
matrixConsumers['input-synthetic-null']='recording_synthetic_null';
for(const row of matrix) row.consumption = matrixConsumers[row.name]
  ? {status:row.name.startsWith('input-metadata-') || row.name==='input-text-original' ? 'pr3 端口已消费' : 'pr2 已消费',test:matrixConsumers[row.name],scope:'runner-owned events / error / JSONL; input cases also loopMessages; counters remain observational'}
  : {status:'仅记录，由 pr3 消费',plannedTest:`live_runner_${row.name.replaceAll('-','_')}`,scope:'LiveSessionAdapter + AgentLoop host integration'};
for(const row of matrix) if (/^invalid-result-(session|number|array|missing-turn|missing-session|turn-undefined)-/.test(row.name))
  row.consumption = {status:'差异已登记，非 TS 对齐',test:matrixConsumers[row.name],scope:'raw TS input/result retained; Rust error terminal, step/turn closure and identity asserted; A7.2-TURN-INVALID-RESULT-001'};
for(const row of sidecar) row.consumption = {status:'仅记录，由 pr3 消费',plannedTest:`live_sidecar_${row.name.replaceAll('-','_')}`,scope:'real TS sidecar + Rust host; approved differences require separate acceptance'};
verifyConsumers([...matrix,...sidecar,...forward], ['runner_tests.rs','live_stream_tests.rs','forward_tests.rs'].map(file=>join(dirname(fileURLToPath(import.meta.url)),'../../../src/runtime/turn',file)));
assertBaseline(root);
writeFileSync(join(output,'oracle.json'), JSON.stringify({sourceCommit,sourceSha256,matrix,recorderCases,sidecar,forward},null,2)+'\n',{flag:'wx'});
for(const row of matrix)console.log(JSON.stringify({name:row.name,events:row.events.map((e:any)=>e.type),error:row.error,
  completes:row.completes,loopCalls:row.loopCalls,modelCalls:row.modelCalls,returns:row.returns}));
console.log(JSON.stringify({sidecar},null,2));

for(const row of forward)console.log(JSON.stringify(row));
