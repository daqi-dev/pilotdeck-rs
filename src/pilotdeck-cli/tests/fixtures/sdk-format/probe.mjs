// Real Node v22.22.0 runSdkCli/SDK probes with an in-process WebSocket double.
// This is a function probe, not a replacement for D0 OS-isolated CLI captures.
// No real socket is created; the double rejects every URL except owned 28999.
import assert from 'node:assert/strict';
import { readFileSync, writeFileSync, mkdirSync, openSync, closeSync, readdirSync, lstatSync, chmodSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { spawnSync } from 'node:child_process';
assert.equal(process.version, 'v22.22.0', '#1097 probe Node version');
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
      if(address!==url) throw Object.assign(new Error('EPERM: fixture denies non-28999 endpoint'),{code:'EPERM'});
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
        reply({type:'hello_ok',protocolVersion:'1.2',serverVersion:'0.1.0-mock',serverInfo:{mode:'remote',projectKey:'d1-project',sessionCount:1,capabilities:[]}});
      } else if (frame.method==='new_session') {
        reply({type:'response',id:frame.id,ok:true,result:{sessionKey:'d1-session'}});
      } else if (frame.method==='resume_session') {
        reply({type:'response',id:frame.id,ok:true,result:{sessionKey:frame.params.sessionKey}});
      } else if (frame.method==='submit_turn') {
        const events=config.events??defaultEvents;
        events.forEach((event,seq)=>{
          const response={type:'event',id:frame.id,seq,final:!config.responseEnd && seq===events.length-1,event};
          reply(response);
          if(config.duplicateFrameAt === seq) reply(response);
        });
        if(config.responseEnd) reply({type:'response',id:frame.id,ok:true,result:{}});
      } else { throw new Error(`#1097 unexpected method ${frame.method}`); }
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
  await new Promise(setImmediate); // Drain Node bootstrap warnings before CLI output.
  try { await runSdkCli(config.argv); }
  catch(error) { console.error(error.message); process.exitCode=1; }
  writeFileSync(config.wire,JSON.stringify(wire,null,2)+'\n',{flag:'wx'});
} else {
  assert.ok(process.argv.length===3 || (process.argv.length===4 && process.argv[3]==='--snapshot-only'),'#1097 probe OUTPUT [--snapshot-only]');
  const output=resolve(process.argv[2]);
  mkdirSync(output,{recursive:true});
  // Terminal state × format matrix. The --json envelope and the --stream-json
  // event lines are the P07 surface; plain projection stays the P05/P06 family.
  const states = [
    {name:'completed',events:defaultEvents},
    {name:'completed-no-text',events:[{type:'turn_completed',usage:{inputTokens:2}}]},
    {name:'completed-structured',events:[{type:'assistant_text_delta',text:'ignored'},{type:'structured_output',payload:{answer:42}},{type:'turn_completed'}]},
    {name:'completed-structured-null',events:[{type:'structured_output',payload:null},{type:'assistant_text_delta',text:'fallback'},{type:'turn_completed'}]},
    {name:'failed',events:[{type:'subagent_text_delta',delta:'partial '},{type:'error',code:'server_error',message:'private message',detail:{b:2,a:1}}]},
    {name:'unknown-empty',events:[],responseEnd:true},
    {name:'unknown-code',events:[{type:'turn_started',runId:'gateway-run-id'},{type:'error',code:'result_unknown'}]},
    {name:'unknown-snapshot',events:[{type:'turn_started',runId:'snapshot-run-id'},{type:'error',code:'result_unknown'},{type:'turn_started',runId:'too-late'}]},
  ];
  const definitions = [];
  for (const command of ['run','resume']) for (const axis of ['json','jsonl','both']) {
    const flags = axis==='json' ? ['--json'] : axis==='jsonl' ? ['--stream-json'] : ['--json','--stream-json'];
    for (const state of states) definitions.push({...state,name:`${command}-${state.name}-${axis}`,
      argv:command==='run'?['run','fixture prompt',...flags]:['resume','d1-session','fixture prompt',...flags]});
  }
  // Mapping edges: the JSONL line shape of every public event and the
  // envelope's structured/finishReason projection.
  definitions.push(
    {name:'run-jsonl-all-types',argv:['run','fixture prompt','--stream-json'],events:[
      {type:'turn_started',runId:'gateway-run-id',sequence:0},
      {type:'input_accepted',sessionId:'wire-session',runId:'wire-run',sequence:9},
      {type:'assistant_thinking_delta',delta:'hidden thinking'},
      {type:'assistant_text_delta',text:'answer'},
      {type:'subagent_text_delta',delta:' side'},
      {type:'tool_call_started',toolCallId:'t1',toolName:'fixture-tool'},
      {type:'tool_progress',toolCallId:'t1',progress:0.5},
      {type:'tool_call_finished',toolCallId:'t1',ok:true},
      {type:'tool_call_finished',toolCallId:'t2',ok:false},
      {type:'tool_result_detail_available',toolCallId:'t1'},
      {type:'permission_request',requestId:'p1'},
      {type:'permission_denied',requestId:'p1'},
      {type:'hook_started',hookId:'h1'},
      {type:'hook_response',hookId:'h1'},
      {type:'hook_async_result',hookId:'h1'},
      {type:'elicitation_request',requestId:'e1'},
      {type:'elicitation_cancelled',requestId:'e1'},
      {type:'user_dialog_request',requestId:'d1'},
      {type:'user_dialog_cancelled',requestId:'d1'},
      {type:'prompt_suggestion',suggestion:'next'},
      {type:'assistant_note',text:'streamed too'},
      {type:'custom.dotted',delta:'kept'},
      {type:'weird_event',text:'not streamed'},
      {type:'context_budget',used:5,displayUsed:6,other:'dropped',breakdown:{a:1}},
      {type:'structured_output',payload:{z:1}},
      {type:'turn_completed',result:'wire terminal',finishReason:'end_turn',usage:{inputTokens:1}},
      {type:'error',code:'late_error'},
    ]},
    {name:'run-jsonl-duplicate-frame',argv:['run','fixture prompt','--stream-json'],duplicateFrameAt:1,events:[{type:'input_accepted'},{type:'subagent_text_delta',text:'one'},{type:'turn_completed'}]},
    {name:'run-json-unknown-prefix',argv:['run','fixture prompt','--json'],events:[{type:'subagent_text_delta',text:'partial '}],responseEnd:true},
    {name:'run-jsonl-sequence-only' ,argv:['run','fixture prompt','--stream-json'],events:[{type:'input_accepted',sequence:7},{type:'turn_completed'}]},
    {name:'run-jsonl-runid-number',argv:['run','fixture prompt','--stream-json'],events:[{type:'turn_started',runId:123},{type:'turn_completed'}]},
    {name:'run-jsonl-type-number',argv:['run','fixture prompt','--stream-json'],events:[{type:5},{type:'turn_completed'}]},
    {name:'run-jsonl-type-null-missing',argv:['run','fixture prompt','--stream-json'],events:[{type:null},{},{type:'turn_completed'}]},
    {name:'run-jsonl-type-surrogate',argv:['run','fixture prompt','--stream-json'],events:[{type:'\ud800'},{type:'turn_completed'}]},
    {name:'run-jsonl-type-conversion',argv:['run','fixture prompt','--stream-json'],events:[{type:{toString:'x'}}]},
    {name:'run-jsonl-error-fields',argv:['run','fixture prompt','--stream-json'],events:[{type:'error',code:'x_code',message:'m',detail:{b:2,a:1}}]},
    {name:'run-json-finish-reason',argv:['run','fixture prompt','--json'],events:[{type:'turn_completed',finishReason:'end_turn'}]},
    ...['json','jsonl','both'].flatMap(axis => ['toString','__proto__'].map(type => ({
      name:`run-prototype-${type}-${axis}`, argv:['run','fixture prompt', ...(axis==='json'?['--json']:axis==='jsonl'?['--stream-json']:['--json','--stream-json'])], events:[{type},{type:'turn_completed'}],
    }))),
    {name:'run-jsonl-spread-array',argv:['run','fixture prompt','--stream-json'],events:[['x'],{type:'turn_completed'}]},
    {name:'run-jsonl-spread-string',argv:['run','fixture prompt','--stream-json'],events:['a\ud800',{type:'turn_completed'}]},
    {name:'run-jsonl-spread-null',argv:['run','fixture prompt','--stream-json'],events:[null,{type:'turn_completed'}]},
    {name:'run-jsonl-all-context-keys',argv:['run','fixture prompt','--stream-json'],events:[{type:'context_budget',used:1,displayUsed:2,budgetUsed:3,localEstimateTokens:4,displayTokens:5,estimateSource:'e',usageTokens:6,calibrationActualInputTokens:7,calibrationEstimatedInputTokens:8,total:9,totalContextTokens:10,maxContextTokens:11,effectiveTotal:12,effectiveContextTokens:13,maxOutputTokens:14,reservedOutputTokens:15,warningRatio:.8,blockingRatio:.9,ratio:.5,state:'ok',source:'s',exact:false,estimatorError:null,breakdown:{a:1},other:'drop'},{type:'turn_completed'}]},
    {name:'run-json-usage-array',argv:['run','fixture prompt','--json'],events:[{type:'turn_completed',usage:[1,2]}]},
    {name:'run-json-usage-scalar',argv:['run','fixture prompt','--json'],events:[{type:'turn_completed',usage:7}]},
    {name:'run-json-structured-scalar' ,argv:['run','fixture prompt','--json'],events:[{type:'structured_output',payload:false},{type:'turn_completed'}]},
  );
  const snapshot=root=>{
    const entries={};
    function visit(path,name){ const s=lstatSync(path);entries[name]=s.isDirectory()?{type:'directory',mode:s.mode&0o777}:{type:'file',mode:s.mode&0o777,size:s.size,sha256:sha(readFileSync(path))};if(s.isDirectory())for(const n of readdirSync(path).sort())visit(join(path,n),name==='.'?n:`${name}/${n}`); }
    visit(root,'.');return entries;
  };
  const cases=[];
  const selected = process.argv[3]==='--snapshot-only' ? definitions.filter(d=>d.name.includes('-unknown-snapshot-')) : definitions;
  for(const d of selected){
    const dir=join(output,d.name);mkdirSync(dir,{recursive:true});
    const runtime=join(dir,'runtime');mkdirSync(runtime,{mode:0o700});
    for(const name of ['home','pilot','project','tmp','bin','xdg-config','xdg-cache','xdg-data','xdg-state'])mkdirSync(join(runtime,name),{mode:0o700});
    for(const [name,bytes] of [['pilot/pilotdeck.yaml',Buffer.from('schemaVersion: 1\n')],['project/keep.txt',Buffer.from([68,48,32,102,105,120,116,117,114,101,0,255,10])],['openssl.cnf',Buffer.alloc(0)]]){writeFileSync(join(runtime,name),bytes,{flag:'wx',mode:0o600});chmodSync(join(runtime,name),0o600);}
    const env={LANG:'C',LC_ALL:'C',TZ:'UTC',TERM:'dumb',NO_COLOR:'1',ANALYTICS_ENABLED:'0',DOCKER_CONTAINER:'1',PDRS_BASELINE:baseline,PILOTDECK_SDK_AUTH_TOKEN:token,PILOTDECK_SDK_GATEWAY_URL:url};
    for(const [name,path] of Object.entries({HOME:'home',USERPROFILE:'home',CFFIXED_USER_HOME:'home',PILOT_HOME:'pilot',PILOTDECK_CONFIG_PATH:'pilot/pilotdeck.yaml',XDG_CONFIG_HOME:'xdg-config',XDG_CACHE_HOME:'xdg-cache',XDG_DATA_HOME:'xdg-data',XDG_STATE_HOME:'xdg-state',TMPDIR:'tmp',TMP:'tmp',TEMP:'tmp',PWD:'project',PATH:'bin',OPENSSL_CONF:'openssl.cnf'}))env[name]=join(runtime,path);
    const input={id:`format-probe--${d.name}`,axis:d.name,argv:d.argv,stdinHex:'',env:{PILOTDECK_SDK_AUTH_TOKEN:token},mockGateway:true,acceptToken:token};
    const config={...d,wire:join(dir,'wire.json')};
    delete config.name;
    writeFileSync(join(dir,'input.json'),JSON.stringify(config,null,2)+'\n',{flag:'wx'});
    writeFileSync(join(dir,'stdin.bin'),'',{flag:'wx'});
    const disk=snapshot(runtime), fd=openSync(join(dir,'stdin.bin'),'r');
    let result;
    try{result=spawnSync(process.execPath,[resolve(process.argv[1]),'--child',join(dir,'input.json')],{cwd:join(runtime,'project'),env,stdio:[fd,'pipe','pipe'],timeout:5000});}finally{closeSync(fd);}
    writeFileSync(join(dir,'stdout.bin'),result.stdout,{flag:'wx'});writeFileSync(join(dir,'stderr.bin'),result.stderr,{flag:'wx'});
    assert.equal(result.error,undefined,`#1097 bounded function probe ${d.name}`);
    assert.equal(result.signal,null,`#1097 function exited ${d.name}`);
    assert.deepEqual(snapshot(runtime),disk,`#1097 function disk unchanged ${d.name}`);
    const wire=JSON.parse(readFileSync(config.wire,'utf8'));
    cases.push({input,source:'Node runSdkCli function + in-process WebSocket double (not D0 CLI)',stdout:result.stdout.toString(),stderrRaw:result.stderr.toString(),exit:result.status,signal:result.signal,connections:wire.filter(r=>r.kind==='connection').length,wire,disk});
    console.log(JSON.stringify({id:input.id,exit:result.status,stdout:result.stdout.toString(),stderr:result.stderr.toString(),connections:cases.at(-1).connections}));
  }
  const sourceFiles=['src/cli/sdkCli.ts','dist/src/cli/sdkCli.js','packages/sdk/src/client.ts','packages/sdk/dist/client.js','packages/sdk/src/transport.ts','packages/sdk/dist/transport.js'];
  writeFileSync(join(output,'supplemental.json'),JSON.stringify({nodeVersion:process.version,kind:'real-ts-function-probe-in-process-wire',sourceSha256:Object.fromEntries(sourceFiles.map(file=>[file,sha(readFileSync(join(baseline,file)))])),cases},null,2)+'\n',{flag:'wx'});
}
