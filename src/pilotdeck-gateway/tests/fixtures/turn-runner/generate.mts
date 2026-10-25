// Real cc7c4e6 TurnRunner + recorder; deterministic writer failure seams only.
import assert from 'node:assert/strict';
import {probeSidecar} from './sidecar.mts';
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
const root = resolve(process.env.PDRS_BASELINE!);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(root);
const sources = ['agent/turn/TurnRunner.ts', 'agent/turn/TurnInputProcessor.ts',
  'agent/session/AgentSessionEventRecorder.ts', 'session/transcript/JsonlTranscriptWriter.ts',
  'session/events/SessionRuntime.ts', 'session/persistence/JsonlSessionPersistence.ts',
  'agent/loop/AgentLoop.ts', 'agent/loop/AgentTurnCapabilities.ts'];
const sourceSha256 = await baselineSourceHashes(root, [...sources,
  'agent/modules/transport/agentLoopSidecarClient.ts', 'agent/modules/transport/agentLoopSidecarServer.ts',
  'agent/modules/transport/stdioAgentLoopSidecarConnection.ts', 'cli/pilotdeck-agent-loop-default-factory.ts',
  'agent/session/createAgentSession.ts', 'session/transcript/InMemoryTranscriptWriter.ts',
  'session/events/SessionDomainValidation.ts', 'agent/protocol/errors.ts', 'status/agentStatus.ts'
].map(p => `src/${p}`));
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
  const runtime = new SessionRuntime({now, uuid:() => `id-${++ids}`});
  const persistence = new JsonlSessionPersistence({path});
  runtime.subscribe(entry => {if (entry.type === fault && ++attempts <= failures) throw new Error('injected write failure');
    return persistence.append(entry);}, {failureMode:'propagate', flush:() => persistence.flush()});
  const writer = new JsonlTranscriptWriter({path, eventStore:runtime});
  return {writer, attempts:()=>attempts, bytes:()=>{try{return readFileSync(path,'utf8');}catch{return '';}}};
}
const result = (type = 'success', turnId = 't') => ({type,sessionId:'s',turnId,stopReason:'completed',
  usage:{},permissionDenials:[],turns:1,startedAt:now().toISOString(),completedAt:now().toISOString()});
const matrix:any[] = [];
for (const name of ['success','start-fails','accepted-fails','abort-before-execute','abort-after-accepted',
  'loop-fails-twice','loop-throws','early-completed','consumer-drop','complete-once','complete-always']) {
  const fault = name === 'start-fails' ? 'turn_started' : name === 'accepted-fails' ? 'accepted_input'
    : name.startsWith('complete-') ? 'turn_result' : '';
  const ctx = writerFor(name, fault, name === 'complete-once' ? 1 : 99);
  const controller = new AbortController();
  let loopCalls=0, modelCalls=0, returns=0, completes=0, closes=0;
  const recorder = new AgentSessionEventRecorder(ctx.writer);
  const complete = recorder.completeTurn.bind(recorder);
  recorder.completeTurn = async (value:any) => {completes++; await complete(value);};
  const config={cwd:output,runMode:'agent'};
  const model={async prepare({request}:any){modelCalls++; return {request};},async *stream(){modelCalls++; throw new Error('unexpected model');}};
  const native = new AgentLoop(config, createSidecarAgentTurnCapabilities(config,
    {ports:{model,toolExecution:{list:()=>[],executeAll:async()=>[]}},now}));
  const loop={snapshotFileState:()=>({}), async *run(input:any) {
    loopCalls++;
    try {
      if(name.startsWith('abort-')) return yield* native.run(input);
      if(name==='loop-throws') throw new Error('injected loop failure');
      if(name==='loop-fails-twice') for(let i=0;i<2;i++) yield {type:'turn_failed',sessionId:'s',turnId:'t',error:{code:'agent_invalid_state',message:'loop failed'}};
      const r=result(name==='loop-fails-twice'?'error':'success');
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
  try {for await(const e of runner.run({sessionId:'s',turnId:'t',messages:[],input:{type:'text',text:'probe'},
    abortSignal:controller.signal,closeSteerMailbox:()=>{closes++;return [];}})) {
      events.push(e);
      if(name==='abort-after-accepted' && e.type==='input_accepted')controller.abort('probe');
      if(name==='consumer-drop' && e.type==='model_event')break;
  }}catch(e:any){error=e.message;}
  matrix.push({name,events,error,loopCalls,modelCalls,returns,completes,closes,writeAttempts:ctx.attempts(),transcript:ctx.bytes()});
}
// Recorder completion is independently replayable in Rust, including retry state.
const recorderCases:any[]=[];
for(const name of ['success','error','aborted','max_turns','no-step','unstarted','step-once','result-once','result-always','new-turn','restore','restart-fails']) {
  const fault=name==='step-once'?'step_completed':name.startsWith('result-')?'turn_result':'';
  const ctx=writerFor(`recorder-${name}`,fault,name==='result-always'?99:1);
  let recorder=new AgentSessionEventRecorder(ctx.writer);
  const operations:any[]=[];
  if(name!=='unstarted')operations.push({op:'start',turn:'t'});
  if(!['no-step','unstarted'].includes(name))operations.push({op:'request',turn:'t'});
  const value=result(['error','aborted','max_turns'].includes(name)?name:'success');
  operations.push({op:'complete',result:value},{op:'complete',result:value});
  if(name==='new-turn')operations.push({op:'start',turn:'other'},{op:'complete',result:value},{op:'complete',result:result('success','other')});
  if(name==='restart-fails')operations.push({op:'start',turn:'t'},{op:'complete',result:value});
  if(name==='restore')operations.push({op:'restore'},{op:'complete',result:value});
  const outcomes:any[]=[];
  for(const op of operations) {try {
    if(op.op==='start')await recorder.startTurn('s',op.turn);
    if(op.op==='request')await recorder.recordModelRequest('s',op.turn,{request:{messages:[]}});
    if(op.op==='complete')await recorder.completeTurn(op.result);
    if(op.op==='restore')recorder=new AgentSessionEventRecorder(ctx.writer);
    outcomes.push('ok');
  }catch(e:any){outcomes.push(e.message);}}
  recorderCases.push({name,fault,failures:name==='result-always'?99:1,operations,outcomes,transcript:ctx.bytes()});
}
const sidecar = await probeSidecar(root, output);
assertBaseline(root);
writeFileSync(join(output,'oracle.json'), JSON.stringify({sourceCommit,sourceSha256,matrix,recorderCases,sidecar},null,2)+'\n',{flag:'wx'});
for(const row of matrix)console.log(JSON.stringify({name:row.name,events:row.events.map((e:any)=>e.type),error:row.error,
  completes:row.completes,loopCalls:row.loopCalls,modelCalls:row.modelCalls,returns:row.returns}));
console.log(JSON.stringify({sidecar},null,2));
