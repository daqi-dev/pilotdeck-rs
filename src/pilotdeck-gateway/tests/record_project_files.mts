// Offline oracle: execute pinned source, never start a service or edit the baseline.
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {stripTypeScriptTypes} from 'node:module';
import {spawnSync} from 'node:child_process';
const baseline=path.resolve(process.argv[2]), output=path.resolve(process.argv[3]), scratch=path.resolve(process.argv[4]);
await fs.mkdir(scratch,{recursive:true});
const cwd=await fs.mkdtemp(path.join(scratch,'files-'));
process.chdir(cwd);
const home=path.join(cwd,'home'), root=path.join(cwd,'project');
await fs.mkdir(home);await fs.mkdir(root);
process.env.PILOT_HOME=home;
const source=(file:string)=>path.join(baseline,'src',file);
const load=(file:string)=>import(pathToFileURL(source(file)).href);
const {GatewayWsConnection}=await load('gateway/server/GatewayWsConnection.ts');
const {InProcessGateway}=await load('gateway/client/InProcessGateway.ts');
const {listWebProjects}=await load('web/server/listProjects.ts');
const {createDialogProjectRegistry}=await load('gateway/dialog/projectRegistry.ts');
const {DialogGatewayError}=await load('gateway/dialog/errors.ts');
const {isPathWithinRoot}=await load('tool/builtin/filesystem/pathSafety.ts');
const storage=path.join(home,'projects','fixture');await fs.mkdir(storage,{recursive:true});await fs.writeFile(path.join(storage,'.cwd'),root);
const catalog=()=>listWebProjects({pilotHome:home});
const projects=createDialogProjectRegistry({pilotHome:home,listProjects:async()=>(await catalog()).projects});
// Extract the actual injected production callback, without constructing unrelated runtime services.
const sourceText=await fs.readFile(source('cli/createLocalGateway.ts'),'utf8');
const body=sourceText.split('    async projectFileRead(input) {\n')[1].split('\n    },')[0];
if(!body || !body.includes('data.subarray'))throw Error('source callback changed');
const AsyncFunction=Object.getPrototypeOf(async function(){}).constructor;
const callback=new AsyncFunction('input','dialog','realpath','resolve','isPathWithinRoot','DialogGatewayError','statAsync','readFileAsync',stripTypeScriptTypes('async function oracle(input) {\n'+body+'\n}').split('\n').slice(1,-1).join('\n'));
const gateway:any={options:{listProjects:catalog},listProjects:catalog,describeServer:async()=>({}),
 projectFilesList(input:any){return InProcessGateway.prototype.projectFilesList.call(this,input)},
 projectFileRead(input:any){return callback(input,{projects},fs.realpath,path.resolve,isPathWithinRoot,DialogGatewayError,fs.stat,fs.readFile)}};
