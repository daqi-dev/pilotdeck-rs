// Production default assembly + real disk + production WS serializer, no socket.
import * as fs from 'node:fs/promises';
import mutable from 'node:fs/promises';
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
for (const key of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR','PILOT_HOME']) {
  const value = await fs.realpath(process.env[key] ?? '');
  assert.ok(value.startsWith(scratch + path.sep), key);
}
await assert.rejects(fs.stat(output), {code:'ENOENT'});
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
const cases:any[]=[];
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
const originalStat=mutable.stat,originalRead=mutable.readFile;
const rows:any[]=[];let current:any;const deniedPaths:string[]=[];const loops:string[]=[];let chatsMoved=false;
for(const api of ['stat','readFile']) {
 const original=(mutable as any)[api];(mutable as any)[api]=async (...args:any[])=>{try{return await original(...args)}catch(error:any){if(current&&String(args[0])===current.target)current.observations.push({api,path:String(args[0]),code:error.code,syscall:error.syscall,message:error.message});throw error}};
}
syncBuiltinESMExports();
try {
 for(const method of ['native_archive_manifest','native_archive_entries']) for(const kind of ['missing','directory','loop','notdir','denied','long']) {
  const key=kind+'-'+method;let target=path.join(chat,key+'.jsonl');
  if(kind==='directory')await fs.mkdir(target);
  if(kind==='loop'){await fs.symlink(target,target);loops.push(target)}
  if(kind==='notdir'){await fs.writeFile(path.join(chat,key),'file');target=path.join(chat,key,'body.jsonl')}
  if(kind==='denied'){await fs.writeFile(target,JSON.stringify(entry(0)));await fs.chmod(target,0);deniedPaths.push(target)}
  if(kind==='long')target=path.join(chat,'x'.repeat(256)+'.jsonl');
  const sessionKey=kind==='notdir'?key+'/body':kind==='long'?'x'.repeat(256):key;
  // Sanitization removes slash from session keys, so ENOTDIR uses the chats parent.
  if(kind==='notdir'){await fs.unlink(path.join(chat,key));await fs.rename(chat,chat+'-kept');chatsMoved=true;await fs.writeFile(chat,'file');target=path.join(chat,key+'.jsonl')}
  current={id:key,method,kind,target,observations:[]};
  await record(key,method,{...base,sessionKey:kind==='notdir'?key:sessionKey});
  current.request=cases.at(-1).request;current.response=cases.at(-1).response;
  const expected:any={missing:'ENOENT',directory:'EISDIR',loop:'ELOOP',notdir:'ENOTDIR',denied:'EACCES',long:'ENAMETOOLONG'};
  current.status=current.observations.some((o:any)=>o.code===expected[kind])?'RUN':'NOT_RUN';rows.push(current);current=undefined;
  if(kind==='denied')await fs.chmod(target,0o600);
  if(kind==='loop')await fs.unlink(target);
  if(kind==='notdir'){await fs.unlink(chat);await fs.rename(chat+'-kept',chat);chatsMoved=false}
 }
 assert.equal(rows.length,12);assert.equal(new Set(rows.map(r=>r.id)).size,12);
 const sources:any={};for(const f of ['cli/createLocalGateway.ts','gateway/client/InProcessGateway.ts','gateway/server/GatewayWsConnection.ts','session/transcript/TranscriptReader.ts'])sources[f]=createHash('sha256').update(await originalRead(path.join(baseline,'src',f))).digest('hex');
 await fs.writeFile(output,JSON.stringify({node:process.version,baseline,sources,cwd,home,cases:rows},null,2)+'\n',{flag:'wx'});
 console.log(JSON.stringify({status:rows.every(r=>r.status==='RUN')?'PASS':'NOT_RUN',cases:rows.length,output}));if(rows.some(r=>r.status!=='RUN'))process.exitCode=2;
}finally{for(const p of deniedPaths)await fs.chmod(p,0o600).catch(()=>{});for(const p of loops)await fs.unlink(p).catch(()=>{});if(chatsMoved){await fs.unlink(chat).catch(()=>{});await fs.rename(chat+'-kept',chat)}(mutable as any).stat=originalStat;(mutable as any).readFile=originalRead;syncBuiltinESMExports();await local.dispose()}
