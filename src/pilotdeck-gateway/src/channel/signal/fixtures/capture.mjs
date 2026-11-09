// Fixed-source offline Telegram golden. No accounts, package install, or baseline writes.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {createRequire} from 'node:module';
import {dirname, join, posix, resolve} from 'node:path';
import {writeFile} from 'node:fs/promises';
import http from 'node:http';
const baseline=resolve(process.env.PDRS_BASELINE || '/Users/da/ws/pdrs-runner/resources/PilotDeck');
const commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const diagnostic=process.env.PDRS_DIAGNOSTIC==='1';
if(!diagnostic)assert.equal(process.version,'v22.22.0');
assert.equal(process.versions.icu,'78.2');
const require=createRequire(join(baseline,'package.json'));
const {transformSync}=createRequire(require.resolve('tsx'))('esbuild');
const hashes={};const cache=new Map();const calls=[];let closed=false;
const server=http.createServer(async(req,res)=>{
 let raw='';for await(const b of req)raw+=b;
 calls.push({method:req.method,path:req.url,...(raw?{body:JSON.parse(raw)}:{})});
 if(req.method==='GET'){
  assert.equal(req.url,'/v1/receive/%2B123');res.setHeader('content-type','text/event-stream');res.writeHead(200);
  const event=Buffer.from('data: '+JSON.stringify({envelope:{source:'+human',dataMessage:{message:'你好',groupInfo:{groupId:'g'}}}})+'\r\n\n');
  const cut=event.indexOf(Buffer.from('你'))+1;res.write(event.subarray(0,cut));res.write(event.subarray(cut));res.on('close',()=>{closed=true;});
 }else {assert.equal(req.url,'/v2/send');res.setHeader('content-type','application/json');res.end('{}');}
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const root=`http://127.0.0.1:${server.address().port}`;
function load(path){
  if(cache.has(path))return cache.get(path).exports;
  const source=execFileSync('git',['-C',baseline,'show',`${commit}:${path}`],{encoding:'utf8'});
  hashes[path]=createHash('sha256').update(source).digest('hex');
  const compiled=transformSync(source,{loader:'ts',format:'cjs',target:'es2022'}).code;
  const module={exports:{}};cache.set(path,module);
  new Function('require','module','exports',compiled)(name=>{
    if(name.startsWith('node:'))return require(name);
    assert.ok(name.startsWith('.'),`unapproved import ${name}`);
    return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));
  },module,module.exports);return module.exports;
}
try{
 const {SignalChannel}=load('src/adapters/channel/signal/SignalChannel.ts');const {SignalSessionMapper}=load('src/adapters/channel/signal/SignalSessionMapper.ts');
 const mapper=new SignalSessionMapper(undefined,()=> '01234567-89ab-4cde-8fab-0123456789ab');const requests=[];const logs=[];
 const channel=new SignalChannel({account:'+123',restUrl:root,mapper});
 const handle=await channel.start({gateway:{async *submitTurn(input){requests.push(input);yield {type:'assistant_text_delta',text:'Signal reply'};}},logger:{info:t=>logs.push(['info',t]),warn:t=>logs.push(['warn',t]),error:t=>logs.push(['error',t])}});
 let deadline=Date.now()+3000;while(!calls.some(c=>c.body?.message==='Signal reply')){assert.ok(Date.now()<deadline,'reply wire deadline');await new Promise(r=>setImmediate(r));}
 await handle.stop('fixture');deadline=Date.now()+3000;while(!closed){assert.ok(Date.now()<deadline,'SSE actual socket closure');await new Promise(r=>setImmediate(r));}
 assert.equal(requests.length,1);assert.equal(requests[0].channelKey,'signal');assert.equal(requests[0].sessionKey,'signal:chat=group:g:general');assert.equal(requests[0].message,'你好');
 const result={environment:{node:process.version,icu:process.versions.icu,diagnostic,formalAcceptance:false},baseline:commit,sourceSha256:hashes,requests,calls,logs,mapper:mapper.snapshot(),receiveSocketClosed:closed};
 const serialized=JSON.stringify(result,null,2)+'\n';if(process.argv[2])await writeFile(process.argv[2],serialized,{flag:'wx'});else process.stdout.write(serialized);
}finally{await new Promise(r=>server.close(r));}