for(const [name,data] of Object.entries({'z.txt':'z','A.txt':'A','a.txt':'a','ä.txt':'accent','_x':'underscore','10':'ten','2':'two','.hidden':'hidden','.gitignore':'ignored.txt\n','ignored.txt':'still listed','你好.txt':'中文','😀AA.txt':'emoji','bom.txt':'\ufeffhi\r\n','..secret':'odd prefix'}))await fs.writeFile(path.join(root,name),data);
await fs.writeFile(path.join(root,'binary'),Buffer.from([0,255,192,175,226,130,172,237,160,128]));
await fs.mkdir(path.join(root,'xonly'));await fs.writeFile(path.join(root,'xonly','readable'),'readable');
await fs.mkdir(path.join(root,'nested'));await fs.writeFile(path.join(root,'nested','note.txt'),'note');
for(const dir of ['.git','.pilotdeck','.tmp','node_modules','dist','build','coverage']){await fs.mkdir(path.join(root,dir));await fs.writeFile(path.join(root,dir,'excluded'),'excluded')}
await fs.writeFile(path.join(cwd,'outside'),'outside');
await fs.symlink(path.join(cwd,'outside'),path.join(root,'escape'));
await fs.symlink('nested',path.join(root,'alias-dir'));await fs.symlink('nested/note.txt',path.join(root,'alias-file'));
await fs.symlink(root,path.join(root,'cycle'));await fs.symlink('missing',path.join(root,'broken'));
// Fixed timestamp independently supplied, with fractional milliseconds.
async function stamp(dir:string){for(const e of await fs.readdir(dir,{withFileTypes:true})){const p=path.join(dir,e.name);if(e.isSymbolicLink())continue;if(e.isDirectory())await stamp(p);await fs.utimes(p,1700000000.123456,1700000000.123456)}}
await stamp(root);
const cases:any[]=[];
async function capture(method:string,params:any,tag=method){
 let receive:any,finish:any;const pending=new Promise<string>(r=>finish=r);
 const ws={onMessage(fn:any){receive=fn},onClose(){},sendText(text:string){if(JSON.parse(text).type==='response')finish(text)},close(){throw Error('closed')}};
 new GatewayWsConnection(ws as any,{gateway,token:'offline',serverVersion:'offline'});
 receive(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'offline',token:'offline'}));await new Promise(r=>setImmediate(r));
 const request=JSON.stringify({type:'request',id:`files-${cases.length+1}`,method,params});receive(request);
 const response=await pending;cases.push({tag,request,response});return JSON.parse(response);
}
await capture('project_files_list',{projectKey:root});
await capture('project_files_list',{projectKey:'project//./',includeDirs:false});
for(const query of [' NOTE ','aa','ALIAS','ignored','\ufeffä ','😀','x'.repeat(257)])await capture('project_files_list',{projectKey:root,query});
const page=await capture('project_files_list',{projectKey:root,limit:2});await capture('project_files_list',{projectKey:root,limit:3,cursor:page.result.nextCursor});
for(const params of [{limit:0},{limit:1.5},{limit:null},{cursor:'bad'},{query:'different',cursor:page.result.nextCursor},{projectKey:home},{projectKey:path.join(cwd,'unknown')}])await capture('project_files_list',{projectKey:root,...params});
await fs.chmod(path.join(root,'xonly'),0o111);await capture('project_file_read',{projectKey:root,path:'xonly/readable'},'execute-only-parent');await fs.chmod(path.join(root,'xonly'),0o755);
for(const file of ['bom.txt','binary','nested','missing','escape','broken','alias-file','nested//./note.txt','..secret'])await capture('project_file_read',{projectKey:root,path:file});
for(const maxBytes of [1,2,3,4,0,-1,2.9,null,'2','bad',true,false,'0x3','0b10','0o3','inf','Infinity',[],[true],[2],[null],[[2]],{}])await capture('project_file_read',{projectKey:root,path:'binary',maxBytes});
await capture('project_file_read',{projectKey:root,path:'binary',encoding:'base64'});
await capture('project_file_read',{projectKey:home,path:'projects/fixture/.cwd'});
await capture('project_file_read',{projectKey:path.join(cwd,'unknown'),path:'x'});
for(const file of ['../outside',path.join(cwd,'outside'),'nested/../bom.txt',path.join(root,'bom.txt')])await capture('project_file_read',{projectKey:root,path:file},'security-comparison');
// Exercise the recursion boundary in the real implementation without dumping
// the large directory result. A nonmatching query still traverses everything.
for(const depth of [64,65]) {
 const deep=path.join(cwd,`deep${depth}`);await fs.mkdir(deep);const deepStorage=path.join(home,'projects',`deep${depth}`);await fs.mkdir(deepStorage);await fs.writeFile(path.join(deepStorage,'.cwd'),deep);
 let nested=deep;for(let i=0;i<depth;i++){nested=path.join(nested,'d');await fs.mkdir(nested)}
 await capture('project_files_list',{projectKey:deep,query:'absent'},`depth-${depth}`);
}
const files:any={};
async function snapshot(p:string){const l=await fs.lstat(p);if(l.isSymbolicLink()){files[p]={link:await fs.readlink(p)};return}const s=await fs.stat(p);files[p]={directory:s.isDirectory(),size:s.size,birth_ms:s.birthtimeMs,ctime_ms:s.ctimeMs,mtime_ms:s.mtimeMs};if(s.isDirectory()){files[p].entries=(await fs.readdir(p,{withFileTypes:true})).map(e=>[e.name,e.isDirectory()]);for(const [name] of files[p].entries)await snapshot(path.join(p,name))}else files[p].base64=(await fs.readFile(p)).toString('base64')}
await snapshot(cwd);
const localeProbe = [];
for (const vars of [{LANG:'C.UTF-8'}, {LANG:'tr_TR.UTF-8'}, {LANG:'sv_SE.UTF-8'}, {LANG:'zh_CN.UTF-8'}, {LANG:'tr_TR.UTF-8',LC_ALL:'en_US.UTF-8'}, {LANG:'tr_TR.UTF-8',LC_MESSAGES:'sv_SE.UTF-8'}, {LANG:'tr_TR.UTF-8',LC_COLLATE:'sv_SE.UTF-8'}]) {
 const env={...process.env};for(const k of Object.keys(env))if(k.startsWith('LC_') || k==='LANG')delete env[k];Object.assign(env,vars);
 const code=`const names=['z','ä','Z','a','A','你好','文件','😀','_x']; console.log(JSON.stringify({locale:new Intl.Collator().resolvedOptions().locale,order:names.sort((a,b)=>a.localeCompare(b)),lower:['Iİıi','ΟΣ','İ','ẞ'].map(s=>s.toLocaleLowerCase())}));`;
 const result=spawnSync(process.execPath,['-e',code],{env,encoding:'utf8'});if(result.status!==0)throw Error(result.stderr);localeProbe.push({env:vars,...JSON.parse(result.stdout)});
}
const bytes=await fs.readFile(path.join(root,'binary'));
const runtime={maxBytesCoercion:["0x3","0b10","0o3","inf","Infinity",[],[true],[2],[null],[[2]],{}].map(value=>({value,bytes:Buffer.alloc(10).subarray(0,Math.max(1,Math.min(value as any,10000000))).length})),localeProbe,node:process.version,icu:process.versions.icu,locale:new Intl.Collator().resolvedOptions(),readdir:await fs.readdir(root),localeOrder:(await fs.readdir(root)).sort((a,b)=>a.localeCompare(b)),stat:files[path.join(root,'bom.txt')],bom:await fs.readFile(path.join(root,'bom.txt'),'utf8'),binary:[...Array(11).keys()].map(n=>({n,utf8:bytes.subarray(0,n).toString('utf8'),base64:bytes.subarray(0,n).toString('base64')})),paths:['','./','nested//./note.txt','nested/../bom.txt','/absolute'].map(p=>({input:p,resolved:path.resolve(root,p)})),links:await Promise.all(['alias-dir','alias-file','escape','cycle'].map(async p=>({path:p,real:await fs.realpath(path.join(root,p)),directory:(await fs.stat(path.join(root,p))).isDirectory(),direntDirectory:(await fs.lstat(path.join(root,p))).isDirectory()}))),sourceSha256:createHash('sha256').update(body).digest('hex')};
await fs.mkdir(output,{recursive:true});await fs.writeFile(path.join(output,'cases.json'),JSON.stringify({cwd,home,root,files,cases},null,2)+'\n');await fs.writeFile(path.join(output,'node-runtime.json'),JSON.stringify(runtime,null,2)+'\n');
console.log(JSON.stringify({cases:cases.length,cwd,output,node:runtime.node,icu:runtime.icu}));

