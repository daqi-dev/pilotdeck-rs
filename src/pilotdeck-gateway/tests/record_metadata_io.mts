// #139 default describe_project chain. No listener, fake FS errors or user home.
// <fixed baseline> <new output.json> <new isolated scratch>; --restore <output.json>
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import {syncBuiltinESMExports} from 'node:module';
import {resolve,join,dirname,sep} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash,randomUUID} from 'node:crypto';
import {bindSourceTree} from './baseline_source_guard.mjs';
const COMMIT='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
if(process.argv[2]==='--restore') {
 const file=resolve(process.argv[3]);
 if(!process.argv[4])throw Error('driver memory restore snapshot required');
 const fixture=JSON.parse(process.argv[4]);
 if(fixture.baselineCommit!==COMMIT||fs.readFileSync(join(fixture.scratch,'.metadata-oracle-owner'),'utf8')!==fixture.owner)throw Error('unowned fixture');
 for(const item of fixture.permissions) {
  const root=fs.realpathSync(fixture.scratch),target=fs.realpathSync(item.path);
  if(!target.startsWith(root+sep)||fs.lstatSync(item.path).isSymbolicLink()||!Number.isInteger(item.mode)||item.mode<0||item.mode>0o777)throw Error('outside owned scratch or invalid mode');
  fs.chmodSync(item.path,item.mode);
  if((fs.statSync(item.path).mode&0o777)!==item.mode)throw Error('mode not restored');
 }
 fs.writeFileSync(file+'.cleanup.json',JSON.stringify({restored:fixture.permissions.length,owner:fixture.owner},null,2)+'\n',{flag:'wx'});
 process.exit(0);
}
const baseline=resolve(process.argv[2]),output=resolve(process.argv[3]),scratch=resolve(process.argv[4]);
const gitExecutable=process.env.PDRS_METADATA_GIT;if(!gitExecutable)throw Error('explicit Git required');
if(process.version!=='v22.22.0')throw Error('fixed Node22.22 required');
if(fs.existsSync(output)||fs.existsSync(join(scratch,'cases')))throw Error('fresh output and scratch required');
for(const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME'])if(!process.env[key]||!resolve(process.env[key]!).startsWith(scratch+sep))throw Error('Unisolated '+key);
if(process.env.SKIP_BROWSER_OPEN!=='1'||process.env.PILOTDECK_SKIP_BROWSER_OPEN!=='1')throw Error('browser guards required');
const sourceBindingBefore=bindSourceTree(baseline,COMMIT,gitExecutable);
const source=(p:string)=>import(pathToFileURL(join(baseline,'src',p)).href);
const {GatewayWsConnection}=await source('gateway/server/GatewayWsConnection.ts');
const {describeWebProject}=await source('web/server/listProjects.ts');
const {createProjectId}=await source('pilot/paths.ts');
const real:any={};for(const [kind,obj,names] of [['sync',fs,['statSync','readdirSync','readFileSync','existsSync']],['async',fsp,['stat','readdir','readFile','open']]] as const)for(const name of names)real[kind+':'+name]=(obj as any)[name];
const owner=randomUUID();fs.writeFileSync(join(scratch,'.metadata-oracle-owner'),owner,{flag:'wx'});
const permissions:any[]=[],cases:any[]=[];
function restrict(path:string,mode:number) {
 const permission={path,mode:fs.statSync(path).mode&0o777};permissions.push(permission);
 const checkpoint='PDRS_METADATA_PERMISSION '+JSON.stringify({baselineCommit:COMMIT,owner,scratch,permission})+'\n';
 if(fs.writeSync(1,checkpoint)!==Buffer.byteLength(checkpoint))throw Error('incomplete permission checkpoint');
 fs.chmodSync(path,mode);
}
const ids=['positive','collision-positive','projects-missing','projects-notdir','projects-denied-legacy','projects-denied-marker','chats-missing','chats-notdir','chats-denied','chats-loop'];
let observations:any[]=[];
function value(v:any):any {
 if(v&&typeof v.isDirectory==='function')return {directory:v.isDirectory(),size:v.size,birth_ms:v.birthtimeMs,ctime_ms:v.ctimeMs,mtime_ms:v.mtimeMs};
 if(Buffer.isBuffer(v))return {base64:v.toString('base64')};
 if(Array.isArray(v))return v.map(e=>typeof e==='string'?e:[e.name,e.isDirectory()]);
 if(v&&typeof v.bytesRead==='number')return {bytesRead:v.bytesRead,base64:v.buffer.subarray(0,v.bytesRead).toString('base64')};
 if(v&&typeof v.fd==='number')return {fd:v.fd};
 return v;
}
function trace(api:string,path:any,stack:string|undefined,result:any,error?:any) {
 observations.push({api,path:String(path),caller:stack,ok:!error,...(error?{code:error.code,syscall:error.syscall,message:error.message}:{result:value(result)})});
}
function install() {
 for(const name of ['statSync','readdirSync','readFileSync','existsSync']) (fs as any)[name]=function(path:any,...args:any[]){const stack=new Error().stack;try{const v=real['sync:'+name](path,...args);trace('fs.'+name,path,stack,v);return v;}catch(e){trace('fs.'+name,path,stack,null,e);throw e;}};
 for(const name of ['stat','readdir','readFile','open']) (fsp as any)[name]=async function(path:any,...args:any[]){const stack=new Error().stack;try{const v=await real['async:'+name](path,...args);trace('fsp.'+name,path,stack,v);
  if(name==='open')for(const method of ['stat','read','readFile']){const fn=v[method].bind(v);v[method]=async(...a:any[])=>{const call=new Error().stack;try{const r=await fn(...a);trace('FileHandle.'+method,path,call,r);return r;}catch(e){trace('FileHandle.'+method,path,call,null,e);throw e;}};}
  return v;
 }catch(e){trace('fsp.'+name,path,stack,null,e);throw e;}};
 syncBuiltinESMExports();
}
function reset() {for(const name of ['statSync','readdirSync','readFileSync','existsSync'])(fs as any)[name]=real['sync:'+name];for(const name of ['stat','readdir','readFile','open'])(fsp as any)[name]=real['async:'+name];syncBuiltinESMExports();}
function snapshot(paths:string[]) {
 const files:any={};for(const path of paths){let s;try{s=real['sync:statSync'](path);files[path]=value(s);}catch(e:any){files[path]={statError:e.code};continue;}
  if(s.isDirectory()){try{files[path].entries=real['sync:readdirSync'](path,{withFileTypes:true}).map((e:any)=>[e.name,e.isDirectory()]);}catch(e:any){files[path].entriesError=e.code;}}
  else{try{files[path].bytes=real['sync:readFileSync'](path).toString('base64');}catch(e:any){files[path].readError=e.code;}}
 }return files;
}
let success=false;
try {
for(const id of ids) {
 const root=join(scratch,'cases',id),home=join(root,'home'),project=join(root,'project');fs.mkdirSync(project,{recursive:true});fs.mkdirSync(home);
 const projects=join(home,'projects'),legacy=join(projects,createProjectId(project)),storage=id==='collision-positive'?join(projects,'collision-registered'):legacy,chats=join(storage,'chats');
 fs.mkdirSync(chats,{recursive:true});if(id!=='projects-denied-legacy')fs.writeFileSync(join(storage,'.cwd'),project+'\n');
 const transcript=join(chats,'visible.jsonl');fs.writeFileSync(transcript,JSON.stringify({type:'session_metadata',sessionId:'visible',sequence:1,createdAt:'2026-10-16T00:00:00.000Z',metadata:{title:'recorded visible session'}})+'\n');fs.utimesSync(transcript,new Date('2026-10-16T00:00:01.234Z'),new Date('2026-10-16T00:00:01.234Z'));
 const paths:string[]=[];const collect=(path:string)=>{paths.push(path);if(real['sync:statSync'](path).isDirectory())for(const name of real['sync:readdirSync'](path))collect(join(path,name));};collect(root);
 if(id==='projects-missing'||id==='projects-notdir'){fs.renameSync(projects,projects+'.held');if(id==='projects-notdir')fs.writeFileSync(projects,'not directory');}
 if(id.startsWith('projects-denied')){restrict(projects,0o111);}
 if(['chats-missing','chats-notdir','chats-loop'].includes(id)){fs.renameSync(chats,chats+'.held');if(id==='chats-notdir')fs.writeFileSync(chats,'not directory');if(id==='chats-loop')fs.symlinkSync(chats,chats);}
 if(id==='chats-denied'){restrict(chats,0o0);}
 const files=snapshot(paths);let receive:any,finish:any;const ws={onMessage(fn:any){receive=fn},onClose(){},sendText(s:string){if(JSON.parse(s).type==='response')finish(s);},close(){throw Error('unexpected close');}};
 new GatewayWsConnection(ws as any,{token:'offline',serverVersion:'offline',gateway:{describeServer:async()=>({}),describeProject:(input:any)=>describeWebProject(input.projectKey,{pilotHome:home})} as any});
 receive(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'fixture',token:'offline'}));await new Promise(r=>setImmediate(r));
 observations=[];const request=JSON.stringify({type:'request',id,method:'describe_project',params:{projectKey:project}});let response:string;
 try {install();response=await new Promise<string>((done,reject)=>{const timer=setTimeout(()=>reject(Error('No response '+id)),5000);finish=(s:string)=>{clearTimeout(timer);done(s);};receive(request);});} finally {reset();}
 const target=id.startsWith('projects-')?projects:chats;const code=id.endsWith('missing')?'ENOENT':id.endsWith('notdir')?'ENOTDIR':id.includes('denied')?'EACCES':id.endsWith('loop')?'ELOOP':null;
 const intended=observations.filter(o=>!o.ok&&o.path===target&&(o.api==='fsp.readdir'||o.api==='fs.readdirSync'));
 const status=code&&!intended.some(o=>o.code===code)?'NOT_RUN':'RUN';
 const row={id,status,expectedError:code,errorTarget:target,home,cwd:root,project,storage,legacy,chats,request,response,files,observations};
 fs.writeFileSync(join(root,'raw-rpc.json'),JSON.stringify({request,response,observations},null,2)+'\n');cases.push(row);console.log(id,status,code??'positive');
}
const sourceBindingAfter=bindSourceTree(baseline,COMMIT,gitExecutable);if(JSON.stringify(sourceBindingBefore)!==JSON.stringify(sourceBindingAfter))throw Error('baseline sources changed during recording');
const sourceFiles=['src/gateway/server/GatewayWsConnection.ts','src/web/server/listProjects.ts','src/pilot/paths.ts','src/session/storage/SessionList.ts','src/session/storage/SessionLiteReader.ts'];const sources=Object.fromEntries(sourceFiles.map(p=>[p,createHash('sha256').update(fs.readFileSync(join(baseline,p))).digest('hex')]));
const oracleText=JSON.stringify({baselineCommit:COMMIT,node:process.version,icu:process.versions.icu,platform:process.platform,owner,scratch,permissions,sources,sourceBinding:{before:sourceBindingBefore,after:sourceBindingAfter},callPath:['GatewayWsConnection','describeWebProject','resolveProjectStorageDir','summarizeProject','getPilotProjectChatDir','listProjectSessions'],cases},null,2)+'\n';
fs.writeFileSync(output,oracleText,{flag:'wx'});
console.log('PDRS_METADATA_ORACLE '+JSON.stringify(oracleText));
success=true;if(cases.some(c=>c.status!=='RUN'))process.exitCode=2;
}finally{reset();if(!success)for(const item of permissions)fs.chmodSync(item.path,item.mode);}
