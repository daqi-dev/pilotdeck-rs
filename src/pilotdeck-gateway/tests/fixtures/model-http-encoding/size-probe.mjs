import assert from 'node:assert/strict';
import http from 'node:http';
import z from 'node:zlib';
import {constants} from 'node:buffer';
import {once} from 'node:events';
assert.equal(process.version,'v22.22.0');
const limit=constants.MAX_STRING_LENGTH;
let wire,encoding;
const server=http.createServer((req,res)=>{res.writeHead(200,{'content-encoding':encoding,'content-length':wire.length});res.end(wire);});
server.listen(0,'127.0.0.1');await once(server,'listening');
const url=`http://127.0.0.1:${server.address().port}/`;
console.log(JSON.stringify({node:process.version,limit}));
try {
 for(encoding of ['gzip','br'])for(const decodedSize of [limit,limit+1]){
  const compressor=encoding==='gzip'?z.createGzip({level:1}):z.createBrotliCompress({params:{[z.constants.BROTLI_PARAM_QUALITY]:1}});
  const pieces=[];compressor.on('data',c=>pieces.push(c));const done=once(compressor,'end');
  const chunk=Buffer.alloc(65536,97);
  for(let left=decodedSize;left>0;left-=chunk.length){if(!compressor.write(chunk.subarray(0,Math.min(chunk.length,left))))await once(compressor,'drain');}
  compressor.end();await done;wire=Buffer.concat(pieces);
  try{const r=await fetch(url,{signal:AbortSignal.timeout(60000)});let text=await r.text();console.log(JSON.stringify({encoding,decodedSize,wireSize:wire.length,length:text.length,first:text.charCodeAt(0),last:text.charCodeAt(text.length-1)}));text=null;}
  catch(e){console.log(JSON.stringify({encoding,decodedSize,wireSize:wire.length,name:e.name,error:e.message,code:e.code,cause:e.cause?.message}));}
  global.gc();
 }
}finally{server.closeAllConnections();server.close();}
