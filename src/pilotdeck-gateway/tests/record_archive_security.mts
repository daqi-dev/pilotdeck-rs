// Production default assembly + real disk + production WS serializer, no socket.
import * as fs from 'node:fs/promises';
import fsMutable from 'node:fs/promises';
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

const manifest='native_archive_manifest', entries='native_archive_entries', artifact='native_archive_artifact';
const outside=path.join(scratch,'outside-fixture.jsonl');
const artifactDir=path.join(cwd,'.pilotdeck/tool-results/fixture');
const links:string[]=[];
const trace:any[]=[];
const originalStat=fsMutable.stat;
const originalRead=fsMutable.readFile;
let armed:any;
async function swap(p:any, stage:string) {
  if(!armed || String(p)!==armed.path || armed.stage!==stage) return;
  trace.push({id:armed.id,stage,path:String(p),event:'replace-leaf-with-symlink'});
  await fs.unlink(armed.path);
  await fs.symlink(outside,armed.path);
  links.push(armed.path);
  armed=undefined;
}
(fsMutable as any).stat=async (...args:any[])=>{
  const result=await (originalStat as any)(...args);
  await swap(args[0],'after-stat-before-read');
  return result;
};
(fsMutable as any).readFile=async (...args:any[])=>{
  if(armed && String(args[0])===armed.path)trace.push({id:armed.id,stage:'readFile-enter',path:String(args[0])});
  await swap(args[0],'before-readFile-open');
  return (originalRead as any)(...args);
};
syncBuiltinESMExports();
let failure:any;
try {
  await fs.mkdir(artifactDir,{recursive:true});
  await fs.writeFile(outside,JSON.stringify(entry(4242,'message','2026-10-16T00:00:00Z'))+'\n');
  for(const method of [manifest,entries,artifact]) {
    const target=method===artifact?path.join(artifactDir,'probe.bin'):path.join(chat,`${method}.jsonl`);
    const params=method===artifact?{...base,artifactName:'probe.bin'}:{...base,sessionKey:method};
    const inside=JSON.stringify(entry(7))+'\n';
    await fs.writeFile(target,inside);
    await record(`${method}-normal`,method,params);
    assert.equal(JSON.parse(cases.at(-1).response).ok,true);
    await fs.rename(target,target+'.inside');await fs.symlink(target+'.inside',target);links.push(target);
    await record(`${method}-in-root-symlink`,method,params);
    assert.equal(JSON.parse(cases.at(-1).response).ok,true);inRootLinks.push(cases.pop());
    await fs.unlink(target);links.splice(links.indexOf(target),1);await fs.rename(target+'.inside',target);
    await fs.unlink(target);
    await fs.symlink(outside,target);links.push(target);
    await record(`${method}-static-outside-symlink`,method,params);
    const got=JSON.parse(cases.at(-1).response);assert.equal(got.ok,true);
    if(method===manifest)assert.equal(got.result.firstSequence,4242);
    else if(method===entries)assert.equal(got.result.entries[0].sequence,4242);
    else assert.equal(Buffer.from(got.result.content,'base64').toString(),await originalRead(outside,'utf8'));
    await fs.unlink(target);links.splice(links.indexOf(target),1);
    await fs.writeFile(target,inside);
    const stage=method===artifact?'before-readFile-open':'after-stat-before-read';
    const id=`${method}-${stage}`;armed={id,path:target,stage};
    await record(id,method,params);
    assert.equal(armed,undefined,'injection must actually fire');
    const raced=JSON.parse(cases.at(-1).response);assert.equal(raced.ok,true);
    if(method===manifest)assert.equal(raced.result.firstSequence,4242);
    else if(method===entries)assert.equal(raced.result.entries[0].sequence,4242);
    else assert.equal(Buffer.from(raced.result.content,'base64').toString(),await originalRead(outside,'utf8'));
    await fs.unlink(target);links.splice(links.indexOf(target),1);
  }
} catch(e) {failure={message:String(e),stack:(e as any)?.stack};process.exitCode=1}
finally {
  (fsMutable as any).stat=originalStat;(fsMutable as any).readFile=originalRead;syncBuiltinESMExports();armed=undefined;
  for(const link of links)await fs.unlink(link).catch(()=>{});
  await local.dispose();
  const sources:any={};
  for(const f of ['cli/createLocalGateway.ts','gateway/client/InProcessGateway.ts','gateway/server/GatewayWsConnection.ts','session/transcript/TranscriptReader.ts'])sources[f]=createHash('sha256').update(await originalRead(path.join(baseline,'src',f))).digest('hex');
  assert.equal(cases.length,9);assert.equal(new Set(cases.map(c=>c.id)).size,9);
  await fs.writeFile(output,JSON.stringify({status:failure?'FAIL':'PASS',failure,node:process.version,v8:process.versions.v8,baseline,sources,cwd,home,cases,inRootLinks,trace,cleanup:{disposed:true,fsRestored:true,ownedSymlinksRemaining:0,listeningSocketsCreated:0},scope:{statReadRaces:['native_archive_manifest','native_archive_entries'],artifactInjection:'pre-readFile-open; baseline has no stat/check',notTested:['ancestor/root replacement','new Rust HEAD','uncontrolled OS race']}},null,2)+'\n');
  console.log(JSON.stringify({status:failure?'FAIL':'PASS',cases:cases.length,trace,output}));
}
