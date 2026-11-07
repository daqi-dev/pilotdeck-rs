import {createServer} from 'node:http';import {createRequire} from 'node:module';import {appendFileSync} from 'node:fs';import {join,resolve} from 'node:path';
const [base,out]=process.argv.slice(2),require=createRequire(join(resolve(base),'package.json'));const {WebSocketServer}=require('ws'),log=x=>appendFileSync(out,JSON.stringify(x)+'\n');
const server=createServer((req,res)=>{log({http:req.url});res.writeHead(req.url==='/health'?200:404);res.end('{}')});const wss=new WebSocketServer({server});
wss.on('connection',ws=>{ws.on('message',data=>{const f=JSON.parse(String(data));log({in:f});const send=o=>{log({out:o});ws.send(JSON.stringify(o))};
if(f.type==='hello')send({type:'hello_ok',protocolVersion:'1.2',serverVersion:'fixture',serverInfo:{capabilities:[]}});
else if(f.method==='list_sessions')send({type:'response',id:f.id,ok:true,result:{sessions:[]}});
else if(f.method==='submit_turn'){
 const long=['LONG_START',...Array.from({length:58},(_,i)=>`item-${i}`),'LONG_END'].join('\n');
 const events=[{type:'tool_call_finished',toolCallId:'t1',toolName:'read_file',ok:true,resultPreview:'short preview',resultLineCount:8},{type:'tool_result_detail_available',toolCallId:'t1',fullText:'SHORT_TOOL_MARK\nsecond line'},{type:'tool_call_finished',toolCallId:'t2',toolName:'bash',ok:true,resultPreview:'long preview',resultLineCount:60},{type:'tool_result_detail_available',toolCallId:'t2',fullText:long},{type:'turn_completed'}];
 events.forEach((event,seq)=>send({type:'event',id:f.id,seq,final:false,event}));send({type:'event',id:f.id,seq:events.length,final:true});
}else send({type:'response',id:f.id,ok:true,result:{}});
});ws.on('close',()=>log({ws_closed:true}));});await new Promise(r=>server.listen(0,'127.0.0.1',r));console.log(server.address().port);
process.on('SIGTERM',()=>{for(const ws of wss.clients)ws.terminate();wss.close(()=>server.close(()=>process.exit(0)));setTimeout(()=>process.exit(2),1000).unref()});
