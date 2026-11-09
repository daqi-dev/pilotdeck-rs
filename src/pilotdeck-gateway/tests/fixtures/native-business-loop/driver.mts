import assert from 'node:assert/strict';
import { appendFileSync, writeFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';
import { parseArgs } from 'node:util';
const {values:a}=parseArgs({options:Object.fromEntries(['url','token','project','file','out','phase'].map(x=>[x,{type:'string'}]))});
for(const k of ['url','token','project','file','out','phase'])assert(a[k],k);
const u=new URL(a.url!);assert(u.hostname==='127.0.0.1'&&u.protocol==='ws:'&&Number(u.port)>=28000&&Number(u.port)<=28999);
let seq=0;const Native=globalThis.WebSocket;let bytes=0;
globalThis.WebSocket=class extends Native{
 constructor(url:any,protocols?:any){super(url,protocols);this.addEventListener('message',e=>record('receive',String(e.data)));this.addEventListener('close',e=>record('close',JSON.stringify({code:e.code,reason:e.reason})));}
 send(data:any){record('send',String(data));super.send(data);}
};
function record(direction:string,raw:string){bytes+=Buffer.byteLength(raw);assert(bytes<16*1024*1024,'wire budget');appendFileSync(a.out+'/wire.jsonl',JSON.stringify({seq:seq++,monotonicNs:process.hrtime.bigint().toString(),direction,raw})+'\n');}
const {GatewayWsClient,RemoteGateway}=await import(pathToFileURL(process.env.PDRS_BASELINE+'/src/gateway/index.js').href);
const c=new GatewayWsClient({url:a.url,token:a.token,clientName:'web'});const results:any={phase:a.phase};
const wait=async(f:()=>any,label:string)=>{const end=Date.now()+20000;while(Date.now()<end){const v=f();if(v)return v;await new Promise(r=>setTimeout(r,20));}throw Error('timeout '+label);};
try{
 await c.connect();assert(c.interactionBinding,'hello binding');
 if(a.phase==='warm'){
  const remote=new RemoteGateway(c);const events:any[]=[];
  for await(const e of remote.submitTurn({sessionKey:'web:business-read',runId:'business-read-1',channelKey:'web',projectKey:a.project,message:'BUSINESS_READ '+a.file,maxTurns:4}))events.push(e);
  assert(events.some(e=>e.type==='turn_completed'),'read terminal');assert(!events.some(e=>e.type==='error'),JSON.stringify(events));
  assert(events.some(e=>e.type==='tool_call_finished'&&e.toolName==='read_file'&&e.ok===true),'actual successful read_file');
  assert(JSON.stringify(events).includes('BUSINESS_FILE_CONTENT_42'),'actual fixture content in events');results.read=events;
  const permissions:any[]=[];let settled=false;
  const turn=(async()=>{for await(const e of remote.submitTurn({sessionKey:'web:business-permission',runId:'business-deny-1',channelKey:'web',projectKey:a.project,message:'BUSINESS_PERMISSION',permissionMode:'default',maxTurns:4}))permissions.push(e);settled=true;})();
  const q=await wait(()=>permissions.find(e=>e.type==='permission_request'),'permission_request');
  const answer=await c.request('permission_decide',{sessionKey:'web:business-permission',requestId:q.requestId,decision:'deny'});assert.equal(answer.delivered,true);
  await turn;assert(settled);assert(permissions.some(e=>e.type==='turn_completed'),'deny terminal');
  assert(permissions.some(e=>e.type==='tool_call_finished'&&e.toolName==='bash'&&e.ok===false),'denied bash result');results.permission={events:permissions,answer};
  await c.request('close_session',{sessionKey:'web:business-read',projectKey:a.project});
  await c.request('close_session',{sessionKey:'web:business-permission',projectKey:a.project});
 }
 const history=await c.request('read_session_messages',{sessionKey:'web:business-read',projectKey:a.project,limit:1000});
 assert(JSON.stringify(history).includes('BUSINESS_FILE_CONTENT_42'),'durable history marker');
 assert(!history.stream?.active,'history idle');results.history=history;
 results.pass=true;writeFileSync(a.out+'/result.json',JSON.stringify(results,null,2)+'\n');
}finally{c.close();}
