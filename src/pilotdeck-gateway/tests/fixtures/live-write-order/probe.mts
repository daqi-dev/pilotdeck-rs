// #748 observation hooks on real TS classes; no baseline source is rewritten.
import assert from 'node:assert/strict';
import {writeFileSync} from 'node:fs';
import {dirname,join,resolve} from 'node:path';
import {fileURLToPath,pathToFileURL} from 'node:url';
import {setImmediate as tick} from 'node:timers/promises';
const [name, directory] = process.argv.slice(2);
const output=resolve(directory), fixture=dirname(fileURLToPath(import.meta.url));
const baseline=process.env.PDRS_BASELINE!;
const load=(p:string)=>import(pathToFileURL(join(baseline,'src',p)).href);
const [{JsonlTranscriptWriter},{TurnRunner},{normalizeGatewayRunMode,InProcessGateway},{classifyDurableMessageEntry}]=await Promise.all([
 load('session/transcript/JsonlTranscriptWriter.ts'),load('agent/turn/TurnRunner.ts'),
 load('gateway/client/InProcessGateway.ts'),load('session/transcript/TranscriptEntry.ts')]);
const trace:string[]=[];
let gateway:any, aborted:Promise<any>|undefined, abortSettled=false;
const submit=InProcessGateway.prototype.submitTurn;
(InProcessGateway.prototype as any).submitTurn=function(...args:any[]) {gateway=this;return submit.apply(this,args as any);};
const instrumented=new WeakSet<object>();
const writer=JsonlTranscriptWriter.prototype as any;
const status=writer.recordAgentStatusMessage, durable=writer.recordDurableMessage, sessionEvent=writer.recordTurnResult;
writer.recordAgentStatusMessage=function(...args:any[]) {
 if(args[2]?.event!=='context_budget') return status.apply(this,args);
 trace.push('context:queued');
 const store=this.eventStore, append=store.append.bind(store);
 if(!instrumented.has(store)) { instrumented.add(store); store.append=async(...items:any[])=>{
  if(items[2]?.type==='agent_status_message' && items[2]?.event==='context_budget') {
   trace.push('context:entered');
   if(name==='context-cancel' && !aborted) aborted=gateway.abortTurn({sessionKey:'live-e2e-session',runId:'run-text',reason:'probe'}).then(()=>{abortSettled=true;});
   // Pause the real append sink while the writer retains its queue ownership.
   for(let n=0;n<8;n++) await tick();
   assert.equal(trace.includes('result:committed'),false,'#748 TS status predecessor blocks result');
   if(name==='context-cancel') {assert.equal(abortSettled,false,'#748 TS blocked writer retains cancel');trace.push('context:cancel-pending');}
   trace.push('context:release');
  }
  return append(...items);
 }; }
 return status.apply(this,args).then((value:any)=>{trace.push('context:committed');return value;});
};
let failed=false;
writer.recordDurableMessage=async function(...args:any[]) {
 const kind=classifyDurableMessageEntry(args[2]);
 trace.push('durable:begin:'+kind);
 for(let n=0;n<8;n++) await tick();
 if(!failed && ((name==='assistant-error' && kind==='assistant_message') || (name==='tool-error' && kind==='tool_result_message'))) {
  failed=true;trace.push('durable:failed:'+kind);throw Error('#748 injected durable sink failure');
 }
 const value=await durable.apply(this,args);trace.push('durable:committed:'+kind);return value;
};
writer.recordTurnResult=function(...args:any[]) {
 trace.push('result:queued');return sessionEvent.apply(this,args).then((value:any)=>{trace.push('result:committed');return value;});
};
const run=TurnRunner.prototype.run;
(TurnRunner.prototype as any).run=async function*(...args:any[]) {
 const it=run.apply(this,args as any);
 try {while(true){const next=await it.next();if(next.done)return next.value;
  if(['assistant_message','tool_results_projected'].includes(next.value.type)) trace.push('yield:'+next.value.type);
  yield next.value;
 }} finally {await it.return(undefined as never);}
};
const scenario=['tool','tool-error'].includes(name)?'tool':'text';
process.argv=[process.argv[0],join(fixture,'../live-e2e/driver.mts'),scenario,output];
await import(pathToFileURL(process.argv[1]).href);
if(aborted) {await aborted;trace.push('context:cancel-settled');}
assert.ok(trace.indexOf('context:committed')<trace.indexOf('result:committed'),'#748 TS context before result');
for(const [wire,kind] of [['assistant_message','assistant_message'],['tool_results_projected','tool_result_message']]) {
 const at=trace.indexOf('yield:'+wire);if(at<0)continue;
 assert.ok(at<trace.indexOf('durable:begin:'+kind),'#748 TS yield before durable callback');
}
assert.equal(failed,name.endsWith('-error'),'#748 TS fault hook reached');
const result={name,node:process.version,trace,runModes:Object.fromEntries(['agent','plan','ask','invalid',''].map(mode=>[mode,normalizeGatewayRunMode(mode)??null]))};
writeFileSync(join(output,'probe.json'),JSON.stringify(result,null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify(result));
