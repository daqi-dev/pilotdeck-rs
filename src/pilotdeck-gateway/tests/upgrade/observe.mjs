import http from 'node:http';
import {syncBuiltinESMExports} from 'node:module';
import {appendFileSync} from 'node:fs';
const original = http.createServer;
let serial=0;
const record = row => appendFileSync(process.env.PDRS_UPGRADE_OBSERVATION,JSON.stringify({...row,at:process.hrtime.bigint().toString()})+'\n');
http.createServer = function(...args) {
  const server=Reflect.apply(original,this,args);
  server.prependListener('upgrade',(request,socket,head)=>{
    const id=++serial;
    record({event:'upgrade',id,url:request.url,headHex:head.toString('hex'),headBytes:head.length});
    socket.prependListener('data',data=>record({event:'socket-data',id,hex:Buffer.from(data).toString('hex')}));
  });
  return server;
};
syncBuiltinESMExports();
