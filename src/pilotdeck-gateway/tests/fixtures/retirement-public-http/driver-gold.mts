import assert from 'node:assert/strict';
import {appendFileSync,writeFileSync,readFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const [url,project,out,phase,priorFile]=process.argv.slice(2);const Native=globalThis.WebSocket;let seq=0;
globalThis.WebSocket=class extends Native {constructor(u:any,p?:any){super(u,p);this.addEventListener('message',e=>appendFileSync(out+'/wire.jsonl',JSON.stringify({seq:seq++,direction:'receive',raw:String(e.data)})+'\n'));}send(data:any){appendFileSync(out+'/wire.jsonl',JSON.stringify({seq:seq++,direction:'send',raw:String(data)})+'\n');super.send(data);}};
const {GatewayWsClient,RemoteGateway}=await import(pathToFileURL(process.env.PDRS_BASELINE+'/src/gateway/index.js').href);
const client=new GatewayWsClient({url,token:'test-token',clientName:'web'});const results:any={};
const approvalRows:any[]=[];results.approvalRows=approvalRows;
async function approval(label:string,path:string,body:any,expected=200,approver:any='Bearer approve-only'){
 const clean={...body};delete clean.approverAuthorization;
 const serialized=JSON.stringify(clean);const headers:any={authorization:'Bearer test-token','content-type':'application/json'};
 if(approver!==null)headers['x-staffdeck-approver-authorization']=approver;
 const response=await fetch(new URL(path,url.replace('ws://','http://')),{method:'POST',headers,body:serialized});
 const raw=Buffer.from(await response.arrayBuffer());const text=raw.toString();
 approvalRows.push({label,path,requestBody:serialized,requestBodyHex:Buffer.from(serialized).toString('hex'),status:response.status,headers:Object.fromEntries(response.headers),body:text,responseBodyHex:raw.toString('hex')});
 assert.equal(response.status,expected,text);assert(response.headers.get('content-type')?.includes('application/json'),text);
 const parsed=text?JSON.parse(text):null;return path.endsWith('/status')&&expected===200?parsed.status:parsed;
}
const base={sessionKey:'web:sop-fixture',projectKey:project,tenantId:'tenant',agentId:'target'};
async function rejected(method:string,input:any,code:string){try{await client.request(method,input);assert.fail('expected '+code);}catch(e:any){assert.equal(e.code,code,String(e));return {code:e.code,message:e.message};}}
async function turn(remote:any,runId:string,message:string){const events:any[]=[];for await(const event of remote.submitTurn({...base,runId,channelKey:'web',message,maxTurns:3}))events.push(event);const terminal=events.filter(e=>e.type==='turn_completed');assert.equal(terminal.length,1,JSON.stringify(events));assert.equal(terminal[0].finishReason,'completed',JSON.stringify(events));assert(!events.some(e=>e.type==='error'||(e.type==='tool_call_finished'&&e.ok===false)),JSON.stringify(events));assert(events.some(e=>e.type==='tool_call_finished'&&e.toolName==='submit_step_result'&&e.ok===true));return events;}
try {await client.connect();const remote=new RemoteGateway(client);
 const prior=phase==='pause'?null:JSON.parse(readFileSync(priorFile,'utf8'));
 if(phase==='pause') {
  // This HTTP profile starts from the source-bound persisted wait seed; A7 is independently witnessed.
  results.seededWait=true;
  results.status=await approval('status-configured','/api/module-host/approvals/status',base);
  assert.equal(results.status.state.status,'handoff');
  assert.equal(results.status.wait.kind,'handoff');
  assert.equal(results.status.approval.assigneeUserId,'approver');
 } else {
  results.status=prior.status;
  results.reloaded=await approval('status-configured','/api/module-host/approvals/status',base);
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
 results.noAuth=await approval('resume-missing-auth','/api/module-host/approvals/resume',resume,401,null);assert.equal(results.noAuth.code,'APPROVAL_AUTH_REQUIRED');
 results.foreign=await approval('status-foreign-actor','/api/module-host/approvals/status',base,403,'Bearer foreign-only');assert.equal(results.foreign.code,'APPROVAL_SUBJECT_MISMATCH');
 const before=await approval('status-configured','/api/module-host/approvals/status',base);
 assert.equal(before.revision,phase==='replay'?10:status.revision,'rejected requests leave durable state unchanged');
 if(phase!=='pause') {
  results.resume=await approval('resume-configured','/api/module-host/approvals/resume',resume);
  results.replay=await approval('resume-configured','/api/module-host/approvals/resume',resume);
  if(phase==='resume') {
   results.active=await approval('status-configured','/api/module-host/approvals/status',base);
   assert.equal(results.active.state.status,'active');
   assert.equal(results.active.state.slots_json.decision,'SOP_APPROVAL_DECISION_42');
   assert.equal(results.active.revision,status.revision+1,'receipt replay cannot mutate twice');
   results.finish=await turn(remote,'sop-human-finish','SOP_TRIGGER Continue the authorized SOP_APPROVAL_DECISION_42 decision.');
  }
  results.final=await approval('status-configured','/api/module-host/approvals/status',base);
  assert.equal(results.final.state.status,'completed');
  assert.equal(results.final.revision,10,'restart receipt replay cannot mutate completed state');
 }
 const principal={pilotDeckUserId:'pilot-owner',tenantId:'tenant',actorUserId:'actor',agentId:'target'};
 const hostRows:any[]=[];results.hostRows=hostRows;
 async function host(label:string,path:string,body:any,method='POST',authorization='Bearer test-token'){
  const serialized=JSON.stringify(body);const endpoint=new URL(path,url.replace('ws://','http://'));const response=await fetch(endpoint,{method,headers:{authorization,'content-type':'application/json'},...(method==='POST'?{body:serialized}:{})});const bytes=new Uint8Array(await response.arrayBuffer());const text=Buffer.from(bytes).toString('utf8');const row={label,path,method,requestBody:method==='POST'?serialized:'',requestBodyHex:Buffer.from(method==='POST'?serialized:'').toString('hex'),status:response.status,headers:Object.fromEntries(response.headers),body:text,responseBodyHex:Buffer.from(bytes).toString('hex')};hostRows.push(row);return row;
 }
 async function call(label:string,operation:string,input:any,expected:number){const row=await host(label,'/api/module-host/call',{principal,operation,input});assert.equal(row.status,expected,JSON.stringify(row));return row;}
 const described=await host('describe-configured','/api/module-host/describe',{principal});assert.equal(described.status,200);const operations=JSON.parse(described.body).operations;assert.deepEqual([...operations].sort(),['list_general_skills','list_model_catalog','model_prepare','model_stream','file_parse','read_sop_authority'].sort());
 const skillsRow=await call('skills-success','list_general_skills',{},200);const skills=JSON.parse(skillsRow.body);
 assert.equal(skills.data.length,10);assert.equal(skills.items.length,10);assert.equal(skills.builtin.length,32);assert.equal(skills.user.length,0);assert.equal(skills.project.length,0);assert.deepEqual(skills.data,skills.items);assert.equal(typeof skills.nextCursor,'string');assert.equal(skills.next_cursor,skills.nextCursor);
 const cursor=JSON.parse(Buffer.from(skills.nextCursor,'base64url').toString());assert.equal(cursor.offset,10);assert.equal(typeof cursor.signature,'string');const signature=JSON.parse(Buffer.from(cursor.signature,'base64url').toString());assert.equal(signature.projectKey,project);assert.equal(signature.query,'');assert.equal(signature.scope,'all');
 results.skillsShape={general:skills.data.length,builtin:skills.builtin.length,keys:Object.keys(skills),cursor,signature};
 const catalog=JSON.parse((await call('catalog-success','list_model_catalog',{},200)).body);const selected=catalog.data.find((item:any)=>item.available);assert(selected,JSON.stringify(catalog));
 const modelInput={requestId:'host-model-42',modelId:selected.id,request:{messages:[{role:'user',content:[{type:'text',text:'HOST_MODEL_42'}]}],maxOutputTokens:32},budget:{maxOutputTokens:32,timeoutMs:5000}};
 await call('model-prepare-success','model_prepare',modelInput,200);
 const streamed=await call('model-stream-success','model_stream',modelInput,200);assert(streamed.body.includes('BUSINESS_DENY_DONE'),streamed.body);
 await call('model-prepare-missing-id','model_prepare',{...modelInput,modelId:undefined},400);
 await call('model-prepare-unknown-model','model_prepare',{...modelInput,modelId:'fixture/absent'},400);
 await call('model-prepare-invalid-messages','model_prepare',{...modelInput,request:{messages:[]}},400);
 await call('model-prepare-budget','model_prepare',{...modelInput,budget:{maxOutputTokens:0}},400);
 await call('model-stream-invalid-messages','model_stream',{...modelInput,request:{messages:[]}},400);
 const parsed=await call('file-text-success','file_parse',{filename:'entry.txt',content_base64:Buffer.from('HOST_TEXT_42\n').toString('base64')},200);assert.equal(JSON.parse(parsed.body).text,'HOST_TEXT_42\n');
 await call('file-markdown-success','file_parse',{filename:'entry.md',media_type:'text/markdown',content_base64:Buffer.from('# HOST_MARKDOWN_42\n').toString('base64')},200);
 await call('file-invalid-base64','file_parse',{filename:'entry.txt',content_base64:'$$'},400);
 await call('file-unsupported','file_parse',{filename:'entry.bin',content_base64:'NDI='},415);
 await call('file-too-large','file_parse',{filename:'entry.txt',content_base64:'NDI=',max_bytes:1},413);
 await call('file-invalid-utf8','file_parse',{filename:'entry.txt',content_base64:'/w=='},422);
 await call('file-invalid-pdf','file_parse',{filename:'entry.pdf',content_base64:'NDI='},422);
 const inactiveInput={sessionKey:base.sessionKey,projectKey:project,requestId:'host-sop-inactive-42',expectedRevision:status.revision,admissionCredentialId:'credential'};
 await call('sop-authority-handoff','read_sop_authority',inactiveInput,403);
 results.hostResume=await approval('resume-configured','/api/module-host/approvals/resume',resume);results.hostReplay=await approval('resume-replay','/api/module-host/approvals/resume',resume);assert.equal(results.hostResume.accepted,true);assert.equal(results.hostResume.duplicate,false);assert.equal(results.hostReplay.duplicate,true);assert.deepEqual(results.hostReplay,{...results.hostResume,duplicate:true});
 const hostActive=await approval('status-configured','/api/module-host/approvals/status',base);assert.equal(hostActive.state.status,'active');assert.equal(hostActive.revision,status.revision+1,'HTTP receipt replay mutates durable revision once');
 const sopInput={...inactiveInput,requestId:'host-sop-42',expectedRevision:hostActive.revision};
 const authority=await call('sop-authority-success','read_sop_authority',sopInput,200);assert.equal(JSON.parse(authority.body).sopId,'fixture');
 await call('sop-authority-invalid','read_sop_authority',{},403);
 await call('sop-authority-revision','read_sop_authority',{...sopInput,expectedRevision:hostActive.revision+1},409);
 await call('sop-authority-admission','read_sop_authority',{...sopInput,admissionCredentialId:'foreign'},403);
 await call('sop-authority-foreign-session','read_sop_authority',{...sopInput,sessionKey:'web:missing'},403);
 await call('scope-override','list_general_skills',{tenantId:'foreign'},400);
 await call('unadvertised-tool','list_tools',{},409);
 await host('describe-missing-principal','/api/module-host/describe',{});
 await host('describe-get','/api/module-host/describe',{},'GET');
 await host('describe-unauthorized','/api/module-host/describe',{principal},'POST','Bearer foreign');
 results.history=await client.request('read_session_messages',{sessionKey:base.sessionKey,projectKey:project,limit:1000});
}finally{writeFileSync(out+'/result.json',JSON.stringify(results,null,2)+'\n',{flag:'wx'});client.close();}
