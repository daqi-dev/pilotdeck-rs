// Real fixed TS class + real ws loopback. No SDK stub or product source edits.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {createRequire} from 'node:module';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const [baseline,out]=process.argv.slice(2);assert(baseline&&out);await fs.mkdir(out,{recursive:false});
const require=createRequire(baseline+'/package.json');const {WebSocketServer}=require('ws');
const root=baseline+'/src/adapters/channel/homeassistant/';
const {HomeAssistantChannel}=await import(pathToFileURL(root+'HomeAssistantChannel.ts'));
const {HomeAssistantSessionMapper}=await import(pathToFileURL(root+'HomeAssistantSessionMapper.ts'));
const {renderHomeAssistantEvent}=await import(pathToFileURL(root+'homeassistant-render.ts'));
const uuid='00000000-0000-0000-0000-000000000001';
const mapped=new HomeAssistantSessionMapper(undefined,()=>uuid);
const mapperInputs=[' hello ','/new','/new next','/newish',' /new\nnext '];
const mapper=mapperInputs.map(text=>({text,result:mapped.resolve({chatId:'conversation.room',text})}));
const renderInputs=[{type:'assistant_text_delta',text:'正文'},{type:'assistant_thinking_delta',text:'hidden'},{type:'tool_call_started'},{type:'tool_call_finished',ok:true},{type:'tool_call_finished',ok:false,toolName:'bash'},{type:'error',message:'oops'},{type:'elicitation_request',questions:[{header:'H',question:'Q',options:[{label:'A'},{label:'B'}]}]},{type:'turn_completed'}];
const render=renderInputs.map(event=>({event,result:renderHomeAssistantEvent(event)??null}));
let socket;const requests=[];let active=false;let release;let entered=0;const calls=[];const logs=[];const denied=[];
const wait=async(fn,label)=>{const until=Date.now.real()+4000;while(Date.now.real()<until){if(fn())return;await new Promise(r=>setTimeout(r,5));}throw Error('timeout '+label);};
const real=Date.now;Date.now=()=>1000;Date.now.real=real;
const server=new WebSocketServer({host:'127.0.0.1',port:0});await new Promise(r=>server.on('listening',r));
server.on('connection',ws=>{socket=ws;ws.send(JSON.stringify({type:'auth_required'}));ws.on('message',raw=>{const m=JSON.parse(String(raw));requests.push(m);if(m.type==='auth')ws.send(JSON.stringify({type:'auth_ok'}));if(m.type==='call_service'&&m.service_data.message!=='NO_ACK')ws.send(JSON.stringify({type:'result',id:m.id,success:m.service_data.message!=='FAIL_ACK'}));});});
const gateway={
 async *submitTurn(request){calls.push(request);entered++;await new Promise(r=>release=r);yield {type:'assistant_text_delta',text:' HA_REPLY '};yield {type:'tool_call_finished',ok:false,toolName:'bash'};},
 async respondElicitation(r){throw Error('unexpected elicitation');},async permissionDecide(r){denied.push(r);return {delivered:true};}
};
const channel=new HomeAssistantChannel({url:'http://127.0.0.1:'+server.address().port,token:'owned-ha-token',mapper:new HomeAssistantSessionMapper(undefined,()=>uuid)});
const logger=Object.fromEntries(['info','warn','error'].map(level=>[level,message=>logs.push({level,message:String(message).replace(/ws:\/\/127\.0\.0\.1:\d+/g,'ws://<MOCK>')})]));
const state=(entity,value,old='prior')=>socket.send(JSON.stringify({type:'event',event:{event_type:'state_changed',data:{entity_id:entity,old_state:{state:old},new_state:{state:value}}}}));
const handle=await channel.start({gateway,logger});
state('sensor.ignored','drop');state('conversation.room','same','same');state('conversation.room','/new');
await wait(()=>requests.filter(x=>x.type==='call_service').length===1,'new acknowledgement');
state('conversation.room','hello');await wait(()=>entered===1,'turn');state('conversation.room','duplicate');await new Promise(r=>setTimeout(r,30));assert.equal(calls.length,1);release();
await wait(()=>requests.filter(x=>x.type==='call_service').length===2,'reply');
const cron={taskId:'t',runId:'r',sessionKey:'homeassistant:chat=conversation.room:general',channelKey:'homeassistant',text:'CRON'};
const cronOk=await channel.deliverCronResult(cron);const cronFail=await channel.deliverCronResult({...cron,text:'FAIL_ACK'});
const pending=channel.deliverCronResult({...cron,text:'NO_ACK'});await wait(()=>requests.some(x=>x.service_data?.message==='NO_ACK'),'pending receipt');
state('conversation.room','inflight');await wait(()=>entered===2,'inflight');await handle.stop('owned-stop');const pendingStopped=await pending;const activeAtStop=entered===2;
release();await new Promise(r=>setTimeout(r,30));
await new Promise(r=>server.close(r));Date.now=real;
const source={};for(const file of ['HomeAssistantChannel.ts','HomeAssistantSessionMapper.ts','homeassistant-render.ts'])source[file]=createHash('sha256').update(await fs.readFile(root+file)).digest('hex');
const result={baseline:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',environment:process.versions,source,mapper,render,requests,calls,logs,cronOk,cronFail,pendingStopped,activeAtStop};
await fs.writeFile(out+'/oracle.json',JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify({requests:requests.length,calls:calls.length,cronOk,cronFail,pendingStopped}));
