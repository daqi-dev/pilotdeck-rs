// #1020 TS goldens: real SessionRouter/ProjectSessionFactory with a disposed
// previous handoff. The four cases pin the TS side of the state x event x
// timing matrix in _tmp/20261026-issue-1020/plan.md:
//   router-disposed-previous       cell 1  router keeps rejecting, no recovery
//   router-disposed-second-dirty   cell 1b re-marking dirty still rejects
//   direct-recreate-disposed       factory-level rejection (matches the
//                                  session-factory recreate-disposed golden)
//   router-prepare-disposal        cell 3  the old disposal completes inside
//                                  the replacement's prepare hook; the
//                                  post-prepare snapshot rejects
// Only the model, tool registry, loop factory and the prepare hook are
// deterministic probes. Output is created exclusively (flag 'wx') in a fresh
// recursive directory; the work directories are removed before writing.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
const base=path.resolve(process.env.PDRS_BASELINE!);
const output=path.resolve(process.argv[2]);
assert.equal(process.version,'v22.22.0','#1020 Node version');
const sourceCommit=assertBaseline(base);
const sourceSha256=await baselineSourceHashes(base,['src/cli/ProjectSessionFactory.ts','src/gateway/SessionRouter.ts']);
const {ProjectSessionFactory}=await import(`${base}/src/cli/ProjectSessionFactory.ts`);
const {SessionRouter}=await import(`${base}/src/gateway/SessionRouter.ts`);
const {ToolRegistry}=await import(`${base}/src/tool/registry/ToolRegistry.ts`);
const {SubagentProviderRegistry}=await import(`${base}/src/agent/sub/SubagentProviderRegistry.ts`);
const {createAgentProjectSessionStorage}=await import(`${base}/src/session/storage/ProjectSessionStorage.ts`);
function deferred(){let resolve!:(x?:any)=>void;const promise=new Promise<any>(r=>{resolve=r});return {promise,resolve};}
async function bounded<T>(promise:Promise<T>,label:string):Promise<T>{
 let timer:any;
 try{return await Promise.race([promise,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(Error('#1020 TS gate timeout: '+label)),5000)})]);}
 finally{clearTimeout(timer!);}
}
const err=(e:any):any=>({name:e?.name??typeof e,message:e?.message??String(e),...(e?.code?{code:e.code}:{}),...(e?.errors?{errors:[...e.errors].map(err)}:{})});
const observe=(p:Promise<any>)=>{const o:any={status:'pending'};return {o,promise:p.then((v:any)=>{o.status='fulfilled';return v},(e:any)=>{o.status='rejected';o.error=err(e);return null})};};
const context={sessionKey:'session-1020',channelKey:'probe'};

function makeFactory(prepareHook?:(composition:number)=>Promise<void>|void){
 const caseRoot=fs.mkdtempSync(path.join(output,'work-'));
 const workspace=path.join(caseRoot,'workspace'),pilotHome=path.join(caseRoot,'home');
 fs.mkdirSync(workspace,{recursive:true});
 let composition=0,storageCount=0,entryId=0;
 const now=()=>new Date(Date.UTC(2026,9,30)+storageCount);
 const model={getCapabilities:()=>({maxContextTokens:32000,maxOutputTokens:1024}),getMultimodal:()=>undefined,getProviderProtocol:()=>'openai',complete:async()=>({content:[]})};
 const runtime:any={projectRoot:workspace,projectStorage:{projectRoot:workspace,pilotHome},snapshot:{config:{agent:{model:{provider:'probe',model:'deterministic'},maxContextTokens:32000},model:{providers:{}},gateway:{maxPerSessionMcpInstances:20}}},profile:{interaction:{canPrompt:false,permissionProvider:'deny',elicitationProvider:'none'}},model,router:{stream:()=>{throw Error('UNEXPECTED_MAIN_MODEL')}},tools:new ToolRegistry(),executionWorld:{shell:{execute:()=>{throw Error('UNEXPECTED_SHELL')}},planStorage:{}},instructionStorage:{},toolResultSpill:{},tokenAccounting:{estimateMessages:()=>0},backgroundSubagents:{start:()=>{throw Error('UNEXPECTED_BACKGROUND')}},
  createSessionTools:()=>new ToolRegistry(),
  createSessionExecution:async()=>({handle:{world:runtime.executionWorld},release:async()=>{}}),
  routerSessionCustomRouters:{register:()=>({release:async()=>{}})},
  pluginRuntime:{refresh:async()=>{composition++;await prepareHook?.(composition);},acquireSessionContributions:()=>({contributions:{generation:1,tools:[],commands:[],skills:[],prompts:[],mcpInstructions:[],routers:[],mcpServers:{},hooks:{}},release:async()=>{}})},
  mcpProvider:{acquireSharedRuntime:async()=>({tools:[],release:async()=>{}}),getPerSessionServerSpecs:()=>[]}};
 const factory=new ProjectSessionFactory({resolveRuntime:()=>runtime,acquireRuntimeLease:()=>async()=>{},acquirePermissionRuleSet:()=>({rules:{allow:[],deny:[],ask:[]},release:async()=>{}}),getSessionOverride:()=>undefined,getGateway:()=>undefined,getSdkSessionConfig:()=>undefined,getSdkSessionPlugins:()=>[],getSdkOutputStyleContent:()=>undefined,getSdkMcpServers:()=>({}),getSdkThinking:()=>undefined,registerSdkConfigChangeHandler:()=>()=>{},registerSessionHookHandler:()=>()=>{},permissionMode:'default',pilotHome,env:{},now,preparePerSessionSpecs:({specs}:any)=>specs,getAlwaysOnToolNames:()=>[],shouldCollectFileArtifacts:()=>false,
  onDiagnostic:()=>{},
  mcpRuntimeFactory:()=>({async start(){return []},async listAllTools(){return []},async stop(){}}),
  continuations:{provider:'probe',providers:new SubagentProviderRegistry(),host:{bindParent:()=>()=>{}},manager:{drainDescendants:async()=>{}}},
  testAgentLoopFactory:()=>({snapshotFileState:()=>({}),async *run(){}}),
  createStorage:async()=>{
   const id=++storageCount;
   const s=createAgentProjectSessionStorage({projectRoot:workspace,pilotHome,sessionId:'session-1020',now});
   (s.events as any).uuid=()=>`entry-${String(++entryId).padStart(4,'0')}`;
   fs.mkdirSync(path.dirname(s.transcriptPath),{recursive:true});
   if(id===1){await s.transcript.recordAcceptedInput('session-1020','seed-turn',[{role:'user',content:[{type:'text',text:' 已存历史 '}]}]);await s.flush();}
   return s;}}) as any;
 const mkRouter=()=>new SessionRouter({createSession:(c:any)=>factory.createSession(c),recreateSession:(c:any,p:any)=>factory.recreateSession(c,p),idleSweepIntervalMs:0,idleSessionTimeoutMs:1e12,now,onSessionEvict:(key:any,reason:any)=>{}}) as any;
 return {factory,mkRouter};
}

