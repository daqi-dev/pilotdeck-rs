// Real TS dispatcher + protocol, with injected context/budget/model ports.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { existsSync, realpathSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const gitExecutable = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], {encoding:'utf8'}).trim());
const binding = bindSourceTree(baseline, sourceCommit, gitExecutable);
const sources = ['agent/modules/transport/sidecarDefaultModuleDispatcher.ts', 'agent/modules/transport/agentLoopSidecarClient.ts',
  'agent/modules/transport/sidecarToolContext.ts', 'agent/loop/modelRequestAssembly.ts', 'model/request/materializeMediaReferences.ts',
  'model/protocol/clone.ts', 'context/cache/CachePlan.ts', 'context/compaction/CompactionPort.ts'];
const sourceSha256: Record<string,string> = {};
for (const p of sources) sourceSha256['src/'+p] = createHash('sha256').update(await readFile(join(baseline,'src',p))).digest('hex');
const load = (p: string) => import(pathToFileURL(join(baseline,'src',p)).href);
const { createSidecarDefaultModuleDispatcher } = await load(sources[0]);
const { createSidecarContextRequestIdentityServices } = await load(sources[2]);
const clientPath = join(baseline,'src',sources[1]);
const requireBaseline = createRequire(join(baseline,'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
const client = transformSync((await readFile(clientPath,'utf8'))+'\nexport { SidecarTurnProtocol };\n', {loader:'ts', format:'esm',target:'es2022',sourcefile:clientPath}).code;
const executable = client.replace(/from (["'])(\.[^"']+)\1/g, (_: string,q: string,p: string) => {
  let file = resolve(dirname(clientPath), p);
  if (file.endsWith('.js') && existsSync(file.slice(0,-3)+'.ts')) file=file.slice(0,-3)+'.ts';
  return 'from '+q+pathToFileURL(file).href+q;
});
const { SidecarTurnProtocol } = await import('data:text/javascript;base64,'+Buffer.from(executable).toString('base64'));
const output = await mkdtemp(join(process.argv[2] ? resolve(process.argv[2]) : tmpdir(),'host-context-'));
await writeFile(join(output,'media.txt'),'aGVsbG8=\n');
const U = {$undefined:true};
const N = (name: string) => ({$number:name});
const json = (v: any) => JSON.parse(JSON.stringify(v));
function decode(v: any): any {
  if (typeof v === 'string') return v.replaceAll('<fixture>',output);
  if (v && typeof v === 'object') {
    if (v.$undefined) return undefined;
    if (v.$number) return Number(v.$number);
    if (Array.isArray(v)) return v.map(decode);
    return Object.fromEntries(Object.entries(v).map(([k,x])=>[k,decode(x)]));
  }
  return v;
}
const normalize = (v: any) => JSON.parse(JSON.stringify(v).replaceAll(output,'<fixture>'));
const names=['prepare_for_model','apply_tool_results','recover_from_model_error','capture_turn','try_auto_compact'];
const methods=['prepareForModel','applyToolResults','recoverFromModelError','captureTurn','tryAutoCompact'];
const request={provider:'p',model:'m',messages:[],systemPrompt:'base prompt',tools:[{name:'base'}],metadata:{kept:true}};
const preparation={provider:'p',model:'m',tools:[],sessionId:'budget-session',messages:['replaced'],previewOnly:false, extra:true};
const snapshot={tokens:25,maxContextTokens:100,warningRatio:0.7,blockingRatio:0.9,ratio:0.25,state:'ok',extra:'kept'};
const messages=[{role:'user',content:[{type:'text',text:'hello'}]}];
const valid={budgetRequest:request,budgetPreparation:preparation,maxContextTokens:100};
const specs: any[]=[];
const add=(name: string, operation: any, input: any={}, extra: any={})=>specs.push({name,payload:{operation,input},...extra});
const compact=(name: string,patch: any={},extra: any={})=>add(name,'try_auto_compact',{...valid,...patch},extra);
for (const operation of [U,null,'',7,false,[],{},' ', '\ud800','unknown']) add('operation-'+specs.length,operation);
for(let mask=0;mask<16;mask++) for(const operation of names) add(`capabilities-${mask}-${operation}`,operation,{messages:'lax',extra:true},{capabilities:[true,...[0,1,2,3].map(b=>!!(mask&(1<<b)))]});
for (const operation of names) {
  for (const input of [U,null,[],7,false,'text',{}, {sessionId:'forged',turnId:'forged',cwd:'forged',permissionMode:'forged',runMode:'forged',abortSignal:'forged',messages:null,tools:7}]) add(`input-${specs.length}`,operation,input);
  for (const result of [U,null,0,false,[],{extra:'preserved'}]) add(`result-${specs.length}`,operation,{}, {result});
  add(`runtime-error-${operation}`,operation,{}, {errorAt:operation,error:{message:'runtime failed',code:'RUNTIME',error:{code:'model_busy',message:'busy',extra:true},retryable:true,retryAfterMs:12.5}});
}
for (const stage of [U,null,'unknown','pre_route','routed','recovery']) compact(`stage-${specs.length}`,{budgetStage:stage});
for (const stage of ['pre_route','routed','recovery','bad',null]) compact(`projection-${specs.length}`,{budgetProjection:{stage}});
for (const max of [U,null,0,-1,0.5,'1',N('NaN'),N('Infinity')]) compact(`maximum-${specs.length}`,{maxContextTokens:max});
compact('config-maximum',{maxContextTokens:U},{config:{maxContextTokens:70}});
compact('config-zero',{maxContextTokens:U},{config:{maxContextTokens:0}});
for (const reserved of [U,null,-1,0,0.5,'1',N('NaN'),N('Infinity')]) compact(`reserved-${specs.length}`,{reservedOutputTokens:reserved});
for (const candidate of [U,null,[],{}, {provider:1,model:'m',messages:[]}, {provider:'p',model:null,messages:[]}, {provider:'p',model:'m',messages:{}}, {...request,provider:'',model:'',messages:[null]}]) {
  compact(`request-${specs.length}`,{budgetRequest:candidate});
  compact(`without-budget-${specs.length}`,{budgetRequest:candidate,budgetPreparation:null,maxContextTokens:null},{budget:false});
}
for (const candidate of [U,null,[],{}, {...preparation,tools:{}}, {...preparation,provider:1}, {...preparation,model:null}, {...preparation,provider:'other',model:''}]) compact(`preparation-${specs.length}`,{budgetPreparation:candidate});
for (const id of [U,null,'',1,'missing','\ud800']) compact(`preparation-id-${specs.length}`,{budgetPreparationId:id});
for (const stage of ['pre_route','routed','recovery',U]) {
  compact(`prepared-${specs.length}`,{budgetStage:stage,budgetPreparationId:'known'},{prepared:true});
  compact(`materialize-${specs.length}`,{budgetStage:stage,budgetPreparationId:'known'},{prepared:true,materializer:true});
  compact(`missing-reference-${specs.length}`,{budgetStage:stage},{materializer:true});
}
compact('route-mismatch',{budgetPreparationId:'known'},{prepared:true,preparedRoute:{provider:'other',model:'m'}});
for (const calibration of [U,null,{}, {provider:'other',model:'m'}, {provider:'p',model:'m',actualInputTokens:0,estimatedInputTokens:1}, {provider:'p',model:'m',actualInputTokens:2,estimatedInputTokens:0}, {provider:'p',model:'m',actualInputTokens:2.5,estimatedInputTokens:1.5,extra:true}]) compact(`calibration-${specs.length}`,{budgetCalibration:calibration});
for (const candidate of [U,null,{},[null],[{role:'system',content:[]}],[{role:'user',content:{}}],[],messages]) compact(`candidate-${specs.length}`,{}, {candidate});
for (const result of [U,null,[],{}, {...snapshot,state:'bad'}]) compact(`snapshot-${specs.length}`,{}, {snapshot:result});
for (const field of ['tokens','maxContextTokens','warningRatio','blockingRatio','ratio']) {
  for (const v of [U,null,-1,0,'1',N('NaN'),N('Infinity')]) compact(`snapshot-${field}-${specs.length}`,{}, {snapshot:{...snapshot,[field]:v}});
}
for (const state of ['warning','blocking']) compact(`state-${state}`,{}, {snapshot:{...snapshot,state}});
for (const errorAt of ['prepare_for_model','budget','materialize']) compact(`nested-error-${errorAt}`,{budgetPreparationId:'known'},{prepared:true,materializer:true,errorAt,error:{message:'nested failed',code:'PORT',retryable:false,retryAfterMs:0}});
for (const signal of ['active','aborted']) compact(`signal-${signal}`,{abortSignal:'forged'},{signal});
compact('without-request',{budgetRequest:U,budgetPreparation:null,maxContextTokens:null});
compact('unsupported-valid',{}, {capabilities:[true,false,false,false,false]});
compact('unsupported-invalid',{budgetRequest:null}, {capabilities:[true,false,false,false,false]});
compact('normalize',{}, {candidate:[{role:'assistant',content:[]},{role:'assistant',content:[{type:'text',text:'a'}],metadata:{a:1},extra:1},{role:'assistant',content:[],metadata:{b:2}},{role:'assistant',content:[{type:'text',text:'b'}],metadata:{a:3}},{role:'assistant',content:[{type:'tool_call',name:'x',input:{a:1}}]},{role:'assistant',content:[]},...messages]});
for (const systemPrompt of [U,null,'', 'preview']) compact(`system-${specs.length}`,{}, {preview:{systemPrompt}});
compact('plan',{}, {config:{permissionMode:'plan'}});
for (const plan of [false,true]) compact(`cache-${plan}`,{}, {config:{permissionMode:plan?'plan':'default'}, preview:{cachePlan:{generation:7},tools:[{name:'z',inputSchema:{}},{name:'ä',inputSchema:{}},{name:'A',inputSchema:{}}],messages:[{role:'system',content:[]},...messages,...messages,...messages,...messages]}});
for (const mediaType of ['image','pdf','audio','unknown']) for (const path of ['media.txt','absent.txt']) compact(`media-${mediaType}-${path}`,{}, {preview:{messages:[{role:'user',content:[{type:'media_reference',path:'<fixture>/'+path,mediaType,mimeType:mediaType==='pdf'?'application/pdf':'image/png',originalBytes:8,preview:'fallback',detail:'high',pages:2}]}]}});
compact('invalid-pdf',{}, {preview:{messages:[{role:'user',content:[{type:'media_reference',path:'<fixture>/media.txt',mediaType:'pdf',mimeType:'wrong',preview:'fallback'}]}]}});
for (const preview of [null,{}, {messages:null},{messages:{}},{messages:[null]},{messages:[{role:'user',content:[null]}]},{messages:[{role:'user',content:[{type:'tool_result',content:null}]}]}]) compact(`invalid-preview-${specs.length}`,{}, {previewResult:preview});
compact('invalid-materialized', {budgetPreparationId:'known'}, {prepared:true,materializer:true,materialized:null});
// Runtime-owned cache inputs keep the native JS coercion and UTF-16 behavior.
for (const tools of [[{name:42},{name:'a'}],[{}, {name:'a'}],[{name:'a'},{name:42}], [{name:'a'},{}], '\ud800', '😀', [{name:'same',b:U},{name:'same',a:[U,null,0]}]]) compact(`cache-lax-${specs.length}`,{}, {preview:{cachePlan:{generation:0},tools}});
let deep:any=0;for(let i=0;i<520;i++)deep={a:deep};
compact('cache-deep',{}, {preview:{cachePlan:{generation:1},messages:[{role:'user',content:[],metadata:deep}]}});
for (const clearPrepared of ['close_stream','dispose']) compact(`cleared-${clearPrepared}`,{budgetPreparationId:'known'}, {prepared:true,clearPrepared});
for (const materialized of [U,null,{provider:'p',model:'m',messages:[],extra:'materialized'}]) {
  add(`model-materialize-${specs.length}`,'materialize_prepared_request',{}, {module:'model',prepared:true,materializer:true,...(materialized===U?{}:{materialized})});
  specs.at(-1).payload={operation:'materialize_prepared_request',preparationId:'known',request};
}
// An explicit budgetStage wins; budgetProjection only fills an undefined stage.
for (const [stage, projected] of [['pre_route','recovery'],['recovery','pre_route'],['routed','pre_route'],['bad','recovery']] as const) compact(`stage-projection-${specs.length}`,{budgetStage:stage,budgetProjection:{stage:projected}});
// String(error) for non-iterable preview tools and null elements is V8-owned.
for (const tools of [{},7,U,null,[null,{name:'a'}],[{name:'a'},null],[null,{}]]) compact(`cache-tools-${specs.length}`,{}, {preview:{cachePlan:{generation:0},tools}});
// Valid budgetRequest, budget module absent: still removes the field and succeeds.
compact('without-budget-valid',{}, {budget:false});
// Installed budget module with no evaluation port (Rust Handler with default Ports).
compact('without-budget-port-valid',{}, {budget:'no-port'});
// The no-port branch still canonicalizes budgetRequest before deleting it.
compact('without-budget-port-invalid-request',{budgetRequest:{provider:1,model:'m',messages:[]}}, {budget:'no-port'});
// The no-port branch skips maxContextTokens/reservedOutputTokens validation.
compact('without-budget-port-lax',{maxContextTokens:null,reservedOutputTokens:null}, {budget:'no-port'});
// #495 fix round 1: JS sort moves `undefined` tool elements to the end
// without comparing them (all succeed; a `null` element still fails at the
// `name` read once the comparator runs), and the comparator's JS evaluation
// order reads `right.name` before the `localeCompare` call check.
for (const tools of [[U,{name:'a'}],[{name:'a'},U],[U,U],[U,null],[null,U],[{name:'z'},U,{name:'a'}],[U,null,{name:42}],[null,{name:42}],[{name:42},null],[{name:null},{name:42}],[{name:42},{name:null}],[{name:'a'},{name:42},{name:null}]]) compact(`cache-r1-${specs.length}`,{}, {preview:{cachePlan:{generation:0},tools}});
// A6.9-pr1 fix round 7: half-route preparations (the in-process adapter's
// modelOverride={} output carries present-undefined route keys) against the
// dispatcher:323 canonical gate and the :334/:536 strict route comparisons,
// with request keys missing vs present-undefined.
compact('route-mismatch-undefined-route',{budgetPreparationId:'known'},{prepared:true,preparedRoute:{provider:U,model:U}});
compact('route-mismatch-undefined-route-missing-provider',{budgetPreparationId:'known',budgetRequest:{model:'m',messages:[],tools:[]}},{prepared:true,preparedRoute:{provider:U,model:U}});
for (const candidate of [{model:'m',messages:[],tools:[]}, {provider:U,model:'m',messages:[],tools:[]}]) compact(`request-route-${specs.length}`,{budgetRequest:candidate});
compact('calibration-present-undefined-route',{budgetCalibration:{provider:U,model:U,actualInputTokens:2,estimatedInputTokens:1}});
const cases=[];
for(const raw of specs) {
  const spec=decode(raw), calls:any[]=[];
  const controller=new AbortController(); if(spec.signal==='aborted')controller.abort();
  const signal=spec.signal?controller.signal:undefined;
  const fail=(at:string)=>{if(spec.errorAt===at)throw Object.assign(new Error(spec.error.message),spec.error);};
  const recordInput=(input:any)=>{
    const {budgetEvaluator,abortSignal,...fields}=input;
    if(signal && abortSignal)assert.equal(abortSignal,signal,'trusted signal identity');
    return {...fields,...(abortSignal!==undefined?{abortSignal:abortSignal===signal?(signal.aborted?'aborted':'active'):abortSignal}:{}),...(budgetEvaluator!==undefined?{budgetEvaluator:typeof budgetEvaluator}: {})};
  };
  const context:any={};
  for(let i=0;i<names.length;i++) if(i===0 || (spec.capabilities??[true,true,true,true,true])[i]) context[methods[i]]=async(input:any)=>{
    calls.push({operation:names[i],input:recordInput(input)});fail(names[i]);
    if(i===0 && input.previewOnly===true) {
      if(Object.hasOwn(spec,'previewResult'))return spec.previewResult;
      return {messages:input.messages,systemPrompt:'preview prompt',tools:[],cacheBreakpoints:[0],...spec.preview};
    }
    if(i===4 && typeof input.budgetEvaluator==='function')return {snapshot:await input.budgetEvaluator(Object.hasOwn(spec,'candidate')?spec.candidate:messages)};
    return Object.hasOwn(spec,'result')?spec.result:{operation:names[i],extra:'kept'};
  };
  const model:any={execution:{prepare:async({request}:any)=>{
    calls.push({operation:'model.prepare',request});return {request,...(spec.preparedRoute??{provider:'p',model:'m'})};
  }}};
  if(spec.materializer)model.materializeRequest=async(prepared:any,candidate:any)=>{
    calls.push({operation:'materialize',prepared,candidate});fail('materialize');
    return Object.hasOwn(spec,'materialized')?spec.materialized:{...candidate,metadata:{materialized:true},systemPrompt:'materialized prompt'};
  };
  const config={cwd:'/pdrs-fixture/project',provider:'p',model:'m',permissionMode:'default',...spec.config};
  const input={sessionId:'session',turnId:'turn',messages:[],...(signal?{abortSignal:signal}:{})};
  const dispatcher=createSidecarDefaultModuleDispatcher({config,input,checkpoint:{},capabilityResultObserver:{},planTodoHandler:async()=>({}),modules:{model,
    context:{execution:context,requestIdentity:createSidecarContextRequestIdentityServices({})},
    ...(spec.budget===false?{}:spec.budget==='no-port'?{budget:{}}:{budget:{evaluateRequestBudget:async(request:any,options:any)=>{
      const {signal:passed,...rest}=options;if(passed)assert.equal(passed,signal);
      calls.push({operation:'budget',request,options:rest,signal:passed?(passed.aborted?'aborted':'active'):'absent'});fail('budget');
      return Object.hasOwn(spec,'snapshot')?spec.snapshot:snapshot;
    }}}),capability:{execution:{list:()=>[]},runtimeContext:{bindTurn:()=>({permissionContext:()=>({})})}}}} as any);
  const call=(payload:any,module='context')=>({kind:'request',method:'module_call',module,messageId:'call',requestId:'request',runId:'run',operationId:'operation',payload});
  if(spec.prepared)await dispatcher.handlers.model(call({operation:'prepare',preparationId:'known',request},'model'));
  const protocol=new SidecarTurnProtocol({config:{},input:{sessionId:'session',turnId:'turn',execution:{runId:'run',operationId:'operation'}},checkpoint:{},manifest:{},moduleHandlers:dispatcher.handlers,uuid:()=> 'response'});
  if(spec.clearPrepared==='close_stream')await dispatcher.handlers.model(call({operation:'close_stream',preparationId:'known'},'model'));
  if(spec.clearPrepared==='dispose')await dispatcher.dispose();
  const response=(await protocol.moduleResponseFor(call(spec.payload,spec.module??'context'))).response;
  cases.push(normalize({...raw,calls,methods:dispatcher.manifest.hostModules.context.methods,response}));
  await dispatcher.dispose();
}
assert.equal(bindSourceTree(baseline,sourceCommit,gitExecutable).sha256,binding.sha256);
await writeFile(join(output,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,cases},null,2)+'\n');
console.log(JSON.stringify({output,cases:cases.length}));
