// Default native provider + real WS encoder oracle. No listener, HTTP, or model.
// node --import <B>/node_modules/tsx/dist/loader.mjs <this> <B> <output> <scratch>
import {mkdir,writeFile,readFile,stat,readdir} from 'node:fs/promises';
import {resolve,join,relative} from 'node:path';
import {pathToFileURL} from 'node:url';
import {spawn,execFileSync} from 'node:child_process';
import {once} from 'node:events';
const baseline=resolve(process.argv[2]),output=resolve(process.argv[3]),scratch=resolve(process.argv[4]);
if(process.versions.node.split('.')[0]!=='22')throw Error('Node 22 required');
for(const k of ['HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME','TMPDIR'])if(!process.env[k]?.startsWith(scratch))throw Error(`Unisolated ${k}`);
if(process.env.SKIP_BROWSER_OPEN!=='1'||process.env.PILOTDECK_SKIP_BROWSER_OPEN!=='1')throw Error('Browser guards missing');
const source=(p:string)=>import(pathToFileURL(join(baseline,'src',p)).href);
const {GatewaySessionModelBundle}=await source('cli/GatewaySessionModelBundle.ts');
const {NativeSessionModelSelectionPort}=await source('model/session/NativeSessionModelSelectionPort.ts');
const {NativeSessionModelSelectionPolicy}=await source('model/session/NativeSessionModelSelectionPolicy.ts');
const {GatewayDialogBundle}=await source('cli/GatewayDialogBundle.ts');
const {createProjectSessionDataPlane}=await source('session/storage/ProjectSessionDataPlane.ts');
const {GatewayWsConnection}=await source('gateway/server/GatewayWsConnection.ts');
const {getPilotProjectChatDir}=await source('pilot/paths.ts');
const {sanitizeSessionIdForPath}=await source('session/storage/ProjectSessionStorage.ts');
const {loadPilotConfig}=await source('pilot/config/loadPilotConfig.ts');
const {listModelCatalog}=await source('gateway/dialog/modelCatalog.ts');
const base='schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: https://example.invalid/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n        alternate: {}\n';
const reasoning=base.replace('deterministic: {}','deterministic: {thinking: {state: enabled, efforts: [low, high]}, capabilities: {supportsSpeed: true}}').replace('      models:','      speedMapping: openai_service_tier\n      models:');
const entry=(type:string,extra:any={},sequence=1,createdAt='2026-10-15T00:00:00.000Z')=>({type,sessionId:'web:fixture',turnId:'turn',sequence,createdAt,...extra});
const selected=(s:any)=>entry('session_metadata',{metadata:{modelSelection:s}});
const explicit={mode:'model',provider:'parity',model:'deterministic'};
const msg={role:'user',content:[{type:'text',text:'hello'}]};
const complete={usage:{},permissionDenials:[]};
const cases:any[]=[];const add=(id:string,extra:any={})=>cases.push({id,...extra});
add('missing-session');add('general-project',{general:true});add('relative-project',{relative:true});add('unknown-project',{unknown:true});
add('params-missing',{omitParams:true});add('params-null',{params:null});add('project-missing',{params:{sessionKey:'web:x'}});add('project-number',{projectValue:7});add('project-null',{projectValue:null});
for(const v of ['', ' \n\uFEFF ',null,7,{},[]])add('session-'+JSON.stringify(v),{sessionValue:v});add('session-missing',{omitSession:true});add('session-path-sanitization',{sessionValue:'///web:folder\\sub---'});
for(const s of [explicit,{...explicit,reasoning:0.8,speed:1,effort:'retired',extra:7},{...explicit,reasoning:0.9,speed:0.3},{...explicit,reasoning:null},{...explicit,provider:'retired',model:'missing',reasoning:0.8},{...explicit,provider:'retired',model:'missing',speed:42},{mode:'auto',reasoning:0.8,speed:1},{mode:'old',extra:7},7,true,'text',[],{}])add('saved-'+cases.length,{entries:[selected(s)],yaml:reasoning});
add('saved-explicit-bypasses-invalid-global',{yaml:'bad: [',entries:[selected(explicit)]});add('saved-reasoning-loads-invalid-global',{yaml:'bad: [',entries:[selected({...explicit,reasoning:0.8})]});
add('saved-auto-loads-project',{projectYaml:'agent: {model: parity/alternate}\n',entries:[selected({mode:'auto'})]});
add('clear-selection',{entries:[selected(explicit),entry('session_metadata',{metadata:{modelSelection:null}},2)]});
add('metadata-merge-no-selection',{entries:[selected(explicit),entry('session_metadata',{metadata:{title:'hello'}},2)]});
add('accepted-selection',{entries:[entry('accepted_input',{messages:[msg],metadata:{modelSelection:explicit}})]});
add('accepted-invalid-selection-ignored',{entries:[selected(explicit),entry('accepted_input',{messages:[],metadata:{modelSelection:{mode:'model',provider:7}}},2)]});
add('snapshot-does-not-clear-selection',{entries:[selected(explicit),entry('session_metadata',{metadata:{isSnapshot:true,title:'snapshot'}},2)]});
add('out-of-order-sequence',{entries:[entry('session_metadata',{metadata:{modelSelection:{mode:'auto'}}},2),selected(explicit)]});
add('equal-sequence-ascii',{entries:[entry('session_metadata',{metadata:{modelSelection:{mode:'auto'}}},1,'z'),entry('session_metadata',{metadata:{modelSelection:explicit}},1,'A')]});
add('equal-sequence-nonascii',{entries:[entry('session_metadata',{metadata:{modelSelection:{mode:'auto'}}},1,'中'),entry('session_metadata',{metadata:{modelSelection:explicit}},1,'国')],forward:true});
add('distinct-sequence-nonascii',{entries:[entry('session_metadata',{metadata:{modelSelection:explicit}},1,'中'),entry('session_metadata',{metadata:{title:'ok'}},2,'国')]});
add('stable-equal-timestamps',{entries:[selected(explicit),selected({mode:'auto'})]});
add('invalid-lines-and-shape',{transcript:'bad\nnull\n{}\n[]\n'+JSON.stringify(selected(explicit))+'\n{"type":"session_metadata"\n'});
add('truncated-final-line',{transcript:JSON.stringify(selected(explicit))+'\n{"type":"session_metadata","metadata":{"modelSelection":null'});
add('non-utf8-in-selection',{transcriptBytes:Buffer.from(JSON.stringify(selected({...explicit,model:'BYTES'}))).toString('base64'),replaceBytes:true});
add('non-utf8-line-skipped',{transcriptBytes:Buffer.concat([Buffer.from([255,128]),Buffer.from('\n'+JSON.stringify(selected(explicit))+'\n')]).toString('base64')});
add('crlf',{transcript:' \r\n'+JSON.stringify(selected(explicit))+'\r\n'});add('too-large',{tooLarge:true});add('transcript-directory',{transcriptDirectory:true});
for(const [type,extra] of [
 ['accepted_input',{}],['accepted_input',{messages:null}],['accepted_input',{messages:7}],['accepted_input',{messages:[null]}],['accepted_input',{messages:[7]}],['accepted_input',{messages:[{content:[null]}]}],['accepted_input',{messages:[{content:[{type:'tool_result'}]}]}],['accepted_input',{messages:[{content:[{type:'tool_result',content:7}]}]}],
 ['assistant_message',{}],['assistant_message',{message:null}],['assistant_message',{message:{content:[null]}}],['context_snapshot',{runtimeContextMessages:'text'}],
 ['turn_result',{}],['turn_result',{result:{}}],['turn_result',{result:{permissionDenials:[]}}],['turn_result',{result:{usage:null,permissionDenials:[]}}],['turn_result',{result:{usage:{},permissionDenials:null}}],['turn_result',{result:{usage:{},permissionDenials:[] ,errors:7}}],
 ['session_metadata',{}],['session_metadata',{metadata:null}],['session_metadata',{metadata:7}],['control_boundary',{}],['control_boundary',{boundary:null}],['control_boundary',{boundary:7}],['control_boundary',{boundary:{kind:'compact',subtype:'compact_boundary',replacementMessages:7}}],
] as any[])add('corrupt-content-'+cases.length,{entries:[entry(type,extra),entry('turn_result',{result:complete},2)],yaml:'bad: ['});
add('incomplete-message-skips-clone',{entries:[entry('assistant_message',{message:{content:[null]}})]});
add('compact-valid',{entries:[entry('accepted_input',{}),entry('control_boundary',{boundary:{kind:'compact',subtype:'compact_boundary',snapshot:{version:1,messages:[msg]}}},2),entry('session_metadata',{metadata:{modelSelection:explicit}},3)]});
add('compact-invalid-snapshot-keeps-prior-errors',{entries:[entry('accepted_input',{}),entry('control_boundary',{boundary:{kind:'compact',subtype:'compact_boundary',snapshot:{version:1,messages:[null]}}},2)]});
add('summary-before-boundary-still-throws',{entries:[entry('turn_result',{result:{} }),entry('control_boundary',{boundary:{kind:'compact',subtype:'compact_boundary',snapshot:{version:1,messages:[msg]}}},2)]});
for(const block of [
 {type:'tool_result',toolCallId:'t',content:[{type:{toString:7}}]},
 {type:'media_reference',path:'p',originalBytes:1,preview:'p',hasMore:false,mimeType:'x',mediaType:{toString:7}},
 {type:'media_reference',path:'p',originalBytes:1,preview:'p',hasMore:false,mimeType:'x',mediaType:[{toString:7}]},
])add('snapshot-string-coercion-'+cases.length,{entries:[entry('control_boundary',{boundary:{kind:'compact',subtype:'compact_boundary',snapshot:{version:1,messages:[{role:'user',content:[block]}]}}})]});
add('context-invalid-user-text',{entries:[entry('accepted_input',{messages:[{role:'user',content:[{type:'text',text:7}]}]}),entry('context_snapshot',{runtimeContextMessages:[msg]},2),entry('turn_result',{result:complete},3)]});
add('project-model',{projectYaml:'agent: {model: parity/alternate}\n'});add('project-env-wins',{projectYaml:'agent: {model: parity/alternate}\n',env:{PILOT_AGENT_MODEL:'parity/deterministic'}});
add('env-invalid-model',{env:{PILOT_AGENT_MODEL:'missing/model'}});add('env-empty-model',{env:{PILOT_AGENT_MODEL:''},projectYaml:'agent: {model: parity/alternate}\n'});
add('project-yaml-invalid',{projectYaml:'bad: ['});add('project-root-invalid',{projectYaml:'[]\n'});add('global-and-project-invalid',{yaml:'bad: [',projectYaml:'[]\n'});add('invalid-unrelated-section',{projectYaml:'memory: {maxMessageChars: -1}\n'});
add('project-replaces-agent',{projectYaml:'agent: 7\n'});add('project-env-repairs-agent',{projectYaml:'agent: 7\n',env:{PILOT_AGENT_MODEL:'parity/deterministic'}});
add('project-router',{projectYaml:'router: {enabled: true, scenarios: {default: parity/deterministic}, fallback: {default: parity/alternate}}\n'});
add('catalog-ignores-project',{method:'model_catalog_list',projectYaml:'bad: ['});
const staff={STAFFDECK_PROJECT_BINDING_ENABLED:'true',STAFFDECK_FORMAL_API_ORIGIN:'https://staffdeck.invalid/',STAFFDECK_COPY_TENANT_ID:'tenant',STAFFDECK_SOP_RUNTIME_ORIGIN:'https://runtime.invalid',STAFFDECK_SOP_MANAGEMENT_API_KEY:'mock-only'};
for(const status of ['pending','failed','bound','binding','other'])for(const ready of [false,true])add(`binding-${status}-${ready}`,{env:staff,binding:{status,runtimeReady:ready},lock:'alive',sops:{sops:[{id:'sop'}]}});
add('binding-live-invalid-endpoint',{env:{...staff,STAFFDECK_SOP_RUNTIME_ORIGIN:'bad'},binding:{status:'binding',runtimeReady:true},lock:'alive',sops:{sops:[{id:'sop'}]}});
add('binding-dead-invalid-endpoint',{env:{...staff,STAFFDECK_SOP_RUNTIME_ORIGIN:'bad'},binding:{status:'binding',runtimeReady:true},lock:'dead',sops:{sops:[{id:'sop'}]}});
for(const sops of [undefined,{sops:[]},{sops:[{skill_id:'sop'}]},{sops:[{id:0}]},null,7,'bad: ['])add('binding-sops-'+cases.length,{env:staff,binding:{status:'bound'},sops});
add('binding-origin-mismatch',{env:staff,binding:{status:'bound',origin:'https://other.invalid'},sops:'bad: ['});add('binding-tenant-mismatch',{env:staff,binding:{status:'bound',tenantId:'other'},sops:'bad: ['});
add('binding-disabled',{env:{...staff,STAFFDECK_PROJECT_BINDING_ENABLED:'TRUE'},bindingStore:'bad'});add('binding-missing-store',{env:staff});
for(const store of ['bad',null,{}, {version:1,installationId:'i',projects:[null]}, {version:2,installationId:'i',projects:[]}])add('binding-invalid-store-'+cases.length,{env:staff,bindingStore:store});
add('binding-catalog-ignores-store',{method:'model_catalog_list',env:staff,bindingStore:'bad'});
add('binding-saved-explicit-bypasses-store',{env:staff,bindingStore:'bad',entries:[selected(explicit)]});add('binding-saved-reasoning-global-only',{env:staff,bindingStore:'bad',yaml:reasoning,entries:[selected({...explicit,reasoning:0.8})]});
for(const agentId of [undefined,7,{toString:7},'\uD800','a /中'])add('binding-agent-id-'+cases.length,{env:staff,binding:{status:'bound',agentId},sops:{sops:[{id:'sop'}]}});
for(const bindingId of [7,null])add('binding-invalid-path-'+cases.length,{env:staff,binding:{status:'bound',bindingId}});
for(const timeouts of [{timeoutMs:42,discoveryTimeoutMs:43},{timeoutMs:-1,discoveryTimeoutMs:'5'},{timeoutMs:1.5,discoveryTimeoutMs:9007199254740992}])add('binding-timeouts-'+cases.length,{env:staff,binding:{status:'bound'},sops:{sops:[{id:'sop'}]},projectYaml:JSON.stringify({modules:{sop:timeouts}})});
const child=spawn(process.execPath,['-e','process.stdin.resume();console.log("ready");'],{env:process.env,stdio:['pipe','pipe','pipe']});await once(child.stdout!,'data');
const pid=child.pid!;
const lockContents=['', ' \n\t ', '0','-0','-1','1.5','Infinity','+Infinity','-Infinity','NaN','invalid','12oops','0x','0o','0b','0xGG','+0x1','0o8','0b2','1e999','9007199254740992','2147483648','4294967296',String(4294967296+pid),String(pid),' \n\t'+pid+'\uFEFF\n','0x'+pid.toString(16),'0o'+pid.toString(8),'0b'+pid.toString(2),pid+'e0',pid+'.000','+'+pid,'00'+pid,String(pid-0.1),'missing','directory','bytes','eperm'];
for(const [i,lock] of lockContents.entries())add('lock-number-'+i,{env:{...staff,STAFFDECK_SOP_RUNTIME_ORIGIN:'bad'},binding:{status:'binding',runtimeReady:true},lock,sops:{sops:[{id:'sop'}]}});
// Regression #108: retained cycles are inspected after source merging.
const toolsModule='enabled: true, implementationId: fixture.tools, contract: pilotdeck.tools/v1, transport: module-http-v2, endpoint: https://tools.invalid';
for(const [path,global,overrides] of [
 ['provider',base.replace('      models:', '      extraBody: &b {self: *b}\n      models:'),[
  ['covered','model: {providers: {parity: {extraBody: 0}}}'],
  ['retained','agent: {model: parity/alternate}'],
  ['partial-retained','model: {providers: {parity: {extraBody: {other: 0}}}}'],
  ['partial-covered','model: {providers: {parity: {extraBody: {self: 0}}}}'],
 ]],
 ['adapter',base+'adapters: {telegram: {extra: &b {self: *b}}}\n',[
  ['covered','adapters: {telegram: {extra: 0}}'],
  ['retained','agent: {model: parity/alternate}'],
  ['partial-retained','adapters: {telegram: {extra: {other: 0}}}'],
  ['partial-covered','adapters: {telegram: {extra: {self: 0}}}'],
 ]],
 ['catalog',base+`modules: {tools: {${toolsModule}, catalog: [{name: fixture, description: fixture, inputSchema: &b {self: *b}}]}}\n`,[
  ['covered','modules: {tools: {catalog: [{name: fixture, description: fixture, inputSchema: {type: object}}]}}'],
  ['retained','agent: {model: parity/alternate}'],
  ['partial-retained','modules: {tools: {endpoint: https://alternate.invalid}}'],
 ]],
] as any[])for(const [kind,projectYaml] of overrides)add(`cycle-merge-${path}-${kind}`,{yaml:global,projectYaml});
add('cycle-merge-both-recursive',{yaml:base.replace('      models:', '      extraBody: &b {self: *b}\n      models:'),projectYaml:'model: {providers: {parity: {extraBody: &c {self: *c}}}}'});
add('cycle-merge-unused',{yaml:base+'unused: &b {self: *b}\n',projectYaml:'agent: {model: parity/alternate}'});
add('cycle-merge-prototype-host',{yaml:base+'unused: &b {self: *b}\n',projectYaml:'__proto__: {unused: 0}',forward:true,loadForward:true});
add('session-nul-general',{general:true,sessionValue:'web:\0',generalMarker:true});
add('session-nul-registered',{sessionValue:'web:\0'});
add('project-nul',{projectNul:true});
add('config-path-nul',{env:{PILOTDECK_CONFIG_PATH:'bad\0.yaml'}});
add('binding-nul-path',{env:staff,binding:{status:'bound',bindingId:'b\0'},sops:{sops:[{id:'sop'}]}});
cases.sort((a,b)=>Number(a.lock==='dead')-Number(b.lock==='dead'));
const results:any[]=[];let routerCalls=0;let dead=false;
try{
 for(const test of cases){
  const root=join(scratch,'cases',String(results.length));const home=join(root,'home'),project=join(root,'project');await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});await mkdir(join(home,'projects','registered','chats'),{recursive:true});await writeFile(join(home,'projects','registered','.cwd'),(test.generalMarker?home:project)+'\n');
  const env={PILOT_HOME:home,...test.env};await writeFile(join(home,'pilotdeck.yaml'),test.yaml??base);
  if(test.projectYaml!==undefined){await mkdir(join(project,'.pilotdeck'),{recursive:true});await writeFile(join(project,'.pilotdeck','pilotdeck.yaml'),test.projectYaml);}
  if(test.binding||'bindingStore' in test){await mkdir(join(home,'staffdeck'),{recursive:true});const store='bindingStore' in test?test.bindingStore:{version:1,installationId:'i',projects:[{projectRoot:project,bindingId:'b',agentId:'a',origin:'https://staffdeck.invalid',tenantId:'tenant',...test.binding}]};await writeFile(join(home,'staffdeck','project-bindings.json'),typeof store==='string'?store:JSON.stringify(store));}
  if(test.sops!==undefined){await mkdir(join(home,'staffdeck','b'),{recursive:true});await writeFile(join(home,'staffdeck','b','sops.json'),typeof test.sops==='string'?test.sops:JSON.stringify(test.sops));}
  if(test.lock==='dead'&&!dead){const exited=once(child,'exit');child.stdin!.end();await exited;dead=true;}
  // Lock-number cases requiring alive run with a second owned child below.
  if(test.id.startsWith('lock-number-')&&dead){throw Error('lock matrix must precede dead case');}
  if(test.lock&&test.lock!=='missing'){
   const path=join(home,'staffdeck','project-bindings.json.lock');
   if(test.lock==='directory')await mkdir(path);else await writeFile(path,test.lock==='alive'||test.lock==='dead'||test.lock==='eperm'?String(pid):test.lock==='bytes'?Buffer.from([255,128]):test.lock);
  }
  const projectKey=test.general?home:test.projectNul?project+'\0':test.unknown?join(root,'unknown'):'projectValue'in test?test.projectValue:test.relative?relative(process.cwd(),project):project;
  const sessionKey='sessionValue'in test?test.sessionValue:'web:fixture';
  const params='params'in test?test.params:{projectKey,...(test.omitSession?{}:{sessionKey})};
  const transcript=join(getPilotProjectChatDir(test.general?home:project,home),sanitizeSessionIdForPath(typeof sessionKey==='string'?sessionKey:'web:fixture')+'.jsonl');if(!transcript.includes('\0'))await mkdir(resolve(transcript,'..'),{recursive:true});
  if(test.transcriptDirectory)await mkdir(transcript);else if(test.tooLarge)await writeFile(transcript,Buffer.alloc(50*1024*1024+1,120));
  else if(test.entries||test.transcript||test.transcriptBytes){let bytes=test.transcriptBytes?Buffer.from(test.transcriptBytes,'base64'):Buffer.from(test.transcript??test.entries.map((e:any)=>JSON.stringify(e)).join('\n')+'\n');if(test.replaceBytes)bytes=Buffer.concat([bytes.subarray(0,bytes.indexOf('BYTES')),Buffer.from([255,254,128]),bytes.subarray(bytes.indexOf('BYTES')+5)]);await writeFile(transcript,bytes);}
  const plane=createProjectSessionDataPlane();const dialog=new GatewayDialogBundle({pilotHome:home,sessionCatalog:plane.catalog});
  const bundle=new GatewaySessionModelBundle({fallbackProjectKey:home,resolveProjectKey:dialog.projects.resolveProjectKey,router:{hasActiveTurn(){routerCalls++;throw Error('runtime touched');},async close(){routerCalls++;throw Error('runtime touched');}},selectionPort:new NativeSessionModelSelectionPort({pilotHome:home,now:()=>new Date(),storageProvider:plane.persistence}),policy:new NativeSessionModelSelectionPolicy(env)});
  let receive:any,finish:any;const responses:string[]=[];const ws={onMessage(fn:any){receive=fn},onClose(){},sendText(s:string){if(JSON.parse(s).type==='response'){responses.push(s);finish(s);}},close(){throw Error('Unexpected close');}};
  new GatewayWsConnection(ws as any,{gateway:{describeServer:async()=>({}),sessionModelGet:(p:any)=>bundle.sessionModelGet(p),modelCatalogList:(p:any)=>listModelCatalog(p,env)} as any,token:'offline',serverVersion:'offline'});
  receive(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'fixture',token:'offline'}));await new Promise(r=>setImmediate(r));
  const request=JSON.stringify({type:'request',id:test.id,method:test.method??'session_model_get',...(test.omitParams?{}:{params})});
  const syscall=(process as any)._kill;const killCalls:number[]=[];
  (process as any)._kill=(pid:number,signal:number)=>{killCalls.push(pid);return test.lock==='eperm'?-1:syscall(pid,signal);};
  let load:any;try{load={ok:true,diagnostics:loadPilotConfig({projectRoot:typeof projectKey==='string'?projectKey:undefined,env}).diagnostics};}catch(e:any){load={ok:false,code:e.code??'gateway_request_failed',message:e.message,diagnostics:e.diagnostics??[]};}
  const loadKillCalls=killCalls.splice(0);
  let response:string;try{response=await new Promise<string>((done,reject)=>{const timer=setTimeout(()=>reject(Error('No response '+test.id)),5000);finish=(s:string)=>{clearTimeout(timer);done(s);};receive(request);});}finally{(process as any)._kill=syscall;}
  const files:any={};const collect=async(path:string)=>{const s=await stat(path);const fact:any={directory:s.isDirectory(),size:s.size};files[path]=fact;if(s.isDirectory()){fact.entries=await Promise.all((await readdir(path,{withFileTypes:true})).map(async e=>[e.name,e.isDirectory()]));for(const [name]of fact.entries)await collect(join(path,name));}else if(s.size<=50*1024*1024)fact.bytes=(await readFile(path)).toString('base64');};await collect(root);
  const replace=(s:string)=>s.replaceAll(root,'/fixture');
  for(const fact of Object.values(files) as any[]){if(fact.bytes){const b=Buffer.from(fact.bytes,'base64');const needle=Buffer.from(root),replacement=Buffer.from('/fixture');let parts:Buffer[]=[],start=0,at=0;while((at=b.indexOf(needle,start))>=0){parts.push(b.subarray(start,at),replacement);start=at+needle.length;}parts.push(b.subarray(start));fact.bytes=Buffer.concat(parts).toString('base64');}}
  const normalizedRequest=JSON.parse(replace(request));if(test.relative)normalizedRequest.params.projectKey='project';
  const invalidPath=transcript.includes('\0')?replace(transcript):undefined;
  if(invalidPath){
   // inspect truncates at 128 UTF-16 units. Path replacement and truncation do
   // not commute: retain the original WS frame, then ask real Node fs for the
   // error at the mapped path (validation rejects NUL before any syscall).
   await writeFile(join(root,'nul-wire-original.json'),JSON.stringify({request,response,path:transcript})+'\n');
   try{await stat(invalidPath);throw Error('NUL accepted');}catch(e:any){if(e.code!=='ERR_INVALID_ARG_VALUE')throw e;const frame=JSON.parse(response);frame.error={code:e.code,message:e.message};response=JSON.stringify(frame);}
  }
  results.push(JSON.parse(replace(JSON.stringify({id:test.id,home,cwd:'/fixture',env,request:JSON.stringify(normalizedRequest),response,load,files,invalidPath,process:{pid,alive:!dead,eperm:test.lock==='eperm',killCalls,loadKillCalls},forward:test.forward??false,loadForward:test.loadForward??false}))));
  console.log(test.id,JSON.parse(response).ok?'OK':JSON.parse(response).error.code);
 }
}finally{if(child.exitCode===null){const e=once(child,'exit');child.stdin!.end();await e;}}
await mkdir(resolve(output,'..'),{recursive:true});await writeFile(output,JSON.stringify({baselineCommit:execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),node:process.version,icu:process.versions.icu,routerCalls,cases:results},null,2)+'\n');
