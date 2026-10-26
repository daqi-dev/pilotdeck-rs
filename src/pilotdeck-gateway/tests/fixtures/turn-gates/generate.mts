// Real cc7c4e6 session, TurnRunner and stdio child. Only faults/gates are injected.
import assert from 'node:assert/strict';
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {dirname, join, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';
import {assertBaseline, baselineSourceHashes} from '../_shared/baseline.mts';
const root=resolve(process.env.PDRS_BASELINE!), out=resolve(process.argv[2]);
const sourceCommit=assertBaseline(root), fixtures=dirname(dirname(fileURLToPath(import.meta.url)));
const sources=new Set<string>();
for(const fixture of ['turn-runner','live-recording']) for(const path of Object.keys(JSON.parse(readFileSync(join(fixtures,fixture,'oracle.json'),'utf8')).sourceSha256)) sources.add(path);
const sourceSha256=await baselineSourceHashes(root,[...sources].sort());
const load=(p:string)=>import(pathToFileURL(join(root,'src',p)).href);
const {createAgentSessionWithStorage}=await load('agent/session/createAgentSession.ts');
const {createAgentLoopSidecarRuntimeFactory}=await load('agent/modules/transport/agentLoopSidecarClient.ts');
const {createStdioAgentLoopSidecarConnectionFactory}=await load('agent/modules/transport/stdioAgentLoopSidecarConnection.ts');
const {InMemoryTranscriptWriter}=await load('session/transcript/InMemoryTranscriptWriter.ts');
const {createDefaultPermissionContext}=await load('permission/index.ts');
mkdirSync(join(out,'home'),{recursive:true});
function gate(){let resolve!:()=>void;const promise=new Promise<void>(r=>{resolve=r;});return {promise,resolve};}
const names=['start-failure','accept-failure',...['cancel','error','protocol'].flatMap(k=>['ok','once','always'].map(f=>`${k}-${f}`)),'drop-before','drop-pending','drop-pending-once','drop-pending-always','drop-after'];
const matrix:any[]=[];
for(const name of names){
  const counts={factory:0,loop:0,connect:0,model:0,complete:0,closed:0,closeCalls:0};
  const entered=gate(), release=gate(), cancelled=gate(), events:any[]=[], wire:any[]=[];
  const transcript=new InMemoryTranscriptWriter();
  const connect=createStdioAgentLoopSidecarConnectionFactory({command:process.execPath,args:['--import',join(root,'node_modules/tsx/dist/loader.mjs'),join(root,'src/cli/pilotdeck-agent-loop-sidecar.ts')],cwd:out,env:{PATH:process.env.PATH,HOME:join(out,'home')},killTimeoutMs:1000});
  const factory=createAgentLoopSidecarRuntimeFactory({connect:async(input:any)=>{
    counts.connect++;const c=await connect(input);let closed=false;
    return {send:(v:any)=>c.send(v),close:async(r:any)=>{counts.closeCalls++;await c.close(r);if(!closed){closed=true;counts.closed++;}},async *receive(){
      for await(const m of c.receive()){wire.push(m);if(name.startsWith('error-') && m.kind==='event')throw new Error('injected receive failure');yield name.startsWith('protocol-') && m.kind==='event' && m.sequence===0 ? {...m,sequence:1}:m;}
    }};
  }});
  const model={async prepare({request}:any){return {request,provider:request.provider,model:request.model};},async *stream(){
    counts.model++;
    yield {type:'request_started',provider:'test',model:'deterministic'};
    if(name.startsWith('cancel-'))await cancelled.promise;
    yield {type:'message_start',role:'assistant'};yield {type:'text_delta',text:'hello'};yield {type:'message_end',finishReason:'stop'};
  }};
  const {session,handle}=createAgentSessionWithStorage({sessionId:'s',collectFileArtifacts:false,config:{provider:'test',model:'deterministic',cwd:out,permissionMode:'default',permissionContext:createDefaultPermissionContext({cwd:out,mode:'default',canPrompt:false})},transcript,dependencies:{router:{},ports:{model,tools:{list:()=>[],executeAll:async()=>[]}},tools:{registry:{list:()=>[]},scheduler:{executeAll:async()=>[]}}},agentLoopFactory:(args:any)=>{
    counts.factory++;const loop=factory(args), run=loop.run.bind(loop);
    loop.run=(...args:any[])=>{counts.loop++;return run(...args);};return loop;
  }});
  const recorder=(session as any).options.turnRunner.sessionEventRecorder;
  if(name==='start-failure')recorder.startTurn=async()=>{throw new Error('injected start failure');};
  if(name==='accept-failure')transcript.recordAcceptedInput=()=>{throw new Error('injected accept failure');};
  const complete=recorder.completeTurn.bind(recorder);
  recorder.completeTurn=async(r:any)=>{
    counts.complete++;
    if(counts.complete===1 && !name.endsWith('failure')){entered.resolve();await release.promise;}
    if(name.endsWith('always') || name.endsWith('once') && counts.complete===1)throw new Error('injected complete failure');
    await complete(r);
  };
  const timer=setTimeout(()=>{console.error(`timeout ${name}`);process.exit(1);},20000);
  const stream=handle.submit({type:'text',text:'probe'},{turnId:'t'});
  let error:string|null=null, before:any=null, dropped=false, pending:Promise<any>|undefined;
  try {
    for(let i=0;i<100;i++){
      pending=stream.next();
      const step=await Promise.race([pending.then(item=>({item})),entered.promise.then(()=>({gate:true}))]);
      if('gate' in step){
        before={...counts,active:handle.inFlight,terminal:events.filter(e=>e.type==='turn_completed').length};
        assert.equal(before.active,1);assert.equal(before.terminal,0);assert.equal(before.closed,1);
        let returning:Promise<any>|undefined;
        if(name.startsWith('drop-pending'))returning=stream.return();
        release.resolve();const item=await pending;if(item.value)events.push(item.value);
        if(name==='drop-after' && item.value?.type==='turn_completed'){await stream.return();dropped=true;break;}
        if(returning){await returning;dropped=true;break;}
        // No longer race the already-resolved gate.
        for(let j=0;j<100;j++){const item=await stream.next();if(item.done)break;events.push(item.value);if(name==='drop-after' && item.value.type==='turn_completed'){await stream.return();dropped=true;break;}}
        break;
      }
      if(step.item.done)break;
      events.push(step.item.value);
      if(name.startsWith('cancel-') && step.item.value.type==='model_event'){session.abort('user-stop');cancelled.resolve();}
      if(name==='drop-before' && step.item.value.type==='model_event'){await stream.return();dropped=true;break;}
    }
  }catch(e:any){error=e.message;}finally{release.resolve();cancelled.resolve();await stream.return();await handle.dispose();clearTimeout(timer);}
  const row={name,counts,before,terminal:events.filter(e=>e.type==='turn_completed').length,result:events.find(e=>e.type==='turn_completed')?.result?.type??null,active:handle.inFlight,error,dropped};
  assert.equal(row.active,0);assert.equal(counts.closed,counts.connect);assert.ok(row.terminal<=1);
  writeFileSync(join(out,`${name}-raw.json`),JSON.stringify({wire,events,entries:transcript.entries},null,2)+'\n',{flag:'wx'});
  matrix.push(row);console.log(JSON.stringify(row));
}
assertBaseline(root);
writeFileSync(join(out,'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,matrix},null,2)+'\n',{flag:'wx'});
