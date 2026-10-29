// Supplemental real CLI observations. This is not the D0 sandbox apparatus.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync, writeFileSync, readFileSync, readdirSync, lstatSync, chmodSync, rmSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { once } from 'node:events';

assert.equal(process.version, 'v22.22.0', '#1028 probe Node version');
assert.ok(process.env.PDRS_BASELINE, '#1028 set PDRS_BASELINE');
assert.equal(process.argv.length, 3, '#1028 probe OUTPUT');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
mkdirSync(output, {recursive:true});
const require = createRequire(join(baseline, 'package.json'));
const { WebSocketServer } = require('ws');
const token = 'd1-fixture-not-a-secret';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const save = (name, value) => writeFileSync(join(output, name), JSON.stringify(value,null,2)+'\n', {flag:'wx'});
const definitions = [
  ['shape-complete', {messages:[{type:'user', text:'中文\n"\\', id:'u1', extra:{'10':10,'2':2,z:[null,false,0,1e20,1e21,1e-7]}, content:[{type:'tool_result',value:'keep'}]}, {type:'assistant.delta',delta:'tail',id:'a1',surrogate:'\ud800'}],cursor:'not-cli-output'}],
  ['messages-missing', {}], ['messages-null', {messages:null}],
  ['messages-false', {messages:false}], ['messages-zero', {messages:0}],
  ['messages-string', {messages:''}], ['messages-object', {messages:{z:1,'10':'ten','2':'two'}}],
  ['result-null', null], ['result-missing', undefined],
  ['id-empty', {messages:[]}, ['']], ['id-whitespace', {messages:[]}, ['  ']],
  ['project-before-id', {messages:[]}, ['--project','/before','d1-session']],
  ['missing-id-missing-token', {messages:[]}, [], false],
];
const captures = [];
function snapshot(root) {
  const entries = {};
  function visit(path, name) {
    const stat = lstatSync(path);
    entries[name] = stat.isDirectory() ? {type:'directory',mode:stat.mode&0o777} :
      {type:'file',mode:stat.mode&0o777,size:stat.size,sha256:sha(readFileSync(path))};
    if (stat.isDirectory()) for (const file of readdirSync(path).sort()) visit(join(path,file),name==='.'?file:`${name}/${file}`);
  }
  visit(root,'.');
  return entries;
}
for (const [axis, result, args=['d1-session'], auth=true] of definitions) {
  const input = {id:`sessions-messages--${axis}`,leaf:'sessions-messages',axis,
    argv:['sessions','messages',...args],stdinHex:'',env:auth?{PILOTDECK_SDK_AUTH_TOKEN:token}:{},
    mockGateway:true,acceptToken:token,mode:'supplemental',timeoutSeconds:12};
  const runtime = join(output,`runtime-${randomUUID()}`);
  mkdirSync(runtime,{mode:0o700});
  for (const dir of ['home','pilot','project','tmp','bin','xdg-config','xdg-cache','xdg-data','xdg-state']) mkdirSync(join(runtime,dir),{mode:0o700});
  for (const [file,bytes] of [['pilot/pilotdeck.yaml',Buffer.from('schemaVersion: 1\n')],['project/keep.txt',Buffer.from([68,48,32,102,105,120,116,117,114,101,0,255,10])],['openssl.cnf',Buffer.alloc(0)]]) {
    writeFileSync(join(runtime,file),bytes,{flag:'wx',mode:0o600});
    chmodSync(join(runtime,file),0o600);
  }
  const env = {LANG:'C',LC_ALL:'C',TZ:'UTC',TERM:'dumb',NO_COLOR:'1',ANALYTICS_ENABLED:'0',DOCKER_CONTAINER:'1',
    COMMIT_HASH:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',...input.env};
  for (const [key,path] of Object.entries({HOME:'home',USERPROFILE:'home',CFFIXED_USER_HOME:'home',PILOT_HOME:'pilot',PILOTDECK_CONFIG_PATH:'pilot/pilotdeck.yaml',XDG_CONFIG_HOME:'xdg-config',XDG_CACHE_HOME:'xdg-cache',XDG_DATA_HOME:'xdg-data',XDG_STATE_HOME:'xdg-state',TMPDIR:'tmp',TMP:'tmp',TEMP:'tmp',PWD:'project',PATH:'bin',OPENSSL_CONF:'openssl.cnf'})) env[key]=join(runtime,path);
  const server = new WebSocketServer({host:'127.0.0.1',port:0});
  const wire = []; const closes = []; let connections=0;
  let child; let timer; let timedOut=false;
  try {
    await once(server,'listening');
    const port=server.address().port;
    Object.assign(env,{PILOTDECK_GATEWAY_HOST:'127.0.0.1',PILOTDECK_GATEWAY_PORT:String(port),PILOTDECK_SDK_GATEWAY_URL:`ws://127.0.0.1:${port}/ws`,PILOTDECK_GATEWAY_URL:`ws://127.0.0.1:${port}/ws`});
    server.on('connection',socket=>{
      connections++;
      socket.on('error',()=>{});
      socket.on('close',(code,reason)=>closes.push({code,reason:reason.toString()}));
      socket.on('message',bytes=>{
        const frame=JSON.parse(bytes.toString());
        wire.push({kind:'receive',frame});
        const response=frame.type==='hello' ? {type:'hello_ok',protocolVersion:'1.2',serverVersion:'test',serverInfo:{}} : {type:'response',id:frame.id,ok:true,...(result===undefined?{}:{result})};
        const raw=JSON.stringify(response);
        wire.push({kind:'send',frame:response,raw});
        socket.send(raw);
      });
    });
    const disk=snapshot(runtime);
    child=spawn(process.execPath,[join(baseline,'dist/src/cli/pilotdeck.js'),...input.argv],{cwd:join(runtime,'project'),env,stdio:['pipe','pipe','pipe']});
    const stdout=[]; const stderr=[];
    child.stdout.on('data',data=>stdout.push(data)); child.stderr.on('data',data=>stderr.push(data));
    timer=setTimeout(()=>{timedOut=true;child.kill('SIGKILL');},12000);
    child.stdin.end();
    const [exit,signal]=await once(child,'close');
    assert.equal(timedOut,false,`#1028 bounded live cell ${axis}`);
    assert.deepEqual(snapshot(runtime),disk,`#1028 live disk unchanged ${axis}`);
    const capture={input,stdout:Buffer.concat(stdout).toString(),stderrRaw:Buffer.concat(stderr).toString(),exit,signal,connections,wire,close:closes,disk};
    captures.push(capture);
    console.log(JSON.stringify(capture));
  } finally {
    clearTimeout(timer);
    if (child && child.exitCode===null && child.signalCode===null) { child.kill('SIGKILL'); await once(child,'close'); }
    for (const socket of server.clients) socket.terminate();
    await new Promise(done=>server.close(done));
    rmSync(runtime,{recursive:true});
  }
}
save('supplemental.json',{nodeVersion:process.version,sourceSha256:Object.fromEntries(['src/cli/sdkCli.ts','packages/sdk/src/client.ts','dist/src/cli/pilotdeck.js'].map(file=>[file,sha(readFileSync(join(baseline,file)))])),cases:captures});
