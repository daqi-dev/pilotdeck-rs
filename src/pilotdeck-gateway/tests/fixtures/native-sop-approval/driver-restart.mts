import assert from 'node:assert/strict';
import {appendFileSync,writeFileSync,readFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const [url,project,out,phase,priorFile]=process.argv.slice(2);const Native=globalThis.WebSocket;let seq=0;
globalThis.WebSocket=class extends Native {constructor(u:any,p?:any){super(u,p);this.addEventListener('message',e=>appendFileSync(out+'/wire.jsonl',JSON.stringify({seq:seq++,direction:'receive',raw:String(e.data)})+'\n'));}send(data:any){appendFileSync(out+'/wire.jsonl',JSON.stringify({seq:seq++,direction:'send',raw:String(data)})+'\n');super.send(data);}};
const {GatewayWsClient,RemoteGateway}=await import(pathToFileURL(process.env.PDRS_BASELINE+'/src/gateway/index.js').href);
const client=new GatewayWsClient({url,token:'test-token',clientName:'web'});const results:any={};
const base={sessionKey:'web:sop-fixture',projectKey:project,tenantId:'tenant',agentId:'target'};
async function rejected(method:string,input:any,code:string){try{await client.request(method,input);assert.fail('expected '+code);}catch(e:any){assert.equal(e.code,code,String(e));return {code:e.code,message:e.message};}}
async function turn(remote:any,runId:string,message:string){const events:any[]=[];for await(const event of remote.submitTurn({...base,runId,channelKey:'web',message,maxTurns:3}))events.push(event);const terminal=events.filter(e=>e.type==='turn_completed');assert.equal(terminal.length,1,JSON.stringify(events));assert.equal(terminal[0].finishReason,'completed',JSON.stringify(events));assert(!events.some(e=>e.type==='error'||(e.type==='tool_call_finished'&&e.ok===false)),JSON.stringify(events));assert(events.some(e=>e.type==='tool_call_finished'&&e.toolName==='submit_step_result'&&e.ok===true));return events;}
try {await client.connect();const remote=new RemoteGateway(client);
 const prior=phase==='pause'?null:JSON.parse(readFileSync(priorFile,'utf8'));
 if(phase==='pause') {
  results.pause=await turn(remote,'sop-human-pause','SOP_TRIGGER Please request the fixture human approval.');
  results.status=await client.request('sop_status',{...base,approverAuthorization:'Bearer approve-only'});
  assert.equal(results.status.state.status,'handoff');
  assert.equal(results.status.wait.kind,'handoff');
  assert.equal(results.status.approval.assigneeUserId,'approver');
 } else {
  results.status=prior.status;
  results.reloaded=await client.request('sop_status',{...base,approverAuthorization:'Bearer approve-only'});
  if(phase==='resume') {
   assert.equal(results.reloaded.state.status,'handoff','new Server reloads original pending wait');
   assert.equal(results.reloaded.wait.id,prior.status.wait.id);
   assert.equal(results.reloaded.revision,prior.status.revision);
  } else {
   assert.equal(results.reloaded.state.status,'completed');
  }
 }
 const status=results.status;
 const resume={...base,approverAuthorization:'Bearer approve-only',source:'human',requestId:'human-reply-42',waitId:status.wait.id,expectedRevision:status.revision,message:'SOP_APPROVAL_DECISION_42',slotUpdates:{decision:'SOP_APPROVAL_DECISION_42'}};
 results.noAuth=await rejected('sop_resume',{...resume,approverAuthorization:undefined},'APPROVAL_AUTH_REQUIRED');
 results.foreign=await rejected('sop_status',{...base,approverAuthorization:'Bearer foreign-only'},'APPROVAL_SUBJECT_MISMATCH');
 const before=await client.request('sop_status',{...base,approverAuthorization:'Bearer approve-only'});
 assert.equal(before.revision,phase==='replay'?10:status.revision,'rejected requests leave durable state unchanged');
 if(phase!=='pause') {
  results.resume=await client.request('sop_resume',resume);
  results.replay=await client.request('sop_resume',resume);
  if(phase==='resume') {
   results.active=await client.request('sop_status',{...base,approverAuthorization:'Bearer approve-only'});
   assert.equal(results.active.state.status,'active');
   assert.equal(results.active.state.slots_json.decision,'SOP_APPROVAL_DECISION_42');
   assert.equal(results.active.revision,status.revision+1,'receipt replay cannot mutate twice');
   results.finish=await turn(remote,'sop-human-finish','SOP_TRIGGER Continue the authorized SOP_APPROVAL_DECISION_42 decision.');
  }
  results.final=await client.request('sop_status',{...base,approverAuthorization:'Bearer approve-only'});
  assert.equal(results.final.state.status,'completed');
  assert.equal(results.final.revision,10,'restart receipt replay cannot mutate completed state');
 }
 results.history=await client.request('read_session_messages',{sessionKey:base.sessionKey,projectKey:project,limit:1000});
}finally{writeFileSync(out+'/result.json',JSON.stringify(results,null,2)+'\n',{flag:'wx'});client.close();}
