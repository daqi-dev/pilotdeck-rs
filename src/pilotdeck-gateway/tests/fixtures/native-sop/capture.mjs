// Self-authored driver; imports the fixed baseline, never copies implementation.
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
const [baselineArg,outputArg] = process.argv.slice(2);
if (!baselineArg || !outputArg) throw Error('usage: capture.mjs BASELINE NEW_OUTPUT');
const baseline=resolve(baselineArg), output=resolve(outputArg);
await mkdir(output); // exclusive evidence directory
const fixed='2026-01-02T03:04:05.000Z';
const NativeDate=Date;
globalThis.Date=class extends NativeDate { constructor(...args){super(...(args.length?args:[fixed]));} static now(){return NativeDate.parse(fixed);} };
let forbidden=0;
globalThis.fetch=async()=>{forbidden++;throw Error('unexpected network fetch');};
const { SopStateStore }=await import(pathToFileURL(join(baseline,'src/sop/staffdeck/SopStateStore.ts')));
const { createFixedApprovalAuthority }=await import(pathToFileURL(join(baseline,'ui/server/staffdeck-approval-authority.mjs')));
const bundle={sops:[{id:'fixture',skill_id:'fixture',version:'v1',content:{nodes:[{node_id:'node',assignee_user_id:'approver'}]}}]};
const state={version:1,selected_skill_id:'fixture',active_skill_id:'fixture',active_step_id:'node',status:'active',slots_json:{old:'retained'}};
const external={...state,status:'waiting_external_task',awaiting_input_json:{wait_id:'wait-external'}};
const handoff={...state,status:'handoff',awaiting_input_json:{wait_id:'wait-human'}};
const submitResult={status:'completed',replyFragment:'fixture reply',slotUpdates:{new:'value'},events:[]};
const raw=(value=state,extra={})=>({schemaVersion:3,sessionId:'s1',bundle,state:value,revision:3,resumeRequests:{},updatedAt:'2026-01-01T00:00:00.000Z',...extra});
const wait=(kind='external_task')=>({id:kind==='handoff'?'wait-human':'wait-external',kind,skillId:'fixture',stepId:'node',createdAt:'2026-01-01T00:00:00.000Z'});
const command={sessionId:'s1',requestId:'request-1',waitId:'wait-external',message:'resume fixture',expectedRevision:3,source:'external_task',slotUpdates:{new:'value'}};
const authority={tenantId:'tenant',sessionId:'s1',subject:{tenantId:'tenant',source:'web',disabled:false,userId:'approver',role:'member'}};
const cases=[];
async function run(name,initial,operations){
 const dir=join(output,'work',name);await mkdir(dir,{recursive:true});
 const file=join(dir,Buffer.from('s1').toString('base64url')+'.json');
 if(initial!==undefined)await writeFile(file,JSON.stringify(initial)+'\n');
 const store=new SopStateStore(dir);const steps=[];
 for(const operation of operations){let outcome;
  try{const result=await store[operation.method](...operation.args);outcome={ok:true,...(result===undefined?{}:{result})};}
  catch(error){outcome={ok:false,name:error.name,code:error.code,message:error.message};}
  let after;try{after=JSON.parse(await readFile(file,'utf8'));}catch(error){if(error.code!=='ENOENT')throw error;}
  steps.push({operation,outcome,...(after===undefined?{}:{after})});
 }
 cases.push({name,...(initial===undefined?{}:{initial}),steps});
}
const op=(method,...args)=>({method,args});
await run('create',undefined,[op('loadOrCreate','s1',bundle,'fixture'),op('loadOrCreate','s1',{sops:[]},'other')]);
await run('migration', {...raw(),schemaVersion:1,revision:0,unknown:'drop',wait:{kind:'bogus'},resumeRequests:{bad:{}},updatedAt:4},[op('loadOrCreate','s1',bundle,'fixture')]);
await run('replace-external',raw(),[op('replace','s1',bundle,external,3),op('replace','s1',bundle,external,4),op('status','s1')]);
await run('replace-handoff',raw(),[op('replace','s1',bundle,handoff,3),op('status','s1')]);
await run('replace-conflict',raw(),[op('replace','s1',bundle,external,2)]);
await run('external-resume',raw(external,{wait:wait()}),[op('resume',command),op('resume',command),op('resume',{...command,message:'conflict'})]);
await run('external-validation',raw(external,{wait:wait()}),[op('resume',{...command,waitId:'stale'}),op('resume',{...command,source:'human'}),op('resume',{...command,expectedRevision:2})]);
await run('not-waiting',raw(),[op('resume',command)]);
await run('missing',undefined,[op('status','s1'),op('resume',command)]);
await run('human-resume',raw(handoff,{wait:wait('handoff')}),[op('resume',{...command,source:'human',waitId:'wait-human',authority}),op('resume',{...command,source:'human',waitId:'wait-human',authority})]);
await run('human-denied',raw(handoff,{wait:wait('handoff')}),[op('resume',{...command,source:'human',waitId:'wait-human'}),op('resume',{...command,source:'human',waitId:'wait-human',authority:{...authority,subject:{...authority.subject,userId:'other'}}}),op('resume',{...command,source:'human',waitId:'wait-human',authority,expectedRevision:undefined})]);
await run('delivery',raw(),[op('commitSubmission','s1',bundle,{...state,status:'completed'},3,'turn-1',submitResult),op('replyDelivery','s1'),op('markReplyDurable','s1','turn-1'),op('markReplyDurable','s1','turn-1'),op('clearReplyDelivery','s1','wrong-turn'),op('clearReplyDelivery','s1','turn-1'),op('replyDelivery','s1')]);
await run('authority',raw(),[op('authority','s1',3),op('authority','s1',2)]);
await run('authority-inactive',raw(handoff,{wait:wait('handoff')}),[op('authority','s1',3)]);
await run('successful-tools',raw(),[op('recordSuccessfulTools','s1',bundle,'fixture',['read_file','','read_file','write_file']),op('recordSuccessfulTools','s1',bundle,'fixture',['read_file'])]);
const auth=[];
const user={tenant_id:'tenant',id:'approver',source:'web',disabled:false,role:'member'};
for(const row of [
 {name:'member',user}, {name:'admin',user:{...user,role:'admin'}}, {name:'wrong-user',user:{...user,id:'other'}},
 {name:'wrong-tenant',user:{...user,tenant_id:'other'}}, {name:'wrong-source',user:{...user,source:'api'}},
 {name:'disabled',user:{...user,disabled:true}}, {name:'wrong-role',user:{...user,role:'owner'}},
 {name:'empty-bearer',user,bearer:''},{name:'rejected',status:401,user},{name:'null-user',user:null}
]) {
 const requests=[];let outcome;
 const authority=createFixedApprovalAuthority({origin:'http://127.0.0.1:1/nested/path',tenantId:'tenant',approverUserId:'approver',fetch:async(url,options)=>{
  requests.push({url:String(url),method:options.method,headers:options.headers,redirect:options.redirect});
  return new Response(JSON.stringify(row.user),{status:row.status??200,headers:{'content-type':'application/json'}});
 }});
 try{outcome={ok:true,result:await authority.authenticate({bearer:row.bearer??'fixture-bearer'})};}
 catch(error){outcome={ok:false,name:error.name,...(error.code===undefined?{}:{code:error.code}),...(error.status===undefined?{}:{status:error.status}),message:error.message};}
 auth.push({...row,requests,outcome});
}
if(forbidden)throw Error('network attempted');
const sources={};
for(const path of ['src/sop/staffdeck/SopStateStore.ts','ui/server/staffdeck-approval-authority.mjs'])sources[path]=createHash('sha256').update(await readFile(join(baseline,path))).digest('hex');
await writeFile(join(output,'oracle.json'),JSON.stringify({baseline:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',node:process.version,icu:process.versions.icu,fixed,sources,forbidden,cases,auth},null,2)+'\n');
console.log(JSON.stringify({cases:cases.length,steps:cases.reduce((n,c)=>n+c.steps.length,0),auth:auth.length,forbidden}));
