// #1022: real stdio checkpoint finalization and AgentHandle abort-error cleanup.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';

function deferred() {
 let resolve!:()=>void;
 const promise=new Promise<void>(done=>{resolve=done});
 return {promise,resolve};
}
async function bounded<T>(promise:Promise<T>,name:string):Promise<T> {
 let timer:ReturnType<typeof setTimeout>;
 try {return await Promise.race([promise,new Promise<never>((_,reject)=>{timer=setTimeout(()=>reject(Error(`#1022 ${name} timed out`)),20000)})]);}
 finally {clearTimeout(timer!);}
}
export async function followups(base:string,output:string) {
 assert.equal(process.version,'v22.22.0','#1022 Node version');
 const {createAgentSessionWithStorage}=await import(`${base}/src/agent/session/createAgentSession.ts`);
 const {createAgentLoopSidecarRuntimeFactory}=await import(`${base}/src/agent/modules/transport/agentLoopSidecarClient.ts`);
 const {createStdioAgentLoopSidecarConnectionFactory}=await import(`${base}/src/agent/modules/transport/stdioAgentLoopSidecarConnection.ts`);
 const {InMemoryTranscriptWriter}=await import(`${base}/src/session/transcript/InMemoryTranscriptWriter.ts`);
 const {createDefaultPermissionContext}=await import(`${base}/src/permission/index.ts`);
 const encode=(v:any)=>JSON.parse(JSON.stringify(v,(_k,x)=>x instanceof Map?Object.fromEntries(x):x));
 const rows=[];
 for(const name of ['drain','abort-throw']) {
  const cwd=path.join(output,`followup-${name}`),home=path.join(cwd,'home');fs.mkdirSync(home,{recursive:true});
  const entered=deferred(),release=deferred();let calls=0,configuredDisposed=0,effectReleased=0,drained:any;
  const connections:any[]=[];
  const connectNative=createStdioAgentLoopSidecarConnectionFactory({command:process.execPath,
   args:['--import',path.join(base,'node_modules/tsx/dist/loader.mjs'),path.join(base,'src/cli/pilotdeck-agent-loop-sidecar.ts')],
   cwd,env:{PATH:process.env.PATH,HOME:home},killTimeoutMs:1000});
  const connect=(input:any)=>{const connection=connectNative(input);connections.push(connection);return connection;};
  const model={async prepare({request}:any){return {request,provider:request.provider,model:request.model}},async *stream(){
   calls++;yield {type:'request_started',provider:'test',model:'deterministic'};yield {type:'message_start',role:'assistant'};
   if(calls===1){yield {type:'tool_call_end',toolCall:{id:'checkpoint-1022',name:'checkpoint',input:{}}};yield {type:'message_end',finishReason:'tool_call'};}
   else {yield {type:'text_delta',text:'done'};yield {type:'message_end',finishReason:'stop'};}
  }};
  const tool={name:'checkpoint',description:'checkpoint',kind:'custom',inputSchema:{type:'object'},isReadOnly:()=>true,isConcurrencySafe:()=>true,execute:async()=>({content:[]})};
  const tools={list:()=>[tool],executeAll:async(calls:any[],context:any)=>{
   context.readFileState.set('during.txt',{mtimeMs:2,kind:'text'});entered.resolve();await release.promise;
   return calls.map(call=>({toolCallId:call.id,toolName:call.name,status:'success',content:[],startedAt:'2026-10-28T00:00:00.000Z',completedAt:'2026-10-28T00:00:00.000Z'}));
  }};
  const {session,handle}=createAgentSessionWithStorage({sessionId:'session-1022',collectFileArtifacts:false,
   seedState:{readFileState:new Map([['seed.txt',{mtimeMs:1,kind:'text'}]])},
   config:{provider:'test',model:'deterministic',cwd,permissionMode:'default',permissionContext:createDefaultPermissionContext({cwd,mode:'default',canPrompt:false})},
   transcript:new InMemoryTranscriptWriter(),dependencies:{router:{},ports:{model,tools},tools:{registry:{list:()=>[tool]},scheduler:tools}},
   __configure:({scope,session}:any)=>{scope.own(()=>{effectReleased++});return ()=>{configuredDisposed++;drained=encode(session.snapshotForRuntimeReload().fileState)}},
   agentLoopFactory:createAgentLoopSidecarRuntimeFactory({connect})});
  const initial=encode(session.snapshotForRuntimeReload().fileState);
  const running=(async()=>{for await(const _event of handle.submit({type:'text',text:'checkpoint probe'},{turnId:'turn-1022'})) { /* consume real stream */ }})();
  try {
   await bounded(entered.promise,'tool entered');
   const captured=encode(session.snapshotForRuntimeReload().fileState);
   if(name==='abort-throw')session.abort=()=>{throw Error('ABORT_1022')};
   const disposing=handle.dispose('followup').then(()=>null,(e:any)=>({name:e.name,message:e.message,errors:e.errors.map((v:any)=>v.message)}));
   const pending={state:handle.state,inFlight:handle.inFlight,configuredDisposed,effectReleased};
   release.resolve();await bounded(running,'turn drain');const error=await bounded(disposing,'dispose');
   const row={name,initial,captured,drained,pending,state:handle.state,inFlight:handle.inFlight,configuredDisposed,effectReleased,error};
   assert.deepEqual(captured,initial,'#1022 snapshot excludes uncommitted turn seed');
   assert(drained.readFileState['during.txt'],'#1022 drain publishes real checkpoint');
   assert.equal(configuredDisposed,1,'#1022 configured disposer runs after abort');
   assert.equal(effectReleased,1,'#1022 scope cleanup runs after abort');
   assert.equal(handle.state,'disposed','#1022 handle disposed after drain');
   if(name==='abort-throw')assert.deepEqual(error,{name:'AggregateError',message:'Failed to dispose agent handle.',errors:['ABORT_1022']},'#1022 abort error retained');
   else assert.equal(error,null,'#1022 ordinary drain');
   rows.push(row);
  } finally {
   release.resolve();
   // An uncooperative stream must not skip sibling cleanup or leave stdio
   // children keeping the ignored regeneration guard alive after a timeout.
   const cleanup=await Promise.allSettled([
    bounded(running.catch(()=>{}),'final turn'),
    bounded(handle.dispose('cleanup').catch(()=>{}),'final cleanup'),
    ...connections.map(connection=>bounded(connection.close('probe-cleanup'),'connection close')),
   ]);
   const failure=cleanup.find(result=>result.status==='rejected');
   if(failure?.status==='rejected')throw failure.reason;
  }
 }
 fs.writeFileSync(path.join(output,'followups-raw.json'),JSON.stringify(rows,null,2)+'\n',{flag:'wx'});
 console.log('PASS: #1022 real stdio drain and abort throw cleanup');
 return rows;
}
