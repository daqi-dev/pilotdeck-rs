import assert from 'node:assert/strict';
export async function observations(base:string) {
 const {projectAgentTranscriptEntries}=await import(`${base}/src/session/projection/AgentTranscriptProjections.ts`);
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
 const {SessionRuntime}=await import(`${base}/src/session/events/SessionRuntime.ts`);
 const {createDefaultSessionProjectionRegistry}=await import(`${base}/src/session/projection/BuiltinSessionProjections.ts`);
 const {SessionProjectionDriver}=await import(`${base}/src/session/projection/SessionProjectionDriver.ts`);
 const storageErrors=[5,null,[null]].map(content=>{
  const entry={type:'durable_message',sessionId:'session-745',turnId:'incomplete-turn',sequence:1,createdAt:'2026-10-28T00:00:00.000Z',entryId:'e1',parentEntryId:null,message:{role:'assistant',content}};
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
 return {projections,storageErrors,hydrated,fileState:{allowedReadFiles:file.allowedReadFiles,readFileState:Object.fromEntries(file.readFileState),writeSnapshots:Object.fromEntries(file.writeSnapshots)}};
}
