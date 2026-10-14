// Execute pinned production list/callback + WS encoder. Hooks mutate owned
// files just before an fs boundary; they never manufacture an Error/message.
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import {syncBuiltinESMExports, stripTypeScriptTypes} from 'node:module';
import {resolve, join, dirname} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
const baseline=resolve(process.argv[2]), output=resolve(process.argv[3]), scratch=resolve(process.argv[4]);
if(process.version!=='v22.22.0'||process.getuid?.()===0)throw Error('non-root Node 22.22.0 required');
for(const k of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR'])if(!process.env[k]?.startsWith(scratch+'/'))throw Error(`Unisolated ${k}`);
if(process.env.SKIP_BROWSER_OPEN!=='1'||process.env.PILOTDECK_SKIP_BROWSER_OPEN!=='1')throw Error('Browser guards missing');
const source=(file:string)=>join(baseline,'src',file), load=(file:string)=>import(pathToFileURL(source(file)).href);
const native={realpath:fsp.realpath,stat:fsp.stat,readFile:fsp.readFile,readdir:fsp.readdir};
const {GatewayWsConnection}=await load('gateway/server/GatewayWsConnection.ts');
const {InProcessGateway}=await load('gateway/client/InProcessGateway.ts');
const {listWebProjects}=await load('web/server/listProjects.ts');
const {createDialogProjectRegistry}=await load('gateway/dialog/projectRegistry.ts');
const {DialogGatewayError}=await load('gateway/dialog/errors.ts');
const {isPathWithinRoot}=await load('tool/builtin/filesystem/pathSafety.ts');
const text=fs.readFileSync(source('cli/createLocalGateway.ts'),'utf8');
const body=text.split('    async projectFileRead(input) {\n')[1].split('\n    },')[0];
if(!body?.includes('data.subarray'))throw Error('Source callback changed');
const AsyncFunction=Object.getPrototypeOf(async function(){}).constructor;
const callback=new AsyncFunction('input','dialog','realpath','resolve','isPathWithinRoot','DialogGatewayError','statAsync','readFileAsync',stripTypeScriptTypes('async function oracle(input) {\n'+body+'\n}').split('\n').slice(1,-1).join('\n'));
const boundaries=[
 ['project_files_list','root-realpath','realpath','project'],
 ['project_files_list','root-stat','stat','project'],
 ['project_files_list','root-scandir','readdir','project'],
 ['project_files_list','entry-realpath','realpath','project/entry'],
 ['project_files_list','entry-stat','stat','project/entry'],
 ['project_files_list','nested-scandir','readdir','project/nested'],
 ['project_file_read','root-realpath','realpath','project'],
 ['project_file_read','file-realpath','realpath','project/entry'],
 ['project_file_read','file-stat','stat','project/entry'],
 ['project_file_read','file-open','readFile','project/entry'],
] as const;
const cases:any[]=[];
for(const [method,boundary,operation,relative] of boundaries)for(const code of ['ENAMETOOLONG','ELOOP','EACCES','ENOENT','ENOTDIR']){
 const dir=join(scratch,'cases',String(cases.length)), home=join(dir,'home'), root=join(dir,'project'), target=join(dir,relative);
 fs.mkdirSync(join(home,'projects','registered'),{recursive:true});fs.mkdirSync(root);
 fs.writeFileSync(join(home,'projects','registered','.cwd'),root);fs.writeFileSync(join(root,'entry'),'inside');fs.mkdirSync(join(root,'nested'));
 process.chdir(dir);process.env.PILOT_HOME=home;
 const files:any={};
 function collect(p:string){const s=fs.statSync(p);files[p]={directory:s.isDirectory(),size:s.size,birth_ms:s.birthtimeMs,ctime_ms:s.ctimeMs,mtime_ms:s.mtimeMs};if(s.isDirectory()){files[p].entries=fs.readdirSync(p,{withFileTypes:true}).map(e=>[e.name,e.isDirectory()]);for(const [n] of files[p].entries)collect(join(p,n));}else files[p].base64=fs.readFileSync(p).toString('base64');}
 collect(dir);
 let active=false,mutated=false;const observed:any[]=[];
 const mutate=()=>{
  mutated=true;
  if(code==='EACCES'){
   // stat/realpath need denied search on the parent; scandir needs denied
   // directory read; readFile needs denied file read.
   fs.chmodSync(operation==='stat'||operation==='realpath'?dirname(target):target,0);
  }else if(code==='ENOTDIR'){
   // Replace the parent with a regular file. For scandir, replacing the
   // directory itself is sufficient; both produce a real ENOTDIR.
   const p=operation==='readdir'?target:dirname(target);fs.renameSync(p,p+'.held');fs.writeFileSync(p,'not directory');
  }else{
   fs.renameSync(target,target+'.held');
   if(code!=='ENOENT')fs.symlinkSync(code==='ELOOP'?target:'x'.repeat(300),target);
  }
 };
 for(const op of Object.keys(native) as (keyof typeof native)[]){
  (fsp as any)[op]=async(p:any,...args:any[])=>{
   const hit=active&&op===operation&&String(p)===target;
   if(hit&&!mutated)mutate();
   try{return await (native[op] as any)(p,...args);}catch(e:any){if(hit)observed.push({code:e.code,syscall:e.syscall,message:e.message,path:e.path});throw e;}
  };
 }
 syncBuiltinESMExports();
 const catalog=async()=>{const result=await listWebProjects({pilotHome:home});active=true;return result;};
 const registry=createDialogProjectRegistry({pilotHome:home,listProjects:async()=>(await listWebProjects({pilotHome:home})).projects});
 const projects={...registry,async resolveProjectKey(p:string){const result=await registry.resolveProjectKey(p);active=true;return result;}};
 const gateway:any={options:{listProjects:catalog},listProjects:catalog,describeServer:async()=>({}),projectFilesList(input:any){return InProcessGateway.prototype.projectFilesList.call(this,input);},projectFileRead(input:any){return callback(input,{projects},fsp.realpath,resolve,isPathWithinRoot,DialogGatewayError,fsp.stat,fsp.readFile);}};
 let receive:any,finish:any;const pending=new Promise<string>(r=>finish=r);
 const ws={onMessage(fn:any){receive=fn;},onClose(){},sendText(text:string){if(JSON.parse(text).type==='response')finish(text);},close(){throw Error('Unexpected close');}};
 new GatewayWsConnection(ws as any,{gateway,token:'offline',serverVersion:'offline'});
 receive(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'offline',token:'offline'}));await new Promise(r=>setImmediate(r));
 const request=JSON.stringify({type:'request',id:`files-io-${cases.length}`,method,params:{projectKey:root,...(method==='project_files_list'?{query:'absent'}:{path:'entry'})}});
 let response:string;
 try{receive(request);response=await pending;}finally{
  Object.assign(fsp,native);syncBuiltinESMExports();
  if(mutated){
   if(code==='EACCES')fs.chmodSync(operation==='stat'||operation==='realpath'?dirname(target):target,0o700);
   else if(code==='ENOTDIR'){const p=operation==='readdir'?target:dirname(target);fs.unlinkSync(p);fs.renameSync(p+'.held',p);}
   else{if(code!=='ENOENT')fs.unlinkSync(target);fs.renameSync(target+'.held',target);}
  }
 }
 if(!mutated||observed.length!==1||observed[0].code!==code)throw Error(JSON.stringify({method,boundary,code,mutated,observed,response:response!}));
 // One common root mapping for requests, wire, actual fs observations and
 // snapshot inputs. All remaining bytes/field order are retained.
 for(const fact of Object.values(files) as any[])if(fact.base64)fact.base64=Buffer.from(Buffer.from(fact.base64,'base64').toString('utf8').split(dir).join('/fixture')).toString('base64');
 const row={method,boundary,operation:operation==='readdir'?'scandir':operation==='readFile'?'open':operation,target,cwd:dir,home,root,files,observed,request,response:response!};
 cases.push(JSON.parse(JSON.stringify(row).split(dir).join('/fixture')));
}
fs.mkdirSync(dirname(output),{recursive:true});fs.writeFileSync(output,JSON.stringify({baselineCommit:execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),node:process.version,icu:process.versions.icu,platform:process.platform,uid:process.getuid?.(),sourceSha256:{read:createHash('sha256').update(body).digest('hex'),list:createHash('sha256').update(fs.readFileSync(source('gateway/dialog/projectFiles.ts'))).digest('hex')},isolation:Object.fromEntries(['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR'].map(k=>[k,process.env[k]])),cases},null,2)+'\n');
console.log(JSON.stringify({cases:cases.length,output}));
