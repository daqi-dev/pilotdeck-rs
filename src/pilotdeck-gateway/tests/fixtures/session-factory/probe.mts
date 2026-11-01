import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import net from 'node:net';
import http from 'node:http';
import https from 'node:https';
import tls from 'node:tls';
import dgram from 'node:dgram';
import child from 'node:child_process';
import {syncBuiltinESMExports} from 'node:module';
import {fileURLToPath} from 'node:url';
import {snapshotFiles} from './file-snapshot.mts';
const ROOT=path.resolve(process.env.MATRIX_ROOT!);
const BASE=path.resolve(process.env.PDRS_BASELINE!);
assert.equal(process.version,'v22.22.0');
const baseline=child.execFileSync('git',['-C',BASE,'rev-parse','HEAD'],{encoding:'utf8'}).trim();
assert.equal(baseline,'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
assert.equal(child.execFileSync('git',['-C',BASE,'diff','HEAD','--','src'],{encoding:'utf8'}),'');
const forbidden:any[]=[];
const deny=(kind:string)=>(...args:any[])=>{forbidden.push({kind,type:typeof args[0]});throw new Error('PROBE_FORBIDDEN_'+kind);};
globalThis.fetch=deny('fetch') as any;
net.Socket.prototype.connect=deny('connect') as any;
net.Server.prototype.listen=deny('listen') as any;
http.request=http.get=https.request=https.get=tls.connect=deny('http-tls') as any;
dgram.createSocket=deny('udp') as any;
for(const k of ['spawn','spawnSync','exec','execSync','execFile','execFileSync','fork']) (child as any)[k]=deny('child:'+k);
syncBuiltinESMExports();
const {ProjectSessionFactory}=await import(`${BASE}/src/cli/ProjectSessionFactory.ts`);
const {SessionRouter}=await import(`${BASE}/src/gateway/SessionRouter.ts`);
const {ToolRegistry}=await import(`${BASE}/src/tool/registry/ToolRegistry.ts`);
const {SubagentProviderRegistry}=await import(`${BASE}/src/agent/sub/SubagentProviderRegistry.ts`);
const {createAgentProjectSessionStorage}=await import(`${BASE}/src/session/storage/ProjectSessionStorage.ts`);
const names=['resume-missing','resume-valid','resume-malformed','resume-eisdir','resume-load-reject','resume-checkpoint-reject','resume-rollback-reject','resume-release-reject','admission-read-reject','recreate-read-reject','recreate-rollback-reject','recreate-idle','recreate-draining','recreate-running','recreate-disposed','same-key-concurrent','same-key-cached','create-factory-dispose','create-owner-shutdown','create-dispose-before-mcp'];
const fenceNames=['idle','quick','never','disposing'].flatMap(state=>['recreate','twice','close','shutdown'].flatMap(event=>['beforeload','afterload','publish'].map(timing=>`fence-${state}-${event}-${timing}`)));
const coreNames=[...names];names.push(...fenceNames);
const requested=process.argv.slice(2);if(requested.some(n=>!names.includes(n)))throw Error('UNKNOWN_CASE');
const selected=requested.length?names.filter(n=>requested.includes(n)):coreNames;
const output=process.env.MATRIX_OUTPUT??'raw';assert.match(output,/^[a-z0-9-]+$/);
const negative=process.env.MATRIX_NEGATIVE==='1';
// A parent generator may retain its own root for check.py's post-exit audit.
// Standalone probes own and clean their root, including output-write failures.
const parentOwned=process.env.MATRIX_WORK_ROOT;
const runDir=parentOwned??fs.mkdtempSync(path.join(ROOT,'run-'));
try {
const outDir=path.join(ROOT,output);fs.mkdirSync(outDir,{recursive:true});
const clone=(x:any)=>JSON.parse(JSON.stringify(x));
const err=(e:any):any=>({name:e?.name??typeof e,message:e?.message??String(e),...(e?.code?{code:e.code}:{}),...(e?.errors?{errors:[...e.errors].map(err)}:{}),...(e?.cause?{cause:err(e.cause)}:{})});
function deferred(){let resolve!:(x?:any)=>void,reject!:(e:any)=>void;const promise=new Promise<any>((a,b)=>{resolve=a;reject=b});return {promise,resolve,reject};}
async function bounded(p:Promise<any>,label:string){let timer:any;try{return await Promise.race([p,new Promise((_,r)=>{timer=setTimeout(()=>r(Error('PROBE_TIMEOUT:'+label)),15000)})]);}finally{clearTimeout(timer)}}
const index:any[]=[];
for(const name of selected){
 const fence=name.startsWith('fence-')?name.split('-').slice(1):null;
 const caseDir=path.join(runDir,name),workspace=path.join(caseDir,'workspace'),pilotHome=path.join(caseDir,'home');fs.mkdirSync(workspace,{recursive:true});
 const report:any={name,baseline,node:process.version,negative,caseDir:path.relative(ROOT,caseDir),timeline:[],snapshots:[],modelRequests:[],events:[],commits:[],results:[],console:[]};
 let ordinal=0,clock=0,entryId=0,composition=0,storageCount=0,mcpCount=0;
 const start=process.hrtime.bigint(),now=()=>new Date(Date.UTC(2026,9,28)+clock++);
 const log=(type:string,data:any={})=>{report.timeline.push({ordinal:++ordinal,elapsedNs:String(process.hrtime.bigint()-start),type,data:clone(data)});return ordinal;};
 const savedConsole={warn:console.warn,error:console.error,debug:console.debug};
 for(const level of ['warn','error','debug'] as const) console[level]=(...args:any[])=>{const text=args.map(String).join(' ');report.console.push({ordinal:log('console',{level,text}),level,text});};
 const active:any={project:[],permission:[],execution:[],plugin:[],router:[],sharedMcp:[],mcp:[],binding:[]};
 const releases:any={};const handles:any[]=[],storages:any[]=[],mcps:any[]=[];const gates:any[]=[];
 const gate=(label:string)=>{const g={label,entered:deferred(),release:deferred()};gates.push(g);return g;};
 const createGates=[gate('create-storage-1'),gate('create-storage-2')],drainGate=gate('old-descendants'),loopGate=gate('old-loop'),refreshGate=gate('refresh');
 const fenceGate=gate('fence-boundary'),oldDisposeReturn=gate('old-dispose-return');
 const earlyTwice=process.env.MATRIX_TWICE_RETURN==='early',earlyClose=process.env.MATRIX_CLOSE_RETURN==='early';let fencePublications=0;
 const atBoundary=async(timing:string)=>{if(fence?.[2]===timing){log('fence.boundary',{state:fence[0],event:fence[1],timing});fenceGate.entered.resolve();await fenceGate.release.promise;}};
 const evicted=deferred(),abortSeen=deferred();let holdDrain=false,holdLoop=false,router:any,old:any,oldDisposal:any;
 const hId=(h:any)=>h?handles.indexOf(h)+1:null;
 const read=(f:()=>any)=>{try{return f()}catch(e){return {readError:err(e)}}};
 const stateView=(state:any)=>({...state,abortController:{aborted:state.abortController.signal.aborted,reason:state.abortController.signal.reason??null}});
 const reloadView=(h:any)=>{const r=h.session.snapshotForRuntimeReload();return {...r,state:stateView(r.state)}};
 const retain=(type:string,id:number)=>{active[type].push(id);log('lease.acquire',{type,id});let done=false;return async()=>{log('lease.release.call',{type,id});if(done)return;done=true;active[type]=active[type].filter((x:number)=>x!==id);releases[type+':'+id]=(releases[type+':'+id]??0)+1;log('lease.release.done',{type,id});if(type==='project'&&name==='resume-release-reject'&&!negative){log('lease.release.error',{type,id,error:{name:'Error',message:'PROJECT_RELEASE_BOOM'}});throw Error('PROJECT_RELEASE_BOOM');}};};
 const model={getCapabilities:()=>({maxContextTokens:32000,maxOutputTokens:1024}),getMultimodal:()=>undefined,getProviderProtocol:()=> 'openai',complete:async(request:any)=>{report.modelRequests.push({ordinal:log('model.request',{request}),request:clone(request)});return {content:[]};}};
 const runtime:any={projectRoot:workspace,projectStorage:{projectRoot:workspace,pilotHome},snapshot:{config:{agent:{model:{provider:'probe',model:'deterministic'},maxContextTokens:32000},model:{providers:{}},gateway:{maxPerSessionMcpInstances:20}}},profile:{interaction:{canPrompt:false,permissionProvider:'deny',elicitationProvider:'none'}},model,router:{stream:()=>{throw Error('UNEXPECTED_MAIN_MODEL')}},tools:new ToolRegistry(),executionWorld:{shell:{execute:()=>{throw Error('UNEXPECTED_SHELL')}},planStorage:{}},instructionStorage:{},toolResultSpill:{},tokenAccounting:{estimateMessages:()=>0},backgroundSubagents:{start:()=>{throw Error('UNEXPECTED_BACKGROUND')}},
 createSessionTools:()=>new ToolRegistry(),
 createSessionExecution:async()=>{const id=composition;return {handle:{world:runtime.executionWorld},release:retain('execution',id)}},
 routerSessionCustomRouters:{register:()=>({release:retain('router',composition)})},
 pluginRuntime:{refresh:async()=>{log('plugin.refresh',{id:composition});if(name==='recreate-idle'&&process.env.MATRIX_DISPOSE_PREPARE==='1'&&composition===2){log('prepare.dispose-old.begin');await old.dispose('during-prepare');log('prepare.dispose-old.end');}if(name==='create-dispose-before-mcp'){refreshGate.entered.resolve();await refreshGate.release.promise;}},acquireSessionContributions:()=>({contributions:{generation:1,tools:[],commands:[],skills:[],prompts:[],mcpInstructions:[],routers:[],mcpServers:{},hooks:{}},release:retain('plugin',composition)})},
 mcpProvider:{acquireSharedRuntime:async()=>({tools:[],release:retain('sharedMcp',composition)}),getPerSessionServerSpecs:()=>[{id:'probe',transport:'stdio',command:'injected-no-child',perSession:true}]}};
 const options:any={resolveRuntime:()=>runtime,acquireRuntimeLease:()=>retain('project',++composition),acquirePermissionRuleSet:()=>({rules:{allow:[],deny:[],ask:[]},release:retain('permission',composition)}),getSessionOverride:()=>undefined,getGateway:()=>undefined,getSdkSessionConfig:()=>undefined,getSdkSessionPlugins:()=>[],getSdkOutputStyleContent:()=>undefined,getSdkMcpServers:()=>({}),getSdkThinking:()=>undefined,registerSdkConfigChangeHandler:()=>()=>{},registerSessionHookHandler:()=>()=>{},permissionMode:'default',pilotHome,env:{},now,preparePerSessionSpecs:({specs}:any)=>specs,getAlwaysOnToolNames:()=>[],shouldCollectFileArtifacts:()=>false,
 onDiagnostic:(message:any,error:any)=>log('diagnostic',{message,error:error?err(error):null}),
 mcpRuntimeFactory:(specs:any)=>{const id=++mcpCount;let release:any,stops=0;log('mcp.construct',{id,specs});const m={id,async start(){release=retain('mcp',id);log('mcp.start',{id,specs});return []},async listAllTools(){return []},async stop(){stops++;log('mcp.stop',{id,stops});await release?.()},get stops(){return stops}};mcps.push(m);return m;},
 continuations:{provider:'probe',providers:new SubagentProviderRegistry(),host:{bindParent:({parent}:any)=>{handles.push(parent);const id=hId(parent);log('handle.created',{id,state:parent.state});const release=retain('binding',id);return ()=>{void release()}}},manager:{drainDescendants:async(h:any,reason:string)=>{log('descendants.drain.begin',{id:hId(h),reason});if(h===old&&holdDrain){drainGate.entered.resolve();await drainGate.release.promise;}log('descendants.drain.end',{id:hId(h)})}}},
 testAgentLoopFactory:({seedState}:any)=>{log('loop.construct',{seedState:seedState??null});return {snapshotFileState:()=>seedState??{},async *run(input:any){log('loop.enter',{turnId:input.turnId,aborted:input.abortSignal.aborted});input.abortSignal.addEventListener('abort',()=>{log('loop.abort',{reason:input.abortSignal.reason});abortSeen.resolve()},{once:true});loopGate.entered.resolve();if(holdLoop)await loopGate.release.promise;log('loop.resume',{aborted:input.abortSignal.aborted});const message={role:'assistant',content:[{type:'text',text:'matrix answer'}]};if(!input.abortSignal.aborted)await input.onDurableMessage(message);const result={type:input.abortSignal.aborted?'aborted':'success',sessionId:input.sessionId,turnId:input.turnId,stopReason:input.abortSignal.aborted?'aborted_streaming':'completed',usage:{inputTokens:1,outputTokens:1},permissionDenials:[],turns:1,startedAt:now().toISOString(),completedAt:now().toISOString()};yield {type:'turn_completed',sessionId:input.sessionId,turnId:input.turnId,result};return {result,messages:input.messages}}};},
 createStorage:async()=>{
  const id=++storageCount;log('storage.create',{id});
  const s=createAgentProjectSessionStorage({projectRoot:workspace,pilotHome,sessionId:'session-745',now});
  (s.events as any).uuid=()=>`entry-${String(++entryId).padStart(4,'0')}`;
  const load=s.persistence.load.bind(s.persistence),checkpoint=s.projectionCheckpointStore.load.bind(s.projectionCheckpointStore),dispose=s.dispose.bind(s),restore=s.restore.bind(s);
  const record:any={id,s,disposeCalls:0,disposeDone:0,restores:[],entriesAtCheckpoint:null};storages.push(record);
  s.events.subscribe((entry:any)=>report.commits.push({ordinal:log('commit',{storage:id,entry}),storage:id,entry:clone(entry)}));
  s.persistence.load=async()=>{log('persistence.load.begin',{id});if(fence&&id===2)await atBoundary('beforeload');if(!negative&&['resume-load-reject','resume-rollback-reject','resume-release-reject','admission-read-reject'].includes(name)){throw Error('LOAD_BOOM')};if(!negative&&id===2&&name.startsWith('recreate-')&&name.includes('reject'))throw Error('RECREATE_LOAD_BOOM');if(id===2&&name==='recreate-running'&&process.env.MATRIX_WRITER_TIMING==='before-read'){log('probe.before-read.release-old');loopGate.release.resolve();await bounded(oldDisposal.promise,'old drained before replacement read');log('probe.before-read.old-drained');}const r=await load();log('persistence.load.end',{id,result:r});if(fence&&id===2)await atBoundary('afterload');return r;};
  s.projectionCheckpointStore.load=async(key:any)=>{record.entriesAtCheckpoint=s.events.snapshotState();log('checkpoint.load',{id,writer:record.entriesAtCheckpoint});if(name==='resume-checkpoint-reject'&&!negative)throw Error('CHECKPOINT_BOOM');return checkpoint(key);};
  s.restore=async()=>{log('storage.restore.begin',{id});try{const r=await restore();record.restores.push(clone(r));log('storage.restore.end',{id,result:r});return r;}catch(e){log('storage.restore.error',{id,error:err(e)});throw e}};
  s.dispose=async()=>{record.disposeCalls++;log('storage.dispose.begin',{id});await dispose();record.disposeDone++;log('storage.dispose.end',{id});if((fence?.[1]==='twice'&&!earlyTwice||fence?.[1]==='close'&&!earlyClose)&&id===1){oldDisposeReturn.entered.resolve();await oldDisposeReturn.release.promise;log('fence.old-dispose-return',{publications:fencePublications});}if(!negative&&(name==='resume-rollback-reject'||name==='recreate-rollback-reject'&&id===2))throw Error('STORAGE_DISPOSE_BOOM')};
  fs.mkdirSync(path.dirname(s.transcriptPath),{recursive:true});
  if(id===1){
   if(name.startsWith('recreate-')||fence){await s.transcript.recordAcceptedInput('session-745','seed-turn',[{role:'user',content:[{type:'text',text:' 已存历史 中文 '}]}]);await s.transcript.recordSessionMetadata('session-745','seed-turn',{title:'原题',titleSource:'user',firstPrompt:' 保留空白 '});await s.flush();}
   if(name==='resume-eisdir'&&!negative)fs.mkdirSync(s.transcriptPath);
   if(name==='resume-malformed')fs.writeFileSync(s.transcriptPath,negative?'':'{broken}\n');
   if(['resume-valid','resume-checkpoint-reject'].includes(name)||name==='resume-missing'&&negative){const seed={type:'session_metadata',sessionId:'session-745',turnId:'seed',sequence:1,createdAt:'2026-10-27T00:00:00.000Z',entryId:'seed-entry',parentEntryId:null,metadata:{firstPrompt:' 已存 中文 ',title:'原题',titleSource:'user'}};fs.writeFileSync(s.transcriptPath,JSON.stringify(seed)+'\n');}
   if(name==='resume-valid'&&negative)fs.writeFileSync(s.transcriptPath,'');
  }
  if(name==='same-key-concurrent'&&!negative||name==='create-factory-dispose'||name==='create-owner-shutdown'){
   const g=createGates[id-1];if(g){log('gate.enter',{label:g.label});g.entered.resolve();await g.release.promise;log('gate.leave',{label:g.label});}
  }
  return s;
 }};
 if(name==='admission-read-reject')options.sessionAdmission=async()=>undefined;
 const factory=new ProjectSessionFactory(options);
 const context={sessionKey:'session-745',projectKey:workspace,channelKey:'probe'};
 const observe=(p:Promise<any>,label:string)=>{const state:any={label,status:'pending'};report.results.push(state);return {state,promise:p.then(value=>{state.status='fulfilled';state.handle=hId(value);log('operation.fulfilled',{label,handle:state.handle});return value},e=>{state.status='rejected';state.error=err(e);log('operation.rejected',{label,error:state.error});return null})};};
 const files=(writing=true)=>snapshotFiles(caseDir,storages.map(r=>r.s.projectionCheckpointPath),writing);
 const snap=async(label:string)=>{await Promise.all(storages.map(r=>r.s.flush()));const state={label,active:clone(active),handles:handles.map((h,i)=>({id:i+1,state:h.state,inFlight:h.inFlight,session:read(()=>stateView(h.session.snapshot())),controllerSameAsFirst:read(()=>h.session.snapshot().abortController===handles[0].session.snapshot().abortController),reload:read(()=>reloadView(h))})),storages:storages.map(r=>({id:r.id,disposeCalls:r.disposeCalls,disposeDone:r.disposeDone,persistenceActive:r.s.persistenceBinding.active,writer:r.s.events.snapshotState(),entriesAtCheckpoint:r.entriesAtCheckpoint,restores:r.restores})),factoryMcp:{state:(factory as any).sessionMcpRuntimes.state,size:(factory as any).sessionMcpRuntimes.size},registry:router?{size:router.agents.size,handle:hId(router.agents.get(context.sessionKey)),hasSession:router.hasSession(context.sessionKey)}:null,results:clone(report.results),files:files(label!=='after-owned-cleanup')};report.snapshots.push({ordinal:log('snapshot',{label}),...clone(state)});};
 const mkRouter=()=>new SessionRouter({setupSession:async(_c:any,h:any)=>{if(process.env.MATRIX_DISPOSE_REASON==='1')h.addDisposeStartListener((reason:string)=>{log('handle.dispose-start',{id:hId(h),reason});});if(fence&&hId(h)===2)await atBoundary('publish');},createSession:(c:any)=>factory.createSession(c),recreateSession:(c:any,p:any)=>factory.recreateSession(c,p),idleSweepIntervalMs:0,idleSessionTimeoutMs:1e12,now,onSessionEvict:(key:any,reason:any)=>{log('router.evict',{key,reason});if(reason==='dirty_recreate'){evicted.resolve();if(fence?.[1]==='twice'&&++fencePublications===2){log('fence.final-publication');oldDisposeReturn.release.resolve();}}}});
 try{
  if(fence){
   const [state,event,timing]=fence;
   router=mkRouter();old=await bounded(router.getOrCreate(context),'fence old create');
   let running:any;
   if(state==='quick'||state==='never'){
    holdLoop=true;
    running=observe((async()=>{for await(const e of old.submit({type:'text',text:'fence input'},{turnId:'turn-old'})){log('fence.old.event',{event:e});}})(),'fence-old-turn');
    await bounded(loopGate.entered.promise,'fence old running');
   }else if(state==='disposing'){
    holdDrain=true;oldDisposal=observe(old.dispose('fence-prior-dispose'),'fence-old-dispose');
    await bounded(drainGate.entered.promise,'fence disposing');
   }
   router.markSessionDirty(context.sessionKey);
   const first=observe(router.getOrCreate(context),'fence-first');
   await bounded(fenceGate.entered.promise,'fence boundary '+timing);
   await snap('fence-boundary');
   let concurrent:any;
   if(event==='twice'){
    concurrent=observe(router.getOrCreate(context),'fence-second');
    // The second TS replacement publishes first, while candidate 1 is held.
    await bounded(evicted.promise,'fence second published');
   }else if(event==='close')concurrent=observe(router.close(context.sessionKey),'fence-close');
   else if(event==='shutdown')concurrent=observe(router.shutdown(),'fence-shutdown');
   await snap('fence-event');
   if(event==='twice'&&earlyTwice||event==='close'&&earlyClose){assert.equal(state,'idle');await bounded(concurrent.promise,'early concurrent returned before first publish');}

   // Never means unreleased throughout the measured prefix, not a probe timeout.
   // Release only for deterministic owned cleanup; Rust tests exercise its bound.
   if(state==='quick')loopGate.release.resolve();
   fenceGate.release.resolve();
   if(event==='recreate')await bounded(evicted.promise,'fence first published');
   await snap('fence-before-cleanup');
   loopGate.release.resolve();drainGate.release.resolve();
   await bounded(first.promise,'fence first completed');
   if(event==='close'){log('fence.first-before-close-return');oldDisposeReturn.release.resolve();}
   if(concurrent)await bounded(concurrent.promise,'fence concurrent completed');
   if(running)await bounded(running.promise,'fence old finished');
   if(oldDisposal)await bounded(oldDisposal.promise,'fence old disposed');
   await snap('result');
  }else if(name.startsWith('resume-')||name==='admission-read-reject'){
   const o=observe(factory.createSession(context),'create');await bounded(o.promise,'resume');await snap('result');
  }else if(['same-key-cached','same-key-concurrent'].includes(name)){
   router=mkRouter();const a=observe(router.getOrCreate(context),'first');
   if(name==='same-key-concurrent'&&!negative){await bounded(createGates[0].entered.promise,'first create entered');const b=observe(router.getOrCreate(context),'second');await bounded(createGates[1].entered.promise,'second create entered');await snap('both-creating');createGates[0].release.resolve();await bounded(a.promise,'first publish');createGates[1].release.resolve();await bounded(b.promise,'second publication');}
   else{await bounded(a.promise,'first');if(name==='same-key-cached'&&negative){router.markSessionDirty(context.sessionKey);}await bounded(observe(router.getOrCreate(context),'second').promise,'second');}await snap('result');
  }else if(name==='create-dispose-before-mcp'){
   const o=observe(factory.createSession(context),'create');await bounded(refreshGate.entered.promise,'refresh entered');if(!negative)await factory.dispose();await snap('factory-disposed-before-register');refreshGate.release.resolve();await bounded(o.promise,'create post dispose');await snap('result');
  }else if(name==='create-factory-dispose'||name==='create-owner-shutdown'){
   if(name==='create-owner-shutdown')router=mkRouter();const o=observe(router?router.getOrCreate(context):factory.createSession(context),'create');await bounded(createGates[0].entered.promise,'storage held');
   let d:any;if(!negative)d=observe(router?router.shutdown():factory.dispose(),'dispose');if(name==='create-factory-dispose'&&d)await bounded(d.promise,'factory dispose');await snap('dispose-during-create');createGates[0].release.resolve();await bounded(o.promise,'create completion');if(d)await bounded(d.promise,'dispose completion');await snap('result');
  }else{
   router=mkRouter();old=await bounded(router.getOrCreate(context),'old create');

   if(name==='recreate-running'){
    holdLoop=true;const run=observe((async()=>{for await(const event of old.submit({type:'text',text:'pending input'},{turnId:'turn-old'})){report.events.push({ordinal:log('agent.event',{handle:1,event}),event:clone(event)})}})(),'old-turn');
    await bounded(loopGate.entered.promise,'loop entered');
    oldDisposal=observe(old.dispose('explicit-old-drain'),'old-dispose');
    await bounded(abortSeen.promise,'old loop abort observed');
    if(negative){loopGate.release.resolve();await bounded(run.promise,'negative finish turn');await bounded(oldDisposal.promise,'negative drain');}
   }
   if(name==='recreate-draining'){holdDrain=true;if(negative){holdDrain=false;}}
   if(name==='recreate-disposed'&&!negative)await old.dispose('prior-dispose');
   if(name==='recreate-idle'&&process.env.MATRIX_MEMORY_ONLY==='1'){const snapshot=old.session.snapshotForRuntimeReload.bind(old.session);old.session.snapshotForRuntimeReload=()=>{const r=snapshot();r.state.messages=[{role:'assistant',content:[{type:'text',text:'MEMORY_ONLY'}]}];r.state.usage={inputTokens:991,outputTokens:992,totalTokens:1983};r.state.permissionDenials=[{toolName:'MEMORY_ONLY',toolCallId:'memory-only'}];log('injected.reload',{state:stateView(r.state)});return r;};}await snap('before-recreate');
   if(name==='recreate-running'||name==='recreate-disposed'){
    const o=observe(factory.recreateSession(context,old.session),'recreate');const replacement=await bounded(o.promise,'direct recreate');await snap('replacement-ready');loopGate.release.resolve();if(oldDisposal)await bounded(oldDisposal.promise,'old disposal');if(process.env.MATRIX_WRITER_TIMING){await snap('old-drained');holdLoop=false;await bounded((async()=>{for await(const event of replacement.submit({type:'text',text:'replacement input'},{turnId:'turn-new'})){log('replacement.event',{event});}})(),'replacement turn');await snap('after-new-turn');await bounded(observe(factory.createSession(context),'resume-after-both-writers').promise,'resume both writers');}await snap('result');
   }else{
    if(!(name==='recreate-idle'&&negative))router.markSessionDirty(context.sessionKey);const o=observe(router.getOrCreate(context),'recreate');
    if(name==='recreate-draining'&&!negative){await bounded(drainGate.entered.promise,'drain entered');await bounded(evicted.promise,'replacement published');await snap('replacement-published-old-draining');await bounded(observe(router.getOrCreate(context),'cached-during-drain').promise,'cached replacement');await snap('second-returned-old-draining');drainGate.release.resolve();}
    await bounded(o.promise,'recreate');await snap('result');
   }
  }
  report.status='OBSERVED';
 }catch(e){report.status='INVALID';report.error=err(e);report.stack=(e as any).stack;}
 finally{
  log('cleanup.begin');for(const g of gates)g.release.resolve();
  try{if(router)await bounded(router.shutdown(),'cleanup router');for(const h of handles)await bounded(h.dispose('probe-cleanup'),'cleanup handle');await factory.dispose();await snap('after-owned-cleanup');
   // Preserve pre-cleanup leak evidence above, then release orphan storage to end this isolated probe safely.
   for(const r of storages)if(r.s.persistenceBinding.active){log('probe.orphan-storage.cleanup',{id:r.id});await r.s.dispose().catch(()=>{});}report.cleanup={active:clone(active),releases,handles:handles.map(h=>h.state),mcpStops:mcps.map(m=>({id:m.id,stops:m.stops})),storageBindings:storages.map(r=>r.s.persistenceBinding.active)};
  }catch(e){report.cleanupError=err(e);report.status='INVALID';}
  report.forbidden=clone(forbidden);report.finalFiles=files(false);
  for(const level of ['warn','error','debug'] as const)console[level]=savedConsole[level];
  fs.writeFileSync(path.join(outDir,name+'.json'),JSON.stringify(report,null,2)+'\n',{flag:'wx'});process.stdout.write(`${report.status} ${name}${report.error?' '+report.error.message:''}\n`);index.push({name,status:report.status});
 }
}
fs.writeFileSync(path.join(outDir,'index.json'),JSON.stringify({baseline,node:process.version,negative,runDir:path.relative(ROOT,runDir),results:index,forbidden},null,2)+'\n',{flag:'wx'});
if(index.some(r=>r.status==='INVALID')||forbidden.length)process.exitCode=1;

} finally {
 if(!parentOwned)fs.rmSync(runDir,{recursive:true,force:true});
}
