// Production default assembly + real disk + production WS serializer, no socket.
import * as fs from 'node:fs/promises';
import fsMutable from 'node:fs/promises';
import fsSync from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
import * as path from 'node:path';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
import assert from 'node:assert/strict';
assert.equal(process.version, 'v22.22.0');
assert.equal(process.env.PILOTDECK_SKIP_BROWSER_OPEN, '1');
assert.equal(process.env.PILOT_SKIP_BROWSER_OPEN, '1');
assert.equal(process.env.SKIP_BROWSER_OPEN, '1');
assert.equal(process.env.PILOTDECK_SKIP_BROWSER, '1');
assert.equal(process.env.GP_POLL_WATCH, '1');
const baseline = path.resolve(process.argv[2]);
const output = path.resolve(process.argv[3]);
const scratch = await fs.realpath(path.resolve(process.argv[4]));
for(const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME']) {
  const value=await fs.realpath(process.env[key] ?? '');assert.ok(value.startsWith(scratch+path.sep),key);
}
await assert.rejects(fs.stat(output),{code:'ENOENT'});
assert.equal((await import('node:child_process')).execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const cwd = path.join(scratch, 'project');
const home = path.join(scratch, 'pilot');
await fs.mkdir(cwd, {recursive:true});
await fs.mkdir(home, {recursive:true});
process.chdir(cwd);
process.env.PILOT_HOME = home;
await fs.writeFile(path.join(home,'pilotdeck.yaml'), `schemaVersion: 1
agent: {model: test/model, maxContextTokens: 65536, maxOutputTokens: 8192}
model:
  providers:
    test:
      protocol: openai
      url: https://example.test/v1
      apiKey: fixture-key
      models: {model: {}}
extension:
  builtinPluginsEnabled: {windows-skills: false, browser-use: false, funasr: false}
telemetry: {enabled: false}
`);
const chat = path.join(home,'projects','registered','chats');
await fs.mkdir(chat, {recursive:true});
await fs.writeFile(path.join(home,'projects','registered','.cwd'), cwd);
const load = (f:string) => import(pathToFileURL(path.join(baseline,'src',f)).href);
const {createLocalGateway} = await load('cli/createLocalGateway.ts');
const {GatewayWsConnection} = await load('gateway/server/GatewayWsConnection.ts');
const local = createLocalGateway(); // no authorizer, retention, storage override
const gateway = local.gateway;
const base = {sessionKey:'fixture', projectKey:cwd};
const entry = (sequence:number, type='message', createdAt='2026-10-15T00:00:00Z') => ({type,sessionId:'different-session-is-accepted',turnId:'',sequence,createdAt,payload:{text:'你好', arbitrary:true}});
const valid = [entry(3,'tool_result'),entry(0),entry(1,'subagent_started'),entry(2,'tool_result_message'),entry(3,'tool_result','2026-10-15T00:00:01Z')];
const cases:any[]=[];const inRootLinks:any[]=[];
const files:any = {};
async function capture(p:string) {
  try {
    const stat = await fs.stat(p);
    files[p] = {directory:stat.isDirectory(),size:stat.size};
    if (stat.isDirectory()) {
      const entries=await fs.readdir(p,{withFileTypes:true});
      files[p].entries=entries.map(e=>[e.name,e.isDirectory()]);
      for(const e of entries) await capture(path.join(p,e.name));
    } else if(stat.size <= 10_000_000) files[p].base64=(await fs.readFile(p)).toString('base64');
  } catch {}
}
async function record(id:string,method:string,params:any, options:any={}) {
  let receive:any, finish:any;
  const pending = new Promise<string>(r=>{finish=r});
  const ws={onMessage(fn:any){receive=fn},onClose(){},close(){throw Error('unexpected close')},sendText(raw:string){if(JSON.parse(raw).type==='response')finish(raw)}};
  const connection=new GatewayWsConnection(ws as any,{gateway,token:'offline',serverVersion:'offline'});
  receive(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'offline',token:'offline'}));
  await new Promise(r=>setImmediate(r));
  const request=JSON.stringify({type:'request',id,method,...(options.omitParams?{}:{params})});
  receive(request);
  let timer:any;
  const response=await Promise.race([pending,new Promise<never>((_,r)=>{timer=setTimeout(()=>r(Error(id)),5000)})]).finally(()=>clearTimeout(timer));
  cases.push({id,request,response,...options});
}

