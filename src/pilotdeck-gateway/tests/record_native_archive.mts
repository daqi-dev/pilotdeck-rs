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
try {
  await fs.writeFile(path.join(chat,'fixture.jsonl'),valid.map(e=>JSON.stringify(e)).join('\r\n')+'\n\n');
  await record('manifest-valid',manifest,base);
  await record('entries-default',entries,base);
  await record('entries-page',entries,{...base,limit:2});
  await record('entries-after',entries,{...base,afterSequence:1,limit:1});
  await record('entries-duplicates',entries,{...base,afterSequence:2,limit:1});
  await record('entries-end',entries,{...base,afterSequence:9007199254740991,limit:500});
  await record('project-fallback',manifest,{sessionKey:'fixture'});
  await record('project-relative',manifest,{...base,projectKey:'.'});
  await record('project-null',manifest,{...base,projectKey:null});
  for(const [id,projectKey] of [['number',1],['bool',true],['array',[]],['object',{}],['empty','']]) await record(`project-${id}`,manifest,{...base,projectKey});
  await record('session-unpaired',manifest,{...base,sessionKey:'\ud800'});
  await record('project-unpaired',manifest,{...base,projectKey:'\ud800'});
  for(const [i,sessionKey] of ["a'\0b",'a"\0b',"a'\"\0b"].entries()) await record(`session-nul-quotes-${i}`,manifest,{...base,sessionKey});
  await record('session-nul',manifest,{...base,sessionKey:'a\0b'});
  await record('artifact-nul',artifact,{...base,artifactName:'a\0b'});
  await record('project-unknown',manifest,{...base,projectKey:path.join(cwd,'unknown')});
  for(const method of [manifest,entries,artifact]) {
    for(const [id,sessionKey] of [['missing',undefined],['null',null],['empty',''],['space',' \uFEFF\t'],['number',1],['false',false],['array',[]],['object',{}]]) await record(`${method}-session-${id}`,method,{...base,sessionKey,artifactName:'body.bin'});
    await record(`${method}-params-null`,method,null);
    await record(`${method}-params-omitted`,method,undefined,{omitParams:true});
    await record(`${method}-params-number`,method,1);
  }
  for(const [key,values] of [['afterSequence',[-1,0,0.5,9007199254740991,9007199254740992,null,'0',false,{},[]]],['limit',[0,1,500,501,1.5,null,'1',false,{},[]]]] as const)
    for(const [i,value] of values.entries()) await record(`entries-${key}-${i}`,entries,{...base,[key]:value});
  await record('entries-priority',entries,{...base,afterSequence:-1,limit:0,projectKey:'unknown'});
  for(const [id,body] of [['empty',''],['blank',' \uFEFF\t\r\n\n'],['broken','{bad\n'],['truncated',JSON.stringify(entry(0))+'\n{"type":'],['shape','{}\n'],['cr-only',JSON.stringify(entry(0))+'\r'+JSON.stringify(entry(1))],['weak',JSON.stringify(entry(-2,'unknown','anything'))+'\n'+JSON.stringify(entry(1.5,'',''))],['overflow',JSON.stringify(entry(1)).replace('"sequence":1','"sequence":1e400')],['ties', [entry(1,'message','b'),entry(1,'subagent_started','A'),entry(1,'tool_result','a'),entry(1,'message','a')].map(e=>JSON.stringify(e)).join('\n')],['unicode-sort',[entry(1,'message','中'),entry(1,'message','阿')].map(e=>JSON.stringify(e)).join('\n')]]) {
    await fs.writeFile(path.join(chat,`${id}.jsonl`),body);
    for(const method of [manifest,entries]) await record(`${id}-${method}`,method,{...base,sessionKey:id},{});
  }
  await fs.writeFile(path.join(chat,'nonutf8.jsonl'),Buffer.concat([Buffer.from(JSON.stringify(entry(0)).replace('你好','')),Buffer.from('\n')]));
  const utf8 = Buffer.concat([Buffer.from('{"type":"message","sessionId":"s","turnId":"t","sequence":0,"createdAt":"'),Buffer.from([0xff,0xfe,0x80]),Buffer.from('"}\n')]);
  await fs.writeFile(path.join(chat,'replacement-sort.jsonl'),utf8);
  await record('replacement-sort-manifest',manifest,{...base,sessionKey:'replacement-sort'},{});
  await fs.writeFile(path.join(chat,'nonutf8.jsonl'),Buffer.concat([Buffer.from(JSON.stringify(entry(0)).slice(0,-1)+',"text":"'),Buffer.from([0xff,0xfe,0x80]),Buffer.from('"}\n')]));
  await record('nonutf8-entries',entries,{...base,sessionKey:'nonutf8'});
  for(const method of [manifest,entries]) await record(`missing-${method}`,method,{...base,sessionKey:'absent'});
  await fs.mkdir(path.join(chat,'directory.jsonl'),{recursive:true});
  await record('directory-transcript',manifest,{...base,sessionKey:'directory'});
  const large=await fs.open(path.join(chat,'large.jsonl'),'w');await large.truncate(50*1024*1024+1);await large.close();
  await record('large-transcript',manifest,{...base,sessionKey:'large'});
  const sanitized='web:a---b';
  await fs.writeFile(path.join(chat,`${sanitized}.jsonl`),JSON.stringify(entry(0)));
  await record('session-sanitized',manifest,{...base,sessionKey:'/web:a/\\--b/'});
  const artdir=path.join(cwd,'.pilotdeck','tool-results','fixture');
  await fs.mkdir(artdir,{recursive:true});
  await fs.writeFile(path.join(artdir,'body.bin'),Buffer.from([0,1,2,0xff,65,66,67]));
  await fs.writeFile(path.join(artdir,'empty.bin'),'');
  await fs.writeFile(path.join(artdir,'default-large.bin'), Buffer.alloc(1_000_001, 65));
  await record('artifact-default-truncated',artifact,{...base,artifactName:'default-large.bin'});
  const huge=await fs.open(path.join(artdir,'huge.bin'),'w');await huge.truncate(2_147_483_648);await huge.close();
  await record('artifact-node-too-large',artifact,{...base,artifactName:'huge.bin'});
  await fs.mkdir(path.join(artdir,'directory.bin'),{recursive:true});
  for(const [id,artifactName] of [['body','body.bin'],['empty','empty.bin'],['absent','absent.bin'],['directory','directory.bin'],['dot','.'],['dotdot','..'],['slash','a/b'],['backslash','a\\b'],['unicode','中'],['empty-name',''],['missing-name',undefined],['null-name',null],['number-name',123],['bool-name',true],['array-name',['body.bin']],['object-name',{}]]) await record(`artifact-${id}`,artifact,{...base,artifactName});
  for(const [i,maxBytes] of [null,0,1,7,8,10000000,10000001,1.5,9007199254740992,'1',false,[],{}].entries()) await record(`artifact-limit-${i}`,artifact,{...base,artifactName:'body.bin',maxBytes});
  await record('artifact-priority',artifact,{...base,artifactName:'../bad',maxBytes:0,projectKey:'unknown'});
  await record('artifact-missing-name-priority',artifact,{...base,maxBytes:0});
  await record('artifact-null-name-priority',artifact,{...base,artifactName:null,maxBytes:0});
  await record('artifact-no-transcript',artifact,{...base,sessionKey:'no-transcript',artifactName:'body.bin'});
  // Direct provider boundary demonstrates -1 is accepted only behind IPG.
  const callback = (gateway as any).options.nativeArchiveEntries;
  const inner = await callback({...base,afterSequence:-1,limit:1});
  const callbackEvidence = {input:{...base,afterSequence:-1,limit:1},result:inner,wire:JSON.stringify(inner)};
  const securityEvidence:any[]=[];
  const outside=path.join(scratch,'outside-fixture');
  await fs.writeFile(outside,JSON.stringify(entry(42))+'\n');
  await fs.symlink(outside,path.join(chat,'escape.jsonl'));
  await fs.symlink(outside,path.join(artdir,'escape.bin'));
  for(const [id,method,params] of [
    ['security-transcript-symlink',manifest,{...base,sessionKey:'escape'}],
    ['security-artifact-symlink',artifact,{...base,artifactName:'escape.bin'}],
  ] as const) {
    await record(id,method,params);
    const evidence=cases.pop();assert.equal(JSON.parse(evidence.response).ok,true);
    securityEvidence.push({...evidence,candidateDifference:'PENDING #134 precise authorization; not an accepted difference'});
  }
  await fs.unlink(path.join(chat,'escape.jsonl'));await fs.unlink(path.join(artdir,'escape.bin'));
  await capture(home);await capture(cwd);
  const sources:any={};
  for(const f of ['cli/createLocalGateway.ts','cli/ProjectRuntimeRegistry.ts','gateway/client/InProcessGateway.ts','gateway/server/GatewayWsConnection.ts','gateway/dialog/projectRegistry.ts','session/storage/ProjectSessionStorage.ts','session/transcript/TranscriptReader.ts','pilot/paths.ts']) sources[f]=createHash('sha256').update(await fs.readFile(path.join(baseline,'src',f))).digest('hex');
  await fs.mkdir(path.dirname(output),{recursive:true});
  await fs.writeFile(output,JSON.stringify({node:process.version,v8:process.versions.v8,locale:new Intl.Collator().resolvedOptions().locale,baseline,sources,cwd,home,files,cases,callbackEvidence,securityEvidence},null,2)+'\n');
  console.log(JSON.stringify({status:'PASS',cases:cases.length,output}));
} finally {await local.dispose()}
