import assert from 'node:assert/strict';
import {readFileSync,writeFileSync,appendFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const [url,tokenFile,project,out,seedFile,phase]=process.argv.slice(2);assert.equal(new URL(url).hostname,'127.0.0.1');
const seed=JSON.parse(readFileSync(seedFile,'utf8'));let bytes=0;const Native=globalThis.WebSocket;
function wire(direction:string,raw:string){bytes+=Buffer.byteLength(raw);assert(bytes<8*1024*1024);appendFileSync(out+'/wire.jsonl',JSON.stringify({direction,raw})+'\n');}
globalThis.WebSocket=class extends Native{constructor(u:any,p?:any){super(u,p);this.addEventListener('message',e=>wire('receive',String(e.data)));}send(v:any){wire('send',String(v));super.send(v);}};
const {GatewayWsClient,RemoteGateway}=await import(pathToFileURL(process.env.PDRS_BASELINE+'/src/gateway/index.js').href);
const client=new GatewayWsClient({url,token:readFileSync(tokenFile,'utf8').trim(),clientName:'web'});const timer=setTimeout(()=>{client.close();throw Error('nonempty memory deadline')},45000);
const result:any={phase};
try{await client.connect();
 const listed=await client.request('memory_list',{projectKey:project});result.before=listed;assert(Array.isArray(listed.items));
 if(phase==='retrieve'||phase==='restart-wipe')assert(JSON.stringify(listed.items).includes(seed.record.relativePath),'seed persisted record is listed');
 if(phase==='restart-wipe'){
  result.wipe=await client.request('memory_wipe',{projectKey:project,scope:'project'});assert.equal(result.wipe.wiped,true);
  result.after=await client.request('memory_list',{projectKey:project});assert.equal(result.after.items.length,0);
 }else{
  if(phase==='wiped-restart')assert.equal(listed.items.length,0,'wipe remains durable after restart');
  const session=phase==='retrieve'?'web:memory-retrieve':'web:memory-empty-after-wipe';
  const events:any[]=[];for await(const event of new RemoteGateway(client).submitTurn({projectKey:project,sessionKey:session,runId:'memory-'+phase,channelKey:'web',message:'What deployment rule should I follow? MEMORY_QUERY',maxTurns:2,canPrompt:false}))events.push(event);
  assert(events.some(e=>e.type==='turn_completed'&&e.finishReason==='completed'));assert(!events.some(e=>e.type==='error'));
  await client.request('close_session',{projectKey:project,sessionKey:session});result.events=events;
 }
 result.pass=true;writeFileSync(out+'/driver-result.json',JSON.stringify(result,null,2)+'\n',{flag:'wx'});
}finally{clearTimeout(timer);client.close();}