const marker=path.join(home,'projects/registered/.cwd'),outside=path.join(scratch,'outside-marker');
const artifactDir=path.join(cwd,'.pilotdeck/tool-results/fixture');await fs.mkdir(artifactDir,{recursive:true});await fs.writeFile(path.join(artifactDir,'probe.bin'),JSON.stringify(entry(7))+'\n');
await fs.writeFile(outside,cwd+'\n\t');
const originalRead=fsMutable.readFile,originalSync=fsSync.readFileSync;let armed=false,current:any;const rows:any[]=[];
function swapSync(){if(!armed)return;fsSync.unlinkSync(marker);fsSync.symlinkSync(outside,marker);armed=false;current.trace.push({event:'marker-replaced-before-real-open',path:marker})}
(fsMutable as any).readFile=async(...args:any[])=>{if(String(args[0])===marker){swapSync();const value=await (originalRead as any)(...args);current?.trace.push({api:'fsp.readFile',path:marker,base64:Buffer.from(value).toString('base64')});return value}return (originalRead as any)(...args)};
(fsSync as any).readFileSync=(...args:any[])=>{if(String(args[0])===marker){swapSync();const value=(originalSync as any)(...args);current?.trace.push({api:'fs.readFileSync',path:marker,base64:Buffer.from(value).toString('base64')});return value}return (originalSync as any)(...args)};
syncBuiltinESMExports();
try{
 for(const method of ['native_archive_manifest','native_archive_entries','native_archive_artifact']) for(const stage of ['static','race','inside']) {
  await fs.writeFile(path.join(chat,method+'.jsonl'),JSON.stringify(entry(7))+'\n');
  await fs.unlink(marker).catch(()=>{});await fs.writeFile(marker,cwd);
  if(stage==='static'){await fs.unlink(marker);await fs.symlink(outside,marker)}
  if(stage==='inside'){await fs.writeFile(marker+'.inside',cwd);await fs.unlink(marker);await fs.symlink(marker+'.inside',marker)}
  current={id:method+'-marker-'+stage,method,stage,trace:[]};armed=stage==='race';
  const params=method==='native_archive_artifact'?{...base,artifactName:'probe.bin'}:{...base,sessionKey:method};
  await record(current.id,method,params);const recorded=cases.pop();current.request=recorded.request;current.response=recorded.response;
  assert.equal(armed,false,'real marker replacement must fire');assert.equal(JSON.parse(current.response).ok,true);
  assert.ok(current.trace.some((e:any)=>e.api&&e.base64===Buffer.from(stage==='inside'?cwd:cwd+'\n\t').toString('base64')),'actual marker contents must be observed');
  rows.push(current);current=undefined;
 }
 const sources:any={};for(const f of ['cli/createLocalGateway.ts','gateway/client/InProcessGateway.ts','gateway/server/GatewayWsConnection.ts','gateway/dialog/projectRegistry.ts','web/server/listProjects.ts','pilot/paths.ts'])sources[f]=createHash('sha256').update(await originalRead(path.join(baseline,'src',f))).digest('hex');
 assert.equal(rows.length,9);await fs.writeFile(output,JSON.stringify({node:process.version,baseline,sources,cwd,home,cases:rows},null,2)+'\n',{flag:'wx'});console.log(JSON.stringify({status:'PASS',cases:rows.length,output}));
}finally{(fsMutable as any).readFile=originalRead;(fsSync as any).readFileSync=originalSync;syncBuiltinESMExports();armed=false;await fs.unlink(marker).catch(()=>{});await fs.writeFile(marker,cwd);await local.dispose()}
