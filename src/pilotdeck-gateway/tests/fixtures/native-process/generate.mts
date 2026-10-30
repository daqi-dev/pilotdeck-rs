import assert from 'node:assert/strict';
import {mkdir, writeFile} from 'node:fs/promises';
import {resolve, join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
const owned:any[]=[];const watchdog=setTimeout(()=>{for(const child of owned)child.kill('SIGKILL');console.error('#1071 generator deadline');process.exitCode=1;process.exit(1);},30000);
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1071 pinned Node');
const root = resolve(process.env.PDRS_BASELINE!);
const sources = ['src/cli/shutdownCoordinator.ts', 'src/cli/PilotDeckServerShutdownBundle.ts',
  'src/cli/PilotDeckServerBootstrapBundle.ts', 'src/cli/pilotdeck.ts',
  'src/lsp/runtime/LspService.ts', 'src/session/title/SessionTitleGenerator.ts',
  'src/agent/modules/transport/sidecarTurnComposition.ts'];
const sourceCommit = assertBaseline(root);
const sourceSha256 = await baselineSourceHashes(root, sources);
const load = (path:string) => import(pathToFileURL(join(root,path)).href);
const {createShutdownAndExit} = await load(sources[0]);
const {PilotDeckServerShutdownBundle} = await load(sources[1]);
const {PilotDeckServerBootstrapBundle} = await load(sources[2]);
const {LspService} = await load(sources[4]);
const title = await load(sources[5]);
const deferred = () => {let release!:()=>void; const promise=new Promise<void>(r=>release=r);return {promise,release};};
const observations:any = {shutdown:[], lsp:[], title:[]};
for (const codes of [[0],[1],[0,0],[0,1],[1,0]]) {
  const gate=deferred();let stops=0;const exits:number[]=[];
  const shutdown=createShutdownAndExit(async()=>{stops++;await gate.promise;},(code:number)=>exits.push(code));
  const waits=codes.map(code=>shutdown(code));await Promise.resolve();
  const before={stops,exits:[...exits],samePromise:waits.every(p=>p===waits[0])};
  gate.release();await Promise.all(waits);
  observations.shutdown.push({codes,before,after:{stops,exits}});
}
for(const mode of ['release','reject','cancel','never']) {
  const events:string[]=[];const gate=deferred();const cancelled=new Error('cancelled');
  const step=async(name:string)=>{events.push(name);if(mode==='reject')throw new Error(name);};
  const bundle=new PilotDeckServerShutdownBundle({
    automation:{async stop(){events.push('automation');await gate.promise;if(mode==='cancel')throw cancelled;}},
    closeServer:()=>step('server'),flushChannelState:()=>step('flush'),disposeGateway:()=>step('gateway'),
    telemetry:{snapshot(){events.push('snapshot');return {};},shutdown:()=>step('telemetry')},log:()=>{},
  });
  const wait=bundle.stop();let settled=false;void wait.then(()=>settled=true,()=>settled=true);const same=wait===bundle.stop();await Promise.resolve();await Promise.resolve();
  const before=[...events];
  if(mode==='never'){assert.equal(settled,false,'#1071 never predecessor remains pending');observations[mode]={before,same,settled};continue;}
  gate.release();let error:any=null;try{await wait;}catch(e:any){error={name:e.name,message:e.message,causes:e.errors?.map((x:any)=>x.message)??[]};}
  observations[mode]={before,same,after:events,error};
}
{
  const events:string[]=[];const boot=new PilotDeckServerBootstrapBundle({
    automation:{async start(){events.push('start');},async stop(){events.push('stop');}},
    async disposeGateway(){events.push('dispose');},telemetry:{async shutdown(){events.push('telemetry');}},warn:()=>{},
  });await boot.startAutomation();let error='';try{await boot.run(async()=>{throw new Error('bind failed');});}catch(e:any){error=e.message;}
  await boot.rollback();observations.bootstrap={events,error};
}
const lsp=new LspService();
const query={workspaceRoot:'/isolated/project',filePath:'file.ts',position:{line:0,character:0},operation:'hover'};
for(const [name, service, request] of [
  ['empty',lsp,query],['bound',lsp.bindSession({}),query],
  ['workspace',lsp,{...query,workspaceRoot:''}],['position',lsp,{...query,position:{line:-1,character:0}}],
  ['escape',lsp,{...query,filePath:'../file.ts'}],['dot',lsp,{...query,filePath:'dir.ts/.'}],['parent',lsp,{...query,filePath:'dir.ts/..'}],
] as const) {
  try{await service.query(request);throw new Error('#1071 expected LSP rejection');}
  catch(e:any){observations.lsp.push({name,error:{name:e.name,code:e.code,message:e.message}});}
}
await lsp.dispose();await lsp.dispose();try{lsp.bindSession({});}catch(e:any){observations.lsp.push({name:'disposed',error:{name:e.name,code:e.code,message:e.message}});}
for(const [name,text,content,model] of [
  ['empty',' \n\t ',[],undefined],
  ['normal','  中文\n请求  ',[{type:'text',text:'```json\n{"title":"  标题  内容  "}\n```'}],undefined],
  ['override','a'.repeat(1201),[{type:'text',text:JSON.stringify({title:'b'.repeat(81)})}],{provider:'other',model:'chosen'}],
  ['invalid','request',[{type:'text',text:'invalid json'}],undefined],
] as const) {
  const calls:any[]=[];
  const provider=title.createNativeSessionTitleProvider({agentModel:{provider:'fixture',model:'model'},systemLanguage:'zh-CN',
    modelRuntime:{async complete(request:any){calls.push({...request,systemPromptSha256:createHash('sha256').update(request.systemPrompt).digest('hex'),systemPrompt:undefined});return {content};}},
  });
  const result=await provider.generate({text,sessionId:'session',turnId:'turn',signal:new AbortController().signal,model});
  observations.title.push({name,providerId:provider.providerId,modelProvenance:provider.modelProvenance,result,calls});
}

observations.titleWait=[];
for(const kind of ['cancel-release','timeout-release','cancel-reject','cancel-never']) {
  const entered=deferred(),release=deferred(),aborted=deferred();const controller=new AbortController();let signal:AbortSignal;let settled=false;
  const provider=title.createNativeSessionTitleProvider({agentModel:{provider:'p',model:'m'},timeoutMs:kind.startsWith('timeout')?1:30000,systemLanguage:'en',
    modelRuntime:{async complete(_:any,opts:any){signal=opts.signal;signal.addEventListener('abort',()=>aborted.release(),{once:true});entered.release();await release.promise;if(kind==='cancel-reject')throw Error('cancelled');return {content:[{type:'text',text:'{"title":"late title"}'}]};}},
  });
  const result=provider.generate({text:'hello',sessionId:'s',turnId:'t',signal:controller.signal}).then((v:any)=>{settled=true;return v;});
  await entered.promise;const keepalive=setInterval(()=>{},1000);if(!kind.startsWith('timeout'))controller.abort();await aborted.promise;clearInterval(keepalive);
  await Promise.resolve();await Promise.resolve();const before={settled,aborted:signal!.aborted};assert.deepEqual(before,{settled:false,aborted:true},'#1071 title does not outrun provider');
  if(kind==='cancel-never'){observations.titleWait.push({kind,before});continue;}release.release();observations.titleWait.push({kind,before,result:await result});
}
observations.signals=[];
for(const first of ['SIGINT','SIGTERM'] as const){
  const child=spawn(process.execPath,['--disable-warning=ExperimentalWarning','--import',join(root,'node_modules/tsx/dist/loader.mjs'),fileURLToPath(new URL('./signal.mts',import.meta.url))],{env:process.env,stdio:['pipe','pipe','inherit']});
  owned.push(child);const lines:string[]=[];let wake:(()=>void)|undefined;createInterface({input:child.stdout}).on('line',line=>{lines.push(line);wake?.();wake=undefined;});const exit=once(child,'exit');
  const deadline=setTimeout(()=>child.kill('SIGKILL'),10000);
  const wait=async(expected:string)=>{while(!lines.includes(expected)){if(child.exitCode!==null||child.signalCode!==null)throw Error('#1071 TS signal child exited');await Promise.race([new Promise<void>(r=>wake=r),exit]);}};
  try{await wait('ready');child.kill(first);await wait('stop');const second=first==='SIGINT'?'SIGTERM':'SIGINT';child.kill(second);await wait(second);assert.equal(lines.filter(x=>x==='stop').length,1,'#1071 single shutdown');child.stdin.end('release');const status=await exit;assert.deepEqual(status,[0,null],'#1071 TS signal exit');observations.signals.push({first,lines,exit:status});}finally{clearTimeout(deadline);if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await exit;}}
}

