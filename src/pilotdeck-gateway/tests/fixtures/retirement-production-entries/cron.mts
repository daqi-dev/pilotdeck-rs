import assert from 'node:assert/strict';
import {appendFileSync,writeFileSync} from 'node:fs';
import {pathToFileURL} from 'node:url';
const [url,token,project,out]=process.argv.slice(2);const endpoint=new URL(url);assert(endpoint.protocol==='ws:'&&endpoint.hostname==='127.0.0.1'&&endpoint.pathname==='/ws'&&/^28\d{3}$/.test(endpoint.port));
const Native=globalThis.WebSocket;let seq=0,bytes=0;
function record(direction:string,raw:string){bytes+=Buffer.byteLength(raw);assert(bytes<8*1024*1024);appendFileSync(out+'/wire.jsonl',JSON.stringify({seq:++seq,direction,raw})+'\n');}
globalThis.WebSocket=class extends Native{constructor(url:any,p?:any){super(url,p);this.addEventListener('message',e=>record('receive',String(e.data)));}send(v:any){record('send',String(v));super.send(v);}};
const {GatewayWsClient}=await import(pathToFileURL(process.env.PDRS_BASELINE+'/src/gateway/index.js').href);
const client=new GatewayWsClient({url,token,clientName:'web'});const rows:any[]=[];const timer=setTimeout(()=>{client.close();throw Error('cron driver deadline')},95000);
try{await client.connect();assert(client.interactionBinding);const created=await client.request('cron_create',{projectKey:project,sessionKey:'web:entry-background-cron',channelKey:'web',message:'ENTRY_BACKGROUND_CRON',schedule:{type:'once',runAt:new Date(Date.now()+1500).toISOString()}});rows.push({method:'cron_create',value:created});
 const derived=created.task.sessionKey;assert(derived.startsWith('cron:'));const until=Date.now()+75000;let history:any;while(Date.now()<until){const tasks=await client.request('cron_list',{projectKey:project,includeHistory:true,limit:100});rows.push({method:'cron_list',value:tasks});try{history=await client.request('read_session_messages',{projectKey:project,sessionKey:derived,limit:1000});if(JSON.stringify(history).includes('ENTRY_RUNTIME_REPLY_cron')&&!history.stream?.active)break;}catch(error){rows.push({method:'read_session_messages',error:String(error)});}await new Promise(r=>setTimeout(r,100));}
 assert(JSON.stringify(history).includes('ENTRY_BACKGROUND_CRON'));assert(JSON.stringify(history).includes('ENTRY_RUNTIME_REPLY_cron'));assert(!history.stream?.active);writeFileSync(out+'/cron.json',JSON.stringify({pass:true,binding:client.interactionBinding,created,history,rows},null,2));
}catch(error){writeFileSync(out+'/cron-error.json',JSON.stringify({pass:false,error:String(error),rows},null,2));throw error;}finally{clearTimeout(timer);client.close();}
