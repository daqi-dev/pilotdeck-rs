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

const {CronScheduler}=load("src/cron/runtime/CronScheduler.ts",{"../CronTimezone.js":timezone,"./CronSchedule.js":schedule});
const START=1767225600000;await mkdir(out);const rows=[];
for(const kind of ["delay","once-recalc","cron-recalc"]){
 const root=join(out,kind);await mkdir(root);const projectDir=join(root,"project");await mkdir(projectDir);
 const storePaths={pilotHome:root,projectKey:"/project",projectId:"fixture",rootDir:root,projectDir,tasksFile:join(projectDir,"tasks.json"),runsDir:join(projectDir,"runs"),runHistoryFile:join(projectDir,"run-history.jsonl")};
 const store=new CronTaskStore(storePaths);let task={schemaVersion:1,taskId:"task-"+String.fromCharCode(0xd800),message:"seeded",schedule:kind==="cron-recalc"?{type:"cron",expression:"0 * 1 * 0",timezone:"UTC"}:{type:"once",runAt:"2025-12-31T00:00:00.000Z"},status:"scheduled",sessionKey:"cron:task",channelKey:"cron",projectKey:"/project",mode:"default",timezone:"UTC",createdAt:"2025-12-31T00:00:00.000Z",updatedAt:"2025-12-31T00:00:00.000Z",revision:0};
 if(kind==="delay")task.nextRunAt="2025-12-31T00:00:00.000Z";if(kind==="cron-recalc"){task.nextRunAt="2026-01-02T00:00:00.000Z";task.scheduleComputationVersion=2}
 const initialTasksFile=JSON.stringify({schemaVersion:1,tasks:[task]},null,2);await writeFile(storePaths.tasksFile,initialTasksFile,{flag:"wx"});
 const scheduler=new CronScheduler({config:{enabled:true,timezone:"UTC",maxConcurrentRuns:1,runTimeoutMinutes:1},store,fire:{runTask(){throw new Error("must not fire")}},uuid(){throw new Error("must not allocate runId")},now(){return new Date(START)},activeRunCount(){return 1}});
 if(kind==="delay")await scheduler.runTickOnce();else await scheduler.recalculateAllNextRuns();await scheduler.stop();
 assert.deepEqual((await readdir(projectDir)).sort(),["tasks.json"]);rows.push({id:kind,initialTasksFile,after:{"tasks.json":await readFile(storePaths.tasksFile,"utf8")}});
}
assert.deepEqual(warnings,[]);await writeFile(join(out,"oracle.json"),JSON.stringify({metadata:{sourceCommit,node:process.version,icu:process.versions.icu,typescript:ts.version,sourceSha256:hashes,normalization:"none",now:START},rows},null,2)+"\n",{flag:"wx"});console.log(JSON.stringify({rows:rows.length}));
