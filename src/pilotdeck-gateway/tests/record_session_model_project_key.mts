// Real TS project-key identity oracle for `session_model_get`.
// node --import <B>/node_modules/tsx/dist/loader.mjs <this> <B> <output> <scratch>
//
// Registry identities that are distinct as JS strings but collision-prone
// under a lossy UTF-16 -> UTF-8 decode:
//   * a project path containing LITERAL U+FFFD, registered in `projects/`;
//   * the same path with U+FFFD replaced by a lone high surrogate.
// `projectRegistry.ts:41-59` resolves both through `node:path.resolve`, then
// compares the resulting JS strings by code unit, so only the literal U+FFFD
// path is a registered project. Node's fs layer separately encodes a lone
// surrogate as U+FFFD, which is why the two must not be conflated at the
// registry/identity seam.
// `path.resolve` is lexical and never inspects segment content, so a `..`
// after a surrogate-bearing segment removes it before the comparison:
// `/fixture/project-\ud800/../project` resolves to the registered
// `/fixture/project`. The relative spelling is resolved against the case root
// (the fixture's `cwd=/fixture`) and must reach the same project.
import {mkdir,writeFile,readFile,stat,readdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {execFileSync} from 'node:child_process';
const baseline=resolve(process.argv[2]),output=resolve(process.argv[3]),scratch=resolve(process.argv[4]);
if(process.versions.node.split('.')[0]!=='22')throw Error('Node 22 required');
for(const k of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR'])if(!process.env[k]?.startsWith(scratch))throw Error(`Unisolated ${k}`);
if(process.env.SKIP_BROWSER_OPEN!=='1'||process.env.PILOTDECK_SKIP_BROWSER_OPEN!=='1')throw Error('Browser guards missing');
const source=(p:string)=>import(pathToFileURL(join(baseline,'src',p)).href);
const {GatewaySessionModelBundle}=await source('cli/GatewaySessionModelBundle.ts');
const {NativeSessionModelSelectionPort}=await source('model/session/NativeSessionModelSelectionPort.ts');
const {NativeSessionModelSelectionPolicy}=await source('model/session/NativeSessionModelSelectionPolicy.ts');
const {GatewayDialogBundle}=await source('cli/GatewayDialogBundle.ts');
const {createProjectSessionDataPlane}=await source('session/storage/ProjectSessionDataPlane.ts');
const {GatewayWsConnection}=await source('gateway/server/GatewayWsConnection.ts');
const base='schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: https://example.invalid/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n';
const root=join(scratch,'case');const home=join(root,'home'),project=join(root,'project');
await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});
await mkdir(join(home,'projects','registered','chats'),{recursive:true});
await writeFile(join(home,'projects','registered','.cwd'),project+'\n');
await writeFile(join(home,'pilotdeck.yaml'),base);
// The U+FFFD project is a registered identity; the lone-surrogate spelling is not.
const fffdProject=join(root,'project-\ufffd');
await mkdir(fffdProject,{recursive:true});
await mkdir(join(home,'projects','registered-fffd','chats'),{recursive:true});
await writeFile(join(home,'projects','registered-fffd','.cwd'),fffdProject+'\n');
const loneProject=join(root,'project-\ud800');
// `join` would fold `..`; keep the raw lexical spelling for the fold cases.
const foldsToProject=`${root}/project-\ud800/../project`;
const foldsToLoneProject=`${root}/project-\ud800/../project-\ud800`;
const cases:Array<[string,string,boolean?]>=[
  ['project-key-literal-fffd',fffdProject],
  // Negative control: lossy-decoding this key would alias it onto fffdProject.
  ['project-key-lone-surrogate',loneProject],
  // No U+FFFD counterpart: isolates the error message's raw units.
  ['project-key-lone-surrogate-unknown',join(root,'missing-\ud800')],
  // `..` drops the surrogate-bearing segment before the registry comparison.
  ['project-key-folded-lone-surrogate',foldsToProject],
  // Same fold with a relative key, resolved against the case root.
  ['project-key-folded-lone-surrogate-relative','project-\ud800/../project',true],
  // Negative control: normalization happens, but the surviving lone surrogate
  // still names an unregistered project and must keep its raw error units.
  ['project-key-folded-surviving-lone-surrogate',foldsToLoneProject],
];
const originalCwd=process.cwd();
const results:any[]=[];let routerCalls=0;
for(const [id,projectKey,relative] of cases){
  process.chdir(relative?root:originalCwd);
  const env={PILOT_HOME:home};
  const plane=createProjectSessionDataPlane();const dialog=new GatewayDialogBundle({pilotHome:home,sessionCatalog:plane.catalog});
  const bundle=new GatewaySessionModelBundle({fallbackProjectKey:home,resolveProjectKey:dialog.projects.resolveProjectKey,router:{hasActiveTurn(){routerCalls++;throw Error('runtime touched');},async close(){routerCalls++;throw Error('runtime touched');}},selectionPort:new NativeSessionModelSelectionPort({pilotHome:home,now:()=>new Date(),storageProvider:plane.persistence}),policy:new NativeSessionModelSelectionPolicy(env)});
  let receive:any,finish:any;const responses:string[]=[];
  const ws={onMessage(fn:any){receive=fn},onClose(){},sendText(s:string){if(JSON.parse(s).type==='response'){responses.push(s);finish(s);}},close(){throw Error('Unexpected close');}};
  new GatewayWsConnection(ws as any,{gateway:{describeServer:async()=>({}),sessionModelGet:(p:any)=>bundle.sessionModelGet(p),modelCatalogList:async()=>({items:[]})} as any,token:'offline',serverVersion:'offline'});
  receive(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'fixture',token:'offline'}));await new Promise(r=>setImmediate(r));
  const request=JSON.stringify({type:'request',id,method:'session_model_get',params:{projectKey,sessionKey:'web:fixture'}});
  let response:string;try{response=await new Promise<string>((done,reject)=>{const timer=setTimeout(()=>reject(Error('No response '+id)),5000);finish=(s:string)=>{clearTimeout(timer);done(s);};receive(request);});}catch(e){throw e;}
  const files:any={};const collect=async(path:string)=>{const s=await stat(path);const fact:any={directory:s.isDirectory(),size:s.size};files[path]=fact;if(s.isDirectory()){const entries=await readdir(path,{withFileTypes:true});entries.sort((a,b)=>a.name<b.name?-1:a.name>b.name?1:0);fact.entries=await Promise.all(entries.map(async e=>[e.name,e.isDirectory()]));for(const [name]of fact.entries)await collect(join(path,name));}else if(s.size<=50*1024*1024)fact.bytes=(await readFile(path)).toString('base64');};await collect(root);
  const replace=(s:string)=>s.replaceAll(root,'/fixture');
  for(const fact of Object.values(files) as any[]){if(fact.bytes){const b=Buffer.from(fact.bytes,'base64');const needle=Buffer.from(root),replacement=Buffer.from('/fixture');let parts:Buffer[]=[],start=0,at=0;while((at=b.indexOf(needle,start))>=0){parts.push(b.subarray(start,at),replacement);start=at+needle.length;}parts.push(b.subarray(start));const normalized=Buffer.concat(parts);fact.bytes=normalized.toString('base64');fact.size=normalized.length;}}
  const normalizedRequest=JSON.parse(replace(request));
  results.push(JSON.parse(replace(JSON.stringify({id,home,cwd:'/fixture',env,request:JSON.stringify(normalizedRequest),response,files,process:{pid:0,alive:true,eperm:false,killCalls:[],loadKillCalls:[]},forward:false}))));
  console.log(id,JSON.parse(response).ok?'OK':JSON.parse(response).error.code);
}
process.chdir(originalCwd);
await mkdir(resolve(output,'..'),{recursive:true});
await writeFile(output,JSON.stringify({baselineCommit:execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),node:process.version,icu:process.versions.icu,routerCalls,cases:results},null,2)+'\n');
