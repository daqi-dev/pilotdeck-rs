// #1082: actual registry leases + actual memory bundle/factory/service.
// Only unrelated resource families and the LLM transport are substituted.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {readFileSync,mkdirSync} from 'node:fs';
import {join,resolve} from 'node:path';
import {load,NOW} from '../edgeclaw-native-default/observe.mts';

export const CASES=['staged-flush-stage','staged-flush-write','staged-retrieve-stage','staged-retrieve-write',
  'disposing-flush-stage','disposing-flush-write','disposing-retrieve-stage','disposing-retrieve-write',
  'never-flush-write','never-retrieve-write','cancelled-retrieve-write','cancelled-release-flush-write'];
function deferred(){let resolve!:()=>void;const promise=new Promise<void>(r=>resolve=r);return {promise,resolve};}
async function bounded<T>(p:Promise<T>,label:string):Promise<T>{let timer:any;try{return await Promise.race([p,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(Error('#1082 INVALID timeout '+label)),10000)})]);}finally{clearTimeout(timer);}}
const body=JSON.stringify({choices:[{message:{content:JSON.stringify({route:'none',should_store:false,labels:[]})}}]});
const messages=[{role:'user',content:[{type:'text',text:'Project Aurora must retain the blue deployment rule.'}]},{role:'assistant',content:[{type:'text',text:'I will retain the blue deployment rule.'}]}];

