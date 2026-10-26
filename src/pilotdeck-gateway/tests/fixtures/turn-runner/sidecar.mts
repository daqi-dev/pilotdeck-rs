// Real stdio sidecar. Only wire sequence/final position are perturbed.
import assert from 'node:assert/strict';
import {join} from 'node:path';
import {pathToFileURL, fileURLToPath} from 'node:url';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
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
  for(const name of ['success','gap','duplicate','early-final','drop','abort-before','abort-after','dispose-before','completed-warning']) {
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
        connections++;const connection=await connect(input);let finalPrefix=true, countedClose=false, offset=0;
        return {send:(m:any)=>connection.send(m),close:async(r:any)=>{await connection.close(r);if(!countedClose){closed++;countedClose=true;}},async *receive(){
          for await(const m of connection.receive()) {
            wire.push(m);
            if(m.kind==='event') {
              if(name==='gap' && m.sequence===0)continue;
              if(name==='early-final' && !m.final && finalPrefix)continue;
              if(m.final)finalPrefix=false;
            }
            // Re-sequence a valid final to zero: isolate premature final from gap detection.
            const delivered=name==='early-final' && m.kind==='event' && m.final ? {...m,sequence:0}:m.kind==='event'?{...m,sequence:m.sequence+offset}:m;
            deliveredWire.push(delivered);
            yield delivered;
            if(name==='completed-warning' && m.kind==='event' && !m.final && m.payload?.type==='turn_completed') {
              const warning={...m,sequence:m.sequence+1,payload:{type:'warning',sessionId:'s',turnId:'t',message:'after loop completed'}};
              offset++;deliveredWire.push(warning);yield warning;
            }
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
    // A rejected stream can race the pr5e instructions hook; raw evidence keeps it.
    rows.push({name,connections,closed,modelCalls,events:events.map(e=>e.type).filter(t=>
      !(['gap','duplicate'].includes(name) && t==='instructions_loaded')),
      // Receive-side tail length varies after rejection; retain full raw record above.
      firstSidecarEvent:sequence[0]??null,
      ...(['success','completed-warning'].includes(name)?{wireEvents:sequence,
        // AgentSession wrapper/setup/hooks are pr5e; retain every loop event.
        hostEvents:events.map(e=>e.type).filter(t=>!['session_started','setup_completed','user_prompt_submitted','instructions_loaded','session_ended'].includes(t))}:{}),terminal:completed?.result?.type??null,
      error:events.find(e=>e.type==='turn_failed')?.error??null});
    if(name==='success'){assert.equal(modelCalls,1);assert.equal(sequence[0].type,'model_request_started');assert.equal(completed.result.type,'success');}
    assert.equal(closed,connections,'all real sidecar connections closed before row completes');
  }
  return rows;
}

// Rust integration uses this transparent stdio proxy around the same real CLI.
// Mutations affect only transport frames; model execution stays in the Rust host.
if(process.argv[1]===fileURLToPath(import.meta.url)) {
  const [root,mode,output]=process.argv.slice(2);
  mkdirSync(output,{recursive:true});
  const child=spawn(process.execPath,['--import',join(root,'node_modules/tsx/dist/loader.mjs'),join(root,'src/cli/pilotdeck-agent-loop-sidecar.ts')],{stdio:['pipe','pipe','inherit']});
  process.stdin.pipe(child.stdin);child.stdin.on('error',()=>{});
  let index=0,offset=0;
  for await(const line of createInterface({input:child.stdout})) {
    const m=JSON.parse(line);
    writeFileSync(join(output,`wire-${index++}.json`),line+'\n',{flag:'wx'});
    if(m.kind==='event') {
      if(mode==='gap' && m.sequence===0)continue;
      if(mode==='early-final' && !m.final)continue;
      if(mode==='early-final')m.sequence=0;
      else m.sequence+=offset;
    }
    console.log(JSON.stringify(m));
    if(mode==='duplicate' && m.kind==='event' && m.sequence===0)console.log(JSON.stringify(m));
    if(mode==='completed-warning' && m.kind==='event' && !m.final && m.payload?.type==='turn_completed') {
      offset++;console.log(JSON.stringify({...m,sequence:m.sequence+1,payload:{type:'warning',sessionId:m.payload.sessionId,turnId:m.payload.turnId,message:'after loop completed'}}));
    }
  }
  process.exitCode=child.exitCode??0;
}
