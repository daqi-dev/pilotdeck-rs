import {createServer} from 'node:http';
import {createRequire} from 'node:module';
import {appendFileSync} from 'node:fs';
import {join,resolve} from 'node:path';
const [base,out]=process.argv.slice(2);const require=createRequire(join(resolve(base),'package.json'));
const {WebSocketServer}=require('ws');const log=x=>appendFileSync(out,JSON.stringify(x)+'\n');
const server=createServer((req,res)=>{log({http:req.url});res.writeHead(req.url==='/health'?200:404);res.end('{}')});
const wss=new WebSocketServer({server});wss.on('connection',ws=>{ws.on('message',data=>{const f=JSON.parse(String(data));log({in:f});const send=o=>{log({out:o});ws.send(JSON.stringify(o))};
if(f.type==='hello')send({type:'hello_ok',protocolVersion:'1.2',serverVersion:'fixture',serverInfo:{capabilities:[]}});
else if(f.method==='list_sessions')send({type:'response',id:f.id,ok:true,result:{sessions:[]}});
else if(f.method==='submit_turn'){for(const [seq,final,event] of [[0,false,{type:'assistant_text_delta',text:'PTY_REPLY'}],[1,false,{type:'turn_completed',finishReason:'completed'}],[2,true,{type:'turn_completed',finishReason:'completed'}]])send({type:'event',id:f.id,seq,final,event});}
else send({type:'response',id:f.id,ok:true,result:{}});
});ws.on('close',()=>log({ws_closed:true}));});
await new Promise(r=>server.listen(0,'127.0.0.1',r));console.log(server.address().port);
process.on('SIGTERM',()=>{for(const ws of wss.clients)ws.terminate();wss.close(()=>server.close(()=>process.exit(0)));setTimeout(()=>process.exit(2),1000).unref()});
