import {createServer} from 'node:http';import {createRequire} from 'node:module';import {appendFileSync} from 'node:fs';import {join,resolve} from 'node:path';
const [base,out]=process.argv.slice(2),{WebSocketServer}=createRequire(join(resolve(base),'package.json'))('ws');const log=x=>appendFileSync(out,JSON.stringify(x)+'\n');
const server=createServer((req,res)=>{log({http:req.url});res.writeHead(req.url==='/health'?200:404);res.end('{}')});const wss=new WebSocketServer({server});
wss.on('connection',ws=>{ws.on('message',data=>{const f=JSON.parse(String(data));log({in:f});const send=o=>{log({out:o});ws.send(JSON.stringify(o))};
if(f.type==='hello')send({type:'hello_ok',protocolVersion:'1.2',serverVersion:'fixture',serverInfo:{capabilities:[]}});
else if(f.method==='list_sessions')send({type:'response',id:f.id,ok:true,result:{sessions:[{sessionKey:'session-a',sessionId:'session-a',cwd:'/projects/alpha',tag:'running',lastModified:20,summary:'ALPHA_SESSION'},{sessionKey:'session-b',sessionId:'session-b',cwd:'/projects/beta',tag:'idle',lastModified:10,summary:'BETA_SESSION'}]}});
else if(f.method==='read_session_messages')send({type:'response',id:f.id,ok:true,result:{messages:[]}});else send({type:'response',id:f.id,ok:true,result:{}});
});ws.on('close',()=>log({ws_closed:true}));});await new Promise(r=>server.listen(0,'127.0.0.1',r));console.log(server.address().port);process.on('SIGTERM',()=>{for(const ws of wss.clients)ws.terminate();wss.close(()=>server.close(()=>process.exit(0)));setTimeout(()=>process.exit(2),1000).unref()});
