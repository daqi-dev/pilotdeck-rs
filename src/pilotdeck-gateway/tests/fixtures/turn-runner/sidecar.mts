// Real stdio sidecar. Only wire sequence/final position are perturbed.
import assert from 'node:assert/strict';
import {join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {mkdirSync, writeFileSync} from 'node:fs';
export async function probeSidecar(root: string, output: string) {
  const load=(p:string)=>import(pathToFileURL(join(root,'src',p)).href);
  const {createAgentSession}=await load('agent/session/createAgentSession.ts');
  const {createAgentLoopSidecarRuntimeFactory}=await load('agent/modules/transport/agentLoopSidecarClient.ts');
  const {createStdioAgentLoopSidecarConnectionFactory}=await load('agent/modules/transport/stdioAgentLoopSidecarConnection.ts');
  const {InMemoryTranscriptWriter}=await load('session/transcript/InMemoryTranscriptWriter.ts');
  const {createDefaultPermissionContext}=await load('permission/index.ts');
  const rows:any[]=[];
  mkdirSync(join(output,'home'),{recursive:true});
  for(const name of ['success','gap','duplicate','early-final','drop','abort-before','abort-after','dispose-before']) {
    const connect=createStdioAgentLoopSidecarConnectionFactory({command:process.execPath,
      args:['--import',join(root,'node_modules/tsx/dist/loader.mjs'),join(root,'src/cli/pilotdeck-agent-loop-sidecar.ts')],
      cwd:output,env:{PATH:process.env.PATH,HOME:join(output,'home')},killTimeoutMs:1000});
    const wire:any[]=[],deliveredWire:any[]=[],events:any[]=[];
    let modelCalls=0,connections=0,closed=0;
    const model={async prepare({request}:any){return {request,provider:request.provider,model:request.model};},async *stream(){
      modelCalls++;
      yield {type:'request_started',provider:'test',model:'deterministic'};
      yield {type:'message_start',role:'assistant'};
      yield {type:'text_delta',text:'hello'};
      yield {type:'message_end',finishReason:'stop'};
    }};
    const session=createAgentSession({sessionId:'s',collectFileArtifacts:false,
      config:{provider:'test',model:'deterministic',cwd:output,permissionMode:'default',
        permissionContext:createDefaultPermissionContext({cwd:output,mode:'default',canPrompt:false})},
      transcript:new InMemoryTranscriptWriter(),dependencies:{router:{},ports:{model,tools:{list:()=>[],executeAll:async()=>[]}},
        tools:{registry:{list:()=>[]},scheduler:{executeAll:async()=>[]}}},
      agentLoopFactory:createAgentLoopSidecarRuntimeFactory({connect:async(input:any)=>{
        connections++;const connection=await connect(input);let finalPrefix=true, countedClose=false;
        return {send:(m:any)=>connection.send(m),close:async(r:any)=>{await connection.close(r);if(!countedClose){closed++;countedClose=true;}},async *receive(){
          for await(const m of connection.receive()) {
            wire.push(m);
            if(m.kind==='event') {
              if(name==='gap' && m.sequence===0)continue;
              if(name==='early-final' && !m.final && finalPrefix)continue;
              if(m.final)finalPrefix=false;
            }
            // Re-sequence a valid final to zero: isolate premature final from gap detection.
            const delivered=name==='early-final' && m.kind==='event' && m.final ? {...m,sequence:0}:m;
            deliveredWire.push(delivered);
            yield delivered;
            if(name==='duplicate' && m.kind==='event' && m.sequence===0){deliveredWire.push(m);yield m;}
          }
        }};
      }})});
    const timer=setTimeout(()=>{console.error(`sidecar ${name} timed out (20s)`);process.exit(1);},20000);
    try {
      for await(const event of session.submit({type:'text',text:'probe'},{turnId:'t'})) {
        events.push(event);
        if(name==='dispose-before' && event.type==='turn_started')await session.dispose();
        if(name==='abort-before' && event.type==='turn_started')session.abort('probe');
        if(name==='abort-after' && event.type==='input_accepted')session.abort('probe');
        if(name==='drop' && event.type==='model_event')break;
      }
    }finally{await session.dispose();clearTimeout(timer);}
    writeFileSync(join(output,`sidecar-${name}-raw.json`),JSON.stringify({wire,deliveredWire,events},null,2)+'\n',{flag:'wx'});
    const sequence=wire.filter(m=>m.kind==='event').map(m=>({sequence:m.sequence,final:m.final,type:m.payload?.type}));
    const completed=events.find(e=>e.type==='turn_completed');
    rows.push({name,connections,closed,modelCalls,events:events.map(e=>e.type),
      // Receive-side tail length varies after rejection; retain full raw record above.
      firstSidecarEvent:sequence[0]??null,terminal:completed?.result?.type??null,
      error:events.find(e=>e.type==='turn_failed')?.error??null});
    if(name==='success'){assert.equal(modelCalls,1);assert.equal(sequence[0].type,'model_request_started');assert.equal(completed.result.type,'success');}
    assert.equal(closed,connections,'all real sidecar connections closed before row completes');
  }
  return rows;
}
