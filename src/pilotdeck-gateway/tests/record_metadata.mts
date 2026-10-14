// Offline source oracle. No servers, sockets, writes to baseline, or real user home.
import { mkdir, mkdtemp, writeFile, readFile, readdir, stat, symlink } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
const baseline = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
const scratch = resolve(process.argv[4]);
await mkdir(scratch, {recursive:true});
const root = await mkdtemp(join(scratch,'metadata-'));
const {GatewayWsConnection} = await import(pathToFileURL(join(baseline,'src/gateway/server/GatewayWsConnection.ts')).href);
const {listWebProjects,describeWebProject} = await import(pathToFileURL(join(baseline,'src/web/server/listProjects.ts')).href);
const {createProjectId} = await import(pathToFileURL(join(baseline,'src/pilot/paths.ts')).href);
const cases: any[]=[];
const extraRoots:string[]=[];
process.chdir(root);
let sequence=0;
async function capture(home:string,cwd:string,method:string,params?:any) {
 let receive: (text:string)=>void;
 let finish: (text:string)=>void;
 const pending=new Promise<string>(resolve => {finish=resolve});
 const ws={onMessage(fn:any){receive=fn},onClose(){},sendText(text:string){if(JSON.parse(text).type==='response') finish!(text)},close(){throw Error('unexpected close')}};
 const gateway={describeServer:async()=>({}),listProjects:()=>listWebProjects({pilotHome:home}),describeProject:(input:any)=>describeWebProject(input.projectKey,{pilotHome:home})};
 new GatewayWsConnection(ws as any,{gateway:gateway as any,token:'offline',serverVersion:'offline'});
 receive!(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'offline',token:'offline'}));
 await new Promise(resolve=>setImmediate(resolve));
 const request=JSON.stringify({type:'request',id:`offline-${++sequence}`,method,...(params===undefined?{}:{params})});
 receive!(request);
 const response=await pending;
 const files:any={};
 async function snapshot(path:string) {
  const s=await stat(path);
  files[path]={directory:s.isDirectory(),size:s.size,birth_ms:s.birthtimeMs,ctime_ms:s.ctimeMs,mtime_ms:s.mtimeMs};
  if (s.isDirectory()) {
   const entries=await readdir(path,{withFileTypes:true});
   files[path].entries=entries.map(entry=>[entry.name,entry.isDirectory()]);
   for(const entry of entries) await snapshot(join(path,entry.name));
  } else {files[path].data=(await readFile(path)).toString('utf8').split(/(x{1024,})/).map(part=>/^x{1024,}$/.test(part)?part.length:part)}
 }
 await snapshot(home);
 // Marked project roots are also filesystem facts, outside PILOT_HOME.
 for(const [path,fact] of Object.entries(files) as any[]) {
  if(!fact.data)continue;
  const text=fact.data.map((part:any)=>typeof part==='number'?'x'.repeat(part):part).join('').trim();
  if(path.endsWith('/.cwd') && text) {
   try {const absolute=resolve(cwd,text);const s=await stat(absolute);files[absolute]={directory:s.isDirectory(),size:s.size,birth_ms:s.birthtimeMs,ctime_ms:s.ctimeMs,mtime_ms:s.mtimeMs};}catch{}
  }
 }
 for(const path of extraRoots) {const s=await stat(path);files[path]={directory:s.isDirectory(),size:s.size,birth_ms:s.birthtimeMs,ctime_ms:s.ctimeMs,mtime_ms:s.mtimeMs};}
 cases.push({home,cwd,request,response,files});
}
const home=join(root,'home');await mkdir(home,{recursive:true});
const project=join(root,'a-','中文 project');await mkdir(project,{recursive:true});
const cwd=root;
// Node process.cwd is deliberately different: inputs below are absolute.
await capture(home,cwd,'list_projects');
await capture(home,cwd,'describe_project',{projectKey:join(root,'unknown')});
const storage=join(home,'projects',createProjectId(project));
await mkdir(join(storage,'chats'),{recursive:true});await writeFile(join(storage,'.cwd'),project+'\n');
let ordinal=0;
const entry=(type:string,extra:any)=>JSON.stringify({type,...extra,sessionId:'test',turnId:'metadata',sequence:++ordinal,createdAt:'2026-10-14T00:00:00.000Z'})+'\n';
await writeFile(join(storage,'chats','empty.jsonl'),'');
await writeFile(join(storage,'chats','unlisted.jsonl'),entry('step_started',{step:0}));
await writeFile(join(storage,'chats','first.jsonl'),entry('accepted_input',{messages:[{content:[{type:'text',text:' first '}]}]}));
await writeFile(join(storage,'chats','whitespace-title.jsonl'),entry('session_metadata',{metadata:{title:' '}}));
await writeFile(join(storage,'chats','surrogate-title.jsonl'),entry('session_metadata',{metadata:{title:'\ud800'}}));
await writeFile(join(storage,'chats','always-on-report:internal.jsonl'),entry('session_metadata',{metadata:{title:'hidden'}}));
await writeFile(join(storage,'chats','always-on-report-public.jsonl'),entry('session_metadata',{metadata:{title:'also hidden'}}));
await writeFile(join(storage,'chats','always-on-other:public.jsonl'),entry('session_metadata',{metadata:{aiTitle:'visible'}}));
await writeFile(join(storage,'chats','wrong-shape.jsonl'),JSON.stringify({type:'tool_call',input:{title:'not a session title'}})+'\n');
await writeFile(join(storage,'chats','large.jsonl'),entry('accepted_input',{messages:[{content:[{type:'text',text:'x'.repeat(140000)}]}]})+entry('session_metadata',{metadata:{firstPrompt:'fallback',title:'tail',isSnapshot:true}}));
await capture(home,cwd,'list_projects');
await capture(home,cwd,'describe_project',{projectKey:project});
await capture(home,cwd,'describe_project',{projectKey:project+'////'});
// Marker-collision storage differs from the legacy ID and catalog uses markers.
const second=join(root,'second');await mkdir(second);
const secondStorage=join(home,'projects','collision-resistant--123');await mkdir(join(secondStorage,'chats'),{recursive:true});
await writeFile(join(secondStorage,'.cwd'),second+'\n');
await writeFile(join(secondStorage,'chats','title.jsonl'),entry('session_metadata',{metadata:{title:'collision'}}));
await mkdir(join(home,'projects','empty-marker'));await writeFile(join(home,'projects','empty-marker','.cwd'),'  \n');
await mkdir(join(home,'projects','stale-marker'));await writeFile(join(home,'projects','stale-marker','.cwd'),join(root,'gone')+'\n');
await writeFile(join(home,'projects','not-a-directory'),'ignore');
const general=join(home,'projects',createProjectId(home));await mkdir(general);await writeFile(join(general,'.cwd'),home+'\n');
await capture(home,cwd,'list_projects');await capture(home,cwd,'describe_project',{projectKey:second});
// Legacy path reconstruction when .cwd is absent.
const legacy=await mkdtemp('/private/tmp/pdrsmetadatalegacy');extraRoots.push(legacy);
await mkdir(join(home,'projects',createProjectId(legacy)),{recursive:true});
await capture(home,cwd,'list_projects');await capture(home,cwd,'describe_project',{projectKey:legacy});
// Relative markers and parameters follow lexical resolve, retaining raw root.
const relative='relative';await mkdir(join(root,relative));
const relativeStorage=join(home,'projects',createProjectId(relative));await mkdir(relativeStorage);await writeFile(join(relativeStorage,'.cwd'),relative+'\n');
await capture(home,cwd,'list_projects');await capture(home,cwd,'describe_project',{projectKey:relative});
// listProjects follows stat symlinks; storage-id scans use Dirent.isDirectory.
await symlink(secondStorage,join(home,'projects','linked-project'));
await capture(home,cwd,'list_projects');
// Invalid escaped fields cause catalog.list to fail, summarizeProject catches it.
await writeFile(join(storage,'chats','bad-field.jsonl'),'{'+'"type":"session_metadata","metadata":{"tag":"\\q"}}\n');
await capture(home,cwd,'describe_project',{projectKey:project});
await writeFile(join(secondStorage,'chats','streaming-fallback.jsonl'),entry('accepted_input',{messages:[{content:[{type:'text',text:'x'.repeat(140000)}]}]})+entry('session_metadata',{metadata:{firstPrompt:'visible from scan'}}));
await writeFile(join(secondStorage,'chats','typed-snapshot.jsonl'),entry('accepted_input',{messages:[{content:[{type:'text',text:'x'.repeat(140000)}]}]})+entry('session_metadata',{metadata:{title:7,firstPrompt:'snapshot string field',isSnapshot:true}}));
await writeFile(join(secondStorage,'chats','object-content.jsonl'),entry('accepted_input',{messages:[{content:{type:'text',text:'flatMap retains object'}}]}));
await writeFile(join(secondStorage,'chats','null-message.jsonl'),entry('accepted_input',{messages:[null,{content:[{type:'text',text:'must be hidden'}]}]}));
await writeFile(join(secondStorage,'chats','bad-created-unlisted.jsonl'),'{'+'"type":"step_started","createdAt":"\\q"}\n');
await writeFile(join(secondStorage,'chats','js-space.jsonl'),'{'+'"type":"session_metadata","metadata":{"title"\uFEFF:\uFEFF"JS whitespace"}}\n');
await writeFile(join(secondStorage,'chats','non-js-space.jsonl'),'{'+'"type":"session_metadata","metadata":{"title"\u0085:\u0085"must not count"}}\n');
await capture(home,cwd,'describe_project',{projectKey:second});
await mkdir(resolve(output,'..'),{recursive:true});await writeFile(output,JSON.stringify(cases,null,2)+'\n');
console.log(JSON.stringify({cases:cases.length,scratch:root,output}));