{
  const {resolveSidecarTurnCompositionHandlers}=await load(sources[6]);
  const controller=new AbortController();const signals:Record<string,AbortSignal>={};const factories:any={};const modules:any={};
  for(const name of ['model','budget','permission','context','lifecycle']){modules[name]={};factories[name]=({turn}:any)=>{signals[name]=turn.abortSignal;return {};};}
  resolveSidecarTurnCompositionHandlers({config:{cwd:'/isolated/project'},modules,turn:{sessionId:'s',turnId:'t',abortSignal:controller.signal}},factories);
  const before=Object.fromEntries(Object.entries(signals).map(([name,signal])=>[name,{same:signal===controller.signal,aborted:signal.aborted}]));
  controller.abort();const after=Object.fromEntries(Object.entries(signals).map(([name,signal])=>[name,signal.aborted]));
  assert.ok(Object.values(before).every(v=>v.same&&!v.aborted),'#1071 shared turn signal');assert.ok(Object.values(after).every(Boolean),'#1071 all ports aborted');
  observations.sharedSignal={before,after};
}
const output={sourceCommit,sourceSha256,node:process.version,observations};
await mkdir(process.argv[2],{recursive:true});
await writeFile(join(process.argv[2],'oracle.json'),JSON.stringify(output,null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify(output,null,2));

clearTimeout(watchdog);
