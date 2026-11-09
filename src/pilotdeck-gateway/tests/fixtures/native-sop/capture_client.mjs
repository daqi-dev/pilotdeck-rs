import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const [baselineArg,outputArg]=process.argv.slice(2),baseline=resolve(baselineArg),output=resolve(outputArg);
await mkdir(output);let forbidden=0;globalThis.fetch=async()=>{forbidden++;throw Error('unexpected external fetch');};
const {StaffDeckSopClient}=await import(pathToFileURL(join(baseline,'src/sop/staffdeck/StaffDeckSopClient.ts')));
const clone=x=>structuredClone(x);
const manifest={status:'ok',protocolVersion:'2.0',moduleId:'sop.runtime',contract:'sop.lifecycle/v2',operations:['prepare','submit']};
const state={status:'active',active_skill_id:'fixture',active_step_id:'node'};
const step={skillId:'fixture',skillName:'Fixture',version:'v1',nodeId:'node',node:{node_id:'node'},instruction:'Follow fixture instruction.',expectedUserInfo:[],knownSlots:{},allowedNextStepIds:[],requiredToolNames:[],allowedActions:['completed'],isTerminal:true,declaresHandoff:false};
const result={status:'completed',replyFragment:'Fixture complete.',slotUpdates:{},events:[]};
const proposal={status:'completed',replyFragment:'Fixture complete.'},bundle={sops:[{id:'fixture'}]};
const expectation={implementationId:'fixture.owner',contract:'sop.lifecycle/v2',transport:'sop-http-v2'};
const boundManifest={...manifest,descriptorVersion:'1.0',...expectation};
const rows=[];
async function run(name,options={}){
 const operation=options.operation??'prepare';const requests=[];let manifestCalls=0;
 const config={...(options.expected?{expectedManifest:options.expected}:{}),...(options.manifestPath?{manifestPath:options.manifestPath}:{})};
 const client=new StaffDeckSopClient('http://127.0.0.1:1/base///',{...config,fetch:async(url,init={})=>{
  requests.push({url:String(url),method:init.method,headers:init.headers,body:init.body});
  if(options.fetchError)throw Error(options.fetchError);
  const health=init.method==='GET';
  const body=health?(options.retryManifest&&manifestCalls++===0?{}:(options.manifest??manifest)):
   (options.envelope??{protocolVersion:'2.0',requestId:options.context?.requestId??`sop.${operation}`,ok:true,outcome:'completed',payload:operation==='prepare'?{state,step:options.step??step}:{state,result:options.result??result}});
  return new Response(options.invalidJson&&!health?'{':JSON.stringify(body),{status:health?(options.healthStatus??200):(options.status??200),headers:{'content-type':'application/json'}});
 }});
 const outcomes=[];const input={bundle,state,...(operation==='submit'?{proposal,successfulToolNames:['read_file']}:{}),...(options.context?{context:options.context}:{})};
 for(let i=0;i<(options.repeat??1);i++){
  try{outcomes.push({ok:true,result:await client[operation](input)});}catch(error){outcomes.push({ok:false,name:error.name,code:error.code,message:error.message,...(error.details===undefined?{}:{details:error.details}),...(error.retryability===undefined?{}:{retryability:error.retryability})});}
 }
 rows.push({name,operation,options,input,requests,outcomes});
}
await run('prepare');await run('submit',{operation:'submit'});
await run('context',{context:{runId:'r',operationId:'o',requestId:'q',sessionId:'s',turnId:'t',idempotencyKey:'key',deadlineAt:'2099-01-01T00:00:00.000Z',expectedRevision:0}});
await run('empty-optional-context',{context:{runId:'r',operationId:'o',requestId:'q',sessionId:'s',turnId:'t',idempotencyKey:'',deadlineAt:''}});
await run('bound-manifest',{manifest:boundManifest,expected:expectation,manifestPath:'/manifest'});
await run('missing-manifest-operation',{manifest:{...manifest,operations:['prepare']}});
await run('wrong-manifest-version',{manifest:{...manifest,protocolVersion:'1.0'}});
await run('wrong-bound-manifest',{manifest,expected:expectation});
await run('health-unavailable',{healthStatus:503});
await run('wrong-request-id',{envelope:{protocolVersion:'2.0',requestId:'foreign',ok:true,outcome:'completed',payload:{state,step}}});
await run('owner-error',{status:409,envelope:{protocolVersion:'2.0',requestId:'sop.prepare',ok:false,outcome:'failed',error:{code:'SOP_OWNER_CONFLICT',message:'Original owner conflict.',retryability:'retry_after_status',details:{revision:4}}}});
await run('malformed-owner-error',{envelope:{protocolVersion:'2.0',requestId:'sop.prepare',ok:false,outcome:'failed',error:{code:'bad',message:'bad',retryability:'always'}}});
await run('outcome-invalid',{envelope:{protocolVersion:'2.0',requestId:'sop.prepare',ok:true,outcome:'unknown',payload:{state,step}}});
await run('success-missing-payload',{envelope:{protocolVersion:'2.0',requestId:'sop.prepare',ok:true,outcome:'completed'}});
await run('success-failed-outcome',{envelope:{protocolVersion:'2.0',requestId:'sop.prepare',ok:true,outcome:'failed',payload:{state,step}}});
await run('http-error-success-envelope',{status:500});
await run('invalid-json',{invalidJson:true});
await run('fetch-error',{fetchError:'fixture transport failed'});
await run('invalid-prepare-state',{envelope:{protocolVersion:'2.0',requestId:'sop.prepare',ok:true,outcome:'completed',payload:{state:[],step}}});
await run('invalid-step-context',{step:{...step,contextMode:'shared'}});
await run('invalid-step-sub-sop',{step:{...step,subSopId:3}});
await run('invalid-required-tools',{step:{...step,requiredToolNames:[3]}});
await run('transition-valid',{step:{...step,transitions:[{nextStepId:'next',condition:'yes',priority:1,label:'Next',targetStep:{node_id:'next'}}]}});
await run('transition-invalid-priority',{step:{...step,transitions:[{nextStepId:'next',condition:'yes',priority:'1',targetStep:{node_id:'next'}}]}});
await run('submit-null-owner-optionals',{operation:'submit',result:{...result,taskSummary:null,nextStepId:null}});
await run('submit-invalid-slot-updates',{operation:'submit',result:{...result,slotUpdates:[]}});
await run('submit-invalid-summary',{operation:'submit',result:{...result,taskSummary:3}});
await run('submit-invalid-event',{operation:'submit',result:{...result,events:[3]}});
await run('cached-manifest',{repeat:2});await run('failed-manifest-retry',{repeat:2,retryManifest:true});
if(forbidden)throw Error('external fetch attempted');
const sources={};for(const path of ['src/sop/staffdeck/StaffDeckSopClient.ts','src/sop/staffdeck/types.ts'])sources[path]=createHash('sha256').update(await readFile(join(baseline,path))).digest('hex');
await writeFile(join(output,'oracle.json'),JSON.stringify({baseline:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',node:process.version,icu:process.versions.icu,sources,forbidden,fixtures:{manifest,state,step,result},rows},null,2)+'\n');
console.log(JSON.stringify({rows:rows.length,outcomes:rows.reduce((sum,row)=>sum+row.outcomes.length,0),forbidden}));
