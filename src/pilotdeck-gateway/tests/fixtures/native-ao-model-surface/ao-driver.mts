import {writeFileSync,appendFileSync} from 'node:fs';import {pathToFileURL} from 'node:url';import assert from 'node:assert/strict';
const [url,project,out]=process.argv.slice(2);const Native=globalThis.WebSocket;const frames:any[]=[];
globalThis.WebSocket=class extends Native{constructor(u:any,p?:any){super(u,p);this.addEventListener('message',e=>{const raw=String(e.data);frames.push(JSON.parse(raw));appendFileSync(out+'/wire.jsonl',JSON.stringify({direction:'receive',raw})+'\n');});}send(data:any){appendFileSync(out+'/wire.jsonl',JSON.stringify({direction:'send',raw:String(data)})+'\n');super.send(data);}};
const {GatewayWsClient}=await import(pathToFileURL(process.env.PDRS_BASELINE+'/src/gateway/index.js').href);
const c=new GatewayWsClient({url,token:'test-token',clientName:'web'});let result:any={};try{
 await c.connect();await new Promise(resolve=>setTimeout(resolve,6000));result.rerun=await c.request('always_on_rerun_plan',{projectKey:project,planId:'suite-plan'});
 assert(result.rerun.runId,'missing run id');assert(!result.rerun.error,JSON.stringify(result.rerun));
 assert(frames.some(x=>x.type==='notification'&&x.name==='always-on:turn-event'),'no real automation turn notification');
 result.notifications=frames.filter(x=>x.type==='notification'&&x.name==='always-on:turn-event');
}finally{c.close();writeFileSync(out+'/result.json',JSON.stringify(result,null,2)+'\n',{flag:'wx'});}
