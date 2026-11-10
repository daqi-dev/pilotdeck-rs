// Private original-TS public model boundary capture; no network, no provider call.
import {writeFileSync,mkdirSync} from 'node:fs';
import {assertBaseline,baselineSourceHashes} from '../_shared/baseline.mts';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
const baseline=process.env.PDRS_BASELINE!, out=process.argv[2];
if(!baseline||!out)throw Error('baseline and exclusive out required');
const head=assertBaseline(baseline);
const {createActiveRuntimeModelPorts}=await import(pathToFileURL(join(baseline,'src/composition/activeRuntimeModelPorts.ts')).href);
const {createRuntimeHostCapabilityProvider}=await import(pathToFileURL(join(baseline,'src/composition/publicHostRuntimeAdapter.ts')).href);
const config={providers:{p:{id:'p',protocol:'openai',baseUrl:'http://127.0.0.1:9',models:{m:{id:'m',capabilities:{maxContextTokens:8192,maxOutputTokens:256,supportsStreaming:true,supportsSystemPrompt:true,supportsToolUse:true},multimodal:{}}}}}};
const catalog={items:[{id:'p/m',provider:'p',model:'m',available:true}]};
const ports=(cat:any,modelConfig:any=config)=>createActiveRuntimeModelPorts({config:modelConfig,model:{getCapabilities:()=>modelConfig.providers.p.models.m.capabilities},catalog:async()=>cat} as any);
const base={requestId:'req',modelId:'p/m',request:{messages:[{role:'user',content:[{type:'text',text:'hello'}]}]}};
const cases:any[]=[];
function add(name:string,edit:(x:any)=>void,cat:any=catalog){const input=structuredClone(base);edit(input);cases.push({case:name,input,catalog:cat});}
add('valid',()=>{});add('budget-output-and-timeout',x=>x.budget={maxOutputTokens:64,timeoutMs:17});
add('fallback-model-id',x=>{delete x.modelId;x.request.model='p/m'});
add('concrete-request-model',x=>x.request.model='m');add('trim-check-preserves-request-id',x=>x.requestId='  req  ');
add('request-not-object',x=>x.request=[]);add('budget-null',x=>x.budget=null);add('missing-model-id',x=>delete x.modelId);add('blank-model-id',x=>x.modelId=' \ufeff');add('missing-request-id',x=>delete x.requestId);
add('unknown-model',x=>x.modelId='missing');add('ambiguous-model',()=>{},{items:[...catalog.items,...catalog.items]});add('unavailable-model',()=>{},{items:[{...catalog.items[0],available:false}]});
add('request-model-mismatch',x=>x.request.model='other');add('request-provider-mismatch',x=>x.request.provider=null);
add('max-input-unsupported',x=>x.budget={maxInputTokens:1});add('max-input-invalid',x=>x.budget={maxInputTokens:0});add('output-zero',x=>x.budget={maxOutputTokens:0});add('output-null',x=>x.budget={maxOutputTokens:null});add('timeout-fractional',x=>x.budget={timeoutMs:0.5});add('timeout-too-large',x=>x.budget={timeoutMs:9007199254740992});add('output-over-cap',x=>x.budget={maxOutputTokens:257});add('request-over-budget',x=>{x.request.maxOutputTokens=65;x.budget={maxOutputTokens:64}});
add('request-output-string-preserved',x=>x.request.maxOutputTokens='legacy');add('request-output-fractional',x=>x.request.maxOutputTokens=0.5);
add('missing-messages',x=>delete x.request.messages);add('messages-empty',x=>x.request.messages=[]);add('message-array',x=>x.request.messages=[[]]);add('system-role',x=>x.request.messages[0].role='system');add('content-string',x=>x.request.messages[0].content='hello');add('content-empty',x=>x.request.messages[0].content=[]);add('block-array',x=>x.request.messages[0].content=[[]]);add('block-unknown',x=>x.request.messages[0].content=[{type:'other'}]);add('invalid-speed',x=>x.request.speed=2);
add('standard-text-prepare-success',()=>{});
add('output-exact-selected-cap',x=>x.budget={maxOutputTokens:256});
add('request-exact-output-budget',x=>{x.request.maxOutputTokens=64;x.budget={maxOutputTokens:64}});
add('timeout-exact-max-safe-integer',x=>x.budget={timeoutMs:9007199254740991});
const textConfig:any=structuredClone(config);textConfig.providers.p.models.m.multimodal={input:['text']};
for(const row of cases.slice(34))row.config=textConfig;
for(const row of cases){
  const selected=ports(row.catalog,row.config??config);
  const provider=createRuntimeHostCapabilityProvider({profile:{id:'fixture'},model:{prepare:selected.prepare},tools:{},skills:{},context:{forTool(){throw Error('unused tool context')}}});
  row.response=await provider.call('model_prepare',row.input,{principal:{pilotDeckUserId:'fixture-owner',tenantId:'fixture-tenant',actorUserId:'fixture-actor',agentId:'fixture-agent'}});
}
if(cases.slice(34).some(row=>row.response.status!==200))throw Error('legal equality must reach actual prepare success');
const sourcePaths=['src/composition/activeRuntimeModelPorts.ts','src/composition/publicHostRuntimeAdapter.ts','src/model/request/validateModelRequest.ts','src/model/protocol/multimodal.ts'];
const sourceSha256=await baselineSourceHashes(baseline,sourcePaths);
const versions=Object.fromEntries(['node','icu','uv'].map(k=>[k,process.versions[k]]));
if(JSON.stringify(versions)!==JSON.stringify({node:'22.22.0',icu:'78.2',uv:'1.52.1'}))throw Error('node version mismatch');
mkdirSync(out,{recursive:true});writeFileSync(join(out,'oracle.json'),JSON.stringify({baselineCommit:head,sourceSha256,nodeVersions:versions,config,cases},null,2)+'\n',{flag:'wx'});console.log(JSON.stringify({cases:cases.length,out}));
