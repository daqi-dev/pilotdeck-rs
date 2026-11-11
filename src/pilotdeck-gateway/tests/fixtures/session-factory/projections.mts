import assert from 'node:assert/strict';
export async function observations(base:string) {
 const {projectAgentTranscriptEntries,projectSubagentReferences}=await import(`${base}/src/session/projection/AgentTranscriptProjections.ts`);
 const {AgentLoop}=await import(`${base}/src/agent/loop/AgentLoop.ts`);
 const message=(text:string,role='user',metadata?:any)=>({role,content:[{type:'text',text}],...(metadata?{metadata}:{})});
 const result=(turnId:string,usage:any={},permissionDenials:any=[],errors?:any)=>({type:'turn_result',turnId,result:{type:'success',sessionId:'session-745',turnId,usage,permissionDenials,...(errors===undefined?{}:{errors})}});
 const inputs:any[]=[
  {name:'null-usage',entries:[result('A',null)]},
  {name:'dynamic-usage',entries:[result('A',{inputTokens:null,outputTokens:'4',cacheReadTokens:true,cacheWriteTokens:[],totalTokens:{}}),result('B',{outputTokens:2})]},
  {name:'primitive-metadata',entries:[{type:'session_metadata',metadata:'text'}]},
  {name:'null-metadata',entries:[{type:'session_metadata',metadata:null}]},
  {name:'incomplete',entries:[{type:'accepted_input',turnId:'A',messages:[message('A')]},{type:'assistant_message',turnId:'A',message:message('hidden','assistant')},{type:'accepted_input',turnId:'B',messages:[message('B')]},{type:'durable_message',turnId:'B',message:message('visible','assistant')},result('B')]},
  {name:'usage',entries:[result('A',{inputTokens:2,totalTokens:4,nativeCost:99},[{toolName:'read',toolCallId:'one'}]),result('B',{inputTokens:3,outputTokens:7,cacheReadTokens:1},[{toolName:'read',toolCallId:'one'}])]},
  {name:'metadata',entries:[{type:'session_metadata',metadata:{title:' 原题 ',firstPrompt:' 中文 ',linkedPullRequest:{number:7,url:'https://example.test/7'},extra:'kept'}},{type:'session_metadata',metadata:{title:null,linkedPullRequest:null,aiTitle:null}},{type:'accepted_input',messages:[],metadata:{modelSelection:{mode:'auto'}}}]},
  {name:'compact',entries:[{type:'accepted_input',messages:[message('before')]},{type:'control_boundary',boundary:{kind:'compact',subtype:'compact_boundary',snapshot:{version:1,messages:[message('retained')]}}},{type:'accepted_input',turnId:'B',messages:[message('after')]}]},
  {name:'legacy-pending',entries:[{type:'accepted_input',messages:[message('before')]},{type:'control_boundary',boundary:{kind:'compact',subtype:'compact_boundary',replacementMessages:[message('pending')]}}]},
  {name:'invalid-compact',entries:[{type:'accepted_input',messages:[message('keep')]},{type:'control_boundary',boundary:{kind:'compact',subtype:'compact_boundary',snapshot:{version:1,messages:[{role:'user',content:[{type:'text',text:7}]}]}}}]},
  {name:'clone-content',entries:[{type:'accepted_input',messages:[{role:'user',content:'not-an-array'}]}]},
  {name:'js-trim',entries:[{type:'accepted_input',messages:[message('first'),message('\u0085')]},{type:'context_snapshot',runtimeContextMessages:[message('context','assistant')]},result('A')]},
  {name:'context',entries:[{type:'accepted_input',messages:[message('request'),message('<hook_context>injected'),message('synthetic','user',{synthetic:true})]},{type:'context_snapshot',runtimeContextMessages:[message('context','assistant')]},result('A')]},
 ];
 const projections=inputs.map(({name,entries})=>{
  entries=entries.map((e:any,i:number)=>({version:1,entryId:`entry-${i+1}`,parentEntryId:i?`entry-${i}`:null,sequence:i+1,sessionId:'session-745',turnId:'A',createdAt:'2026-10-28T00:00:00.000Z',...e}));
  try {
   const r=projectAgentTranscriptEntries(entries);
   return {name,entries,state:{messages:r.messages,usage:r.usage,permissionDenials:r.permissionDenials,metadata:r.metadata}};
  } catch(error:any) { return {name,entries,error:{name:error.name,message:error.message}}; }
 });
 // #2042: the subagent-references cell (AgentTranscriptProjections.ts:380-456).
 // The two indexes are JS Maps, so each row records the key's `typeof` plus the
 // key itself (omitted for `undefined`, which JSON cannot carry).
 const subagentEntry=(i:number,extra:any)=>({version:1,entryId:`subagent-${i}`,parentEntryId:null,sequence:i,sessionId:'session-745',turnId:'A',createdAt:'2026-10-28T00:00:00.000Z',...extra});
 const index=(map:Map<any,any>)=>[...map].map(([key,value]:any)=>({keyType:typeof key,...(key===undefined?{}:{key}),entry:value}));
 const subagentCells:any[]=[
  ['normal',[subagentEntry(1,{type:'subagent_started',subagentId:'sub/a',subagentType:'explore',transcriptRelativePath:'subagents/sub-a.jsonl'}),subagentEntry(2,{type:'subagent_completed',subagentId:'sub/a',status:'ok'})]],
  ['dangling',[subagentEntry(1,{type:'subagent_started',subagentId:'sub/x'})]],
  ['duplicate-started',[subagentEntry(1,{type:'subagent_started',subagentId:'sub/a',subagentType:'first'}),subagentEntry(2,{type:'subagent_started',subagentId:'sub/a',subagentType:'second'})]],
  ['duplicate-completed',[subagentEntry(1,{type:'subagent_started',subagentId:'sub/a'}),subagentEntry(2,{type:'subagent_completed',subagentId:'sub/a',status:'first'}),subagentEntry(3,{type:'subagent_completed',subagentId:'sub/a',status:'second'})]],
  ['missing-subagent-id',[subagentEntry(1,{type:'subagent_started',subagentType:'first'}),subagentEntry(2,{type:'subagent_started',subagentType:'second'}),subagentEntry(3,{type:'subagent_completed'})]],
  ['null-subagent-id',[subagentEntry(1,{type:'subagent_started',subagentId:null}),subagentEntry(2,{type:'subagent_completed',subagentId:null})]],
  ['mixed-key-types',[subagentEntry(1,{type:'subagent_started',subagentId:1}),subagentEntry(2,{type:'subagent_started',subagentId:'1'})]],
  ['object-key',[subagentEntry(1,{type:'subagent_started',subagentId:{id:'a'}}),subagentEntry(2,{type:'subagent_started',subagentId:{id:'a'}})]],
  ['errored',[subagentEntry(1,{type:'subagent_started',subagentId:'sub/a',errored:true}),subagentEntry(2,{type:'subagent_completed',subagentId:'sub/a',errored:'yes'})]],
  ['other-entries',[subagentEntry(1,{type:'accepted_input',messages:[]}),subagentEntry(2,{type:'subagent_started',subagentId:'sub/a'}),subagentEntry(3,{type:'turn_result',turnId:'A',result:{type:'success',sessionId:'session-745',turnId:'A',usage:{},permissionDenials:[]}})]],
  ['primitive-entries',[42,'text',[1,2],true,subagentEntry(5,{type:'subagent_started',subagentId:'sub/a'})]],
  ['near-miss-type',[subagentEntry(1,{type:'Subagent_Started',subagentId:'sub/a'}),subagentEntry(2,{type:'subagent_started ',subagentId:'sub/x'})]],
  ['empty',[]],
  ['null-entry',[null]],
 ];
 const subagentReferences=subagentCells.map(([name,entries])=>{
  try{const r=projectSubagentReferences(entries);
   return {name,entries,started:r.started,completed:r.completed,startedById:index(r.startedById),completedById:index(r.completedById)};
  }catch(error:any){return {name,entries,error:{name:error.name,message:error.message}};}
 });
 // #2044: the web-history host view (`projectWebHistory`,
 // WebHistoryProjections.ts:279-311) over the web-only registry (:266-277).
 // The driver scans the whole log per cell before any finalize, so the error
 // rows pin which pass throws first; the `lastSequence` constructor read makes
 // a null element report 'sequence', unlike the registry.project subagent
 // rows above that read `entry.type` first.
 const {projectWebHistory}=await import(`${base}/src/session/projection/WebHistoryProjections.ts`);
 const webEntry=(i:number,extra:any)=>({version:1,entryId:`web-${i}`,parentEntryId:i?`web-${i-1}`:null,sequence:i,sessionId:'session-745',turnId:'A',createdAt:'2026-10-28T00:00:00.000Z',...extra});
 const toolMessage=(i:number,turnId:string)=>webEntry(i,{type:'durable_message',turnId,message:{role:'assistant',content:[{type:'tool_result'}]}});
 const webCells:Array<[string,any[]]>=[
  ['full',[
   toolMessage(1,'tools'),
   webEntry(2,{type:'file_artifacts',turnId:'tools',artifacts:[{source:'workspace_diff',path:'a'},{source:'session',path:'b'},{path:'c'}]}),
   webEntry(3,{type:'file_artifacts',turnId:'plain',artifacts:[{source:'workspace_diff'},{source:'session'}]}),
   webEntry(4,{type:'agent_status_message',turnId:'info',kind:'info',detail:{}}),
   webEntry(5,{type:'agent_status_message',turnId:'hidden',kind:'error',detail:{visible:false}}),
   webEntry(6,{type:'agent_status_message',turnId:'shown',kind:'error'}),
   webEntry(7,{type:'turn_result',turnId:'hidden',result:{type:'error'}}),
   webEntry(8,{type:'turn_result',turnId:'shown',result:{type:'error'}}),
   webEntry(9,{type:'agent_status_message',event:'context_budget',turnId:'budget',detail:{displayUsed:5,used:7,total:100,effectiveTotal:90,reservedOutputTokens:3,state:'ok',ratio:0.5,budgetUsed:6}}),
   webEntry(10,{type:'control_boundary',boundary:{kind:'compact',subtype:'compact_boundary',compactMetadata:{postTokens:100,preTokens:200,messagesSummarized:3}}}),
   webEntry(11,{type:'turn_result',turnId:'last',result:{type:'success',usage:{inputTokens:2,outputTokens:3,extra:'kept'},permissionDenials:[]}}),
  ]],
  ['workspace-diff-filtered',[webEntry(1,{type:'file_artifacts',turnId:'plain',artifacts:[{source:'workspace_diff'},{source:'session'},{path:'c'}]})]],
  ['tool-turn-keeps-diff',[toolMessage(1,'tools'),webEntry(2,{type:'file_artifacts',turnId:'tools',artifacts:[{source:'workspace_diff'}]})]],
  ['artifacts-primitive-elements',[webEntry(1,{type:'file_artifacts',artifacts:[5]}),webEntry(2,{type:'file_artifacts',artifacts:[{type:'x'}]})]],
  ['artifacts-str',[webEntry(1,{type:'file_artifacts',artifacts:'str'})]],
  ['artifacts-str-tool-turn',[toolMessage(1,'tools'),webEntry(2,{type:'file_artifacts',turnId:'tools',artifacts:'str'})]],
  ['artifacts-hex-length',[webEntry(1,{type:'file_artifacts',artifacts:{length:'0x10'}})]],
  ['artifacts-hex-length-tool-turn',[toolMessage(1,'tools'),webEntry(2,{type:'file_artifacts',turnId:'tools',artifacts:{length:'0x10'}})]],
  ['artifact-null',[webEntry(1,{type:'file_artifacts',artifacts:[null]})]],
  ['artifact-null-tool-turn',[toolMessage(1,'tools'),webEntry(2,{type:'file_artifacts',turnId:'tools',artifacts:[null]})]],
  ['artifacts-empty',[webEntry(1,{type:'file_artifacts',artifacts:[]})]],
  ['artifacts-missing',[webEntry(1,{type:'file_artifacts'})]],
  ['artifacts-null',[webEntry(1,{type:'file_artifacts',artifacts:null})]],
  ['message-null',[webEntry(1,{type:'durable_message',message:null})]],
  ['message-content-number',[webEntry(1,{type:'durable_message',message:{role:'assistant',content:5}})]],
  ['content-null-block',[webEntry(1,{type:'durable_message',message:{role:'assistant',content:[null]}})]],
  ['turn-error-visibility',[
   webEntry(1,{type:'agent_status_message',turnId:'hidden',kind:'error',detail:{visible:false}}),
   webEntry(2,{type:'agent_status_message',turnId:'shown',kind:'error'}),
   webEntry(3,{type:'agent_status_message',turnId:'visible-flag',kind:'error',detail:{visible:true}}),
   webEntry(4,{type:'agent_status_message',turnId:'primitive-detail',kind:'error',detail:5}),
   webEntry(5,{type:'turn_result',turnId:'hidden',result:{type:'error'}}),
   webEntry(6,{type:'turn_result',turnId:'shown',result:{type:'error'}}),
   webEntry(7,{type:'turn_result',turnId:'visible-flag',result:{type:'error'}}),
   webEntry(8,{type:'turn_result',turnId:'primitive-detail',result:{type:'error'}}),
  ]],
  ['turn-result-null-result',[webEntry(1,{type:'turn_result',result:null})]],
  ['usage-null',[webEntry(1,{type:'turn_result',result:{type:'success',usage:null}})]],
  ['usage-primitive',[webEntry(1,{type:'turn_result',result:{type:'success',usage:5}})]],
  ['compact-metadata-null',[webEntry(1,{type:'control_boundary',boundary:{kind:'compact',subtype:'compact_boundary',compactMetadata:null}})]],
  ['boundary-null',[webEntry(1,{type:'control_boundary',boundary:null})]],
  ['context-budget',[webEntry(1,{type:'agent_status_message',event:'context_budget',detail:{displayUsed:5,used:7,total:100,effectiveTotal:90,reservedOutputTokens:3,state:'ok',ratio:0.5,budgetUsed:6}})]],
  ['context-budget-partial',[webEntry(1,{type:'agent_status_message',event:'context_budget',detail:{used:2,total:10}})]],
  ['context-budget-used-fallback',[webEntry(1,{type:'agent_status_message',event:'context_budget',detail:{used:2,displayUsed:0,total:10,effectiveTotal:8}})]],
  ['compact-budget',[webEntry(1,{type:'control_boundary',boundary:{kind:'compact',subtype:'compact_boundary',compactMetadata:{postTokens:100,preTokens:200,messagesSummarized:3}}})]],
  ['compact-budget-nopost',[webEntry(1,{type:'control_boundary',boundary:{kind:'compact',subtype:'compact_boundary',compactMetadata:{preTokens:200}}})]],
  ['turn-usage',[webEntry(1,{type:'turn_result',result:{type:'success',usage:{inputTokens:2,outputTokens:3,extra:'kept'},permissionDenials:[]}})]],
  ['incomplete',[
   webEntry(1,{type:'durable_message',turnId:'A',message:{role:'assistant',content:[{type:'text',text:'x'}]}}),
   webEntry(2,{type:'durable_message',turnId:'A',message:{role:'assistant',content:[{type:'text',text:'y'}]}}),
   webEntry(3,{type:'durable_message',turnId:'B',message:{role:'assistant',content:[{type:'text',text:'z'}]}}),
   webEntry(4,{type:'turn_result',turnId:'A',result:{type:'success'}}),
  ]],
  ['empty',[]],
  ['null-entry',[webEntry(1,{type:'file_artifacts',artifacts:[]}),null]],
  ['primitive-entries',[42,'text',true]],
  ['near-miss-type',[webEntry(1,{type:'File_Artifacts',artifacts:'str'}),webEntry(2,{type:'file_artifacts ',artifacts:'str'})]],
  ['error-priority-finalize-vs-reduce',[webEntry(1,{type:'file_artifacts',artifacts:'str'}),webEntry(2,{type:'turn_result',result:null})]],
  ['error-priority-reduce-passes',[webEntry(1,{type:'file_artifacts',artifacts:null}),webEntry(2,{type:'turn_result',result:null})]],
 ];
 const webHistory=webCells.map(([name,raw])=>{
  const entries=raw.map((e:any)=>e);
  try{const r=projectWebHistory(entries);
   return {name,entries,view:{fileArtifacts:r.fileArtifacts,agentStatuses:r.agentStatuses,turnErrors:r.turnErrors,tokenUsage:r.tokenUsage,incompleteTurnIds:r.incompleteTurnIds}};
  }catch(error:any){return {name,entries,error:{name:error.name,message:error.message}};}
 });
 const {SessionRuntime}=await import(`${base}/src/session/events/SessionRuntime.ts`);
 const {createDefaultSessionProjectionRegistry}=await import(`${base}/src/session/projection/BuiltinSessionProjections.ts`);
 const {SessionProjectionDriver}=await import(`${base}/src/session/projection/SessionProjectionDriver.ts`);
 // #2058: fileArtifact reducer's `entry.artifacts.length` read
 // (WebHistoryProjections.ts:85). Measured on real TS (Node v22.22.0):
 // artifacts:null rejects with "Cannot read properties of null (reading
 // 'length')", a missing artifacts key with "... of undefined ..."; an empty
 // array restores fine and stays out of the oracle.
 const storageEntries=[5,null,[null]].map(content=>({type:'durable_message',sessionId:'session-745',turnId:'incomplete-turn',sequence:1,createdAt:'2026-10-28T00:00:00.000Z',entryId:'e1',parentEntryId:null,message:{role:'assistant',content}}))
  .concat([{type:'file_artifacts',sessionId:'session-745',turnId:'incomplete-turn',sequence:1,createdAt:'2026-10-28T00:00:00.000Z',entryId:'e1',parentEntryId:null,artifacts:null},{type:'file_artifacts',sessionId:'session-745',turnId:'incomplete-turn',sequence:1,createdAt:'2026-10-28T00:00:00.000Z',entryId:'e1',parentEntryId:null}]);
 const storageErrors=storageEntries.map(entry=>{
  const runtime=new SessionRuntime();
  const projections=new SessionProjectionDriver({runtime,registry:createDefaultSessionProjectionRegistry()});
  runtime.restore([entry]);
  try { projections.hydrate([entry]); throw Error('Expected damaged content to reject'); }
  catch(error:any) { return {entries:[entry],writer:runtime.snapshotState(),error:{name:error.name,message:error.message}}; }
  finally { projections.dispose(); }
 });
 const file=AgentLoop.prototype.snapshotFileState.call({readFileState:new Map([['seed.txt',{mtimeMs:7,kind:'text',offset:2}]]),writeSnapshots:new Map(),allowedReadFiles:new Set(['seed.txt'])});
 assert(file.readFileState instanceof Map);
 // #1985 block 1: the durable state comes from SessionRuntime.restore +
 // SessionProjectionDriver.hydrate, which runs each reducer's `reduce` and
 // never its `finalize` (stateOf reads the raw cell state). That is why a
 // string permissionDenials spreads here while projectAgentTranscriptEntries
 // throws in cloneTurnResult: the two ports differ and each gets its own rows.
 const hydratedInputs:Array<[string,any[]]>=[
  ['usage-full',[result('A',{inputTokens:10,outputTokens:5,cacheReadTokens:2,cacheWriteTokens:1,totalTokens:18})]],
  ['usage-partial',[result('A',{inputTokens:3})]],
  ['usage-zero',[result('A',{inputTokens:0,outputTokens:0,cacheReadTokens:0,cacheWriteTokens:0,totalTokens:0})]],
  ['usage-empty',[result('A',{})]],
  ['usage-sum',[result('A',{inputTokens:2}),result('B',{inputTokens:3,outputTokens:7})]],
  ['usage-primitive',[result('A',5)]],
  ['usage-string',[result('A','u')]],
  ['denials-one',[result('A',{inputTokens:1},[{toolName:'read',toolCallId:'one'}])]],
  ['denials-string',[result('A',{inputTokens:1},'d')]],
  ['denials-string-multi',[result('A',{inputTokens:1},'ab')]],
  // Codex #1985 r1 P2: ASCII-only rows let UTF-16 units, UTF-8 bytes and code
  // points all pass. This row pins the JS string iterator: a surrogate pair is
  // one element, not two UTF-16 units or four UTF-8 bytes. (Lone surrogates
  // cannot ride in oracle.json: serde_json rejects unpaired escapes, so that
  // case stays in the Rust unit test next to a real Node measurement.)
  ['denials-string-nonbmp',[result('A',{inputTokens:1},'\u{1F600}')]],
  ['denials-object',[result('A',{inputTokens:1},{})]],
  ['errors-string',[result('A',{inputTokens:1},[{toolName:'read',toolCallId:'one'}],'e')]],
  ['errors-object',[result('A',{inputTokens:1},[{toolName:'read',toolCallId:'one'}],{})]],
  ['metadata-title-number',[{type:'session_metadata',metadata:{title:5}}]],
  ['metadata-merge',[{type:'session_metadata',metadata:{title:' 原题 ',extra:'kept'}},{type:'session_metadata',metadata:{title:null,aiTitle:'AI'}}]],
 ];
 const hydrated=hydratedInputs.map(([name,raw])=>{
  const entries=raw.map((e:any,i:number)=>({version:1,entryId:`entry-${i+1}`,parentEntryId:i?`entry-${i}`:null,sequence:i+1,sessionId:'session-745',turnId:'A',createdAt:'2026-10-28T00:00:00.000Z',...e}));
  const runtime=new SessionRuntime();
  const driver=new SessionProjectionDriver({runtime,registry:createDefaultSessionProjectionRegistry()});
  try {
   runtime.restore(entries);
   driver.hydrate(entries);
   const summary:any=driver.stateOf('agent-transcript.turn-summary');
   return {name,entries,state:{usage:summary.usage,permissionDenials:summary.permissionDenials,metadata:driver.stateOf('agent-transcript.metadata')}};
  } catch(error:any) { return {name,entries,error:{name:error.name,message:error.message}}; }
  finally { driver.dispose(); }
 });
 return {projections,subagentReferences,webHistory,storageErrors,hydrated,fileState:{allowedReadFiles:file.allowedReadFiles,readFileState:Object.fromEntries(file.readFileState),writeSnapshots:Object.fromEntries(file.writeSnapshots)}};
}
