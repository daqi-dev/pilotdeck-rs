// Real Node v22.22.0 runSdkCli/SDK probes with an in-process WebSocket double.
// This is a function probe, not a replacement for D0 OS-isolated CLI captures.
// No real socket is created; the double rejects every URL except owned 28999.
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, mkdirSync, openSync, closeSync, readdirSync, lstatSync, chmodSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
assert.equal(process.version, 'v22.22.0', '#1058 probe Node version');
const sha = x => createHash('sha256').update(x).digest('hex');
const baseline = resolve(process.env.PDRS_BASELINE ?? '/Users/da/ws/pilotdeck-rs/resources/PilotDeck');
const url = 'ws://127.0.0.1:28999/ws';
const token = 'd1-fixture-not-a-secret';
const defaultEvents = [
  {type:'assistant_text_delta',text:'fixture answer'},
  {type:'subagent_text_delta',delta:' subagent'},
  {type:'turn_completed',result:'wire terminal',usage:{inputTokens:1,outputTokens:2}},
];
if (process.argv[2] === '--child') {
  const config = JSON.parse(readFileSync(process.argv[3], 'utf8'));
  const wire = [];
  let connection = 0;
  class Socket extends EventTarget {
    constructor(address) {
      super();
      assert.equal(address,url,'#1058 in-process socket URL');
      this.connection = ++connection;
      this.readyState=1;
      wire.push({kind:'connection',connection:this.connection});
      queueMicrotask(()=>this.dispatchEvent(new Event('open')));
    }
    send(raw) {
      const frame=JSON.parse(raw);
      wire.push({kind:'receive',connection:this.connection,frame,raw});
      const reply = response => queueMicrotask(()=>{
        const raw=JSON.stringify(response);
        wire.push({kind:'send',connection:this.connection,frame:response,raw});
        this.dispatchEvent(new MessageEvent('message',{data:raw}));
      });
      if (frame.type==='hello') {
        reply({type:'hello_ok',protocolVersion:'1.2',serverVersion:'0.1.0-mock',serverInfo:{mode:'remote',projectKey:'d1-project',sessionCount:1,capabilities:config.capabilities??[]}});
      } else if (frame.method==='new_session') {
        reply({type:'response',id:frame.id,ok:true,result:{sessionKey:'d1-session'}});
      } else if (frame.method==='resume_session') {
        reply({type:'response',id:frame.id,ok:true,result:{sessionKey:frame.params.sessionKey}});
      } else if (frame.method==='model_catalog_list') {
        reply({type:'response',id:frame.id,ok:true,result:{items:config.catalog??[{id:'fixture/model',provider:'fixture',model:'model'}]}});
      } else if (frame.method==='submit_turn') {
        if(config.unrelatedFrame) reply({type:'event',id:'unrelated-stream',seq:0,final:true,event:{type:'subagent_text_delta',text:'must ignore'}});
        const events=config.events??defaultEvents;
        events.forEach((event,seq)=>{
          const response={type:'event',id:frame.id,seq,final:!config.responseEnd && seq===events.length-1,event};
          reply(response);
          if (config.duplicateSequence===seq) reply(response);
        });
        if(config.responseEnd) reply({type:'response',id:frame.id,ok:true,result:{}});
      } else { throw new Error(`#1058 unexpected method ${frame.method}`); }
    }
    close() {
      if(this.readyState===3)return;
      this.readyState=3;
      wire.push({kind:'close',connection:this.connection,code:1005,reason:''});
      const event=new Event('close');
      Object.defineProperties(event,{code:{value:1005},reason:{value:''}});
      this.dispatchEvent(event);
    }
  }
  globalThis.WebSocket=Socket;
  const {runSdkCli}=await import(pathToFileURL(join(baseline,'dist/src/cli/sdkCli.js')));
  try { await runSdkCli(config.argv); }
  catch(error) { console.error(error.message); process.exitCode=1; }
  writeFileSync(config.wire,JSON.stringify(wire,null,2)+'\n',{flag:'wx'});
} else {
  assert.equal(process.argv.length,3,'#1058 probe OUTPUT');
  const output=resolve(process.argv[2]);
  mkdirSync(output,{recursive:true});
  const definitions=[
    {name:'stdin-text',argv:['run'],stdin:'  fixture stdin 中文\n'},
    {name:'stdin-json',argv:['run'],stdin:'{"prompt":"json input"}\n'},
    {name:'stdin-jsonl',argv:['run'],stdin:'{"prompt":"first"}\n{"prompt":"second"}\n'},
    {name:'stdin-whitespace-argv',argv:['run','\ufeff \t'],stdin:'\ufeff fallback \u00a0'},
    {name:'stdin-empty-argv',argv:['run',''],stdin:'fallback'},
    {name:'empty-prompt',argv:['run',' '],stdin:''},
    {name:'whitespace-prompt',argv:['run',' '],stdin:' \n\t\ufeff'},
    {name:'argv-trim',argv:['run',' \ufeff hello','world \u00a0'],stdin:'ignored'},
    {name:'text-parts',events:[{type:'assistant_text_delta',text:'甲'},{type:'assistant.message',text:'乙'},{type:'turn_completed',result:'ignored'}]},
    {name:'text-split-surrogate',events:[{type:'assistant_text_delta',text:'\ud83d'},{type:'assistant_text_delta',text:'\ude00'},{type:'turn_completed'}]},
    {name:'text-empty',events:[{type:'assistant_text_delta',text:''},{type:'turn_completed'}]},
    {name:'text-absent',events:[{type:'turn_completed',result:'ignored'}]},
    {name:'text-priority',events:[{type:'assistant_thinking_delta',text:'hidden'},{type:'subagent_text_delta',text:'visible',delta:'ignored'},{type:'turn_completed'}]},
    {name:'text-empty-priority',events:[{type:'subagent_text_delta',text:'',delta:'ignored'},{type:'assistant_block',kind:'text',text:'block'},{type:'turn_completed'}]},
    {name:'terminal-before-final',events:[{type:'turn_completed'},{type:'subagent_text_delta',text:'tail'}]},
    {name:'response-end',events:[{type:'assistant_text_delta',text:'done'},{type:'turn_completed'}],responseEnd:true},
    {name:'duplicate-sequence',duplicateSequence:1},
    {name:'unrelated-stream',unrelatedFrame:true},
    {name:'sdk-defaults-capability',capabilities:['sdk_session_defaults']},
    ...['default','plan','auto','bypassPermissions','acceptEdits','dontAsk','invalid'].map(mode=>({name:`permission-${mode}`,argv:['run','fixture prompt','--permission-mode',mode]})),
    {name:'model-by-name',argv:['run','fixture prompt','--project','/d1-project','--model','model']},
    {name:'model-by-id',argv:['run','fixture prompt','--project','/d1-project','--model','alias'],catalog:[{id:'alias',provider:'fixture',model:'model'}]},
    {name:'model-by-pair',argv:['run','fixture prompt','--project','/d1-project','--model','fixture/model'],catalog:[{id:'alias',provider:'fixture',model:'model'}]},
    {name:'model-disabled-sibling',argv:['run','fixture prompt','--project','/d1-project','--model','fixture/model'],catalog:[{id:'fixture/model',provider:'wrong',model:'wrong',available:false},{id:'fixture/model',provider:'fixture',model:'model'}]},
    {name:'model-disabled',argv:['run','fixture prompt','--project','/d1-project','--model','fixture/model'],catalog:[{id:'fixture/model',provider:'fixture',model:'model',available:false}]},
    {name:'model-ambiguous',argv:['run','fixture prompt','--project','/d1-project','--model','model'],catalog:[{provider:'a',model:'model'},{provider:'b',model:'model'}]},
  ];
  const snapshot=root=>{
    const entries={};
    function visit(path,name){ const s=lstatSync(path);entries[name]=s.isDirectory()?{type:'directory',mode:s.mode&0o777}:{type:'file',mode:s.mode&0o777,size:s.size,sha256:sha(readFileSync(path))};if(s.isDirectory())for(const n of readdirSync(path).sort())visit(join(path,n),name==='.'?n:`${name}/${n}`); }
    visit(root,'.');return entries;
  };
  const cases=[];
  for(const d of definitions){
    const dir=join(output,d.name);mkdirSync(dir,{recursive:true});
    const runtime=join(dir,'runtime');mkdirSync(runtime,{mode:0o700});
    for(const name of ['home','pilot','project','tmp','bin','xdg-config','xdg-cache','xdg-data','xdg-state'])mkdirSync(join(runtime,name),{mode:0o700});
    for(const [name,bytes] of [['pilot/pilotdeck.yaml',Buffer.from('schemaVersion: 1\n')],['project/keep.txt',Buffer.from([68,48,32,102,105,120,116,117,114,101,0,255,10])],['openssl.cnf',Buffer.alloc(0)]]){writeFileSync(join(runtime,name),bytes,{flag:'wx',mode:0o600});chmodSync(join(runtime,name),0o600);}
    const env={LANG:'C',LC_ALL:'C',TZ:'UTC',TERM:'dumb',NO_COLOR:'1',ANALYTICS_ENABLED:'0',DOCKER_CONTAINER:'1',PDRS_BASELINE:baseline,PILOTDECK_SDK_AUTH_TOKEN:token,PILOTDECK_SDK_GATEWAY_URL:url};
    for(const [name,path] of Object.entries({HOME:'home',USERPROFILE:'home',CFFIXED_USER_HOME:'home',PILOT_HOME:'pilot',PILOTDECK_CONFIG_PATH:'pilot/pilotdeck.yaml',XDG_CONFIG_HOME:'xdg-config',XDG_CACHE_HOME:'xdg-cache',XDG_DATA_HOME:'xdg-data',XDG_STATE_HOME:'xdg-state',TMPDIR:'tmp',TMP:'tmp',TEMP:'tmp',PWD:'project',PATH:'bin',OPENSSL_CONF:'openssl.cnf'}))env[name]=join(runtime,path);
    const argv=d.argv??['run','fixture prompt'];
    const input={id:`run-probe--${d.name}`,axis:d.name,argv,stdinHex:Buffer.from(d.stdin??'').toString('hex'),env:{PILOTDECK_SDK_AUTH_TOKEN:token},mockGateway:true,acceptToken:token};
    const config={...d,argv,wire:join(dir,'wire.json')};
    writeFileSync(join(dir,'input.json'),JSON.stringify(config,null,2)+'\n',{flag:'wx'});
    writeFileSync(join(dir,'stdin.bin'),d.stdin??'',{flag:'wx'});
    const disk=snapshot(runtime), fd=openSync(join(dir,'stdin.bin'),'r');
    let result;
    try{result=spawnSync(process.execPath,[resolve(process.argv[1]),'--child',join(dir,'input.json')],{cwd:join(runtime,'project'),env,stdio:[fd,'pipe','pipe'],timeout:5000});}finally{closeSync(fd);}
    writeFileSync(join(dir,'stdout.bin'),result.stdout,{flag:'wx'});writeFileSync(join(dir,'stderr.bin'),result.stderr,{flag:'wx'});
    assert.equal(result.error,undefined,`#1058 bounded function probe ${d.name}`);
    assert.equal(result.signal,null,`#1058 function exited ${d.name}`);
    assert.deepEqual(snapshot(runtime),disk,`#1058 function disk unchanged ${d.name}`);
    const wire=JSON.parse(readFileSync(config.wire,'utf8'));
    cases.push({input,source:'Node runSdkCli function + in-process WebSocket double (not D0 CLI)',stdout:result.stdout.toString(),stderrRaw:result.stderr.toString(),exit:result.status,signal:result.signal,connections:wire.filter(r=>r.kind==='connection').length,wire,disk});
    console.log(JSON.stringify({id:input.id,exit:result.status,stdout:result.stdout.toString(),stderr:result.stderr.toString(),connections:cases.at(-1).connections}));
  }
  const sourceFiles=['src/cli/sdkCli.ts','dist/src/cli/sdkCli.js','packages/sdk/src/client.ts','packages/sdk/dist/client.js','packages/sdk/src/transport.ts','packages/sdk/dist/transport.js'];
  writeFileSync(join(output,'supplemental.json'),JSON.stringify({nodeVersion:process.version,kind:'real-ts-function-probe-in-process-wire',sourceSha256:Object.fromEntries(sourceFiles.map(file=>[file,sha(readFileSync(join(baseline,file)))])),cases},null,2)+'\n',{flag:'wx'});
}