// Separate deterministic race evidence: run the unchanged production callback
// with real Node filesystem calls, inserting a replacement at an await boundary.
// Keep these responses out of ordinary parity goldens: they expose a TS bug.
const races:any[]=[];
for (const stage of ['after-realpath','after-stat']) {
 for (const component of ['leaf','parent']) {
  const dir=path.join(root,`race-${stage}-${component}`);await fs.mkdir(dir);
  const target=path.join(dir,'file');await fs.writeFile(target,'INSIDE');
  const outside=path.join(cwd,`outside-${stage}-${component}`);await fs.mkdir(outside);await fs.writeFile(path.join(outside,'file'),'OUTSIDE_SECRET');
  let swapped=false;
  async function swap(){
   if(swapped)throw Error('duplicate swap');swapped=true;
   if(component==='leaf'){await fs.rename(target,target+'.saved');await fs.symlink(path.join(outside,'file'),target)}
   else {await fs.rename(dir,dir+'.saved');await fs.symlink(outside,dir)}
  }
  const realpath=async(p:string)=>{const result=await fs.realpath(p);if(p===target && stage==='after-realpath')await swap();return result};
  const stat=async(p:string)=>{const result=await fs.stat(p);if(p===target && stage==='after-stat')await swap();return result};
  const input={projectKey:root,path:path.relative(root,target)};
  const result=await callback(input,{projects},realpath,path.resolve,isPathWithinRoot,DialogGatewayError,stat,fs.readFile);
  if(!swapped || result.content!=='OUTSIDE_SECRET')throw Error('race did not reproduce');
  races.push({stage,component,input,result});
  if(component==='leaf'){await fs.unlink(target);await fs.rm(dir,{recursive:true})}
  else {await fs.unlink(dir);await fs.rm(dir+'.saved',{recursive:true})}
  await fs.rm(outside,{recursive:true});
 }
}
await fs.writeFile(path.join(root,'\ufffd'),'inside');
const utf16:any[]=[];
for(const inputPath of ['a/../\ud800',path.join(root,'\ud800'),'../\ud800','/outside/\ud800']) {
 const input={projectKey:root,path:inputPath};
 try {utf16.push({request:JSON.stringify(input),response:JSON.stringify({result:await gateway.projectFileRead(input)})})}
 catch(error:any){utf16.push({request:JSON.stringify(input),response:JSON.stringify({error:{code:error.code,message:error.message}})})}
}
await fs.unlink(path.join(root,'\ufffd'));
await fs.writeFile(path.join(output,'race-probes.json'),JSON.stringify({node:process.version,sourceSha256:runtime.sourceSha256,races,utf16},null,2)+'\n');
console.log(JSON.stringify({raceProbes:races.length,utf16Probes:utf16.length,output:path.join(output,'race-probes.json')}));

