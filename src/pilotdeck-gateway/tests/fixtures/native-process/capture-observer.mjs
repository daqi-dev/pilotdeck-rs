import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {readFileSync,writeFileSync,mkdirSync,realpathSync} from 'node:fs';
import {join,resolve,dirname,basename} from 'node:path';
import {createHash} from 'node:crypto';
const args=process.argv.slice(2);assert.equal(args.length,2);const base=realpathSync(resolve(args[0]));const requested=resolve(args[1]),out=join(realpathSync(dirname(requested)),basename(requested));assert.ok(out!==base&&!out.startsWith(base+'/'));mkdirSync(out,{mode:0o700});
assert.equal(process.version,'v22.22.0');assert.equal(process.versions.icu,'78.2');assert.equal(process.env.PDRS_VERIFIED_TS_SHA,'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const ts=createRequire(join(base,'package.json'))('typescript');assert.equal(ts.version,'5.9.3');const warnings=[];process.on('warning',w=>warnings.push({name:w.name,message:w.message}));
const file='src/agent/sub/OneShotSubagentPort.ts',raw=readFileSync(join(base,file),'utf8'),tree=ts.createSourceFile(file,raw,ts.ScriptTarget.ES2022,true),spans={};
const wanted=['launchObserverSubagent','buildObserverActivityDigest','observerActivityLine','truncateObserverText'];const found={},constantNodes=[],activity=[];
function visit(node){
 if(ts.isFunctionDeclaration(node)&&wanted.includes(node.name?.text)){assert.equal(found[node.name.text],undefined);found[node.name.text]=node;}
 if(ts.isVariableStatement(node)&&node.declarationList.declarations.some(d=>d.name.getText(tree)==='OBSERVER_MAX_ACTIVITY_EVENTS'||d.name.getText(tree)==='OBSERVER_MAX_TEXT_CHARS'))constantNodes.push(node);
 if(ts.isPropertyAssignment(node)&&node.name.getText(tree)==='onActivity'&&ts.isArrowFunction(node.initializer))activity.push(node.initializer);
 ts.forEachChild(node,visit);
}visit(tree);assert.equal(Object.keys(found).length,4);assert.equal(constantNodes.length,2);assert.equal(activity.length,1);
const hash=v=>createHash('sha256').update(v).digest('hex');function text(name,node){const v=node.getText(tree);spans[name]={start:node.getStart(tree),end:node.end,line:tree.getLineAndCharacterOfPosition(node.getStart(tree)).line+1,sha256:hash(v)};return v;}
const constants=constantNodes.map((n,i)=>text('constant-'+i,n)).join('\n');const functions=wanted.map(n=>text(n,found[n])).join('\n');const arrow=text('onActivity',activity[0]);
const code=constants+'\n'+functions+'\nfunction createCollector(observerActivity){return '+arrow+';}';
const js=ts.transpileModule(code,{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022,esModuleInterop:true}}).outputText;
const original=new Function('randomUUID',js+'\nreturn {launchObserverSubagent,buildObserverActivityDigest,createCollector};')(()=> 'observer-random-fixture');
const definition={id:'watcher',description:'Watcher description',allowedTools:['*'],disallowedTools:['x'],isReadOnly:false,permissionMode:'bypassPermissions',mcpServers:{secret:{command:'must-not-run'}},skills:['secret'],memory:'inherit',initialPrompt:'must-not-inject',background:true,observer:'recursive',observerMessage:'recursive-message',systemPromptSuffix:'fixture observer prompt',extra:{retained:true}};
const all=[{type:'model_request_started',provider:'fixture',model:'mock'},{type:'tool_calls_detected',calls:[{name:'read_file'},{name:'grep'}]},{type:'tool_result',result:{toolName:'read_file',type:'success'}},{type:'tool_result',result:{toolName:'grep',type:'error'}},{type:'assistant_message',content:'body not included'},{type:'warning',code:'warn-code',message:'private detail excluded'},{type:'agent_status',event:'running'},{type:'text_delta',text:'not included'}];
const cases=[
 {id:'success-empty'},
 {id:'all-events',activity:all,outcome:{success:false,error:'observed failure'}},
 {id:'empty-tool-calls',activity:[{type:'tool_calls_detected',calls:[]},{type:'tool_calls_detected',calls:[{name:''}]}]},
 {id:'directive-truncated',directive:'d'.repeat(2001)},
 {id:'report-truncated',outcome:{success:true,markdown:'m'.repeat(4001)}},
 {id:'error-truncated',outcome:{success:false,error:'e'.repeat(1001)}},
 {id:'activity-warnings-truncated',activity:[{type:'warning',code:'w'.repeat(121)},{type:'agent_status',event:'s'.repeat(121)}]},
 {id:'first-64-before-filter',activity:[...Array.from({length:64},()=>({type:'text_delta',text:'ignored'})),{type:'warning',code:'must-not-enter'}]},
 {id:'first-64-recognized',activity:Array.from({length:65},(_,i)=>({type:'warning',code:'warning-'+i}))},
 {id:'outer-truncation',activity:Array.from({length:64},()=>({type:'model_request_started',provider:'p'.repeat(500),model:'m'}))},
 {id:'unicode-js-trim',directive:'\uFEFF  directive \uFEFF',observerMessage:'\uFEFF note \uFEFF',outcome:{success:true,markdown:'\uFEFF report \uFEFF'}},
 {id:'nel-is-not-js-trim',directive:'\u0085directive\u0085',observerMessage:'\u0085note\u0085'},
 {id:'astral-slice',directive:'x'.repeat(1927)+'\u{1f600}'+'x'.repeat(80)},
 {id:'observer-fork-failure',forkError:'observer failed'},
 {id:'observer-nonerror-failure',forkNonError:'raw failure'},
 {id:'emitter-success-throws',emitFailure:'first'},
 {id:'emitter-always-throws',emitFailure:'all'},
 {id:'launcher-missing',missingLauncher:true},
 {id:'launcher-rejects',launchFailure:'launcher rejected'},
 {id:'no-emitter',noEmitter:true},
 {id:'uuid-fallback',noUuid:true},
 {id:'negative-clock',negativeClock:true},
 {id:'sparse-definition',sparseDefinition:true},
];
// Boundary sizes are fixed solely from the real TS digest before Rust exists.
const digestBase={observedDefinition:{id:'research'},directive:'Observed directive',activity:[],outcome:{success:true,markdown:'Observed completed report'}};
const plain=original.buildObserverActivityDigest(digestBase),messageSize=12000-plain.length-2;
cases.push({id:'outer-at-limit',observerMessage:'x'.repeat(messageSize)},{id:'outer-over-limit',observerMessage:'x'.repeat(messageSize+1)},{id:'outer-astral-cut',observerMessage:'x'.repeat(12000-72-plain.length-3)+'\u{1f600}'+'x'.repeat(100)});
const trailing={type:'model_request_started',provider:'',model:''};
const tailBase=original.buildObserverActivityDigest({...digestBase,activity:[trailing],outcome:{success:true,markdown:'   '}});
cases.push({id:'outer-trailing-empty-report',activity:[{...trailing,provider:'p'.repeat(12000-tailBase.length)}],outcome:{success:true,markdown:'   '}});
const rows=[];
for(const spec of cases){
 const events=[],forkCalls=[],launches=[],collected=[],capture=original.createCollector(collected);for(const event of (spec.activity??[]))capture(event);
 const outcome=spec.outcome??{success:true,markdown:'Observed completed report'};let clock=0,emit=0;
 const emitter=event=>{events.push(event);emit++;if(spec.emitFailure==='all'||(spec.emitFailure==='first'&&emit===1))throw Error('emitter failure');};
 const deps={now:()=>new Date(spec.negativeClock?1000-100*(clock++):1000+10*(clock++)),...(spec.noUuid?{}:{uuid:()=> 'observer-fixture'}),...(spec.noEmitter?{}:{eventEmitter:emitter})};
 if(!spec.missingLauncher)deps.observerSubagents={launch:async task=>{launches.push({sessionId:task.sessionId,turnId:task.turnId,observedSubagentId:task.observedSubagentId,observerSubagentId:task.observerSubagentId,observerSubagentType:task.observerSubagentType});if(spec.launchFailure)throw Error(spec.launchFailure);await task.run(new AbortController().signal);}};
 const currentDefinition=spec.sparseDefinition?{id:definition.id,description:definition.description}:definition;
 const before=JSON.stringify(currentDefinition);
 const input={options:{sessionId:'parent-session',turnId:'parent-turn',dependencies:deps},api:{fork:async request=>{const {abortSignal,...safe}=request;forkCalls.push({...safe,abortSignalPresent:abortSignal instanceof AbortSignal,definitionOwn:Object.keys(request.definitionOverride).map(key=>({key,isUndefined:request.definitionOverride[key]===undefined}))});if(spec.forkError)throw Error(spec.forkError);if(spec.forkNonError)throw spec.forkNonError;return {markdown:'Observer report: fixture'};}},definitions:{},observedDefinition:{id:'research'},observerDefinition:currentDefinition,observedSubagentId:'observed-child',activity:collected,directive:spec.directive??'Observed directive',observerMessage:spec.observerMessage,timeoutMs:1234,outcome};
 let output;try{await original.launchObserverSubagent(input);output={ok:true};}catch(e){output={ok:false,error:{name:e instanceof Error?e.name:'non-Error',message:e instanceof Error?e.message:String(e)}};}
 assert.equal(JSON.stringify(currentDefinition),before);rows.push({id:spec.id,input:{...spec,observerDefinition:currentDefinition,outcome,timeoutMs:1234},collectedCount:collected.length,digest:original.buildObserverActivityDigest({...input,observedDefinition:{id:'research'}}),launches,forkCalls,events,clockCalls:clock,output});
}
await new Promise(r=>setImmediate(r));assert.deepEqual(warnings,[]);
writeFileSync(join(out,'oracle.json'),JSON.stringify({metadata:{sourceCommit:process.env.PDRS_VERIFIED_TS_SHA,node:process.version,icu:process.versions.icu,typescript:ts.version,sourceSha256:{[file]:hash(raw)},extraction:spans,method:'exact AST function+constant+collector text transpiled in memory; actual launchObserverSubagent with injected launcher/fork/emitter/clock/uuid seams, no actual child/provider',normalization:'none; injected deterministic UUID/clock before capture, error name/message and own undefined slots explicitly projected'},rows},null,2)+'\n',{flag:'wx'});console.log(JSON.stringify({cases:rows.length}));
