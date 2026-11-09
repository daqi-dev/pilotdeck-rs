// Fixture only. Never selected as a production default bridge.
import {createServer} from 'node:http';
const messages=[],sent=[],requests=[];
const server=createServer(async(req,res)=>{let body='';for await(const b of req)body+=b;requests.push({method:req.method,path:req.url});res.setHeader('Content-Type','application/json');if(req.url==='/messages')res.end(JSON.stringify({messages}));else if(req.url==='/inject'){messages.push(...JSON.parse(body));res.end('{}');}else if(req.url==='/send'){const m=JSON.parse(body);sent.push(m);res.statusCode=m.message==='FAIL'?500:200;res.end(JSON.stringify(res.statusCode===200?{ok:true}:{error:'owned fail'}));}else if(req.url==='/inspect')res.end(JSON.stringify({pid:process.pid,messages,sent,requests}));else {res.statusCode=404;res.end('{}');}});
server.listen(Number(process.env.BRIDGE_PORT),'127.0.0.1');process.on('SIGTERM',()=>server.close(()=>process.exit(0)));
