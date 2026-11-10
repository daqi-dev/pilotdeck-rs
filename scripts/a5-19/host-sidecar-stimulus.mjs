/** Fixture for the ORIGINAL TS CLI's public PILOTDECK_AGENT_LOOP_FACTORY seam.
 * The server, protocol, ledger and stdio ownership remain upstream production code.
 * This is the #1371 loop.json/release.json contract, not a sidecar server mock.
 */
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
const directory=process.env.PILOTDECK_NATIVE_LOOP_STIMULUS;
assert.ok(directory,'existing #1371 control directory required');
const plan=JSON.parse(fs.readFileSync(join(directory,'loop.json'),'utf8'));
assert.equal(typeof plan.id,'string');
assert.ok(['model','tool'].includes(plan.phase));
assert.ok(['timeout-first','unknown-first','known-final-first','after-complete','known-final-before-timeout-pending'].includes(plan.order));
assert.ok(Number.isSafeInteger(plan.boundMs)&&plan.boundMs>0&&plan.boundMs<=120000);
const baseline=process.env.A19_HOST_BASELINE;
assert.ok(baseline,'fixed upstream baseline required');
const {AgentLoopResultUnknownError}=await import(pathToFileURL(join(baseline,'src/agent/modules/transport/operationLedger.ts')).href);
let sequence=0;
function publish(phase,key,extra={}){
 const row={pid:process.pid,seq:sequence++,site:'loop',phase,key,...extra};
 const path=join(directory,'witness',`${String(process.pid).padStart(8,'0')}-${String(row.seq).padStart(6,'0')}.json`);
 fs.mkdirSync(join(directory,'witness'),{recursive:true});
 fs.writeFileSync(path+'.tmp',JSON.stringify(row)+'\n');fs.renameSync(path+'.tmp',path);
}
async function release(stage){
 const end=performance.now()+plan.boundMs;
 while(performance.now()<end){
  try{const value=JSON.parse(fs.readFileSync(join(directory,'release.json'),'utf8'));if(value.id===plan.id&&value.stage===stage)return value}catch(error){if(error.code!=='ENOENT')throw error}
  await delay(2);
 }
 publish('bound',stage);throw Error(`#1371 controlled loop ${stage} bound expired without a release.`);
}
export default function({request,abortSignal}){
 const sessionId=String(request.sessionId??request.operationId),turnId=String(request.turnId??request.operationId);
 const input={sessionId,turnId,messages:request.payload?.messages??[],abortSignal};
 return {input,loop:{snapshotFileState:()=>({}),async *run(value){
  publish('input',plan.id,{planPhase:plan.phase,planOrder:plan.order});
  yield{type:'model_request_started',sessionId,turnId,provider:'fixture',model:'controlled'};
  if(plan.phase==='tool')yield{type:'tool_calls_detected',sessionId,turnId,calls:[{id:'tool-1',name:'controlled_wait',input:{}}]};
  publish('wait',plan.id,{planPhase:plan.phase});
  const answer=await release('outcome');
  if(answer.outcome==='unknown'){
   publish('final','result_unknown',{code:'DEADLINE_EXCEEDED',aborted:abortSignal.aborted});
   throw new AgentLoopResultUnknownError();
  }
  assert.equal(answer.outcome,'known');
  const time=process.env.A19_HOST_TIME;
  assert.ok(time,'registered fixture time required');
  const result={type:'success',sessionId,turnId,stopReason:'completed',usage:{},permissionDenials:[],turns:1,startedAt:time,completedAt:time};
  publish('final','completed',{result});
  yield{type:'turn_completed',sessionId,turnId,result};
  if(plan.order==='known-final-before-timeout-pending'){
   publish('return','block');await release('return');publish('return','release');
  }
  return{result,messages:value.messages};
 }}};
}