// Round 3: retain the earlier goldens; record runtime boundaries separately.
if (process.getuid?.() === 0) throw Error('Round 3 EACCES probe requires a non-root user');
const round3:any={node:process.version,icu:process.versions.icu,platform:process.platform,uid:process.getuid?.(),sourceSha256:runtime.sourceSha256,root};
const huge=path.join(root,'huge');
const handle=await fs.open(huge,'w');await handle.truncate(2**31);await handle.close();
try {
 round3.largeFile={size:(await fs.stat(huge)).size,blocks:(await fs.stat(huge)).blocks,
  response:await capture('project_file_read',{projectKey:root,path:'huge',maxBytes:7},'large-file')};
} finally {await fs.unlink(huge)}
const deniedFile=path.join(root,'unreadable');await fs.writeFile(deniedFile,'hidden');await fs.chmod(deniedFile,0);
round3.fsErrors=[];
const originalRead=gateway.projectFileRead;
try {
 // The real callback masks canonicalization errors as PATH_NOT_ALLOWED.
 round3.readErrors=[];
 for(const file of ['unreadable','missing','bom.txt/child'])round3.readErrors.push({path:file,response:await capture('project_file_read',{projectKey:root,path:file})});
 // Measure Node's actual errno/message, then pass that exact Error through the
 // production WS wrapper (no invented Error objects or copied wrapper logic).
 for(const [operation,p,call] of [
  ['open',deniedFile,()=>fs.readFile(deniedFile)],
  ['open',path.join(root,'missing'),()=>fs.readFile(path.join(root,'missing'))],
  ['open',path.join(root,'bom.txt/child'),()=>fs.readFile(path.join(root,'bom.txt/child'))],
  ['realpath',path.join(root,'missing'),()=>fs.realpath(path.join(root,'missing'))],
  ['stat',path.join(root,'missing'),()=>fs.stat(path.join(root,'missing'))],
  ['scandir',deniedFile,()=>fs.readdir(deniedFile)],
 ] as const) {
  let observed:any;try {await call();throw Error('expected fs failure')}catch(error:any){if(!error.code)throw error;observed=error}
  gateway.projectFileRead=async()=>{throw observed};
  round3.fsErrors.push({operation,path:p,nodeError:{code:observed.code,message:observed.message},response:await capture('project_file_read',{projectKey:root,path:'bom.txt'})});
 }
} finally {gateway.projectFileRead=originalRead;await fs.chmod(deniedFile,0o600);await fs.unlink(deniedFile)}
round3.localeProbe=[];
for(const vars of [
 {LANG:'tr_TR.UTF-8'}, {LANG:'lt_LT.UTF-8'},
 {LANG:'tr_TR.UTF-8',LC_ALL:''}, {LANG:'tr_TR.UTF-8',LC_MESSAGES:''}, {LANG:''},
 {LANG:'sv_SE.UTF-8',LC_ALL:'',LC_MESSAGES:'tr_TR.UTF-8'},
 {LANG:'en-US-u-co-xxx'}, {LANG:'sv-SE-u-co-xxx'}, {LANG:'tr-TR-u-co-xxx'},
 {LANG:'en-US-u-co-search'}, {LANG:'de-DE-u-co-phonebk'},
 {LANG:'lt-LT-u-co-xxx'}, {LANG:'vi-VN-u-co-xxx'}, {LANG:'ee-u-co-xxx'},
 {LANG:'vi-VN-u-co-phonebk'}, {LANG:'ee-u-co-phonebk'},
]) {
 const env={...process.env};for(const k of Object.keys(env))if(k.startsWith('LC_')||k==='LANG')delete env[k];Object.assign(env,vars);
 const code=`const names=['z','ä','Z','a','A','你好','文件','😀','_x'];const lowerInputs=['FILE.txt','FILE😀','Ì','Ì😀','I','Iİıi'];console.log(JSON.stringify({locale:new Intl.Collator().resolvedOptions().locale,order:names.sort((a,b)=>a.localeCompare(b)),lower:lowerInputs.map(input=>({input,output:input.toLocaleLowerCase()}))}));`;
 const child=spawnSync(process.execPath,['-e',code],{env,encoding:'utf8'});if(child.status!==0)throw Error(child.stderr);
 round3.localeProbe.push({env:vars,...JSON.parse(child.stdout)});
}
round3.base64Probe=['QUJD','őUJD','\ud800','QUJD\ud800','\u013dQUJD','\ud851UJD','Q😀UJD'].map(input=>({input:JSON.stringify(input),hex:Buffer.from(input,'base64url').toString('hex')}));
// Alphabet and padding: '+'/'/' decode as '-'/'_', and the first '=' ends decoding.
round3.base64Probe.push(...['Pj4-Pz8_','Pj4+Pz8/','QQ==QUJD','QUJD=zzzz','QU=JD','=QUJD'].map(input=>({input:JSON.stringify(input),hex:Buffer.from(input,'base64url').toString('hex')})));
const cursorPage=await capture('project_files_list',{projectKey:root,limit:2});
const validCursor=cursorPage.result.nextCursor;
round3.cursorProbe=[];
for(const [variant,cursor] of [
 ['lone-surrogate','\ud800'], ['suffix-surrogate',validCursor+'\ud800'],
 ['wide-low-byte',String.fromCharCode(validCursor.charCodeAt(0)+256)+validCursor.slice(1)],
 ['surrogate-low-byte',String.fromCharCode(0xd800+validCursor.charCodeAt(0))+validCursor.slice(1)],
] as const) {
 round3.cursorProbe.push({variant,cursor:JSON.stringify(cursor),decoded:Buffer.from(cursor,'base64url').toString('utf8'),response:await capture('project_files_list',{projectKey:root,limit:2,cursor})});
}
await fs.writeFile(path.join(output,'node-runtime-round3.json'),JSON.stringify(round3,null,2)+'\n');
console.log(JSON.stringify({round3:path.join(output,'node-runtime-round3.json'),largeFile:round3.largeFile,fsErrors:round3.fsErrors.map((p:any)=>p.nodeError),localeProbes:round3.localeProbe.length,cursorProbes:round3.cursorProbe.length}));
