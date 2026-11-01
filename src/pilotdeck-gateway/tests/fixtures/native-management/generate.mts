// #1231: real fixed TS gateway calls plus the explicit refresh seam.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
import {EventEmitter} from 'node:events';
// Deterministic watcher seam: no OS timing, explicit reload drives notifications.
// All filesystem operations and gateway/domain implementations remain real.
fs.watch=((..._args:any[])=>Object.assign(new EventEmitter(),{close(){},ref(){return this;},unref(){return this;}})) as any;
syncBuiltinESMExports();
import {mkdir, mkdtemp, writeFile, rm, rename, readFile, utimes} from 'node:fs/promises';
import {execFileSync} from 'node:child_process';
import {setTimeout as nextTick} from 'node:timers/promises';
import {tmpdir} from 'node:os';
import {join, resolve, dirname} from 'node:path';
import {pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#1231 pinned Node');
assert.equal(process.versions.icu, '78.2', '#1231 pinned ICU');
assert.equal(new Intl.Collator().resolvedOptions().locale, 'en-US', '#1231 locale');
assert.ok(process.env.PDRS_BASELINE, '#1231 baseline');
assert.ok(process.argv[2], '#1231 output');
const baseline=resolve(process.env.PDRS_BASELINE!), output=resolve(process.argv[2]);
const sourceCommit=assertBaseline(baseline);
const sourceSha256=await baselineSourceHashes(baseline, ['src/cli/createLocalGateway.ts','src/cli/GatewayRuntimeRefreshBundle.ts','src/gateway/client/InProcessGateway.ts','src/gateway/dialog/commands.ts','src/extension/skills/SkillManager.ts']);
const root=await mkdtemp(join(tmpdir(),'pdrs-1231-'));
const home=join(root,'home'), project=join(root,'project');
for(const path of [home,project,join(home,'builtin')]) await mkdir(path,{recursive:true});
process.env.HOME=home; process.env.PILOT_HOME=home;
process.env.XDG_CONFIG_HOME=join(home,'xdg'); process.env.XDG_CACHE_HOME=join(home,'xdg'); process.env.XDG_DATA_HOME=join(home,'xdg');
process.env.PILOTDECK_STORAGE_ROOT=join(home,'storage');
delete process.env.PILOTDECK_SNAPSHOT_ROOT;
process.chdir(project);
await writeFile(join(home,'pilotdeck.yaml'),'schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: test-only\n      models:\n        test: {}\n',{flag:'wx'});
await mkdir(join(project,'.pilotdeck','commands'),{recursive:true});
await writeFile(join(project,'.pilotdeck','commands','a8demo.md'),'# A8 command\n',{flag:'wx'});
await writeFile(join(project,'.pilotdeck','commands','replacement�.md'),'# Replacement command\n',{flag:'wx'});
const {createLocalGateway}=await import(pathToFileURL(join(baseline,'src/cli/createLocalGateway.ts')).href);
const {getPilotProjectChatDir}=await import(pathToFileURL(join(baseline,'src/pilot/paths.ts')).href);
const chat=getPilotProjectChatDir(project,home);await mkdir(chat,{recursive:true});
await writeFile(join(dirname(chat),'.cwd'),project+'\n',{flag:'wx'});
const sessionPath=join(chat,'a8-session.jsonl');
await writeFile(sessionPath,await readFile(new URL('./session.jsonl',import.meta.url)),{flag:'wx'});
await utimes(sessionPath,new Date(1700000000000),new Date(1700000000000));
const {listProjectSessions}=await import(pathToFileURL(join(baseline,'src/session/storage/SessionList.ts')).href);
const local=createLocalGateway({projectRoot:project,pilotHome:home,builtinSkillsRoot:join(home,'builtin'),sessionCatalog:{list:listProjectSessions}});
const clean=(v:any):any=>{
 if(typeof v==='string') return v.split(root).join('<root>');
 if(Array.isArray(v)) return v.map(clean);
 if(v&&typeof v==='object') return Object.fromEntries(Object.entries(v).map(([key,value])=>{
  if(key==='nextCursor'&&typeof value==='string'){
   const cursor=JSON.parse(Buffer.from(value,'base64url').toString());
   cursor.signature=JSON.parse(Buffer.from(cursor.signature,'base64url').toString());
   return [key,clean(cursor)];
  }
  if(key==='mtime'){assert.equal(typeof value,'number','#1231 skill mtime numeric');assert.ok(Number.isFinite(value),'#1231 finite mtime');return [key,'<mtime>'];}
  return [key,clean(value)];
 }));
 return v;
};
const expand=(v:any):any=>typeof v==='string'?v.replaceAll('<root>',root):Array.isArray(v)?v.map(expand):v&&typeof v==='object'?Object.fromEntries(Object.entries(v).map(([k,x])=>[k,expand(x)])):v;
const methods:any={snapshot_list:'snapshotList',snapshot_get:'snapshotGet',snapshot_restore:'snapshotRestore',manager_sessions:'managerSessions',manager_browsers:'managerBrowsers',commands_list:'commandsList',skill_list:'skillsList',skill_read:'skillRead',skill_write:'skillWrite',skill_create:'skillCreate',skill_delete:'skillDelete',skill_import:'skillImport',skill_validate:'skillValidate',skill_scan:'skillScan'};
const call=async(method:string,params:any)=>{try{return {ok:clean(await local.gateway[methods[method]](expand(params)))};}catch(e:any){return {error:clean({code:e.code||'gateway_request_failed',message:e.message,...(e.validation?{validation:e.validation}:{})})};}};
let trace:any[]=[];
const registry:any=local.registry, router:any=(local.gateway as any).router;
let created=0;
const session=()=>{const id=++created;return {sessionId:`fixture-${id}`,pendingTurnCount:0,snapshot:()=>({sessionId:`fixture-${id}`,messages:[]}),abort(){},async dispose(){},pendingTurns:()=>[],async discardQueuedTurns(){}};};
router.options.createSession=session;router.options.recreateSession=session;
const sessionContext={sessionKey:'fixture',projectKey:project,channelKey:'test'};
const invalidate=registry.invalidate.bind(registry), dirty=router.markProjectDirty.bind(router);
registry.invalidate=(key?:string)=>{invalidate(key);trace.push({event:'invalidate',projectKey:clean(key)});};
router.markProjectDirty=(key:string,reason:string)=>{const count=dirty(key,reason);trace.push({event:'dirty',projectKey:clean(key),reason,count});return count;};
local.bindServer({broadcastNotification:(name:string,payload:any)=>trace.push({event:'notification',name,payload:clean(payload)})} as any);
const p={projectKey:'<root>/project'};
const specs=[
 {family:'U3',method:'snapshot_list',params:p,read:'snapshot_list',readParams:p},
 {family:'U4',method:'manager_sessions',params:p,read:'manager_sessions',readParams:p},
 {family:'U5',method:'commands_list',params:{...p,query:'a8demo'},read:'commands_list',readParams:{...p,query:'a8demo'}},
 {family:'U11',method:'skill_create',params:{...p,scope:'project',slug:'demo',content:'# Demo\n'},read:'skill_read',readParams:{...p,scope:'project',slug:'demo'}},
];
const cases:any[]=[];
try {
 for(const spec of specs){
  trace=[];
  const before=registry.resolve(project);
  const beforeSession=await router.getOrCreate(sessionContext);
  const first=await call(spec.method,spec.params);
  const directTrace=[...trace]; trace=[];
  const refreshed=await local.gateway.reloadExtensions({projectKey:project,changedPaths:[spec.family]});
  const refreshTrace=[...trace]; trace=[];
  const second=await call(spec.read,spec.readParams);
  const after=registry.resolve(project);
  const afterSession=await router.getOrCreate(sessionContext);
  cases.push({...spec,first,directTrace,refreshed,refreshTrace,second,generationChanged:before!==after,dirtyRecreated:beforeSession!==afterSession});
 }
 const requests:any[]=[];
 for(const queryUnits of [[0xd800],[32,0xdc00,32],Array(257).fill(0xd800)]) {
  requests.push({method:'commands_list',params:p,queryUnits,result:await call('commands_list',{...p,query:String.fromCharCode(...queryUnits)})});
 }

 for(const cursorForm of ['array','buffer','arraylike','fractional-length','string-bytes','wrapped-bytes','bad-valueOf','string-length','bad-buffer-data','empty-array','empty-buffer','zero-length','buffer-length','null-length-buffer','valueOf-array','high-base64']) {
  const params={...p,query:'no-such-command-1231'};
  const signature=Buffer.from(JSON.stringify({projectKey:project,query:params.query})).toString('base64url');
  const bytes=[...Buffer.from(JSON.stringify({offset:0,signature}))];
  const arraylike=Object.assign({length:bytes.length},bytes);
  const cursor:any=cursorForm==='valueOf-array'?{...arraylike,valueOf:[5]}:cursorForm==='high-base64'?Buffer.from(bytes).toString('base64url').split('').map(c=>String.fromCharCode(c.charCodeAt(0)+256)).join(''):cursorForm==='empty-array'?[]:cursorForm==='empty-buffer'?{type:'Buffer',data:[]}:cursorForm==='zero-length'?{length:0}:cursorForm==='buffer-length'?{type:'Buffer',data:bytes,length:3}:cursorForm==='null-length-buffer'?{type:'Buffer',data:bytes,length:null}:cursorForm==='array'?bytes:cursorForm==='buffer'?{type:'Buffer',data:bytes}:cursorForm==='string-bytes'?bytes.map(String):cursorForm==='wrapped-bytes'?bytes.map(n=>n+256):cursorForm==='fractional-length'?{...arraylike,length:bytes.length+0.9}:cursorForm==='bad-valueOf'?{...arraylike,valueOf:5}:cursorForm==='string-length'?{...arraylike,length:String(bytes.length)}:cursorForm==='bad-buffer-data'?{type:'Buffer',data:arraylike}:arraylike;
  requests.push({method:'commands_list',params,cursorForm,result:await call('commands_list',{...params,cursor})});
 }

 for(const params of [{},...[-1,0,1,100,101,1.5,4294967297,'1',true,null].map(limit=>({limit})),{query:3},{query:'x'.repeat(257)},{cursor:'bad'}]) {
  const input={...p,...params}; requests.push({method:'commands_list',params:input,result:await call('commands_list',input)});
 }
 for(const params of [null,{}, {projectKey:null},{projectKey:4}]) requests.push({method:'commands_list',params,result:await call('commands_list',params)});
 for(const [method,params] of [
  ['snapshot_get',{...p,snapshotId:'missing'}],['snapshot_restore',{...p,snapshotId:'missing'}],['snapshot_list',{projectKey:null}],
  ['manager_browsers',p],['skill_list',{...p,scope:'project'}],['skill_write',{...p,scope:'project',slug:'demo',content:'# Updated\n'}],
  ['skill_read',{...p,scope:'project',slug:'demo'}],['skill_validate',{sourcePath:'<root>/project/.pilotdeck/skills/demo'}],
  ['skill_scan',{parentPath:'<root>/project/.pilotdeck/skills'}],['skill_delete',{...p,scope:'project',slug:'demo'}],
  ['skill_read',{...p,scope:'project',slug:'demo'}],['skill_import',{...p,scope:'project',sourcePath:'<root>/absent'}],
 ] as any[]) requests.push({method,params,result:await call(method,params)});
 // A FIFO keeps the real skill read pending. The writer is released only
 // after an independent RPC completes; no CPU load or timing sleep is used.
 const fifoDir=join(project,'.pilotdeck','skills','pending');await mkdir(fifoDir,{recursive:true});
 const fifo=join(fifoDir,'SKILL.md');execFileSync('mkfifo',[fifo]);
 let settled=false;
 const pending=call('skill_read',{...p,scope:'project',slug:'pending'}).finally(()=>{settled=true;});
 const deadline=performance.now()+3000;let writer:number|undefined;
 while(writer===undefined){try{writer=fs.openSync(fifo,fs.constants.O_WRONLY|fs.constants.O_NONBLOCK);}catch(e:any){if(e.code!=='ENXIO'||performance.now()>deadline)throw e;await nextTick(1);}}
 let independent:any;let readPending=false;
 try {
  independent=await Promise.race([call('snapshot_list',p),new Promise((_,reject)=>{const timer=setTimeout(()=>reject(Error('#1231 independent RPC deadline')),3000);timer.unref();})]);
  readPending=!settled;
 } finally {
  await writeFile(fifo+'.next','# Released\n',{flag:'wx'});await rename(fifo+'.next',fifo);
  fs.writeSync(writer,'# Released\n');fs.closeSync(writer);
 }
 const pendingIo={readPending,independent,result:await pending};
 assert.equal(readPending,true,'#1231 predecessor remains pending through independent RPC');
 assertBaseline(baseline);
 const oracle={sourceCommit,node:process.version,icu:process.versions.icu,sourceSha256,cases,requests,pendingIo};
 await mkdir(output,{recursive:true});
 await writeFile(join(output,'oracle.json'),JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
 console.log(JSON.stringify({node:process.version,icu:process.versions.icu,cases,requests,pendingIo},null,2));
} finally {await local.dispose();process.chdir(tmpdir());await rm(root,{recursive:true,force:true});}
