// Real TS stdio session: observe the saved seed at the public terminal boundary.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
export async function fileStateTiming(base:string, output:string) {
 const {createAgentSession}=await import(`${base}/src/agent/session/createAgentSession.ts`);
 const {createAgentLoopSidecarRuntimeFactory}=await import(`${base}/src/agent/modules/transport/agentLoopSidecarClient.ts`);
 const {createStdioAgentLoopSidecarConnectionFactory}=await import(`${base}/src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts`);
 const {InMemoryTranscriptWriter}=await import(`${base}/src/session/transcript/InMemoryTranscriptWriter.ts`);
 const {createDefaultPermissionContext}=await import(`${base}/src/permission/index.ts`);
 const home=path.join(output,'sidecar-home');fs.mkdirSync(home,{recursive:true});
 const encode=(value:any)=>JSON.parse(JSON.stringify(value,(_key,v)=>v instanceof Map?Object.fromEntries(v):v));
 const connect=createStdioAgentLoopSidecarConnectionFactory({command:process.execPath,
  args:['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(base,'src/cli/pilotdeck-agent-loop-sidecar.ts')],
  cwd:output,env:{PATH:process.env.PATH,HOME:home},killTimeoutMs:1000});
 let calls=0,held:any,running:any,terminal:any;const events:string[]=[];
 const model={async prepare({request}:any){return {request,provider:request.provider,model:request.model};},async *stream(){
  calls++;yield {type:'request_started',provider:'test',model:'deterministic'};yield {type:'message_start',role:'assistant'};
  if(calls===1){yield {type:'tool_call_end',toolCall:{id:'checkpoint-1',name:'checkpoint',input:{}}};yield {type:'message_end',finishReason:'tool_call'};}
  else {yield {type:'text_delta',text:'done'};yield {type:'message_end',finishReason:'stop'};}
 }};
 const tool={name:'checkpoint',description:'checkpoint',kind:'custom',inputSchema:{type:'object'},isReadOnly:()=>true,isConcurrencySafe:()=>true,execute:async()=>({content:[]})};
 const tools={list:()=>[tool],executeAll:async(calls:any[],context:any)=>{
  held=context;context.readFileState.set('during.txt',{mtimeMs:2,kind:'text'});
  running=encode(session.snapshotForRuntimeReload().fileState);
  return calls.map(call=>({toolCallId:call.id,toolName:call.name,status:'success',content:[],startedAt:'2026-10-28T00:00:00.000Z',completedAt:'2026-10-28T00:00:00.000Z'}));
 }};
 const session=createAgentSession({sessionId:'session-745',collectFileArtifacts:false,
  seedState:{readFileState:new Map([['seed.txt',{mtimeMs:1,kind:'text'}]])},
  config:{provider:'test',model:'deterministic',cwd:output,permissionMode:'default',permissionContext:createDefaultPermissionContext({cwd:output,mode:'default',canPrompt:false})},
  transcript:new InMemoryTranscriptWriter(),dependencies:{router:{},ports:{model,tools},tools:{registry:{list:()=>[tool]},scheduler:tools}},
  agentLoopFactory:createAgentLoopSidecarRuntimeFactory({connect})});
 const initial=encode(session.snapshotForRuntimeReload().fileState);
 const timer=setTimeout(()=>{console.error('#745 sidecar seed probe timed out');process.exit(1);},20000);
 let after:any;
 try {
  for await(const event of session.submit({type:'text',text:'checkpoint probe'},{turnId:'turn-file'})) {
   events.push(event.type);
   if(event.type==='turn_completed') {
    assert.equal(event.result.type,'success');assert(held,'checkpoint tool executed');
    terminal=encode(session.snapshotForRuntimeReload().fileState);
    held.readFileState.set('late.txt',{mtimeMs:3,kind:'text'});
   }
  }
  after=encode(session.snapshotForRuntimeReload().fileState);
 } finally {await session.dispose();clearTimeout(timer);}
 assert.equal(calls,2);assert.deepEqual(running,initial);assert(terminal.readFileState['during.txt']);assert.deepEqual(after,terminal);
 const result={initial,running,terminal,after,modelCalls:calls,events};
 fs.writeFileSync(path.join(output,'file-state-raw.json'),JSON.stringify(result,null,2)+'\n',{flag:'wx'});
 return result;
}
