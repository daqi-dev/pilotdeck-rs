import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import assert from 'node:assert/strict';
import net from 'node:net';
import http from 'node:http';
import https from 'node:https';
import tls from 'node:tls';
import { syncBuiltinESMExports } from 'node:module';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
const ROOT = path.resolve(process.argv[2]);
fs.mkdirSync(ROOT,{recursive:true});
fs.writeFileSync(path.join(ROOT,'.generation-owner'),'turn-runner-tail\n',{flag:'wx'});
const BASE = path.resolve(process.env.PDRS_BASELINE!);
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
assertBaseline(BASE);
assert.equal(process.version, 'v22.22.0');
const baseline = execFileSync('git', ['-C', BASE, 'rev-parse', 'HEAD'], {encoding:'utf8'}).trim();
assert.equal(baseline, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const forbiddenNetwork: any[] = [];
const denyNetwork = (...args: any[]) => { forbiddenNetwork.push({call: 'network', argumentType: typeof args[0]}); throw new Error('PROBE_NETWORK_FORBIDDEN'); };
globalThis.fetch = denyNetwork as any;
net.Socket.prototype.connect = denyNetwork as any;
http.request = http.get = https.request = https.get = tls.connect = denyNetwork as any;
syncBuiltinESMExports();
const { TurnRunner } = await import(`${BASE}/src/agent/turn/TurnRunner.ts`);
const { AgentSession } = await import(`${BASE}/src/agent/session/AgentSession.ts`);
const { JsonlTranscriptWriter } = await import(`${BASE}/src/session/transcript/JsonlTranscriptWriter.ts`);
const { JsonlSessionEventStore } = await import(`${BASE}/src/session/events/JsonlSessionEventStore.ts`);
const { SessionMetadataStore } = await import(`${BASE}/src/session/metadata/SessionMetadataStore.ts`);
const { createNativeSessionTitleProvider } = await import(`${BASE}/src/session/title/SessionTitleGenerator.ts`);
const { createPromptSuggestionGenerator } = await import(`${BASE}/src/session/prompt/PromptSuggestionGenerator.ts`);
const { readSessionInfo } = await import(`${BASE}/src/session/storage/SessionList.ts`);
const outputName = process.env.MATRIX_OUTPUT ?? 'raw';
assert.match(outputName, /^[a-z0-9-]+$/);
const rawDir = path.join(ROOT, outputName);
fs.mkdirSync(rawDir, {recursive:true});
const rel = (p: string) => path.relative(ROOT, p);
const clone = (v: any) => JSON.parse(JSON.stringify(v));
function deferred<T=any>() { let resolve!: (x:T)=>void, reject!: (e:any)=>void; const promise = new Promise<T>((a,b)=>{resolve=a;reject=b;}); return {promise,resolve,reject}; }
async function bounded<T>(p:Promise<T>, label:string): Promise<T> { let timer: any; try { return await Promise.race([p, new Promise<never>((_,r)=>{timer=setTimeout(()=>r(new Error(`PROBE_TIMEOUT:${label}`)),10000);})]); } finally {clearTimeout(timer);} }
const cases: any[] = [
 {name:'title-before',title:'before'}, {name:'title-after',title:'after'}, {name:'title-during-suggestion',title:'during-suggestion',suggestion:'delayed'},
 {name:'title-abort-cooperative',title:'abort-cooperative'}, {name:'title-abort-ignored',title:'abort-ignored'},
 {name:'title-abort-after-terminal',title:'abort-after'}, {name:'title-dispose-after-terminal',title:'dispose'},
 {name:'title-failure-native',title:'failure'}, {name:'title-failure-port',title:'port-failure'},
 {name:'files-created',file:'created',suggestion:'on'}, {name:'files-none'},
 {name:'files-updated',file:'updated'}, {name:'files-explicit-path',file:'explicit'}, {name:'files-same-bytes',file:'same'},
 {name:'files-unreported-write',file:'unreported'}, {name:'files-collection-disabled',file:'disabled'},
 {name:'files-aborted',file:'aborted'},
 {name:'suggestion-on',suggestion:'on'}, {name:'suggestion-off'},
 {name:'suggestion-empty',suggestion:'empty'}, {name:'suggestion-null',suggestion:'null'},
 {name:'suggestion-failure-native',suggestion:'failure'}, {name:'suggestion-failure-injected',suggestion:'injected-failure'},
 {name:'suggestion-delayed',suggestion:'delayed'},
 {name:'listing-two-turns',listing:'two'}, {name:'listing-utf16-limit',listing:'long'},
 {name:'listing-empty-model',listing:'empty-model'}, {name:'listing-empty-no-model',listing:'empty'},
 {name:'listing-synthetic',listing:'synthetic'}, {name:'listing-multiblock',listing:'blocks'},
];
const requested = process.argv.slice(3);
const selected = requested.length ? cases.filter(c=>requested.includes(c.name)) : cases;
assert(selected.length > 0);
const results: any[] = [];
const rows: any[] = [];
const runDir = fs.mkdtempSync(path.join(ROOT, 'run-'));
try {
for (const config of selected) {
 const report: any = {name:config.name, baseline, node:process.version, config, scope:'real AgentSession/TurnRunner/session persistence; deterministic fake loop and auxiliary ModelRuntime.complete', timeline:[], events:[], commits:[], snapshots:[], console:[], modelRequests:[], checks:[]};
 let ordinal=0, uuid=0;
 const now = () => new Date(Date.UTC(2026,9,27,0,0,0));
 const start=process.hrtime.bigint();
 const log=(channel:string, value:any) => { const item={ordinal:++ordinal,elapsedNs:String(process.hrtime.bigint()-start),channel,value:clone(value)}; report.timeline.push(item); return item.ordinal; };
 const check=(name:string,condition:boolean)=> {report.checks.push({name,pass:condition}); assert(condition,`${config.name}: ${name}`);};
 const originalConsole = {debug:console.debug,warn:console.warn,error:console.error};
 for(const level of ['debug','warn','error'] as const) console[level]=(...args:any[])=>{const text=args.map(a=>String(a)).join(' '); const n=log('console',{level,text}); report.console.push({ordinal:n,level,text});};
 const caseDir=path.join(runDir,config.name); const workspace=path.join(caseDir,'workspace');
 fs.mkdirSync(workspace,{recursive:true});
 const transcriptPath=path.join(caseDir,'session.jsonl');
 report.workspace=rel(workspace); report.transcriptPath=rel(transcriptPath);
 if(['updated','same'].includes(config.file)) fs.writeFileSync(path.join(workspace,'report.txt'),'old\n');
 const disk=()=>fs.existsSync(transcriptPath)?fs.readFileSync(transcriptPath,'utf8'):'';
 const store=new JsonlSessionEventStore({path:transcriptPath, now, uuid:()=>`entry-${String(++uuid).padStart(4,'0')}`});
 const writer=new JsonlTranscriptWriter({path:transcriptPath,eventStore:store,now});
 const metadata=new SessionMetadataStore({transcript:writer,sessionId:'session-742',now});
 store.subscribe((entry:any)=>{ const bytes=disk(); const n=log('session_commit',entry); report.commits.push({ordinal:n,entry:clone(entry),diskUtf8:bytes,metadataAtCallback:clone(metadata.getSnapshot())}); });
 const mainGates: any[]=[]; let latestMain:any; let titleInput:any; let titleSignal:any; let titleStarted=false; let titleAbortObserved=false;
 const titleGate=deferred<any>(); const suggestionGate=deferred<any>(); const suggestionStarted=deferred<void>();
 const titleResponse={content:[{type:'text',text:'```json\n{"title":"  矩阵   标题  "}\n```'}]};
 const suggestionResponse={content:[{type:'text',text:'```text\nSuggestion: "请  继续\n检查输出"\n```'}]};
 const modelRuntime={complete: async (request:any, options:any)=>{
   const purpose=request.metadata.purpose; const signal=options.signal;
   const n=log('model_request',{request,signalAborted:signal.aborted}); report.modelRequests.push({ordinal:n,request:clone(request)});
   if(purpose==='session_title_generation') {
    titleStarted=true; titleSignal=signal;
    signal.addEventListener('abort',()=>{titleAbortObserved=true;log('model_signal_abort',{purpose,reason:signal.reason});if(config.title==='abort-cooperative')titleGate.reject(new Error('TITLE_ABORTED'));},{once:true});
    return await titleGate.promise;
   }
   suggestionStarted.resolve();
   if(config.suggestion==='failure')throw new Error('SUGGESTION_MODEL_BOOM');
   if(config.suggestion==='empty')return {content:[]};
   if(config.suggestion==='delayed')return await suggestionGate.promise;
   return suggestionResponse;
 }};
 const nativeTitle = createNativeSessionTitleProvider({modelRuntime,agentModel:{provider:'probe',model:'aux-title'},systemLanguage:'zh-CN'});
 const titleProvider = config.title ? {...nativeTitle, generate:(input:any)=>{titleInput=input;log('title_generate',{...input,signal:{aborted:input.signal.aborted}});if(config.title==='port-failure')return titleGate.promise;return nativeTitle.generate(input);}} : undefined;
 const nativeSuggestion=createPromptSuggestionGenerator({modelRuntime,agentModel:{provider:'probe',model:'aux-suggestion'}});
 const suggestion = !config.suggestion ? undefined : config.suggestion==='null' ? async()=>{log('suggestion_injected',{result:null});return null;} : config.suggestion==='injected-failure' ? async()=>{log('suggestion_injected',{throws:'SUGGESTION_INJECTED_BOOM'});throw new Error('SUGGESTION_INJECTED_BOOM');} : nativeSuggestion;
 const loop={
  snapshotFileState:()=>({}),
  async *run(options:any) {
   const gate=latestMain; log('loop_started',{turnId:options.turnId});gate.started.resolve();
   await gate.release.promise;
   if(config.file) {
    fs.writeFileSync(path.join(workspace,'report.txt'),config.file==='same'?'old\n':'hello matrix\n');
    log('fixture_write',{path:'report.txt',text:config.file==='same'?'old\n':'hello matrix\n'});
    if(config.file!=='unreported')yield {type:'tool_result',sessionId:options.sessionId,turnId:options.turnId,result:{type:'success',toolCallId:'write-1',toolName:'write_file',content:[{type:'text',text:'written'}],data:config.file==='explicit'?{filePath:'report.txt'}:{path:'report.txt'}}};
   }
   const aborted=options.abortSignal.aborted;
   const message={role:'assistant',content:[{type:'text',text:'  已完成矩阵检查。  '}]};
   if(!aborted){await options.onDurableMessage(message);yield {type:'assistant_message',sessionId:options.sessionId,turnId:options.turnId,message};}
   const result={type:aborted?'aborted':'success',sessionId:options.sessionId,turnId:options.turnId,...(!aborted?{finalMessage:message}:{}),stopReason:aborted?'aborted_streaming':'completed',usage:{inputTokens:3,outputTokens:4},permissionDenials:[],turns:1,startedAt:now().toISOString(),completedAt:now().toISOString()};
   yield {type:'turn_completed',sessionId:options.sessionId,turnId:options.turnId,result};
   return {result,messages:[...options.messages,...(!aborted?[message]:[])]};
  }
 };
 const runner=new TurnRunner(loop,writer,undefined,now,undefined,{cwd:workspace,transcriptPath,collectFileArtifacts:config.file!=='disabled'},{metadataStore:metadata,sessionTitleProvider:titleProvider,promptSuggestionGenerator:suggestion,autoGenerateSessionTitle:!!config.title});
 const session=new AgentSession({sessionId:'session-742',turnRunner:runner,cwd:workspace,transcriptPath});
 const snapshot=(label:string)=>{const value={label,metadata:metadata.getSnapshot(),diskUtf8:disk(),sessionState:session.snapshot(),pendingTitle:(runner as any).pendingSessionTitle?{completed:(runner as any).pendingSessionTitle.completed,title:(runner as any).pendingSessionTitle.title,aborted:(runner as any).pendingSessionTitle.controller.signal.aborted}:null};const n=log('snapshot',{label});report.snapshots.push({ordinal:n,...clone(value)});};
 async function settleTitle(){const pending=(runner as any).pendingSessionTitle;check('title pending promise exists',!!pending);await bounded(pending.promise,'title settle');await store.flush();snapshot('title-settled');}
 function releaseTitle(){log('control',{action:'resolve-title'});titleGate.resolve(titleResponse);}
 async function submit(input:any,submitOptions:any={}){
   const gate={started:deferred<void>(),release:deferred<void>()};mainGates.push(gate);latestMain=gate;
   const done=(async()=>{for await (const event of session.submit(input,{turnId:`turn-${mainGates.length}`,...submitOptions})){const n=log('agent_event',event);report.events.push({ordinal:n,event:clone(event)});snapshot(`event:${event.type}`);}log('stream_closed',{});snapshot('stream-closed');})();
   await bounded(gate.started.promise,'loop entered');snapshot('main-held');return {gate,done};
 }
 try {
   const input=config.listing==='blocks'?{type:'blocks',content:[{type:'text',text:'  第一段  '},{type:'text',text:'第二段  '}]}:config.listing==='long'?{type:'text',text:'a'.repeat(1199)+'😀尾'}:config.listing?.startsWith('empty')?{type:'text',text:' \n\t '}:config.listing==='synthetic'?{type:'text',text:'  人类  原始\n文本  '}:{type:'text',text:'  帮我检查  矩阵\n结果  '};
   report.input=input;
   const submitOptions=config.listing==='empty-model'?{modelSelection:{mode:'model',provider:'probe',model:'main-model'}}:config.listing==='synthetic'?{syntheticMessages:[{role:'user',content:[{type:'text',text:'不应成为 listing prompt'}],metadata:{synthetic:true}}]}:{};
   report.submitOptions=submitOptions;
   const {gate,done}=await submit(input,submitOptions);
   if(config.title==='before'){releaseTitle();await settleTitle();}
   if(['failure','port-failure'].includes(config.title)){log('control',{action:'reject-title'});titleGate.reject(new Error('TITLE_MODEL_BOOM'));await settleTitle();}
   if(['abort-cooperative','abort-ignored'].includes(config.title)||config.file==='aborted'){
    log('control',{action:'session.abort',reason:'probe-abort'});session.abort('probe-abort');snapshot('after-abort');
    if(config.title==='abort-cooperative')await settleTitle();
   }
   log('control',{action:'release-main'});gate.release.resolve();
   if(config.suggestion==='delayed'){
    await bounded(suggestionStarted.promise,'suggestion entered');snapshot('suggestion-held');
    check('delayed suggestion holds terminal event',!report.events.some((x:any)=>x.event.type==='turn_completed'));
    check('turn_result already persisted before delayed suggestion',report.commits.some((x:any)=>x.entry.type==='turn_result'));
    if(config.title==='during-suggestion'){releaseTitle();await settleTitle();}
    log('control',{action:'resolve-suggestion'});suggestionGate.resolve(suggestionResponse);
   }
   await bounded(done,'session stream closed');snapshot('before-late-title');
   if(['after','abort-ignored','abort-after','dispose'].includes(config.title)){
     if(config.title==='abort-after'){log('control',{action:'session.abort-after-stream',reason:'late-abort'});session.abort('late-abort');snapshot('after-late-abort');}
     if(config.title==='dispose'){log('control',{action:'session.dispose-before-title'});await session.dispose();snapshot('after-dispose');}
     releaseTitle();await settleTitle();
   }
   if(config.listing==='two'){
    const next=await submit({type:'text',text:'  第二轮\n  继续  '},{modelSelection:{mode:'model',provider:'probe',model:'second-model'}});
    next.gate.release.resolve();await bounded(next.done,'second stream closed');
   }
   await store.flush();snapshot('final');
   report.listingInfo=await readSessionInfo(transcriptPath,'session-742',workspace);
   report.observation={titleStarted,titleAbortObserved,titleSignalAborted:titleSignal?.aborted??null,titlePortSignalAborted:titleInput?.signal.aborted??null,networkAttempts:forbiddenNetwork.length};
   check('network absent',forbiddenNetwork.length===0);
   check('stream contains exactly one terminal per turn',report.events.filter((x:any)=>x.event.type==='turn_completed').length===mainGates.length);
   check('all committed entries were readable from disk in callback',report.commits.every((x:any)=>x.diskUtf8.endsWith(JSON.stringify(x.entry)+'\n')));
   report.status='PASS';
 } catch(e:any){report.status='INVALID';report.error={message:e.message,stack:e.stack};}
 finally {
   for(const level of ['debug','warn','error'] as const)console[level]=originalConsole[level];
   log('control',{action:'cleanup-session-dispose',observationScope:'observation and final snapshot above precede cleanup'});
   await session.dispose().catch(()=>{});
   report.cleanupObservation={titleAbortObserved,titleSignalAborted:titleSignal?.aborted??null,titlePortSignalAborted:titleInput?.signal.aborted??null};
   report.finalDiskUtf8=disk();report.finalDiskSha256=crypto.createHash('sha256').update(report.finalDiskUtf8).digest('hex');
   report.finalMetadata=clone(metadata.getSnapshot());
   fs.writeFileSync(path.join(rawDir,`${config.name}.json`),JSON.stringify(report,null,2)+'\n',{flag:'wx'});
   process.stdout.write(`${report.status} ${config.name}${report.error?' '+report.error.message:''}\n`);
   rows.push({name:config.name,config,input:report.input,submitOptions:report.submitOptions,events:report.events.map((x:any)=>x.event).filter((x:any)=>!['session_started','setup_completed','session_ended'].includes(x.type)),transcript:report.finalDiskUtf8,metadata:report.finalMetadata,observation:report.observation,upstream:{console:report.console.map(({level,text}:any)=>({level,text})),modelRequests:report.modelRequests.map((x:any)=>x.request)},snapshots:report.snapshots.filter((x:any)=>['main-held','suggestion-held','before-late-title','after-late-abort','after-dispose'].includes(x.label)).map(({label,metadata,diskUtf8,pendingTitle}:any)=>({label,metadata,diskUtf8,pendingTitle}))});
   results.push({name:config.name,status:report.status,raw:`${outputName}/${config.name}.json`,error:report.error?.message});
 }
}
fs.writeFileSync(path.join(rawDir,'index.json'),JSON.stringify({baseline,node:process.version,runDir:rel(runDir),results,networkAttempts:forbiddenNetwork},null,2)+'\n',{flag:'wx'});
if(results.some(x=>x.status!=='PASS'))process.exitCode=1;

assertBaseline(BASE);
const {FileArtifactCollector}=await import(`${BASE}/src/session/artifacts/FileArtifactCollector.ts`);
const collectorRows=[];
for(const c of [
 {name:'root-link',path:'output.txt',before:'old',rootLink:true},
 {name:'fifo',path:'pipe',fifo:true,explicit:true},
 {name:'env-dot',path:'.env.'},
 {name:'sensitive',path:'.env'}, {name:'example-env',path:'.env.example'},
 {name:'hard-internal',path:'.git/output.txt',allowed:true},
 {name:'excluded',path:'.cache/output.txt'}, {name:'allowed-input',path:'.cache/output.txt',allowed:true,before:'old'},
 {name:'internal-file',path:'trace.log'}, {name:'allowed-internal',path:'trace.log',allowed:true},
 {name:'readonly',path:'output.txt',tool:'read_file'},
 {name:'denied',path:'output.txt',error:'permission_denied'}, {name:'execution-error',path:'output.txt',error:'execution_failed'},
 {name:'explicit',path:'output.txt',explicit:true,tool:'send_attachment'},
 {name:'known-depth4',path:'output.txt',depth:4}, {name:'known-depth5',path:'output.txt',depth:5},
 {name:'overlap-diff',path:'output.txt',overlap:true}, {name:'overlap-explicit',path:'output.txt',overlap:true,explicit:true},
 {name:'secret-key',path:'credential.json'}, {name:'pem',path:'private.pem'},
 {name:'same-hash',path:'output.txt',before:'artifact',explicit:true},
]) {
 const workspace=path.join(runDir,'collector',c.name);fs.mkdirSync(path.dirname(path.join(workspace,c.path)),{recursive:true});
 if(c.fifo)execFileSync('mkfifo',[path.join(workspace,c.path)]);
 if(c.before!==undefined)fs.writeFileSync(path.join(workspace,c.path),c.before,{flag:'wx'});
 const cwd=c.rootLink?workspace+'-link':workspace;if(c.rootLink)fs.symlinkSync(workspace,cwd,'dir');
 const options={cwd,allowedInputPaths:c.allowed?[c.path]:[],now:()=>new Date('2026-10-27T00:00:00.000Z')};
 const collector=await FileArtifactCollector.start(options),other=c.overlap?await FileArtifactCollector.start(options):null;
 const result:any=c.error?{type:'error',toolName:c.tool??'write_file',error:{code:c.error,message:'probe'}}:{type:'success',toolName:c.tool??'write_file',content:c.explicit?[{type:'file',path:c.path}]:[]};
 if(c.depth!==undefined){let data:any={filePath:c.path};for(let n=0;n<c.depth;n++)data={nested:data};result.data=data;}
 if(!c.fifo)fs.writeFileSync(path.join(workspace,c.path),'artifact',{flag:c.before!==undefined?'w':'wx'});
 collector.observeToolResult(result);other?.observeToolResult(result);
 const artifacts=await collector.finish('complete');const peer=other?await other.finish('complete'):null;
 collectorRows.push({config:c,result,artifacts,peer});
}
const sources=['agent/turn/TurnRunner.ts','agent/turn/TurnInputProcessor.ts','agent/session/AgentSession.ts','agent/session/AgentSessionEventRecorder.ts','session/metadata/SessionMetadataStore.ts','session/artifacts/FileArtifactCollector.ts','session/title/SessionTitleGenerator.ts','session/prompt/PromptSuggestionGenerator.ts','session/transcript/JsonlTranscriptWriter.ts','session/events/SessionRuntime.ts','session/events/JsonlSessionEventStore.ts','session/persistence/JsonlSessionPersistence.ts'];
const oracle={sourceCommit:baseline,sourceSha256:await baselineSourceHashes(BASE,sources.map(p=>'src/'+p)),rows,collectorRows};
fs.writeFileSync(path.join(ROOT,'oracle.json'),JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
} finally {
 // #1253: raw reports already contain transcript bytes and hashes. Remove
 // only this invocation's mkdtemp root, including collector FIFO/symlinks.
 fs.rmSync(runDir,{recursive:true,force:true});
}
