// Self-authored driver: direct imports of the fixed TS owners, no copied implementation.
import {mkdir,readFile,writeFile} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
const [baseArg,outArg]=process.argv.slice(2);if(!baseArg||!outArg)throw Error('capture.mjs BASELINE NEW_OUT');
const base=resolve(baseArg),out=resolve(outArg);
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw Error('requires fixed Node v22.22.0 / ICU78.2');
if(execFileSync('/usr/bin/git',['-C',base,'rev-parse','HEAD'],{encoding:'utf8'}).trim()!=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85')throw Error('wrong baseline SHA');
await mkdir(out);
const NativeDate=Date,fixed='2026-01-02T03:04:05.000Z';globalThis.Date=class extends NativeDate{constructor(...args){super(...(args.length?args:[fixed]));}static now(){return NativeDate.parse(fixed)}};
let networkAttempts=0;globalThis.fetch=async()=>{networkAttempts++;throw Error('network forbidden')};
const {StaffDeckSopControlPlane}=await import(pathToFileURL(join(base,'src/sop/staffdeck/StaffDeckSopControlPlane.ts')));
const bundle={sops:[{id:'fixture',skill_id:'fixture',version:'v1',content:{nodes:[{node_id:'node',assignee_user_id:'approver'}]}}]};
const state={version:1,selected_skill_id:'fixture',active_skill_id:'fixture',active_step_id:'node',status:'active',slots_json:{old:'retained'}};
const wait={id:'wait-external',kind:'external_task',skillId:'fixture',stepId:'node',createdAt:'2026-01-01T00:00:00.000Z'};
const raw=(extra={})=>({schemaVersion:3,sessionId:'s1',bundle,state,revision:3,resumeRequests:{},updatedAt:'2026-01-01T00:00:00.000Z',...extra});
const waiting=()=>raw({state:{...state,status:'waiting_external_task',awaiting_input_json:{wait_id:wait.id}},wait});
const command={sessionKey:'s1',requestId:'request-1',waitId:wait.id,message:'resume fixture',expectedRevision:3,source:'external_task',slotUpdates:{new:'value'}};
const cases=[];
async function run(name,initial,commands,enabled=true){const root=join(out,'work',name),dir=join(root,'sessions');await mkdir(dir,{recursive:true});const file=join(dir,'czE.json');if(initial!==undefined)await writeFile(file,JSON.stringify(initial)+'\n');const control=new StaffDeckSopControlPlane(()=>enabled?{stateRoot:root}:undefined);const steps=[];for(const input of commands){const before=await readFile(file,'utf8').catch(e=>{if(e.code!=='ENOENT')throw e;return null});let outcome;try{outcome={ok:true,result:await control.resume(input)}}catch(e){outcome={ok:false,name:e.name,...(e.code===undefined?{}:{code:e.code}),message:e.message}}const after=await readFile(file,'utf8').catch(e=>{if(e.code!=='ENOENT')throw e;return null});steps.push({input,outcome,unchanged:before===after,...(after===null?{}:{after:JSON.parse(after)})})}cases.push({name,...(initial===undefined?{}:{initial}),enabled,steps})}
const variants=[['unknown-source',{source:'bogus'}],['null-source',{source:null}],['number-revision',{expectedRevision:0}],['string-revision',{expectedRevision:'3'}],['null-revision',{expectedRevision:null}],['array-slots',{slotUpdates:['x','y']}],['string-slots',{slotUpdates:'A😀'}],['bool-slots',{slotUpdates:true}],['numeric-wait',{waitId:3}],['numeric-request',{requestId:7}],['numeric-message',{message:9}],['object-message',{message:{text:'x'}}]];
for(const [name,delta]of variants){for(const [phase,initial]of [['missing',undefined],['active',raw()],['waiting',waiting()]])await run(`${phase}-${name}`,initial,[{...command,...delta}]);}
await run('disabled-invalid',undefined,[{...command,source:'bogus',requestId:3}],false);
for(const value of ['',0,false,null])await run(`falsey-slots-${JSON.stringify(value)}`,waiting(),[{...command,slotUpdates:value}]);
await run('array-slots-replay',waiting(),[{...command,slotUpdates:['x']},{...command,slotUpdates:['x']},{...command,slotUpdates:{0:'x'}}]);
await run('numeric-message-replay',waiting(),[{...command,message:9},{...command,message:9}]);
const receipt={accepted:true,duplicate:false,sessionId:'s1',requestId:'request-1',revision:4,message:'resume fixture'};
await run('legacy-receipt-unknown-source',raw({resumeRequests:{'request-1':receipt}}),[{...command,source:'bogus',expectedRevision:'bad',slotUpdates:['different']}]);
await run('receipt-before-parameter-validation',waiting(),[command,{...command,expectedRevision:'bad'},{...command,source:'bogus'}]);
for(const sessionKey of [7,null,{},['s1']])await run(`session-${JSON.stringify(sessionKey)}`,undefined,[{...command,sessionKey}]);
if(networkAttempts)throw Error('network attempted');const sources={};for(const f of ['src/sop/staffdeck/StaffDeckSopControlPlane.ts','src/sop/staffdeck/SopStateStore.ts'])sources[f]=createHash('sha256').update(await readFile(join(base,f))).digest('hex');
await writeFile(join(out,'oracle.json'),JSON.stringify({baseline:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',node:process.version,icu:process.versions.icu,fixed,sources,networkAttempts,cases},null,2)+'\n');console.log(JSON.stringify({cases:cases.length,steps:cases.reduce((n,c)=>n+c.steps.length,0),networkAttempts}));
