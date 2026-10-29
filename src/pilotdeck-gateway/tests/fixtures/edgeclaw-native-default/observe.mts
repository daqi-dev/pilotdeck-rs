// #778: execute the real source factory, bundle and maintenance controller.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdirSync,mkdtempSync,readFileSync,readdirSync,rmSync,statSync,writeFileSync} from 'node:fs';
import {register} from 'node:module';
import {tmpdir} from 'node:os';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {DatabaseSync} from 'node:sqlite';
import {stub} from '../edgeclaw-llm/stub.mts';
export const ENTRY='src/context/memory/createEdgeClawMemoryProviderFromConfig.ts';
const CORE='src/context/memory/edgeclaw-memory-core/src';
export const SOURCES=[ENTRY,'src/cli/ProjectMemoryBundle.ts','src/cli/ProjectMemoryMaintenanceController.ts',`${CORE}/index.ts`,`${CORE}/service.ts`,`${CORE}/core/storage/sqlite.ts`,`${CORE}/core/pipeline/heartbeat.ts`,`${CORE}/core/retrieval/reasoning-loop.ts`,`${CORE}/core/skills/llm-extraction.ts`];
export const NOW='2026-10-22T00:00:00.000Z';
export const RANDOM=[0.25,0.5,0.75,0.125,0.875,0.375,0.625,0.0625];
export const M21=['staged-dispose','disposing-dispose','disposed-dispose','staged-maintenance','disposing-maintenance','disposed-maintenance','staged-retrieve','disposing-retrieve','disposed-retrieve','generation-maintenance','generation-retrieve','never-maintenance','cancelled-retrieve'];
const ENV_KEYS=['EDGECLAW_MEMORY_MODEL','EDGECLAW_MEMORY_PROVIDER','EDGECLAW_MEMORY_BASE_URL','EDGECLAW_MEMORY_API_KEY','EDGECLAW_MEMORY_API_TYPE','OPENAI_MODEL','OPENAI_BASE_URL','OPENAI_API_KEY','OPENCLAW_HOME','OPENCLAW_STATE_DIR','OPENCLAW_CONFIG_PATH'];
export function encode(value:any):any {
  if(value===undefined)return {$undefined:true};
  if(typeof value==='number'&&!Number.isFinite(value))return {$number:String(value)};
  if(Array.isArray(value))return value.map(encode);
  if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,encode(v)]));
  return value;
}
function deferred(){let resolve!:(v?:any)=>void,reject!:(v:any)=>void;const promise=new Promise<any>((a,b)=>{resolve=a;reject=b});return{promise,resolve,reject};}
async function bounded<T>(promise:Promise<T>,label:string):Promise<T>{let timer:any;try{return await Promise.race([promise,new Promise<T>((_,reject)=>{timer=setTimeout(()=>reject(Error(`#778 timeout: ${label}`)),10000)})]);}finally{clearTimeout(timer);}}
const pump=()=>new Promise<void>(r=>setImmediate(r));
const errorShape=(error:any)=>({name:error?.name??null,message:error?.message??String(error),...(error?.code?{code:error.code}:{})});
const response=(value:any)=>({status:200,body:JSON.stringify({choices:[{message:{content:JSON.stringify(value)}}]})});
const messages=[{role:'user',content:[{type:'text',text:'Project Aurora must use Rust and retain the blue deployment rule.'}]},{role:'assistant',content:[{type:'text',text:'I will retain the blue deployment rule for Project Aurora.'}]}];
let installed=false;
export async function load(baseline:string){
  if(!installed){
    const moduleSource=`let core;export function initialize(data){core=data.core}export async function resolve(specifier,context,next){return next(specifier==='edgeclaw-memory-core'?core:specifier,context)}`;
    register('data:text/javascript;base64,'+Buffer.from(moduleSource).toString('base64'),{parentURL:import.meta.url,data:{core:pathToFileURL(join(baseline,CORE,'index.ts')).href}});
    installed=true;
  }
  const [factory,bundle,controller,service]=await Promise.all([ENTRY,SOURCES[1],SOURCES[2],`${CORE}/service.ts`].map(p=>import(pathToFileURL(join(baseline,p)).href)));
  return {factory:factory.createEdgeClawMemoryProviderFromConfig,Bundle:bundle.ProjectMemoryBundle,Controller:controller.ProjectMemoryMaintenanceController,Service:service.EdgeClawMemoryService};
}
function normalized(value:any,root:string,workspaces:string[],origin?:string):any {
  if(typeof value==='string'){
    for(const workspace of workspaces){const slug=createHash('sha1').update(resolve(workspace)).digest('hex').slice(0,10);value=value.split('/workspaces/'+slug).join('/workspaces/$HASH');}
    if(origin)value=value.split(origin).join('$HTTP');
    return value.split(root).join('$ROOT');
  }
  if(Array.isArray(value))return value.map(v=>normalized(v,root,workspaces,origin));
  if(value&&typeof value==='object')return Object.fromEntries(Object.entries(value).map(([k,v])=>[k,normalized(v,root,workspaces,origin)]));
  return value;
}
export function database(path:string){
  const db=new DatabaseSync(path,{readOnly:true});
  const quote=(name:string)=>'"'+name.replaceAll('"','""')+'"';
  try{
    const names=db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").all().map(row=>String(row.name));
    return Object.fromEntries(names.map(table=>{
      const columns=db.prepare(`PRAGMA table_info(${quote(table)})`).all().map(row=>String(row.name));
      const rows=db.prepare(`SELECT * FROM ${quote(table)} ORDER BY ${columns.map(quote).join(',')}`).all();
      return [table,rows];
    }));
  }finally{db.close();}
}
export function tree(root:string,base=root):any[]{
  if(!statSync(root,{throwIfNoEntry:false}))return [];
  return readdirSync(root).sort().flatMap(name=>{const p=join(root,name);return statSync(p).isDirectory()?tree(p,base):[{path:p.slice(base.length+1),text:readFileSync(p,'utf8')}];});
}
function gateFetch(){
  const original=globalThis.fetch,entered=deferred(),release=deferred();let count=0;
  globalThis.fetch=(async(input:any,init:any)=>{if(++count===1){entered.resolve();await release.promise;}return original(input,init);}) as any;
  return {entered:entered.promise,release:()=>release.resolve(),restore:()=>{globalThis.fetch=original}};
}
function telemetry(events:any[]){return {trackFeatureLoopStage(event:any){events.push({kind:'stage',event});},trackError(error:any,event:any){events.push({kind:'error',error:errorShape(error),event});}};}
function spawn(api:any,root:string,origin:string,events:any[],config:any={},label='old',workspaceOverride?:string){
  const workspace=workspaceOverride??join(root,label,'workspace'),store=join(root,label,'store');
  const options={projectRoot:workspace,config:{enabled:true,provider:'edgeclaw',rootDir:store,model:'fixture/model',schedule:{autoIndexIntervalMinutes:1,autoDreamIntervalMinutes:999999},...config},modelConfig:{providers:{fixture:{protocol:'openai',url:origin+'/v1',apiKey:'fixture-key'}}},now:()=>new Date(NOW),telemetry:telemetry(events)};
  const bundle=new api.Bundle(options),resources=bundle.stage(),service=resources.memoryService;
  assert.ok(service instanceof api.Service,'#778 factory must construct bound source service, not lib');
  let closes=0;const close=service.close.bind(service);service.close=()=>{closes++;return close();};
  const runtime={projectRoot:workspace,...resources};
  return {bundle,resources,service,runtime,workspace,store,closes:()=>closes};
}
function retrieve(generation:any,signal?:AbortSignal,query='remember the blue deployment rule'){
  return generation.resources.memory.retrieve({query,sessionId:'session-old',projectRoot:generation.workspace,recentMessages:[],signal});
}
async function capture(generation:any,session='session-old'){
  await generation.resources.memory.captureTurn({sessionId:session,messages,errored:false});
}
function controller(api:any,current:()=>any,events:any[]){return new api.Controller({resolveRuntime(){return current().runtime},telemetry:telemetry(events),onDiagnostic(message:any,error:any){events.push({kind:'diagnostic',message,error:errorShape(error)});}});}
async function settled(controller:any,runtime:any){
  await bounded((async()=>{for(;;){const pending=controller.states.get(runtime)?.inFlight;if(!pending)return;await pending;await pump();}})(),'maintenance settlement');
}
async function lifecycle(api:any,base:string,name:string,reset:()=>void){
  const root=join(base,name);mkdirSync(root,{recursive:true});
  const script=[response({route:'none',should_store:false,labels:[]})],server=await stub(script,()=>Date.parse(NOW)),events:any[]=[];
  const old=spawn(api,root,server.origin,events);let current=old,newer:any,gate:any,pending:Promise<any>|undefined;
  const c=controller(api,()=>current,events);reset();
  const observation:any={name,state:name.split('-')[0],events};
  try{
    if(name.endsWith('-dispose')){
      const first=old.bundle.dispose();
      if(name==='disposed-dispose')await bounded(first,name);
      const second=name==='staged-dispose'?first:old.bundle.dispose();
      observation.samePromise=first===second;
      await bounded(first,name);await bounded(old.bundle.dispose(),name);
      try{old.bundle.stage();}catch(e){observation.stageAfter=errorShape(e)}
    }else if(name==='disposed-maintenance'){
      await old.bundle.dispose();c.schedule(old.workspace);await settled(c,old.runtime);
    }else if(name==='disposed-retrieve'){
      await old.bundle.dispose();observation.result=await bounded(retrieve(old),name);
    }else{
      const maintenance=name.includes('maintenance');
      if(maintenance){await capture(old);old.service.repository.setPipelineState("autoIndexAnchorAt","2026-10-21T23:58:00.000Z");}
      gate=gateFetch();
      const abort=new AbortController();
      if(maintenance)c.schedule(old.workspace);else pending=retrieve(old,abort.signal);
      await bounded(gate.entered,name+' LLM entered');
      observation.llmEntered=true;
      if(name.startsWith('generation-')){
        newer=spawn(api,root,server.origin,events,{},'new',old.workspace);current=newer;
        await capture(newer,'session-new');
        if(maintenance){newer.service.repository.setPipelineState("autoIndexAnchorAt","2026-10-21T23:58:00.000Z");c.schedule(newer.workspace);await settled(c,newer.runtime);}
        const before=database(newer.service.dbPath);
        gate.release();
        if(maintenance)await settled(c,old.runtime);else{observation.result=await bounded(pending!,name);await capture(old);}
        const after=database(newer.service.dbPath);
        assert.deepEqual(after,before,'#778 old generation must not write new database');
        observation.newDatabaseUnchanged=true;observation.oldDatabase=database(old.service.dbPath);observation.newDatabase=after;
      }else{
        if(name==='cancelled-retrieve'){abort.abort('fixture parent cancelled');observation.parentAborted=abort.signal.aborted;}
        if(name.startsWith('disposing-')||name==='never-maintenance'||name==='cancelled-retrieve'){
          await bounded(old.bundle.dispose(),name+' dispose while LLM pending');
          observation.disposeCompletedBeforeRelease=true;
          if(name==='never-maintenance'){
            let predecessorSettled=false;
            const predecessor=c.states.get(old.runtime)?.inFlight;
            assert.ok(predecessor,'#778 predecessor must be in flight');
            predecessor.then(()=>{predecessorSettled=true});
            newer=spawn(api,root,server.origin,events,{},'new',old.workspace);current=newer;
            await capture(newer,'session-new');
            newer.service.repository.setPipelineState('autoIndexAnchorAt','2026-10-21T23:58:00.000Z');
            c.schedule(newer.workspace);await settled(c,newer.runtime);
            assert.equal(predecessorSettled,false,'#778 new generation finishes while predecessor never completes');
            observation.successorCompletedBeforePredecessor=true;
            observation.newDatabase=database(newer.service.dbPath);
          }
        }
        gate.release();
        if(maintenance)await settled(c,old.runtime);else observation.result=await bounded(pending!,name);
      }
    }
    await bounded(old.bundle.dispose(),name+' cleanup');
    if(newer)await bounded(newer.bundle.dispose(),name+' new cleanup');
    observation.closeCount=old.closes();assert.equal(observation.closeCount,1,'#778 native bundle close exactly once');
    observation.requestCount=server.requests.length;observation.requests=server.requests;
    if(newer)observation.newCloseCount=newer.closes();
    return normalized(encode(observation),root,[old.workspace,...(newer?[newer.workspace]:[])],server.origin);
  }finally{gate?.release();gate?.restore();await old.bundle.dispose();if(newer)await newer.bundle.dispose();await server.close();}
}
export function configSpecs(){
  const on={enabled:true,provider:'edgeclaw'};
  return [
    {name:'missing'}, {name:'disabled',config:{...on,enabled:false}}, {name:'enabled-missing-provider',config:{enabled:true}}, {name:'other-provider',config:{...on,provider:'other'}},
    {name:'enabled-no-model',config:on}, {name:'explicit-root',config:{...on,rootDir:'$ROOT/custom'}}, {name:'root-lone-surrogate',config:{...on,rootDir:'$ROOT/root-\ud800'}}, {name:'root-is-file',config:{...on,rootDir:'$ROOT/not-a-directory'},rootIsFile:true}, {name:'sqlite-corrupt',config:{...on,rootDir:'$ROOT/store'},sqliteFault:'corrupt'}, {name:'sqlite-directory',config:{...on,rootDir:'$ROOT/store'},sqliteFault:'directory'},
    {name:'agent-fallback',config:on,agentModel:'p/agent'}, {name:'model-precedence',config:{...on,model:'p/memory'},agentModel:'p/agent'},
    {name:'empty-model-fallback',config:{...on,model:''},agentModel:'p/agent'}, {name:'invalid-ref',config:{...on,model:'invalid'}},
    {name:'empty-provider',config:{...on,model:'/model'}}, {name:'empty-model-id',config:{...on,model:'p/'}}, {name:'nested-model-id',config:{...on,model:'p/model/nested'}},
    ...['openai','openai-responses','anthropic','google','unknown'].map(protocol=>({name:'protocol-'+protocol,config:{...on,model:'p/model'},modelConfig:{providers:{p:{protocol,url:'http://127.0.0.1:1/v1',apiKey:'fixture-key'}}}})),
    {name:'api-type-override',config:{...on,model:'p/model',apiType:'google'},modelConfig:{providers:{p:{protocol:'openai',url:'http://127.0.0.1:1/v1',apiKey:'fixture-key'}}}},
    {name:'all-options',config:{...on,model:'p/model',captureStrategy:'full_session',includeAssistant:false,maxMessageChars:42,heartbeatBatchSize:7,schedule:{reasoningMode:'accuracy_first',autoIndexIntervalMinutes:4,autoDreamIntervalMinutes:9}}},
  ];
}
function configuration(api:any,base:string,spec:any){
  const root=join(base,'config',spec.name),workspace=join(root,'workspace');mkdirSync(workspace,{recursive:true});
  const config=spec.config&&{...spec.config,...(spec.config.rootDir?{rootDir:spec.config.rootDir.replace('$ROOT',root)}:{})};
  const options={projectRoot:workspace,config,modelConfig:spec.modelConfig,agentModel:spec.agentModel};
  if(spec.rootIsFile)writeFileSync(config.rootDir,'fixture occupies rootDir',{flag:'wx'});
  const data=config&&join(config.rootDir??'', 'workspaces',createHash('sha1').update(workspace).digest('hex').slice(0,10));
  if(spec.sqliteFault){mkdirSync(data,{recursive:true});if(spec.sqliteFault==='directory')mkdirSync(join(data,'control.sqlite'));else writeFileSync(join(data,'control.sqlite'),'not a database'.repeat(256),{flag:'wx'});}
  let created:any;
  try{created=api.factory(options);}catch(error){return normalized(encode({name:spec.name,input:spec,created:false,error:errorShape(error),...(spec.sqliteFault?{files:[...tree(join(data,'memory')).map(f=>({...f,path:'memory/'+f.path})),...tree(join(config.rootDir,'global')).map(f=>({...f,path:'global/'+f.path}))]}:{})}),base,[workspace]);}
  if(!created)return {name:spec.name,input:encode(spec),created:false};
  const {service,provider}=created;assert.ok(service instanceof api.Service,'#778 config source service');
  try{return normalized(encode({name:spec.name,input:spec,created:true,service:{workspaceDir:service.workspaceDir,dataDir:service.dataDir,dbPath:service.dbPath,memoryDir:service.memoryDir,source:service.source,captureStrategy:service.captureStrategy,includeAssistant:service.includeAssistant,maxMessageChars:service.maxMessageChars,heartbeatBatchSize:service.indexer.batchSize,defaultIndexingSettings:service.defaultIndexingSettings,extractorConfig:service.extractor.config},provider:{source:provider.options.source,retrievalMode:provider.options.retrievalMode}}),base,[workspace]);}finally{service.close();}
}
const E2E_SCRIPT=[
  response({route:'none'}),
  response({should_store:true,labels:[{type:'project',reason:'durable project constraint',evidence:'blue deployment rule'}]}),
  response({skip:false,name:'Aurora note',description:'blue deployment rule',markdown:'## Summary\nProject Aurora retains the blue deployment rule.'}),
  response({route:'project'}),response({selected_ids:[]}),
];
function persistence(generation:any,root:string,origin:string){
  const fileTree=tree(generation.service.memoryDir).map(file=>({...file,path:'memory/'+file.path}));
  const globalTree=tree(join(generation.store,'global')).map(file=>({...file,path:'global/'+file.path}));
  return normalized(encode({database:database(generation.service.dbPath),files:[...fileTree,...globalTree]}),root,[generation.workspace],origin);
}
async function endToEndRun(api:any,root:string,reset:()=>void){
  // An export is a caller-owned new child, never an existing store.
  mkdirSync(root,{recursive:false});
  const server=await stub(E2E_SCRIPT,()=>Date.parse(NOW)),events:any[]=[];
  const generation=spawn(api,root,server.origin,events),c=controller(api,()=>generation,events);
  reset();
  try{
    const first=await bounded(retrieve(generation,undefined,'do we already know the deployment rule?'),'E2E initial retrieve');
    assert.ok(!first.systemContext,'#778 initial retrieve must be empty');
    await capture(generation);
    generation.service.repository.setPipelineState('autoIndexAnchorAt','2026-10-21T23:58:00.000Z');
    c.schedule(generation.workspace);await settled(c,generation.runtime);
    const second=await bounded(retrieve(generation,undefined,'recall the retained Project Aurora deployment rule'),'E2E recall after flush');
    assert.ok(second.systemContext?.includes('blue deployment rule'),'#778 post-flush retrieve must hit captured content');
    assert.equal(server.requests.length,5,'#778 exact E2E LLM request sequence');
    const persisted=persistence(generation,root,server.origin);
    const readback=encode(generation.service.get(generation.service.list({limit:100}).map((entry:any)=>entry.relativePath)));
    await bounded(generation.bundle.dispose(),'E2E close');
    assert.equal(generation.closes(),1,'#778 E2E close exactly once');
    return normalized(encode({first,second,script:E2E_SCRIPT,requests:server.requests,events,persisted,readback,closeCount:generation.closes(),layout:{workspace:'old/workspace',store:'old/store',database:'old/store/workspaces/$HASH/control.sqlite'}}),root,[generation.workspace],server.origin);
  }finally{await generation.bundle.dispose();await server.close();}
}
async function readPersisted(api:any,root:string,reset:()=>void){
  const workspace=join(root,'old/workspace'),slug=createHash('sha1').update(resolve(workspace)).digest('hex').slice(0,10),path=join(root,'old/store/workspaces',slug,'control.sqlite');
  assert.ok(statSync(path,{throwIfNoEntry:false})?.isFile(),'#778 cross-language database must already exist at bound workspace hash');
  const script=[response({route:'project'}),response({selected_ids:[]})],server=await stub(script,()=>Date.parse(NOW)),events:any[]=[];
  // Record the persisted database before opening the real TS service; constructor
  // sets workspaceDir, so this makes a missing/wrong original state observable.
  const before=normalized(encode(database(path)),root,[workspace]);
  const generation=spawn(api,root,server.origin,events);reset();
  try{
    const persisted=persistence(generation,root,server.origin);
    const readback=encode(generation.service.get(generation.service.list({limit:100}).map((entry:any)=>entry.relativePath)));
    const result=await bounded(retrieve(generation,undefined,'recall the retained Project Aurora deployment rule'),'cross-language real TS retrieve');
    assert.ok(result.systemContext?.includes('blue deployment rule'),'#778 TS must retrieve Rust-written captured content');
    await generation.bundle.dispose();
    return normalized(encode({before,persisted,readback,result,requests:server.requests,closeCount:generation.closes()}),root,[workspace],server.origin);
  }finally{await generation.bundle.dispose();await server.close();}
}
function maximumLines(api:any,root:string){
  mkdirSync(root,{recursive:true});
  const created=api.factory({projectRoot:join(root,'workspace'),config:{enabled:true,provider:'edgeclaw',rootDir:join(root,'store')}});
  assert.ok(created?.service instanceof api.Service,'#778 #1052 real source service');
  const service=created.service,body=Array.from({length:81},(_,i)=>`line-${i+1}`).join('\n');
  try{
    const entry=service.repository.getFileMemoryStore().upsertCandidate({type:'project',scope:'project',name:'Max lines defaults',description:'81 lines',body});
    const ids=[entry.relativePath];
    const read=(rows:any)=>{assert.equal(rows.length,1,'#778 #1052 one record');return rows[0].content;};
    return {body,cases:[{name:'omitted',content:read(service.get(ids))},{name:'undefined',content:read(service.get(ids,undefined))},{name:'null',content:read(service.get(ids,null))}]};
  }finally{service.close();}
}
export async function collect(baseline:string,options:{exportRoot?:string,readRoot?:string}={}){
  assert.equal(process.version,'v22.22.0','#778 exact Node baseline');
  const root=mkdtempSync(join(tmpdir(),'pdrs-native-default-')),home=join(root,'home');mkdirSync(home,{recursive:true});
  const saved=Object.fromEntries(['HOME',...ENV_KEYS].map(key=>[key,process.env[key]])),OriginalDate=Date,random=Math.random;let draw=0;
  process.env.HOME=home;for(const key of ENV_KEYS)delete process.env[key];
  globalThis.Date=class extends OriginalDate{constructor(...args:any[]){super(...((args.length?args:[NOW]) as [any]));}static now(){return OriginalDate.parse(NOW);}} as any;
  Math.random=()=>RANDOM[draw++%RANDOM.length];
  try{
    const api=await load(baseline),scenarios:any[]=[];
    if(options.readRoot)return await readPersisted(api,resolve(options.readRoot),()=>{draw=0});
    for(const name of M21){console.error('M21 '+name);scenarios.push(await lifecycle(api,root,name,()=>{draw=0}));}
    const configurations=configSpecs().map(spec=>configuration(api,root,spec));
    const endToEnd=await endToEndRun(api,options.exportRoot?resolve(options.exportRoot):join(root,'end-to-end'),()=>{draw=0});
    const maxLines=maximumLines(api,join(root,"max-lines"));
    return {scenarios,configurations,endToEnd,maxLines};
  }finally{globalThis.Date=OriginalDate;Math.random=random;for(const [key,value] of Object.entries(saved)){if(value===undefined)delete process.env[key];else process.env[key]=value;}rmSync(root,{recursive:true,force:true});}
}