const cases:any[]=[];
function record(name:string,observations:any){cases.push({name,node:process.version,observations});}

// cell 1: the router's cached handle has completed its disposal already.
{
 const m=makeFactory();
 const router=m.mkRouter();
 const first=await bounded(router.getOrCreate({...context}),'first create');
 const out:any={firstState:first.state,registrySizeBefore:router.agents.size};
 await bounded(first.dispose('probe-prior-dispose'),'old dispose completes');
 out.afterDisposeState=first.state;
 out.dirty1=router.markSessionDirty(context.sessionKey,'#1020 probe dirty');
 const second=observe(router.getOrCreate({...context}));
 await bounded(second.promise,'second getOrCreate');
 out.secondError=second.o.error??null;
 const third=observe(router.getOrCreate({...context}));
 await bounded(third.promise,'third getOrCreate');
 out.thirdError=third.o.error??null;
 out.registrySizeAfter=router.agents.size;
 await router.shutdown();
 record('router-disposed-previous',out);
}
// cell 1b: re-marking dirty after the rejection still hands the dead session over.
{
 const m=makeFactory();
 const router=m.mkRouter();
 const first=await bounded(router.getOrCreate({...context}),'first create');
 const out:any={};
 await bounded(first.dispose('probe-prior-dispose'),'old dispose completes');
 out.dirty1=router.markSessionDirty(context.sessionKey,'#1020 probe dirty');
 const second=observe(router.getOrCreate({...context}));
 await bounded(second.promise,'second getOrCreate');
 out.secondError=second.o.error??null;
 out.dirty2=router.markSessionDirty(context.sessionKey,'#1020 probe dirty again');
 const third=observe(router.getOrCreate({...context}));
 await bounded(third.promise,'third getOrCreate');
 out.thirdError=third.o.error??null;
 await router.shutdown();
 record('router-disposed-second-dirty',out);
}
// factory-level self-check: direct recreate with a fully disposed previous.
{
 const m=makeFactory();
 const old=await bounded(m.factory.createSession({...context}),'old create');
 const out:any={oldState:old.state};
 await bounded(old.dispose('probe-prior-dispose'),'old dispose completes');
 const recreate=observe(m.factory.recreateSession({...context},old.session));
 await bounded(recreate.promise,'direct recreate');
 out.recreateError=recreate.o.error??null;
 await m.factory.dispose();
 record('direct-recreate-disposed',out);
}
// cell 3: the old disposal completes inside the replacement's prepare hook
// (refresh, composition 2), before the post-prepare snapshot is taken. This
// mirrors the prepareDisposal golden of session-factory (MATRIX_DISPOSE_PREPARE)
// at the router level: `old` is captured by the closure, the hook awaits its
// dispose before letting prepare finish, and the post-prepare
// snapshotForRuntimeReload rejects.
{
 const entered=deferred(),release=deferred();
 let old:any=null,disposalObserved:any=null;
 const m=makeFactory(async(composition:number)=>{
  if(composition===2&&old){
   const disposed=observe(old.dispose('during-prepare'));
   await bounded(disposed.promise,'prepare dispose completes');
   disposalObserved=disposed.o;
   entered.resolve();
   await release.promise;
  }
 });
 const router=m.mkRouter();
 old=await bounded(router.getOrCreate({...context}),'old create');
 const out:any={oldState:old.state};
 router.markSessionDirty(context.sessionKey,'#1020 probe dirty');
 const second=observe(router.getOrCreate({...context}));
 await bounded(entered.promise,'prepare hook entered');
 release.resolve();
 await bounded(second.promise,'second getOrCreate');
 out.secondError=second.o.error??null;
 out.disposalInsidePrepare=disposalObserved?.status??null;
 await router.shutdown();
 record('router-prepare-disposal',out);
}

fs.mkdirSync(output,{recursive:true});
fs.rmSync(path.join(output,'work-'),{recursive:true,force:true,maxRetries:5});
fs.writeFileSync(path.join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,cases},null,2)+'\n',{flag:'wx'});
console.log('PASS: #1020 real TS SessionRouter disposed-previous probes',cases.length,'cases');
