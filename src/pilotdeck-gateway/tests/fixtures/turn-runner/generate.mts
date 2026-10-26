// Real cc7c4e6 TurnRunner + recorder; deterministic writer failure seams only.
import assert from 'node:assert/strict';
import {probeSidecar} from './sidecar.mts';
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, join, relative, resolve} from 'node:path';
import {createRequire} from 'node:module';
import {existsSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
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
function writerFor(name: string, fault = '', failures = 0) {
  const path = join(output, `${name}.jsonl`);
  writeFileSync(path, '', {flag:'wx'});
  let ids = 0, attempts = 0;
  const runtime = new SessionRuntime({now, uuid:() => `id-${++ids === 2 && name === 'accepted-duplicate' ? 1 : ids}`});
  const persistence = new JsonlSessionPersistence({path});
  runtime.subscribe(entry => {if (entry.type === fault && ++attempts <= failures) throw new Error('injected write failure');
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
  'loop-fails-twice','loop-throws','early-completed','consumer-drop','complete-once','complete-always','input-text','input-blocks','accepted-duplicate','loop-aborted-error','status-fails','loop-null-hint','input-metadata-all','input-metadata-false','input-metadata-null','input-metadata-absent','input-metadata-array','input-metadata-string','input-metadata-number','input-text-original',...completionCases,...catchCases,'loop-missing-message','loop-undefined-message','loop-null-message']) {
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
  const synthetic=inputCase?[{role:'user',content:[{type:'text',text:'synthetic'}],metadata:{synthetic:true}}]:[];
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
  matrix.push({name,...(invalidResult?{rawResult:malformedResult(name),...(malformedResult(name)===undefined?{rawResultUndefined:true}:{}),...(name.startsWith('invalid-result-turn-undefined-')?{rawTurnUndefined:true}:{}),openStep:name.endsWith('-step')}:{}),...(catchCases.includes(name)?{rawResult:result('error',name.startsWith('loop-failed-invalid-')?'other':'t'),openStep:name.endsWith('-step')}:{}),...(messageCase?{failureError,failureMessageUndefined:Object.hasOwn(failureError,'message') && failureError.message===undefined,statusTextOwn:events.filter(e=>e.type==='agent_status').map(e=>Object.hasOwn(e,'text')),statusTextUndefined:events.filter(e=>e.type==='agent_status').map(e=>e.text===undefined)}:{}),events,error,loopCalls,modelCalls,returns,completes,closes,writeAttempts:ctx.attempts(),transcript:ctx.bytes(),...(inputCase?{rawInput,history,synthetic,loopMessages}: {}),...(name.startsWith('input-metadata-')?{submitOptions}: {})});
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
for(const row of matrix) row.consumption = matrixConsumers[row.name]
  ? {status:row.name.startsWith('input-metadata-') || row.name==='input-text-original' ? 'pr3 端口已消费' : 'pr2 已消费',test:matrixConsumers[row.name],scope:'runner-owned events / error / JSONL; input cases also loopMessages; counters remain observational'}
  : {status:'仅记录，由 pr3 消费',test:`live_runner_${row.name.replaceAll('-','_')}`,scope:'LiveSessionAdapter + AgentLoop host integration'};
for(const row of matrix) if (/^invalid-result-(session|number|array|missing-turn|missing-session|turn-undefined)-/.test(row.name))
  row.consumption = {status:'差异已登记，非 TS 对齐',test:matrixConsumers[row.name],scope:'raw TS input/result retained; Rust error terminal, step/turn closure and identity asserted; A7.2-TURN-INVALID-RESULT-001'};
for(const row of sidecar) row.consumption = {status:'仅记录，由 pr3 消费',test:`live_sidecar_${row.name.replaceAll('-','_')}`,scope:'real TS sidecar + Rust host; approved differences require separate acceptance'};
assertBaseline(root);
writeFileSync(join(output,'oracle.json'), JSON.stringify({sourceCommit,sourceSha256,matrix,recorderCases,sidecar},null,2)+'\n',{flag:'wx'});
for(const row of matrix)console.log(JSON.stringify({name:row.name,events:row.events.map((e:any)=>e.type),error:row.error,
  completes:row.completes,loopCalls:row.loopCalls,modelCalls:row.modelCalls,returns:row.returns}));
console.log(JSON.stringify({sidecar},null,2));
