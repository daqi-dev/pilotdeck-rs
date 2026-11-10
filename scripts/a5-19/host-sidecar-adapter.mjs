/** Compose a real production sidecar runner into the original 72-scene harness.
 * All storage gates, timer firing, client pauses and verdicts stay in six_cells.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {join,resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {setTimeout as delay} from 'node:timers/promises';
const baseline=resolve(process.env.A19_HOST_BASELINE);
const upstream=path=>import(pathToFileURL(join(baseline,'src',path+'.ts')).href);
const {createAgentSession}=await upstream('agent/session/createAgentSession');
const {createAgentLoopSidecarRuntimeFactory}=await upstream('agent/modules/transport/agentLoopSidecarClient');
const {createStdioAgentLoopSidecarConnectionFactory}=await upstream('agent/modules/transport/stdioAgentLoopSidecarConnection');
const {createDefaultPermissionContext}=await upstream('permission/index');
function atomic(path,value){fs.writeFileSync(path+'.tmp',JSON.stringify(value)+'\n');fs.renameSync(path+'.tmp',path)}
export async function wrapLoop({dir,phase,order,journal,outcome,knownReturn,transcript,time,id}){
 const spec=JSON.parse(fs.readFileSync(process.env.A19_HOST_SIDECAR_SPEC,'utf8'));
 assert.ok(['ts','rust'].includes(spec.language));assert.ok(typeof spec.command==='string');assert.ok(Array.isArray(spec.args));
 const control=join(dir,'sidecar-loop');fs.mkdirSync(control,{recursive:true});
 atomic(join(control,'loop.json'),{id,phase,order,boundMs:8000});
 const environment={...process.env,PILOTDECK_NATIVE_LOOP_STIMULUS:control,A19_HOST_BASELINE:baseline,A19_HOST_TIME:time,A52B_CHILD_LOADLOG:join(dir,'child-loaded.ndjson')};
 if(spec.language==='ts')environment.PILOTDECK_AGENT_LOOP_FACTORY=resolve(import.meta.dirname,'host-sidecar-stimulus.mjs');
 else delete environment.PILOTDECK_AGENT_LOOP_FACTORY;
 const release=(stage,answer)=>{atomic(join(control,'release.json'),{id,stage,...(answer===undefined?{}:{outcome:answer})});journal.add('sidecar.control.release',{stage,answer})};
 const oldOutcome=outcome.resolve,oldReturn=knownReturn.resolve;
 outcome.resolve=answer=>{release('outcome',answer);oldOutcome(answer)};
 knownReturn.resolve=()=>{release('return');oldReturn()};
 const seen=new Set(),connections=[];let stopped=false,pollFailure;
 const scan=()=>{
  const directory=join(control,'witness');if(!fs.existsSync(directory))return;
  for(const name of fs.readdirSync(directory).filter(name=>name.endsWith('.json')).sort()){
   if(seen.has(name))continue;
   const path=join(directory,name),receipt=JSON.parse(fs.readFileSync(path,'utf8'));seen.add(name);
   assert.ok(connections.some(connection=>connection.child.pid===receipt.pid),'receipt must belong to this scene sidecar');
   journal.add('sidecar.receipt',{path,receipt});
   if(receipt.site!=='loop')continue;
   if(receipt.phase==='wait')journal.add('loop.wait',{phase});
   if(receipt.phase==='return'&&receipt.key==='block')journal.add('loop.return.block');
   if(receipt.phase==='final')journal.add('loop.final',{outcome:receipt.key,...(receipt.result?{result:receipt.result}:{})});
   if(receipt.phase==='bound')throw Error('actual sidecar exceeded original bounded control: '+receipt.key);
  }
 };
 const polling=(async()=>{try{while(!stopped){scan();await delay(2)}}catch(error){pollFailure=error;journal.add('sidecar.receipt.failure',{message:String(error)})}})();
 const connect=createStdioAgentLoopSidecarConnectionFactory({command:spec.command,args:spec.args,cwd:dir,env:environment,killTimeoutMs:1000});
 let sequence=0,runtime;
 const factory=createAgentLoopSidecarRuntimeFactory({connect:input=>{
  const connection=connect(input);connections.push(connection);
  journal.add('sidecar.spawn',{pid:connection.child.pid,language:spec.language,command:spec.command,args:spec.args});
  const send=connection.send.bind(connection);connection.send=message=>{journal.add('sidecar.send',{message});return send(message)};
  const receive=connection.receive.bind(connection);connection.receive=async function*(){for await(const message of receive()){journal.add('sidecar.receive',{message});yield message}};
  connection.child.on('exit',(code,signal)=>journal.add('sidecar.exit',{pid:connection.child.pid,code,signal}));return connection;
 },uuid:()=>`host-sidecar-${++sequence}`,transportObserver:{observe:observation=>journal.add('sidecar.observation',{observation})}});
 // Production composition supplies the real host module ports and ledger. Its
 // auxiliary session is never submitted: the original TurnRunner stays owner.
 const owner=createAgentSession({sessionId:'session',transcript,collectFileArtifacts:false,
  config:{provider:'fixture',model:'controlled',cwd:dir,permissionMode:'default',permissionContext:createDefaultPermissionContext({cwd:dir,canPrompt:false})},
  dependencies:{router:{},now:()=>new Date(time),ports:{model:{prepare:async()=>{throw Error('controlled loop must not call model.prepare')},async *stream(){throw Error('controlled loop must not call model.stream')}},tools:{list:()=>[],executeAll:async()=>{throw Error('controlled loop must not execute host tools')}}},tools:{registry:{list:()=>[]},scheduler:{executeAll:async()=>{throw Error('controlled loop must not schedule tools')}}}},
  agentLoopFactory:input=>{runtime=factory(input);return runtime}});
 assert.ok(runtime);
 return {loop:runtime,collect:scan,async close(){
  const errors=[];
  try{await Promise.all(connections.map(connection=>connection.close('formal_host_scene_complete')))}catch(error){errors.push(error)}
  stopped=true;await polling;
  try{scan()}catch(error){errors.push(error)}
  outcome.resolve=oldOutcome;knownReturn.resolve=oldReturn;
  try{await owner.dispose?.()}catch(error){errors.push(error)}
  if(pollFailure)errors.push(pollFailure);
  assert.equal(connections.length,1,'each original scene must spawn exactly one real sidecar');
  assert.ok(journal.rows.some(row=>row.kind==='sidecar.observation'&&row.observation.type==='handshake_completed'),'original protocol handshake required');
  if(errors.length)throw new AggregateError(errors,'formal sidecar cleanup/receipts failed');
 }};
}
