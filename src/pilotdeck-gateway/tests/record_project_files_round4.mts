// Offline Node v22 oracle: execute the pinned production code, without services.
import * as fs from 'node:fs/promises';
import * as path from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {stripTypeScriptTypes} from 'node:module';
if (process.versions.node.split('.')[0] !== '22') throw Error('Node v22 required');
if (process.getuid?.() === 0) throw Error('EACCES probe requires a non-root user');
const baseline=path.resolve(process.argv[2]), output=path.resolve(process.argv[3]), scratch=path.resolve(process.argv[4]);
const locale=new Intl.Collator().resolvedOptions().locale;
if (locale !== 'tr-TR') throw Error(`Start with LANG=tr_TR and unset LC_*; got ${locale}`);
await fs.mkdir(scratch,{recursive:true});
const cwd=await fs.realpath(await fs.mkdtemp(path.join(scratch,'files-')));
process.chdir(cwd);
const home=path.join(cwd,'home'), root=path.join(cwd,'project');
await fs.mkdir(path.join(home,'projects','fixture'),{recursive:true});
await fs.mkdir(path.join(root,'nested'),{recursive:true});
await fs.writeFile(path.join(home,'projects','fixture','.cwd'),root);
process.env.PILOT_HOME=home;
const source=(file:string)=>path.join(baseline,'src',file);
const load=(file:string)=>import(pathToFileURL(source(file)).href);
const {GatewayWsConnection}=await load('gateway/server/GatewayWsConnection.ts');
const {InProcessGateway}=await load('gateway/client/InProcessGateway.ts');
const {listWebProjects}=await load('web/server/listProjects.ts');
const {createDialogProjectRegistry}=await load('gateway/dialog/projectRegistry.ts');
const {DialogGatewayError}=await load('gateway/dialog/errors.ts');
const {isPathWithinRoot}=await load('tool/builtin/filesystem/pathSafety.ts');
const catalog=()=>listWebProjects({pilotHome:home});
const projects=createDialogProjectRegistry({pilotHome:home,listProjects:async()=>(await catalog()).projects});
const sourceText=await fs.readFile(source('cli/createLocalGateway.ts'),'utf8');
const body=sourceText.split('    async projectFileRead(input) {\n')[1].split('\n    },')[0];
if(!body?.includes('data.subarray'))throw Error('source callback changed');
const AsyncFunction=Object.getPrototypeOf(async function(){}).constructor;
const callback=new AsyncFunction('input','dialog','realpath','resolve','isPathWithinRoot','DialogGatewayError','statAsync','readFileAsync',stripTypeScriptTypes('async function oracle(input) {\n'+body+'\n}').split('\n').slice(1,-1).join('\n'));
const gateway:any={options:{listProjects:catalog},listProjects:catalog,describeServer:async()=>({}),
 projectFilesList(input:any){return InProcessGateway.prototype.projectFilesList.call(this,input)},
 projectFileRead(input:any){return callback(input,{projects},fs.realpath,path.resolve,isPathWithinRoot,DialogGatewayError,fs.stat,fs.readFile)}};
