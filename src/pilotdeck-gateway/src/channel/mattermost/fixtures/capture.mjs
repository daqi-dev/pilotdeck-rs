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
const hashes={};const cache=new Map();const calls=[];const wsModule=require('ws');const wsVersion=require('ws/package.json').version;const wss=new wsModule.WebSocketServer({noServer:true});let closed=false;
const server=http.createServer(async(req,res)=>{
 let raw='';for await(const b of req)raw+=b;assert.equal(req.headers.authorization,'Bearer fixture');calls.push({method:req.method,path:req.url,...(raw?{body:JSON.parse(raw)}:{})});res.setHeader('content-type','application/json');
 res.end(JSON.stringify(req.url==='/users/me'?{id:'bot'}:{}));
});
server.on('upgrade',(req,socket,head)=>{
 assert.equal(req.url,'/api/v4/websocket?token=fixture');assert.equal(req.headers.origin,root);calls.push({method:'WS',path:req.url,origin:req.headers.origin});
 wss.handleUpgrade(req,socket,head,ws=>{
  for(const post of [{user_id:'bot',channel_id:'channel',message:'self'},{user_id:'human',channel_id:'channel',message:'hook',props:{from_webhook:'true'}},{user_id:'human',channel_id:'channel',root_id:'thread',message:' hello\r\nworld ',props:{from_bot:true}}])ws.send(JSON.stringify({event:'posted',data:{post:JSON.stringify(post)}}));
  ws.on('close',()=>{closed=true;});
 });
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const root=`http://127.0.0.1:${server.address().port}`;
function load(path){
  if(cache.has(path))return cache.get(path).exports;
  const source=execFileSync('git',['-C',baseline,'show',`${commit}:${path}`],{encoding:'utf8'});
  hashes[path]=createHash('sha256').update(source).digest('hex');
  const compiled=transformSync(source,{loader:'ts',format:'cjs',target:'es2022'}).code;
  const module={exports:{}};cache.set(path,module);
  new Function('require','module','exports',compiled)(name=>{
    if(name==='ws')return wsModule;
    if(name.startsWith('node:'))return require(name);
    assert.ok(name.startsWith('.'),`unapproved import ${name}`);
    return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));
  },module,module.exports);return module.exports;
}
try{
 const {MattermostChannel}=load('src/adapters/channel/mattermost/MattermostChannel.ts');const {MattermostSessionMapper}=load('src/adapters/channel/mattermost/MattermostSessionMapper.ts');const mapper=new MattermostSessionMapper(undefined,()=> '01234567-89ab-4cde-8fab-0123456789ab');const requests=[];const logs=[];
 const channel=new MattermostChannel({token:'fixture',serverUrl:root,mapper});const handle=await channel.start({gateway:{async *submitTurn(input){requests.push(input);yield {type:'assistant_text_delta',text:'Mattermost reply'};}},logger:{info:t=>logs.push(['info',t]),warn:t=>logs.push(['warn',t]),error:t=>logs.push(['error',t])}});
 let deadline=Date.now()+3000;while(!calls.some(c=>c.body?.message==='Mattermost reply')){assert.ok(Date.now()<deadline,'reply wire deadline');await new Promise(r=>setImmediate(r));}
 await handle.stop('fixture');deadline=Date.now()+3000;while(!closed){assert.ok(Date.now()<deadline,'WS actual closure');await new Promise(r=>setImmediate(r));}
 assert.equal(requests.length,1);assert.equal(requests[0].channelKey,'mattermost');assert.equal(requests[0].sessionKey,'mattermost:chat=channel:thread:general');assert.equal(requests[0].message,'hello\nworld');
 const result={environment:{node:process.version,icu:process.versions.icu,wsVersion,diagnostic,formalAcceptance:false},baseline:commit,sourceSha256:hashes,requests,calls,logs,mapper:mapper.snapshot(),websocketClosed:closed};const serialized=JSON.stringify(result,null,2)+'\n';if(process.argv[2])await writeFile(process.argv[2],serialized,{flag:'wx'});else process.stdout.write(serialized);
}finally{await new Promise(r=>wss.close(r));await new Promise(r=>server.close(r));}
