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
 let raw='';for await(const b of req)raw+=b;assert.equal(req.headers.authorization,'Bot fixture');calls.push({method:req.method,path:req.url,...(raw?{body:JSON.parse(raw)}:{})});res.setHeader('content-type','application/json');
 res.end(JSON.stringify(req.url==='/gateway/bot'?{url:root.replace('http:','ws:')+'/gateway'}:req.url==='/channels/c'?{id:'c',type:0}:{}));
});
server.on('upgrade',(req,socket,head)=>{
 assert.equal(req.url,'/gateway?v=10&encoding=json');calls.push({method:'WS',path:req.url});wss.handleUpgrade(req,socket,head,ws=>{
  ws.send(JSON.stringify({op:10,d:{heartbeat_interval:1000}}));ws.once('message',raw=>{const identify=JSON.parse(String(raw));assert.equal(identify.op,2);assert.equal(identify.d.intents,37377);assert.equal(identify.d.token,'fixture');
   ws.send(JSON.stringify({op:0,s:1,t:'READY',d:{session_id:'session',user:{id:'bot',tag:'fixture'}}}));
   for(const [s,event]of [[2,{author:{id:'bot',bot:false},channel_id:'c',content:'self',type:0}],[3,{author:{id:'human',bot:true},channel_id:'c',content:'bot',type:0}],[4,{author:{id:'human',bot:false},channel_id:'c',content:'system',type:1}],[5,{author:{id:'human',bot:false},channel_id:'c',content:' hello ',type:0}]])ws.send(JSON.stringify({op:0,s,t:'MESSAGE_CREATE',d:event}));
  });ws.on('close',()=>{closed=true;});
 });
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const root=`http://127.0.0.1:${server.address().port}`;
class Client{
 constructor(options){assert.deepEqual(options.intents,[1,512,4096,32768]);assert.deepEqual(options.partials,['channel','message']);this.handlers={};this.channels={fetch:async id=>{const channel=await this.rest('GET','/channels/'+id);return {...channel,send:body=>this.rest('POST','/channels/'+id+'/messages',body),sendTyping:()=>this.rest('POST','/channels/'+id+'/typing')};}};}
 on(name,fn){this.handlers[name]=fn;}
 async rest(method,path,body){const response=await fetch(root+path,{method,headers:{authorization:'Bot '+this.token},...(body?{body:JSON.stringify(body)}:{})});return response.json();}
 async login(token){this.token=token;const {url}=await this.rest('GET','/gateway/bot');return new Promise((resolve,reject)=>{this.socket=new wsModule.WebSocket(url+'?v=10&encoding=json');this.socket.on('error',reject);this.socket.on('message',raw=>{const event=JSON.parse(String(raw));if(event.op===10)this.socket.send(JSON.stringify({op:2,d:{token,intents:37377,properties:{os:'fixture',browser:'fixture',device:'fixture'}}}));if(event.t==='READY'){this.handlers.ready({user:event.d.user});resolve();}if(event.t==='MESSAGE_CREATE'){const m=event.d;this.handlers.messageCreate({...m,channel:{id:m.channel_id},system:![0,19].includes(m.type)});}});});}
 destroy(){this.socket?.close();}
}
const sdk={Client,GatewayIntentBits:{Guilds:1,GuildMessages:512,DirectMessages:4096,MessageContent:32768},Partials:{Channel:'channel',Message:'message'}};
function load(path){
  if(cache.has(path))return cache.get(path).exports;
  const source=execFileSync('git',['-C',baseline,'show',`${commit}:${path}`],{encoding:'utf8'});
  hashes[path]=createHash('sha256').update(source).digest('hex');
  const compiled=transformSync(source,{loader:'ts',format:'cjs',target:'es2022'}).code;
  const module={exports:{}};cache.set(path,module);
  new Function('require','module','exports',compiled)(name=>{
    if(name==='discord.js')return sdk;
    if(name.startsWith('node:'))return require(name);
    assert.ok(name.startsWith('.'),`unapproved import ${name}`);
    return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));
  },module,module.exports);return module.exports;
}
try{
 const {DiscordChannel}=load('src/adapters/channel/discord/DiscordChannel.ts');const {DiscordSessionMapper}=load('src/adapters/channel/discord/DiscordSessionMapper.ts');const mapper=new DiscordSessionMapper(undefined,()=> '01234567-89ab-4cde-8fab-0123456789ab');const requests=[];const logs=[];
 const channel=new DiscordChannel({token:'fixture',mapper});const handle=await channel.start({gateway:{async *submitTurn(input){requests.push(input);yield {type:'assistant_text_delta',text:'Discord reply'};}},logger:{info:t=>logs.push(['info',t]),warn:t=>logs.push(['warn',t]),error:t=>logs.push(['error',t])}});
 let deadline=Date.now()+3000;while(!calls.some(c=>c.body?.content==='Discord reply')){assert.ok(Date.now()<deadline,'reply wire deadline');await new Promise(r=>setImmediate(r));}
 await handle.stop('fixture');deadline=Date.now()+3000;while(!closed){assert.ok(Date.now()<deadline,'WS actual closure');await new Promise(r=>setImmediate(r));}
 assert.equal(requests.length,1);assert.equal(requests[0].channelKey,'discord');assert.equal(requests[0].sessionKey,'discord:chat=c:general');assert.equal(requests[0].message,'hello');
 const result={environment:{node:process.version,icu:process.versions.icu,wsVersion,diagnostic,formalAcceptance:false},baseline:commit,sourceSha256:hashes,requests,calls,logs,mapper:mapper.snapshot(),websocketClosed:closed};const serialized=JSON.stringify(result,null,2)+'\n';if(process.argv[2])await writeFile(process.argv[2],serialized,{flag:'wx'});else process.stdout.write(serialized);
}finally{await new Promise(r=>wss.close(r));await new Promise(r=>server.close(r));}
