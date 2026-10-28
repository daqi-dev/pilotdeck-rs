import assert from 'node:assert/strict';
export async function observations(base:string) {
 const {projectAgentTranscriptEntries}=await import(`${base}/src/session/projection/AgentTranscriptProjections.ts`);
 const {AgentLoop}=await import(`${base}/src/agent/loop/AgentLoop.ts`);
 const message=(text:string,role='user',metadata?:any)=>({role,content:[{type:'text',text}],...(metadata?{metadata}:{})});
 const result=(turnId:string,usage:any={},permissionDenials:any[]=[])=>({type:'turn_result',turnId,result:{type:'success',sessionId:'session-745',turnId,usage,permissionDenials}});
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
 return {projections,storageErrors,fileState:{allowedReadFiles:file.allowedReadFiles,readFileState:Object.fromEntries(file.readFileState),writeSnapshots:Object.fromEntries(file.writeSnapshots)}};
}
