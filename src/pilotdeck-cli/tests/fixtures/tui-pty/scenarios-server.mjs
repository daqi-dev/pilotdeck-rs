import {createServer} from 'node:http';
import {createRequire} from 'node:module';
import {appendFileSync} from 'node:fs';
import {join,resolve} from 'node:path';
const [base,out,scenario]=process.argv.slice(2), require=createRequire(join(resolve(base),'package.json'));
const {WebSocketServer}=require('ws'), log=x=>appendFileSync(out,JSON.stringify(x)+'\n');
const server=createServer((req,res)=>{log({http:req.url});res.writeHead(req.url==='/health'?200:404);res.end('{}')});
const wss=new WebSocketServer({server});let count=0,streamId,ws;
const send=f=>{log({out:f});ws.send(JSON.stringify(f))};
wss.on('connection',socket=>{ws=socket;ws.on('message',data=>{const f=JSON.parse(String(data));log({in:f});
if(f.type==='hello')send({type:'hello_ok',serverInfo:{capabilities:[]}});
else if(f.method==='list_sessions')send({type:'response',id:f.id,ok:true,result:{sessions:scenario==='switch'?[{sessionKey:'saved-session',summary:'Saved'}]:[]}});
else if(f.method==='resume_session' && scenario==='switch') {}
else if(f.method==='read_session_messages' && scenario==='switch')send({type:'response',id:f.id,ok:true,result:{messages:[{role:'tool',kind:'tool_result',ok:false,toolCallId:'t1',toolName:'bash'}]}});
else if(f.method==='new_session')setTimeout(()=>send({type:'response',id:f.id,ok:true,result:{sessionKey:'new-session'}}),1200);
else if(f.method==='abort_turn' && ['abort-late','abort-timeout'].includes(scenario)) {send({type:'event',id:streamId,final:false,event:{type:'turn_completed'}});send({type:'event',id:streamId,final:true});if(scenario==='abort-late')setTimeout(()=>send({type:'response',id:f.id,ok:true,result:{ok:true}}),2000);}
else if(f.method==='abort_turn') {send({type:'response',id:f.id,ok:true,result:{ok:true}});send({type:'event',id:streamId,final:false,event:{type:'turn_completed'}});send({type:'event',id:streamId,final:true});}
else if(f.method==='permission_decide' && scenario==='pending-budget') {let n=0;const tick=()=>{if(ws.readyState!==ws.OPEN||n>=17)return;send({type:'event',id:streamId,final:false,event:{type:'assistant_text_delta',text:'x'.repeat(524288)}});n++;setTimeout(tick,250)};tick();}
else if(f.method==='permission_decide') {count++;send({type:'response',id:f.id,ok:true,result:{delivered:count>1}});if(count>1){send({type:'event',id:streamId,final:false,event:{type:'turn_completed'}});send({type:'event',id:streamId,final:true})}}
else if(f.method==='submit_turn') {streamId=f.id;if(['permission','permission-abort','permission-disconnect','pending-budget'].includes(scenario)){send({type:'event',id:f.id,final:false,event:{type:'permission_request',requestId:'p1',toolName:'bash',payload:{command:'git status'}}});if(scenario==='permission-disconnect')setTimeout(()=>ws.close(),500);}
else if(['abort-late','abort-timeout'].includes(scenario) && count++===0)send({type:'event',id:f.id,final:false,event:{type:'turn_started'}});
else if(scenario==='grapheme'){send({type:'event',id:f.id,final:false,event:{type:'assistant_text_delta',text:'BEGIN_MARK\n'+'x'.repeat(4095)+'👩‍💻e\u0301\nTAIL_MARK'}});send({type:'event',id:f.id,final:false,event:{type:'turn_completed'}});send({type:'event',id:f.id,final:true});}
else if(scenario==='throughput'){let n=0;const tick=()=>{if(ws.readyState!==ws.OPEN)return;if(n===300){send({type:'event',id:f.id,seq:n++,final:false,event:{type:'assistant_text_delta',text:'THROUGHPUT_DONE'}});send({type:'event',id:f.id,seq:n++,final:false,event:{type:'turn_completed'}});send({type:'event',id:f.id,seq:n,final:true});return;}send({type:'event',id:f.id,seq:n,final:false,event:{type:'assistant_text_delta',text:'x'}});n++;setTimeout(tick,10)};tick();}
else if(scenario==='flood'){let n=0;const tick=()=>{if(ws.readyState!==ws.OPEN||n>=20000)return;send({type:'event',id:f.id,final:false,event:{type:'assistant_text_delta',text:`line-${n} `+'z'.repeat(180)+'\n'}});n++;setTimeout(tick,1)};tick();}
else if(scenario==='budget'){let n=0;const tick=()=>{if(ws.readyState!==ws.OPEN||n>=17)return;send({type:'event',id:f.id,final:false,event:{type:'assistant_text_delta',text:'x'.repeat(524288)}});n++;setTimeout(tick,250)};tick();}
else if(scenario==='final' && count++===0)send({type:'event',id:f.id,final:true,event:{type:'error',message:'result_unknown'}});
else {send({type:'event',id:f.id,final:false,event:{type:'assistant_text_delta',text:'PTY_REPLY'}});send({type:'event',id:f.id,final:false,event:{type:'turn_completed'}});send({type:'event',id:f.id,final:true});}}
else send({type:'response',id:f.id,ok:true,result:{}});
});ws.on('close',()=>log({ws_closed:true}));});
await new Promise(ok=>server.listen(0,'127.0.0.1',ok));console.log(server.address().port);
process.on('SIGTERM',()=>{for(const ws of wss.clients)ws.terminate();wss.close(()=>server.close(()=>process.exit(0)));setTimeout(()=>process.exit(2),1000).unref()});
