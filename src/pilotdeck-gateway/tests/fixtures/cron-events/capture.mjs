// Self-written offline oracle driver. Imports fixed real TS CronFire + store.
import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {readFileSync} from 'node:fs';
import {mkdir,writeFile,readdir,readFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {createHash} from 'node:crypto';
const baseline='/Users/da/ws/pdrs-runner/resources/PilotDeck';
const sourceCommit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
assert.equal(process.env.PDRS_VERIFIED_TS_SHA,sourceCommit);
assert.equal(process.version,'v22.22.0');assert.equal(process.versions.icu,'78.2');
const out=resolve(process.argv[2]);assert.ok(out.startsWith(process.env.PDRS_CRON_SCRATCH+'/'));
assert.ok(!out.startsWith(baseline+'/'));
const req=createRequire(join(baseline,'package.json'));const ts=req('typescript');
const sources=['src/cron/CronTimezone.ts','src/cron/runtime/CronSchedule.ts','src/cron/storage/CronPaths.ts','src/cron/storage/CronTaskStore.ts','src/cron/runtime/CronFire.ts'];
const hashes={};const warnings=[];process.on('warning',w=>warnings.push({name:w.name,message:w.message}));
globalThis.fetch=()=>{throw new Error('fixture forbids network')};
for(const m of ['node:http','node:https','node:net','node:tls','node:child_process']){
 const mod=req(m);for(const name of ['request','get','connect','createConnection','exec','execFile','spawn','fork','execSync','execFileSync','spawnSync'])if(typeof mod[name]==='function')mod[name]=()=>{throw new Error('fixture forbids '+m+'.'+name)};
}
function load(file,imports={}){
 const raw=readFileSync(join(baseline,file),'utf8');hashes[file]=createHash('sha256').update(raw).digest('hex');
 const code=ts.transpileModule(raw,{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
 const module={exports:{}};
 new Function('require','module','exports',code)(name=>{
  if(Object.hasOwn(imports,name))return imports[name];
  if(name.startsWith('node:'))return req(name);
  throw new Error('unmapped dependency '+file+' -> '+name);
 },module,module.exports);return module.exports;
}
const timezone=load(sources[0]);const schedule=load(sources[1],{'../CronTimezone.js':timezone});
const paths=load(sources[2],{'../../pilot/paths.js':{createProjectId(){throw new Error('not exercised: derive project identity')}}});
const {CronTaskStore}=load(sources[3],{'./CronPaths.js':paths});
const {CronFire}=load(sources[4],{'../CronTimezone.js':timezone,'./CronSchedule.js':schedule});
const START=1767225600000,FINISH=START+1000;
const E=(type,fields={})=>({type,...fields});
const delta=text=>E('assistant_text_delta',{text});const failure=(code,message)=>E('error',{...(code===null?{}:{code}),message});
const cases=[
 {id:'once-text',events:[delta('  hello'),delta(' 世界  ')]},
 {id:'empty-text',events:[delta(' \t\n')]},
 {id:'recurring-text',recurring:true,events:[delta('recur')]},
 {id:'timeout-then-abort',events:[failure('turn_timeout','limit hit'),failure('agent_aborted','aborted later')]},
 {id:'interaction-then-timeout',events:[E('permission_request'),failure('turn_timeout','limit last')]},
 {id:'timeout-then-interaction',events:[failure('turn_timeout','limit first'),E('elicitation_request')]},
 {id:'two-interactions',events:[E('permission_request'),E('elicitation_request'),failure('agent_aborted','later abort'),delta('ignored for failed delivery')]},
 {id:'untyped-error',events:[failure(null,'plain failure')]},
 {id:'agent-aborted',events:[failure('agent_aborted','abort terminal')]},
 {id:'throw-before-events',events:[],throwMessage:'fixture submit throw'},
 {id:'timeout-then-throw',events:[failure('turn_timeout','latched timeout')],throwMessage:'fixture stream throw'},
 {id:'interaction-then-throw',events:[E('permission_request')],throwMessage:'fixture stream throw'},
 {id:'stopped-success',events:[delta('text before stop')],stopped:true},
 {id:'stopped-failure',events:[failure('bad_run','failed before stop')],stopped:true},
 {id:'stale-claim',events:[delta('must not submit')],stale:true},
 {id:'edited-during-run',events:[delta('edited result')],edit:true},
 {id:'deleted-during-run',events:[delta('deleted result')],deleted:true},
 {id:'delivery-throws',events:[delta('delivered text')],deliveryThrows:true},
 {id:'turn-callback-throws',events:[delta('still completes')],turnThrows:true},
 {id:'phase-started-throws',events:[delta('must not submit')],phaseThrows:'cron_started'},
 {id:'phase-completed-throws',events:[delta('turn completed')],phaseThrows:'cron_completed'},
 {id:'title-utf16',message:' \uFEFF'+('😀'.repeat(60))+'X\rinside\nsecond line',events:[delta(' ﻿ raw 😀 \t')]},
 {id:'task-id-lone-surrogate',taskId:'task-\ud800',events:[delta('kept UTF16 identity')]},
];
const clean=value=>JSON.parse(JSON.stringify(value));
async function files(root,prefix=''){
 let result={};
 for(const entry of await readdir(root,{withFileTypes:true})){
  const name=prefix+entry.name,p=join(root,entry.name);
  if(entry.isDirectory())Object.assign(result,await files(p,name+'/'));
  else {assert.ok(!name.endsWith('.tmp'),'no temp file may be silently ignored');result[name]=await readFile(p,'utf8');}
 }return Object.fromEntries(Object.entries(result).sort(([a],[b])=>a.localeCompare(b,'en')));
}
await mkdir(out,{recursive:false});const rows=[];
for(const input of cases){
 const root=join(out,input.id);await mkdir(root);const projectDir=join(root,'project');await mkdir(projectDir);
 const storePaths={pilotHome:root,projectKey:'/project',projectId:'fixture',rootDir:root,projectDir,tasksFile:join(projectDir,'tasks.json'),runsDir:join(projectDir,'runs'),runHistoryFile:join(projectDir,'run-history.jsonl')};
 const store=new CronTaskStore(storePaths);
 const task={schemaVersion:1,taskId:input.taskId??'task',message:input.message??'  first line\r\nsecond line',schedule:input.recurring?{type:'cron',expression:'*/5 * * * *',timezone:'UTC'}:{type:'once',runAt:'2025-12-31T00:00:00.000Z'},status:'scheduled',sessionKey:'cron:task',channelKey:'cron',projectKey:'/project',mode:'default',timezone:'UTC',createdAt:'2025-12-31T00:00:00.000Z',updatedAt:'2025-12-31T00:00:00.000Z',nextRunAt:'2025-12-31T00:00:00.000Z',revision:0,...(input.recurring?{scheduleComputationVersion:3}:{}),originSessionKey:'feishu:chat:fixture',originChannelKey:'feishu'};
 const initialTasksFile=JSON.stringify({schemaVersion:1,tasks:[task]},null,2);await writeFile(storePaths.tasksFile,initialTasksFile,{flag:'wx'});
 const snapshot=clean(task);if(input.stale)snapshot.revision=7;
 const runId='run-'+input.id;const active=new Map();const gatewayCalls=[],phases=[],turnEvents=[],deliveries=[],releases=[],warns=[];let clockCalls=0;
 const gateway={async *submitTurn(value){gatewayCalls.push({method:'submitTurn',input:clean(value)});for(const event of input.events)yield clean(event);
   if(input.edit)await store.updateTask(task.taskId,current=>({...current,message:'edited',revision:current.revision+1}));
   if(input.deleted)await store.updateTask(task.taskId,()=>undefined);
   if(input.stopped)active.get(runId).stopRequested=true;
   if(input.throwMessage)throw new Error(input.throwMessage);
  },async abortTurn(value){gatewayCalls.push({method:'abortTurn',input:clean(value)})}};
 const fire=new CronFire({gateway,store,now(){return new Date(clockCalls++===0?START:FINISH)},registerActiveRun(value){active.set(value.runId,value)},unregisterActiveRun(id){const value=active.get(id);active.delete(id);return value},getActiveRun(id){return active.get(id)},runTimeoutMs:60000,defaultTimezone:'UTC',async releaseTaskSession(value){releases.push(clean(value))},async onResultDelivery(value){deliveries.push(clean(value));if(input.deliveryThrows)throw new Error('fixture delivery throw')},onTurnEvent(sessionKey,channelKey,event){turnEvents.push({sessionKey,channelKey,event:clean(event)});if(input.turnThrows)throw new Error('fixture turn callback throw')},onPhaseEvent(value){phases.push(clean(value));if(input.phaseThrows===value.phase)throw new Error('fixture phase throw')},logger:{warn(message,meta){warns.push({message,meta:clean(meta)})}}});
 let error=null;try{await fire.runTask(snapshot,runId)}catch(e){error={name:e.name,message:e.message}};
 rows.push({id:input.id,input:clean(input),initialTasksFile,snapshot,runId,gatewayCalls,phases,turnEvents,deliveries,releases,warns,active:[...active.values()],clockCalls,error,after:await files(projectDir)});
}
assert.deepEqual(warnings,[]);await writeFile(join(out,'oracle.json'),JSON.stringify({metadata:{sourceCommit,node:process.version,icu:process.versions.icu,typescript:ts.version,sourceSha256:hashes,clock:{start:START,finish:FINISH},network:'seatbelt-deny-all, no fork; runtime APIs throw',normalization:'none; exact after file text, no ignored temp files'},rows},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({rows:rows.length,warnings,oracle:join(out,'oracle.json')}));
