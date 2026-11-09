// Owned loopback bridge fixture only; never a product default.
import {createServer,request} from 'node:http';
import {readFileSync} from 'node:fs';
import {join} from 'node:path';
const target=readFileSync(join(process.env.PILOT_HOME,'lifecycle-bridge-target'),'utf8');
const server=createServer((req,res)=>{const upstream=request(target+req.url,{method:req.method,headers:req.headers},reply=>{res.writeHead(reply.statusCode,reply.headers);reply.pipe(res);});upstream.on('error',()=>{res.statusCode=502;res.end('{}');});req.pipe(upstream);});
server.listen(Number(process.env.BRIDGE_PORT),'127.0.0.1');
process.on('SIGTERM',()=>server.close(()=>process.exit(0)));