const cases:any[]=[];
async function capture(method:string,params:any,tag:string){
 let receive:any,finish:any;const pending=new Promise<string>(r=>finish=r);
 const ws={onMessage(fn:any){receive=fn},onClose(){},sendText(text:string){if(JSON.parse(text).type==='response')finish(text)},close(){throw Error('closed')}};
 new GatewayWsConnection(ws as any,{gateway,token:'offline',serverVersion:'offline'});
 receive(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'offline',token:'offline'}));await new Promise(r=>setImmediate(r));
 const request=JSON.stringify({type:'request',id:`round4-${cases.length+1}`,method,params});receive(request);
 const response=await pending;const recorded={tag,request,response};cases.push(recorded);return JSON.parse(response);
}
for(const n of [12,13,14])for(const prefix of ['', 'nested/'])await fs.writeFile(path.join(root,prefix+'I'.repeat(n)),'boundary');
await fs.writeFile(path.join(root,'nested/FILEFILEFILEFILE.txt'),'query');
// A nested alias to a top-level target tests canonical relativePath provenance.
for(const n of [12,13,14])await fs.symlink('../'+'I'.repeat(n),path.join(root,'nested/ALIAS'+'I'.repeat(n)));
for(const query of ['FILEFILEFILEFILE',' FILEFILEFILEFILE '])await capture('project_files_list',{projectKey:root,query},'file-query-trim');
for(const n of [12,13,14]){
 for(const [variant,query] of [
  ['untrimmed','I'.repeat(n)], ['trim-both',' '+'I'.repeat(n)+' '],
  ['trim-start','\ufeff'+'I'.repeat(n)], ['trim-end','I'.repeat(n)+'\t'],
  ['dotless','ı'.repeat(n)],
 ])await capture('project_files_list',{projectKey:root,query},`${variant}-${n}`);
}
// Cursor signatures must reflect the same query normalization as matching.
for(const n of [12,13,14]){
 const query=' '+'I'.repeat(n)+' ';
 const page=await capture('project_files_list',{projectKey:root,query,limit:1},`cursor-first-${n}`);
 if(!page.result.nextCursor)throw Error('expected another page');
 await capture('project_files_list',{projectKey:root,query,limit:1,cursor:page.result.nextCursor},`cursor-next-${n}`);
 await capture('project_files_list',{projectKey:root,query:query.trim(),limit:1,cursor:page.result.nextCursor},`cursor-untrimmed-${n}`);
}
const files:any={};
async function snapshot(p:string){
 const l=await fs.lstat(p);if(l.isSymbolicLink()){files[p]={link:await fs.readlink(p)};return}
 const s=await fs.stat(p);files[p]={directory:s.isDirectory(),size:s.size,birth_ms:s.birthtimeMs,ctime_ms:s.ctimeMs,mtime_ms:s.mtimeMs};
 if(s.isDirectory()){files[p].entries=(await fs.readdir(p,{withFileTypes:true})).map(e=>[e.name,e.isDirectory()]);for(const [name] of files[p].entries)await snapshot(path.join(p,name))}
 else files[p].base64=(await fs.readFile(p)).toString('base64');
}
await snapshot(cwd);
const listCases=cases.splice(0);
const huge=path.join(root,'huge');
const handle=await fs.open(huge,'w');await handle.truncate(2**31);await handle.close();
const largeFile:any={size:(await fs.stat(huge)).size,blocks:(await fs.stat(huge)).blocks};
try {
 await fs.chmod(huge,0);
 largeFile.denied=await capture('project_file_read',{projectKey:root,path:'huge',maxBytes:7},'huge-unreadable');
 await fs.chmod(huge,0o600);
 largeFile.readable=await capture('project_file_read',{projectKey:root,path:'huge',maxBytes:7},'huge-readable');
 if(largeFile.denied.error.code!=='EACCES'||largeFile.readable.error.code!=='ERR_FS_FILE_TOO_LARGE')throw Error('unexpected size/permission precedence');
} finally {await fs.chmod(huge,0o600);await fs.unlink(huge)}
const result={node:process.version,v8:process.versions.v8,icu:process.versions.icu,platform:process.platform,uid:process.getuid?.(),env:{LANG:process.env.LANG},locale,sourceSha256:createHash('sha256').update(body).digest('hex'),listSourceSha256:createHash('sha256').update(await fs.readFile(source('gateway/dialog/projectFiles.ts'))).digest('hex'),cwd,home,root,files,cases:listCases,largeFile,readCases:cases};
await fs.mkdir(path.dirname(output),{recursive:true});await fs.writeFile(output,JSON.stringify(result,null,2)+'\n');
console.log(JSON.stringify({node:result.node,v8:result.v8,locale,output,listCases:listCases.length,largeFile}));
for(const c of listCases.filter(c=>!c.tag.startsWith('cursor')))console.log(JSON.stringify({tag:c.tag,query:JSON.parse(c.request).params.query,items:JSON.parse(c.response).result.items.map((i:any)=>({path:i.relativePath,matches:i.matches}))}));
