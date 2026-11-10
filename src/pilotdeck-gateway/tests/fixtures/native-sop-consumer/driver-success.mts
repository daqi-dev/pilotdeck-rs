import assert from 'node:assert/strict';
import {appendFileSync,writeFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const [url,project,out]=process.argv.slice(2);const Native=globalThis.WebSocket;let seq=0;
globalThis.WebSocket=class extends Native {constructor(u:any,p?:any){super(u,p);this.addEventListener('message',e=>appendFileSync(out+'/wire.jsonl',JSON.stringify({seq:seq++,direction:'receive',raw:String(e.data)})+'\n'));}send(data:any){appendFileSync(out+'/wire.jsonl',JSON.stringify({seq:seq++,direction:'send',raw:String(data)})+'\n');super.send(data);}};
const {GatewayWsClient,RemoteGateway}=await import(pathToFileURL(process.env.PDRS_BASELINE+'/src/gateway/index.js').href);
const client=new GatewayWsClient({url,token:'test-token',clientName:'web'});const results:any={turns:[]};
try {await client.connect();const remote=new RemoteGateway(client);
 for(let i=0;i<5;i++){const events:any[]=[];for await(const event of remote.submitTurn({sessionKey:'web:sop-fixture',runId:'sop-compact-'+i,channelKey:'web',projectKey:project,message:'SOP_TRIGGER COMPACT_MESSAGE_'+i+' '+('independent evidence context '.repeat(180)),maxTurns:3}))events.push(event);results.turns.push(events);const terminal=events.filter(e=>e.type==='turn_completed');assert.equal(terminal.length,1);assert.equal(terminal[0].finishReason,'completed',JSON.stringify(events));assert(!events.some(e=>e.type==='error'),JSON.stringify(events));assert(!events.some(e=>e.type==='tool_call_finished'&&e.ok===false),JSON.stringify(events));if(i===0)assert(events.some(e=>e.type==='tool_call_finished'&&e.toolName==='submit_step_result'&&e.ok===true));}
 results.history=await client.request('read_session_messages',{sessionKey:'web:sop-fixture',projectKey:project,limit:1000});
 writeFileSync(out+'/result.json',JSON.stringify(results,null,2)+'\n',{flag:'wx'});
} finally {client.close();}
