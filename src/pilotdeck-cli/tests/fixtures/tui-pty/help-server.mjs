import {createServer} from 'node:http';
import {createRequire} from 'node:module';
import {appendFileSync} from 'node:fs';
import {join,resolve} from 'node:path';
const [base,out,scenario]=process.argv.slice(2);
const {WebSocketServer}=createRequire(join(resolve(base),'package.json'))('ws');
const log=x=>appendFileSync(out,JSON.stringify(x)+'\n');
const server=createServer((req,res)=>{log({http:req.url});res.writeHead(req.url==='/health'?200:404);res.end('{}')});
const wss=new WebSocketServer({server});
wss.on('connection',ws=>{let stream;const send=o=>{log({out:o});ws.send(JSON.stringify(o))};ws.on('message',data=>{const f=JSON.parse(String(data));log({in:f});
 if(f.type==='hello') send({type:'hello_ok',protocolVersion:'1.2',serverVersion:'fixture',serverInfo:{capabilities:[]}});
 else if(f.method==='list_sessions')send({type:'response',id:f.id,ok:true,result:{sessions:[]}});
 else if(f.method==='new_session' && scenario==='long-session')send({type:'response',id:f.id,ok:true,result:{sessionKey:'S'.repeat(100000)}});
 else if(f.method==='submit_turn') {stream=f.id;if(scenario==='permission'||scenario==='permission-abort')setTimeout(()=>{if(ws.readyState===ws.OPEN)send({type:'event',id:stream,final:false,event:{type:'permission_request',requestId:'p1',toolName:'bash',payload:{command:'git status'}}})},1200);else {send({type:'event',id:stream,final:false,event:{type:'turn_completed'}});send({type:'event',id:stream,final:true});}}
 else if(f.method==='permission_decide'||f.method==='abort_turn'){send({type:'response',id:f.id,ok:true,result:{delivered:true}});send({type:'event',id:stream,final:false,event:{type:'turn_completed'}});send({type:'event',id:stream,final:true});}
 else send({type:'response',id:f.id,ok:true,result:{}});
 });ws.on('close',()=>log({ws_closed:true}));});
await new Promise(r=>server.listen(0,'127.0.0.1',r));console.log(server.address().port);
process.on('SIGTERM',()=>{for(const ws of wss.clients)ws.terminate();wss.close(()=>server.close(()=>process.exit(0)));setTimeout(()=>process.exit(2),1000).unref()});
