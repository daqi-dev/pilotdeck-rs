// Real Node filesystem failures through the default provider/config/binding chain.
// Hooks only schedule owned filesystem mutations; no errno or message is injected.
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import {syncBuiltinESMExports} from 'node:module';
import {resolve,join,dirname} from 'node:path';
import {pathToFileURL} from 'node:url';
import {execFileSync} from 'node:child_process';
const baseline=resolve(process.argv[2]),output=resolve(process.argv[3]),scratch=resolve(process.argv[4]);
if(process.versions.node.split('.')[0]!=='22'||process.getuid?.()===0)throw Error('non-root Node 22 required');
for(const k of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR'])if(!process.env[k]?.startsWith(scratch))throw Error(`Unisolated ${k}`);
if(process.env.SKIP_BROWSER_OPEN!=='1'||process.env.PILOTDECK_SKIP_BROWSER_OPEN!=='1')throw Error('Browser guards missing');
const source=(p:string)=>import(pathToFileURL(join(baseline,'src',p)).href);
const {GatewaySessionModelBundle}=await source('cli/GatewaySessionModelBundle.ts');
const {NativeSessionModelSelectionPort}=await source('model/session/NativeSessionModelSelectionPort.ts');
const {NativeSessionModelSelectionPolicy}=await source('model/session/NativeSessionModelSelectionPolicy.ts');
const {GatewayDialogBundle}=await source('cli/GatewayDialogBundle.ts');
const {createProjectSessionDataPlane}=await source('session/storage/ProjectSessionDataPlane.ts');
const {GatewayWsConnection}=await source('gateway/server/GatewayWsConnection.ts');
const {loadPilotConfig}=await source('pilot/config/loadPilotConfig.ts');
const {applyProjectBinding}=await source('pilot/staffdeck/projectBinding.ts');
const real={exists:fs.existsSync,stat:fs.statSync,read:fs.readFileSync,asyncStat:fsp.stat,asyncRead:fsp.readFile};
const base='schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: https://example.invalid/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n';
const results:any[]=[];let routerCalls=0;
const killCalls:number[]=[];const nativeKill=(process as any)._kill;
(process as any)._kill=(pid:number)=>{killCalls.push(pid);throw Error('Unexpected PID probe in failing-read fixture');};
try {
for(const target of ['transcript','global','project','store','sops','lock']){
 const scenarios=target==='lock'?['read-denied','read-missing','read-notdir','read-directory']:['stat-denied','stat-missing','stat-notdir','read-denied','read-missing','read-notdir','read-directory'];
 for(const scenario of scenarios){
  const id=`io-${target}-${scenario}`,root=join(scratch,'cases',String(results.length)),home=join(root,'home');fs.mkdirSync(home,{recursive:true});
  const paths={transcript:join(home,'projects','registered','chats','web:fixture.jsonl'),global:join(root,'global','pilotdeck.yaml'),project:join(home,'.pilotdeck','pilotdeck.yaml'),store:join(home,'staffdeck','project-bindings.json'),sops:join(home,'staffdeck','b','sops.json'),lock:join(home,'staffdeck','project-bindings.json.lock')};
  const env:any={PILOT_HOME:home,PILOTDECK_CONFIG_PATH:paths.global};
  if(['store','sops','lock'].includes(target))Object.assign(env,{STAFFDECK_PROJECT_BINDING_ENABLED:'true',STAFFDECK_FORMAL_API_ORIGIN:'https://staffdeck.invalid/',STAFFDECK_COPY_TENANT_ID:'tenant',STAFFDECK_SOP_RUNTIME_ORIGIN:'https://runtime.invalid',STAFFDECK_SOP_MANAGEMENT_API_KEY:'mock-only'});
  for(const p of Object.values(paths))fs.mkdirSync(dirname(p),{recursive:true});
  fs.writeFileSync(join(home,'projects','registered','.cwd'),home+'\n');fs.writeFileSync(paths.global,base);fs.writeFileSync(paths.project,'{}');fs.writeFileSync(paths.transcript,'');
  fs.writeFileSync(paths.store,JSON.stringify({version:1,installationId:'i',projects:[{projectRoot:home,bindingId:'b',agentId:'a',origin:'https://staffdeck.invalid',tenantId:'tenant',status:target==='lock'?'binding':'bound',runtimeReady:true}]}));
  fs.writeFileSync(paths.sops,'{"sops":[{"id":"sop"}]}');fs.writeFileSync(paths.lock,'invalid');
  const files:any={};const collect=(p:string)=>{const s=real.stat(p),f:any={directory:s.isDirectory(),size:s.size};files[p]=f;if(s.isDirectory()){f.entries=fs.readdirSync(p,{withFileTypes:true}).map(e=>[e.name,e.isDirectory()]);for(const [n] of f.entries)collect(join(p,n));}else f.bytes=real.read(p).toString('base64');};collect(root);
  const path=paths[target as keyof typeof paths],race=(target!=='lock'&&['read-missing','read-notdir'].includes(scenario))||(target==='lock'&&scenario==='read-notdir');let mutated=false;
  const mutate=()=>{if(mutated)return;mutated=true;if(scenario.endsWith('denied'))fs.chmodSync(scenario.startsWith('stat')?dirname(path):path,0);else if(scenario.endsWith('missing'))fs.unlinkSync(path);else if(scenario.endsWith('notdir')){fs.renameSync(dirname(path),dirname(path)+'.held');fs.writeFileSync(dirname(path),'parent replaced');}else{fs.unlinkSync(path);fs.mkdirSync(path);}};
  const reset=()=>{fs.existsSync=real.exists;fs.statSync=real.stat;fs.readFileSync=real.read;fsp.stat=real.asyncStat;fsp.readFile=real.asyncRead;syncBuiltinESMExports();if(!mutated)return;if(scenario.endsWith('denied'))fs.chmodSync(scenario.startsWith('stat')?dirname(path):path,0o700);else if(scenario.endsWith('missing'))fs.writeFileSync(path,Buffer.from(files[path].bytes,'base64'));else if(scenario.endsWith('notdir')){fs.unlinkSync(dirname(path));fs.renameSync(dirname(path)+'.held',dirname(path));}else{fs.rmdirSync(path);fs.writeFileSync(path,Buffer.from(files[path].bytes,'base64'));}mutated=false;};
  const observations:any[]=[];const observe=(stage:string,e:any)=>{observations.push({stage,code:e.code,syscall:e.syscall,message:e.message,path:e.path});};
  const install=()=>{
   if(!race)mutate();
   // existsSync deliberately suppresses errors; retain a real stat diagnostic
   // as evidence without changing the boolean returned to the source loader.
   fs.existsSync=((p:any)=>{const yes=real.exists(p);if(String(p)===path){if(!yes){try{real.stat(p);}catch(e){observe('stat',e);}}else if(race)mutate();}return yes;}) as any;
   fs.readFileSync=((p:any,...args:any[])=>{if(target==='lock'&&String(p)===path&&race)mutate();try{return (real.read as any)(p,...args);}catch(e){if(String(p)===path)observe('read',e);throw e;}}) as any;
   fsp.stat=(async(p:any,...args:any[])=>{try{const s=await (real.asyncStat as any)(p,...args);if(String(p)===path&&race)mutate();return s;}catch(e){if(String(p)===path)observe('stat',e);throw e;}}) as any;
   fsp.readFile=(async(p:any,...args:any[])=>{try{return await (real.asyncRead as any)(p,...args);}catch(e){if(String(p)===path)observe('read',e);throw e;}}) as any;syncBuiltinESMExports();
  };
  let load:any;try{install();try{load={ok:true,diagnostics:loadPilotConfig({projectRoot:home,env}).diagnostics};}catch(e:any){load={ok:false,code:e.code??'gateway_request_failed',message:e.message,diagnostics:e.diagnostics??[]};}}finally{reset();}
  const loadKillCalls=killCalls.splice(0);
  const plane=createProjectSessionDataPlane(),dialog=new GatewayDialogBundle({pilotHome:home,sessionCatalog:plane.catalog});
  const bundle=new GatewaySessionModelBundle({fallbackProjectKey:home,resolveProjectKey:dialog.projects.resolveProjectKey,router:{hasActiveTurn(){routerCalls++;throw Error('runtime touched');},async close(){routerCalls++;throw Error('runtime touched');}},selectionPort:new NativeSessionModelSelectionPort({pilotHome:home,now:()=>new Date(),storageProvider:plane.persistence}),policy:new NativeSessionModelSelectionPolicy(env)});
  let receive:any,finish:any;const ws={onMessage(fn:any){receive=fn},onClose(){},sendText(s:string){if(JSON.parse(s).type==='response')finish(s);},close(){throw Error('Unexpected close');}};
  new GatewayWsConnection(ws as any,{gateway:{describeServer:async()=>({}),sessionModelGet:(p:any)=>bundle.sessionModelGet(p)} as any,token:'offline',serverVersion:'offline'});receive(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'fixture',token:'offline'}));await new Promise(r=>setImmediate(r));
  const request=JSON.stringify({type:'request',id,method:'session_model_get',params:{projectKey:home,sessionKey:'web:fixture'}});let response:string;
  try{install();response=await new Promise<string>((done,reject)=>{const timer=setTimeout(()=>reject(Error('No response '+id)),5000);finish=(s:string)=>{clearTimeout(timer);done(s);};receive(request);});}finally{reset();}
  const rpcKillCalls=killCalls.splice(0);
  if(loadKillCalls.length||rpcKillCalls.length)throw Error('Unexpected kill calls '+id);
  if(!observations.length)throw Error('No actual fs failure '+id);
  const expectedCode=scenario.endsWith('denied')?'EACCES':scenario.endsWith('missing')?'ENOENT':scenario.endsWith('notdir')?'ENOTDIR':'EISDIR';
  for(const o of observations){if(o.code!==expectedCode||o.syscall!==(o.stage==='stat'?'stat':expectedCode==='EISDIR'?'read':'open'))throw Error('Unexpected fs diagnostic '+JSON.stringify(o));files[path][o.stage+'Error']=o.code;}
  // Keep initial tree facts, plus exact observed stat/read failures for replay.
  const replace=(s:string)=>s.replaceAll(root,'/fixture');for(const fact of Object.values(files) as any[])if(fact.bytes)fact.bytes=Buffer.from(replace(Buffer.from(fact.bytes,'base64').toString())).toString('base64');
  results.push(JSON.parse(replace(JSON.stringify({id,home,cwd:root,env,request,response,load,files,io:{target:path,scenario,race,observations},process:{pid:0,alive:false,eperm:false,killCalls:rpcKillCalls,loadKillCalls},forward:false,loadForward:false}))));console.log(id,expectedCode);
 }
}
// Record the binding transformation itself; this RPC does not expose webui.
const spreadRoot=join(scratch,'spread'),spreadHome=join(spreadRoot,'home');fs.mkdirSync(join(spreadHome,'staffdeck','b'),{recursive:true});fs.writeFileSync(join(spreadHome,'staffdeck','project-bindings.json'),JSON.stringify({version:1,installationId:'i',projects:[{projectRoot:spreadHome,bindingId:'b',agentId:'a',origin:'https://staffdeck.invalid',tenantId:'tenant',status:'bound'}]}));fs.writeFileSync(join(spreadHome,'staffdeck','b','sops.json'),'{"sops":[]}');
const spread:any[]=[];for(const enabled of [false,true])for(const webui of [[],[null,'x',{nested:[1,2]},false],Array.from({length:12},(_,i)=>i),'😀x',null,7,{staffdeckCopy:'old',keep:true}]){
 const env:any={PILOT_HOME:spreadHome,STAFFDECK_PROJECT_BINDING_ENABLED:'true',...(enabled?{STAFFDECK_FORMAL_API_ORIGIN:'https://staffdeck.invalid',STAFFDECK_COPY_TENANT_ID:'tenant'}:{})};
 const input={webui};const next=applyProjectBinding(input,spreadHome,env);spread.push({enabled,input,webui:next.webui});
}
fs.mkdirSync(dirname(output),{recursive:true});fs.writeFileSync(output,JSON.stringify({baselineCommit:execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),node:process.version,icu:process.versions.icu,routerCalls,cases:results},null,2)+'\n');

fs.writeFileSync(join(dirname(output),'webui-spread.json'),JSON.stringify({node:process.version,spread},null,2)+'\n');

} finally {(process as any)._kill=nativeKill;}