export async function lifecycle(baseline:string,base:string){
  base=resolve(base);
  const api=await load(baseline),requireBaseline=createRequire(join(baseline,'package.json'));
  const {transformSync}=createRequire(requireBaseline.resolve('tsx'))('esbuild');
  const source='src/cli/ProjectRuntimeRegistry.ts';
  const compiled=transformSync(readFileSync(join(baseline,source),'utf8'),{loader:'ts',format:'cjs',target:'es2022',sourcefile:source}).code;
  const originalFetch=globalThis.fetch,savedHome=process.env.HOME;
  const keys=['EDGECLAW_MEMORY_MODEL','EDGECLAW_MEMORY_PROVIDER','EDGECLAW_MEMORY_BASE_URL','EDGECLAW_MEMORY_API_KEY','EDGECLAW_MEMORY_API_TYPE','OPENAI_MODEL','OPENAI_BASE_URL','OPENAI_API_KEY','OPENCLAW_HOME','OPENCLAW_STATE_DIR','OPENCLAW_CONFIG_PATH'];
  const saved=keys.map(k=>[k,process.env[k]] as const);for(const k of keys)delete process.env[k];
  process.env.HOME=join(base,'home');mkdirSync(process.env.HOME,{recursive:true});
  const cells:any={};
  try{for(const name of CASES){
    const root=join(base,name),workspace=join(root,'workspace');mkdirSync(workspace,{recursive:true});
    const entered=deferred(),release=deferred(),closeEntered=deferred(),closeRelease=deferred();
    const generations:any[]=[];let requests=0,settled=false;
    globalThis.fetch=(async()=>{if(++requests===1){entered.resolve();await release.promise;}return new Response(body,{status:200,headers:{'content-type':'application/json'}});}) as any;
    class Owner {
      bundle:any;service:any;provider:any;closes=0;id=generations.length;
      constructor(options:any){
        this.bundle=new api.Bundle({projectRoot:options.projectRoot,config:{enabled:true,provider:'edgeclaw',rootDir:join(root,'store'),model:'fixture/model'},modelConfig:{providers:{fixture:{protocol:'openai',url:'http://fixture.invalid/v1',apiKey:'fixture-key'}}},now:()=>new Date(NOW)});
        generations.push(this);
      }
      stage(){const resources=this.bundle.stage();this.service=resources.memoryService;this.provider=resources.memory;assert.ok(this.service instanceof api.Service,'#1082 real service');return resources;}
      async dispose(){if(this.id===0){closeEntered.resolve();await closeRelease.promise;}await this.bundle.dispose();this.closes++;}
    }
    const module={exports:{} as any};
    new Function('require','module','exports',compiled)((specifier:string)=>{
      if(specifier.startsWith('node:'))return requireBaseline(specifier);
      if(specifier==='./ProjectRuntimeResourcesBundle.js')return {ProjectRuntimeResourcesBundle:Owner};
      if(specifier==='../agent/sub/BackgroundSubagentRuntime.js')return {BackgroundSubagentRuntime:class{shutdown(){}}};
      return {};
    },module,module.exports);
    const registry=Object.create(module.exports.ProjectRuntimeRegistry.prototype);
    Object.assign(registry,{runtimes:new Map(),reloadTail:Promise.resolve(),runtimeDisposals:new Set(),disposed:false,
      options:{fallbackProjectRoot:workspace,pilotHome:join(base,'home'),env:{},builtinPlugins:[]},_extraTools:[],routerEvents:{create(){throw Error('#1082 unexpected router');}},permissionModes:{dispose(){}},ownsSharedSessionState:false});
    const runtime=registry.resolve(),releaseLease=registry.acquireRuntimeLease(runtime),old=generations[0];
    const flush=name.includes('flush'),disposing=name.startsWith('disposing')||name.startsWith('cancelled-release');
    const abort=new AbortController();
    if(flush)await old.provider.captureTurn({sessionId:'old-session',messages,errored:false});
    const pending=(flush?old.service.flush({}):old.provider.retrieve({query:'remember the blue deployment rule',sessionId:'old-session',projectRoot:workspace,recentMessages:[],signal:abort.signal})).then((v:any)=>{settled=true;return v;});
    await bounded(entered.promise,'LLM entered');
    registry.invalidate();
    const observed:any={leasedBefore:runtime.sessionLeases};
    let releasing:Promise<any>|undefined;
    if(disposing){releasing=releaseLease();await bounded(closeEntered.promise,'dispose entered');}
    if(name==='cancelled-retrieve-write')abort.abort('fixture cancelled waiter');
    if(name.startsWith('cancelled')){
      const cancel=deferred();const waiter=Promise.race([name.startsWith('cancelled-release')?releasing!:pending,cancel.promise.then(()=> 'cancelled')]);cancel.resolve();
      observed.waiterCancelled=(await bounded(waiter,'cancel waiter'))==='cancelled';
    }
    observed.leasesAtSuccessor=runtime.sessionLeases;
    observed.oldCloseBefore=old.closes;
    observed.predecessorPending=!settled;
    old.service.repository.setPipelineState('workspaceDir','#1082-before-successor');
    registry.resolve();const next=generations[1];
    observed.sameDatabase=old.service.dbPath===next.service.dbPath;
    observed.workspaceWritten=next.service.repository.getPipelineState('workspaceDir')===workspace;
    if(!name.endsWith('-stage'))await next.provider.captureTurn({sessionId:'new-session',messages,errored:false});
    observed.newRows=next.service.repository.listAllL0().filter((row:any)=>row.sessionKey==='new-session').length;
    observed.predecessorPendingAfterSuccessor=!settled;
    release.resolve();const result=await bounded(pending,'old operation');
    observed.operation=flush?result:{diagnostics:result.diagnostics??null,route:result.metadata?.debug?.route??null,systemContext:result.systemContext??null};
    observed.oldIndexed=next.service.repository.listAllL0().filter((r:any)=>r.sessionKey==='old-session').map((r:any)=>r.indexed);
    observed.oldRows=next.service.repository.listAllL0().filter((row:any)=>row.sessionKey==='old-session').length;
    if(!releasing)releasing=releaseLease();await bounded(closeEntered.promise,'last lease disposal');
    closeRelease.resolve();await bounded(releasing,'old cleanup');
    next.service.repository.setPipelineState('after-old-close','alive');
    observed.afterOldClose=next.service.repository.getPipelineState('after-old-close');
    try{old.service.repository.getPipelineState('workspaceDir');observed.oldClosed=false;}catch{observed.oldClosed=true;}
    observed.oldCloses=old.closes;
    await bounded(registry.disposeProjectRuntimes(),'new cleanup');observed.newCloses=next.closes;
    cells[name]=observed;
  }}finally{globalThis.fetch=originalFetch;if(savedHome===undefined)delete process.env.HOME;else process.env.HOME=savedHome;for(const [k,v]of saved){if(v===undefined)delete process.env[k];else process.env[k]=v;}}
  return cells;
}
