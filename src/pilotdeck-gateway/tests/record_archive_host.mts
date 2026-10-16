// Production default assembly + real disk + production WS serializer, no socket.
import * as fs from 'node:fs/promises';
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
const methods=['native_archive_manifest','native_archive_entries'];
const hostCases:any[]=[];
try {
  for(const [label,values] of [
    ['ties-lone',[[0,'\ud800'],[0,'a']]],
    ['unique-lone',[[0,'\ud800'],[1,'a']]],
    ['zero-tie',[[0,'\ud800'],[-0,'a']]],
    ['paired',[[0,'😀'],[0,'a']]],
    ['invalid-before-host',[[0,'\ud800'],[0,'a']]],
  ] as const) {
    const body=values.map(([sequence,createdAt])=>JSON.stringify(entry(sequence,'message',createdAt))).join('\n')+(label==='invalid-before-host'?'\n{broken':'');
    await fs.writeFile(path.join(chat,label+'.jsonl'),body);
    for(const method of methods) await record(label+'-'+method,method,{...base,sessionKey:label});
  }
  const dir=path.join(cwd,'.pilotdeck/tool-results/ties-lone');await fs.mkdir(dir,{recursive:true});await fs.writeFile(path.join(dir,'body'),'safe-artifact');
  await record('artifact-independent-of-host', 'native_archive_artifact',{...base,sessionKey:'ties-lone',artifactName:'body'});
  const files:any={};
  async function capture(p:string) {const stat=await fs.stat(p);files[p]={directory:stat.isDirectory(),size:stat.size};if(stat.isDirectory()){const rows=await fs.readdir(p,{withFileTypes:true});files[p].entries=rows.map(e=>[e.name,e.isDirectory()]);for(const row of rows)await capture(path.join(p,row.name))}else files[p].base64=(await fs.readFile(p)).toString('base64')}
  await capture(home);await capture(cwd);
  const sources:any={};for(const f of ['cli/createLocalGateway.ts','gateway/client/InProcessGateway.ts','gateway/server/GatewayWsConnection.ts','session/transcript/TranscriptReader.ts'])sources[f]=createHash('sha256').update(await fs.readFile(path.join(baseline,'src',f))).digest('hex');
  assert.equal(cases.length,11);assert.equal(new Set(cases.map(c=>c.id)).size,11);
  await fs.writeFile(output,JSON.stringify({node:process.version,baseline,sources,cwd,home,files,cases},null,2)+'\n',{flag:'wx'});
  console.log(JSON.stringify({status:'PASS',cases:cases.length,output}));
}finally{await local.dispose()}
