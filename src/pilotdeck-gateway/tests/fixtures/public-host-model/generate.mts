// Private original-TS public model boundary capture; no network, no provider call.
import {readFileSync,writeFileSync,mkdirSync} from 'node:fs';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {pathToFileURL} from 'node:url';
const baseline=process.env.PDRS_BASELINE!, out=process.argv[2];
if(!baseline||!out)throw Error('baseline and exclusive out required');
const head=execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim();
if(head!=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85')throw Error('fixed baseline mismatch');
const {createActiveRuntimeModelPorts}=await import(pathToFileURL(join(baseline,'src/composition/activeRuntimeModelPorts.ts')).href);
const config={providers:{p:{id:'p',protocol:'openai',baseUrl:'http://127.0.0.1:9',models:{m:{id:'m',capabilities:{maxContextTokens:8192,maxOutputTokens:256,supportsStreaming:true,supportsSystemPrompt:true,supportsToolUse:true},multimodal:{}}}}}};
const catalog={items:[{id:'p/m',provider:'p',model:'m',available:true}]};
const ports=(cat:any)=>createActiveRuntimeModelPorts({config,model:{getCapabilities:()=>config.providers.p.models.m.capabilities},catalog:async()=>cat} as any);
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
for(const row of cases){try{row.response={status:200,body:await ports(row.catalog).prepare(row.input)}}catch(e:any){row.response={status:400,body:{code:e.code,message:e.message,...(e.details===undefined?{}:{details:e.details})}}}}
const sourcePaths=['src/composition/activeRuntimeModelPorts.ts','src/composition/publicHostRuntimeAdapter.ts','src/model/request/validateModelRequest.ts','src/model/protocol/multimodal.ts'];
const sourceSha256=Object.fromEntries(sourcePaths.map(p=>[p,createHash('sha256').update(readFileSync(join(baseline,p))).digest('hex')]));
const versions=Object.fromEntries(['node','icu','uv'].map(k=>[k,process.versions[k]]));
if(JSON.stringify(versions)!==JSON.stringify({node:'22.22.0',icu:'78.2',uv:'1.52.1'}))throw Error('node version mismatch');
mkdirSync(out,{recursive:true});writeFileSync(join(out,'oracle.json'),JSON.stringify({baselineCommit:head,sourceSha256,nodeVersions:versions,config,cases},null,2)+'\n',{flag:'wx'});console.log(JSON.stringify({cases:cases.length,out}));
