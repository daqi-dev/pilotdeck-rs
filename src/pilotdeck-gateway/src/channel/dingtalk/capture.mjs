// Approved #1342 / #1786 minimal used-member SDK stub; actual fixed TS adapter.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {createRequire} from 'node:module';
import {createServer} from 'node:http';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const [baseline,out]=process.argv.slice(2);assert(baseline&&out);await fs.mkdir(out,{recursive:false});
const require=createRequire(baseline+'/package.json');const {WebSocket,WebSocketServer}=require('ws');
const requests=[],acks=[],calls=[],logs=[];let socket;let release;let entered=0;
const server=createServer(async(req,res)=>{let body='';for await(const b of req)body+=b;const parsed=JSON.parse(body);requests.push({path:req.url,body:parsed});if(req.url==='/open'){res.setHeader('Content-Type','application/json');res.end(JSON.stringify({endpoint:'ws://127.0.0.1:'+server.address().port+'/stream',ticket:'owned-ticket'}));}else {res.statusCode=parsed.markdown.text==='FAIL'?500:200;res.end(res.statusCode===200?'ok':'owned fail');}});
const wss=new WebSocketServer({noServer:true});server.on('upgrade',(req,s,h)=>wss.handleUpgrade(req,s,h,ws=>{socket=ws;ws.on('message',raw=>acks.push(JSON.parse(String(raw))));wss.emit('connection',ws);}));await new Promise(r=>server.listen(0,'127.0.0.1',r));
const base='http://127.0.0.1:'+server.address().port;const realFetch=globalThis.fetch;
// Keep fixed adapter's production origin check intact, redirect only transport after check.
globalThis.fetch=(url,options)=>realFetch(String(url).startsWith('https://api.dingtalk.com/')?base+'/reply':url,options);
class DWClient{
 constructor(options){this.options=options;}
 registerAllEventListener(listener){this.listener=listener;}
 async connect(){const r=await realFetch(base+'/open',{method:'POST',headers:{'Content-Type':'application/json','Accept':'application/json'},body:JSON.stringify({clientId:this.options.clientId,clientSecret:this.options.clientSecret,ua:'',subscriptions:[{type:'EVENT',topic:'*'}]})});const {endpoint,ticket}=await r.json();this.socket=new WebSocket(endpoint+'?ticket='+ticket);await new Promise((resolve,reject)=>{this.socket.once('open',resolve);this.socket.once('error',reject);});this.socket.on('message',raw=>{const msg=JSON.parse(String(raw));if(msg.type==='EVENT'){const ack=this.listener(msg);this.socket.send(JSON.stringify({code:200,headers:{contentType:'application/json',messageId:msg.headers.messageId},message:'OK',data:JSON.stringify(ack)}));}});}
 disconnect(){this.socket?.close();}
}
globalThis.require=name=>{if(name==='dingtalk-stream')return {DWClient,EventAck:{SUCCESS:'SUCCESS'},TOPIC_ROBOT:'/v1.0/im/bot/messages/get'};return require(name);};
const root=baseline+'/src/adapters/channel/dingtalk/';const {DingTalkChannel}=await import(pathToFileURL(root+'DingTalkChannel.ts'));const {DingTalkSessionMapper}=await import(pathToFileURL(root+'DingTalkSessionMapper.ts'));const {renderDingTalkEvent}=await import(pathToFileURL(root+'dingtalk-render.ts'));
const wait=async(fn,label)=>{const deadline=Date.now()+4000;while(Date.now()<deadline){if(fn())return;await new Promise(r=>setTimeout(r,5));}throw Error('timeout '+label);};
const uuid='00000000-0000-0000-0000-000000000001';const map=new DingTalkSessionMapper(undefined,()=>uuid);const mapper=['hello','/new','/new next','/newish',' /new\nnext '].map(text=>({text,result:map.resolve({chatId:'room',text})}));
const render=[{type:'assistant_text_delta',text:'正文'},{type:'tool_call_finished',ok:false,toolName:'bash'},{type:'error',message:'oops'},{type:'turn_completed'}].map(event=>({event,result:renderDingTalkEvent(event)??null}));
const gateway={async *submitTurn(r){calls.push(r);entered++;await new Promise(r=>release=r);yield {type:'assistant_text_delta',text:' REPLY '};yield {type:'tool_call_finished',ok:false,toolName:'bash'};}};
const channel=new DingTalkChannel({clientId:' owned-id ',clientSecret:' owned-secret ',mapper:new DingTalkSessionMapper(undefined,()=>uuid)});const logger=Object.fromEntries(['info','warn','error'].map(level=>[level,message=>logs.push({level,message:String(message)})]));const handle=await channel.start({gateway,logger});
let seq=0;const send=(data,topic='/v1.0/im/bot/messages/get')=>{const messageId='m'+(++seq);socket.send(JSON.stringify({specVersion:'1.0',type:'EVENT',headers:{topic,messageId},data:JSON.stringify(data)}));};
const event=(id,text,extra={})=>({msgId:id,conversationId:'room',text,sessionWebhook:'https://api.dingtalk.com/mock-hook',...extra});
send(event('ignored','drop'),'other');send(event('new','/new'));await wait(()=>requests.length===2,'new reply');
send(event('first','hello'));await wait(()=>entered===1,'turn');send(event('first','repeated'));send(event('busy','busy'));await new Promise(r=>setTimeout(r,30));assert.equal(calls.length,1);release();await wait(()=>requests.length===3,'turn reply');
const cron={taskId:'t',runId:'r',sessionKey:'dingtalk:chat=room:general',channelKey:'dingtalk',text:'CRON'};const cronOk=await channel.deliverCronResult(cron);const cronFail=await channel.deliverCronResult({...cron,text:'FAIL'});assert.equal(cronOk,true);assert.equal(cronFail,false);
send(event('second',{content:'inflight'}));await wait(()=>entered===2,'second turn');await handle.stop('owned-stop');release();await new Promise(r=>setTimeout(r,30));const stoppedCron=await channel.deliverCronResult(cron);assert.equal(stoppedCron,false);
const source={};for(const f of ['DingTalkChannel.ts','DingTalkSessionMapper.ts','dingtalk-render.ts'])source[f]=createHash('sha256').update(await fs.readFile(root+f)).digest('hex');await fs.writeFile(out+'/oracle.json',JSON.stringify({baseline:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',environment:process.versions,source,mapper,render,requests,acks,calls,logs,cronOk,cronFail,stoppedCron},null,2)+'\n');
for(const ws of wss.clients)ws.terminate();wss.close();await new Promise(r=>server.close(r));console.log(JSON.stringify({requests:requests.length,acks:acks.length,calls:calls.length,cronOk,cronFail,stoppedCron}));
