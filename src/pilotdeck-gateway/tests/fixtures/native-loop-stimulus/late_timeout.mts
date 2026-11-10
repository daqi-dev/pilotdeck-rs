// Actual fixed TS TurnRunner oracle; independent of native scoring.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import { mkdirSync,readFileSync,writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
const [baseline,out]=process.argv.slice(2);mkdirSync(out,{recursive:false});
assert.equal(execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim(),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
const sources=Object.fromEntries(['src/agent/turn/TurnRunner.ts','src/agent/modules/transport/operationLedger.ts','src/session/transcript/JsonlTranscriptWriter.ts','src/session/events/JsonlSessionEventStore.ts'].map(path=>[path,createHash('sha256').update(readFileSync(baseline+'/'+path)).digest('hex')]));
const load=(p:string)=>import(pathToFileURL(baseline+'/'+p).href);
const {TurnRunner}=await load('src/agent/turn/TurnRunner.ts');
const {AgentLoopResultUnknownError}=await load('src/agent/modules/transport/operationLedger.ts');
const {JsonlTranscriptWriter}=await load('src/session/transcript/JsonlTranscriptWriter.ts');
const {JsonlSessionEventStore}=await load('src/session/events/JsonlSessionEventStore.ts');
const cases=[['exact','run-A','timeout:run-A',true],['different-turn','run-A','timeout:agent-turn',true],['old-run','run-A','timeout:old-run',true],['ordinary','run-A','user:abort',true],['unknown-first','run-A',null,true],['missing',undefined,'timeout:agent-turn',true],['null',null,'timeout:agent-turn',true],['number',42,'timeout:42',true],['ordinary-error','run-A','timeout:run-A',false]];
const rows=[];
for(const [name,runId,reason,typed]of cases){
 const dir=out+'/'+name;mkdirSync(dir);const controller=new AbortController();
 const store=new JsonlSessionEventStore({path:dir+'/events.jsonl'});
 const writer=new JsonlTranscriptWriter({path:dir+'/transcript.jsonl',eventStore:store});
 const loop={snapshotFileState:()=>({}),async *run(input:any){yield {type:'model_request_started',sessionId:input.sessionId,turnId:input.turnId,provider:'fixture',model:'controlled'};if(reason!==null)controller.abort(reason);throw typed?new AgentLoopResultUnknownError():new Error('owned ordinary failure');}};
 const runner=new TurnRunner(loop,writer,undefined,()=>new Date('2026-11-10T00:00:00Z'),undefined,{cwd:dir,transcriptPath:dir+'/transcript.jsonl',collectFileArtifacts:false},{autoGenerateSessionTitle:false});
 const events=[];let thrown=null;
 try{for await(const e of runner.run({sessionId:'session',turnId:'agent-turn',input:{type:'text',text:'owned probe'},messages:[],execution:{runId},abortSignal:controller.signal}))events.push(e);}catch(e:any){thrown={name:e.name,message:e.message,typed:e instanceof AgentLoopResultUnknownError};}
 const suppressed=typed&&typeof runId==='string'&&reason===`timeout:${runId}`;
 assert.equal(!!thrown,suppressed,name);assert.equal(events.some(e=>e.type==='turn_failed'),!suppressed,name);
 await writer.close?.();rows.push({name,runId,reason,typed,suppressed,events,thrown,transcript:readFileSync(dir+'/events.jsonl','utf8')});
}
writeFileSync(out+'/result.json',JSON.stringify({baseline,node:{execPath:process.execPath,versions:process.versions},sources,rows,scope:'actual fixed TS TurnRunner and actual JSONL writer; mock loop only'},null,2));
