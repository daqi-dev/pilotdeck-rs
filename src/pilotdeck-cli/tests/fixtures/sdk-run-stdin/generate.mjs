// #1136: real pinned runSdkCli with piped stdin and a deterministic RPC barrier.
// WebSocket is an in-process double restricted to mock 28999; no real provider.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {spawn} from 'node:child_process';
assert.equal(process.version, 'v22.22.0', '#1136 Node version');
const baseline = resolve(process.env.PDRS_BASELINE ?? '/Users/da/ws/pilotdeck-rs/resources/PilotDeck');
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const url = 'ws://127.0.0.1:28999/ws';
const repeat = 128 * 1024;
const stdin = `  ${'x'.repeat(repeat)}中文\n`;
const events = [
  {type:'assistant_text_delta',text:'fixture answer'},
  {type:'subagent_text_delta',delta:' subagent'},
  {type:'turn_completed',result:'wire terminal',usage:{inputTokens:1,outputTokens:2}},
];
if (process.argv[2] === '--child') {
  // Node spawn uses a nonblocking socket for stdin on macOS. Restore blocking
  // pipe semantics for the baseline's fs.readFile('/dev/stdin'), before importing TS.
  assert.ok(process.stdin._handle, '#1136 owned stdin handle');
  process.stdin._handle.setBlocking(true);
  const wire = [];
  const requests = [];
  const closes = [];
  let connections = 0;
  class Socket extends EventTarget {
    constructor(address) {
      super(); assert.equal(address, url, '#1136 mock URL');
      this.connection = ++connections; this.readyState = 1;
      wire.push({kind:'connection', connection:this.connection});
      queueMicrotask(() => this.dispatchEvent(new Event('open')));
    }
    send(raw) {
      const frame = JSON.parse(raw);
      wire.push({kind:'receive',connection:this.connection,raw});
      const reply = frame => queueMicrotask(() => {
        const raw = JSON.stringify(frame);
        wire.push({kind:'send',connection:this.connection,raw});
        this.dispatchEvent(new MessageEvent('message', {data:raw}));
      });
      if (frame.type === 'hello') {
        reply({type:'hello_ok',protocolVersion:'1.2',serverVersion:'0.1.0-mock',serverInfo:{mode:'remote',projectKey:'d1-project',sessionCount:1,capabilities:[]}});
      } else if (frame.method === 'new_session') {
        requests.push(frame.method);
        process.send({kind:'new_session'});
        reply({type:'response',id:frame.id,ok:true,result:{sessionKey:'d1-session'}});
      } else if (frame.method === 'resume_session') {
        requests.push(frame.method);
        reply({type:'response',id:frame.id,ok:true,result:{sessionKey:frame.params.sessionKey}});
      } else if (frame.method === 'submit_turn') {
        requests.push(frame.method);
        this.prompt = frame.params.message;
        process.send({kind:'prompt',bytes:Buffer.byteLength(this.prompt),sha256:sha(this.prompt)});
        events.forEach((event,seq) => reply({type:'event',id:frame.id,seq,final:seq === events.length-1,event}));
      } else {
        throw new Error(`#1136 unexpected request ${frame.method}`);
      }
    }
    close() {
      if (this.readyState === 3) return;
      this.readyState = 3; closes.push(this.connection);
      wire.push({kind:'close',connection:this.connection});
      const event = new Event('close');
      Object.defineProperties(event,{code:{value:1005},reason:{value:''}});
      this.dispatchEvent(event);
    }
  }
  globalThis.WebSocket = Socket;
  const {runSdkCli} = await import(pathToFileURL(join(baseline,'dist/src/cli/sdkCli.js')));
  await runSdkCli(['run']);
  await new Promise((resolve,reject) => process.send({kind:'done',connections,requests,closes,wire}, error => error ? reject(error) : resolve()));
  process.disconnect();
} else {
  assert.equal(process.argv.length,3,'#1136 generator OUTPUT');
  const output = resolve(process.argv[2]);
  mkdirSync(output,{recursive:true});
  const rows = [];
  for (const delayed of [false,true]) {
    const id = delayed ? 'after-create' : 'early';
    const directory = join(output,id);
    mkdirSync(directory,{recursive:true});
    for (const name of ['home','pilot','project','tmp','bin','config','cache','data','state']) mkdirSync(join(directory,name),{mode:0o700});
    writeFileSync(join(directory,'pilot/pilotdeck.yaml'),'schemaVersion: 1\n',{flag:'wx'});
    const env = {LANG:'C',LC_ALL:'C',TZ:'UTC',TERM:'dumb',NO_COLOR:'1',ANALYTICS_ENABLED:'0',DOCKER_CONTAINER:'1',PDRS_BASELINE:baseline,PILOTDECK_SDK_AUTH_TOKEN:'d1-fixture-not-a-secret',PILOTDECK_SDK_GATEWAY_URL:url,PILOTDECK_GATEWAY_URL:url};
    for (const [key,path] of Object.entries({HOME:'home',USERPROFILE:'home',CFFIXED_USER_HOME:'home',PILOT_HOME:'pilot',PILOTDECK_CONFIG_PATH:'pilot/pilotdeck.yaml',XDG_CONFIG_HOME:'config',XDG_CACHE_HOME:'cache',XDG_DATA_HOME:'data',XDG_STATE_HOME:'state',TMPDIR:'tmp',TMP:'tmp',TEMP:'tmp',PWD:'project',PATH:'bin'})) env[key]=join(directory,path);
    const child = spawn(process.execPath,[resolve(process.argv[1]),'--child'],{env,cwd:join(directory,'project'),stdio:['pipe','pipe','pipe','ipc']});
    const stdout = [], stderr = [], messages = [];
    child.stdout.on('data',bytes => stdout.push(bytes));
    child.stderr.on('data',bytes => stderr.push(bytes));
    let inputError;
    child.stdin.on('error',error => {inputError=error;});
    const timer = setTimeout(() => child.kill('SIGKILL'),15000);
    child.on('message',message => {
      messages.push(message);
      if (delayed && message.kind === 'new_session') child.stdin.end(stdin);
    });
    if (!delayed) child.stdin.end(stdin);
    const status = await new Promise((resolve,reject) => {
      child.once('error',reject);
      child.once('close',(code,signal) => resolve({code,signal}));
    }).finally(() => clearTimeout(timer));
    const out = Buffer.concat(stdout), err = Buffer.concat(stderr);
    for (const [file,bytes] of [['stdout.bin',out],['stderr.bin',err],['stdin.bin',stdin],['messages.json',JSON.stringify(messages,null,2)+'\n']]) writeFileSync(join(directory,file),bytes,{flag:'wx'});
    assert.equal(inputError,undefined,'#1136 complete piped stdin');
    assert.equal(status.signal,null,'#1136 bounded TS exit');
    assert.equal(status.code,0,'#1136 successful TS exit');
    const prompt = messages.find(message => message.kind === 'prompt');
    const done = messages.find(message => message.kind === 'done');
    assert.equal(prompt?.sha256,sha(stdin.trim()),'#1136 complete submitted prompt');
    assert.deepEqual(done?.requests,['new_session','resume_session','submit_turn'],'#1136 RPC order');
    assert.deepEqual(done?.closes,[2,1],'#1136 query then control closed');
    // Only the approved SQLite warning prefix is removed; raw stderr is retained.
    const stderrText = err.toString().replace(/^\(node:\d+\) ExperimentalWarning: SQLite is an experimental feature and might change at any time\n\(Use `node --trace-warnings \.\.\.` to show where the warning was created\)\n/,'');
    const row={id,delayed,repeat,stdinBytes:Buffer.byteLength(stdin),promptBytes:prompt.bytes,promptSha256:prompt.sha256,stdout:out.toString(),stderr:stderrText,exit:status.code,signal:status.signal,connections:done.connections,closes:done.closes};
    rows.push(row); console.log(JSON.stringify(row));
  }
  const files=['src/cli/sdkCli.ts','dist/src/cli/sdkCli.js','packages/sdk/src/client.ts','packages/sdk/dist/client.js','packages/sdk/src/transport.ts','packages/sdk/dist/transport.js'];
  const oracle={nodeVersion:process.version,sourceSha256:Object.fromEntries(files.map(file=>[file,sha(readFileSync(join(baseline,file)))])),cases:rows};
  writeFileSync(join(output,'oracle.json'),JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
}
