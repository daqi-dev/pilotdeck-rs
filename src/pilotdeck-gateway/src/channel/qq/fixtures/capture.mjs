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
const hashes={};const cache=new Map();const calls=[];const wsModule=require('ws');const wsVersion=require('ws/package.json').version;const wss=new wsModule.WebSocketServer({noServer:true});const sockets=[];let closed=0;let identifies=0;let resumes=0;let heartbeats=0;
const server=http.createServer(async(req,res)=>{
 let raw='';for await(const b of req)raw+=b;const body=raw?JSON.parse(raw):undefined;calls.push({method:req.method,path:req.url,...(body?{body}:{})});res.setHeader('content-type','application/json');
 if(req.url==='/token'){assert.deepEqual(body,{appId:'app',clientSecret:'fixture'});res.end(JSON.stringify({access_token:'access',expires_in:'3600'}));}
 else {assert.equal(req.headers.authorization,'QQBot access');res.end(JSON.stringify(req.url==='/gateway'?{url:root.replace('http:','ws:')+'/socket'}:{}));}
});
server.on('upgrade',(req,socket,head)=>{
 assert.equal(req.url,'/socket');wss.handleUpgrade(req,socket,head,ws=>{
  sockets.push(ws);ws.send(JSON.stringify({op:10,d:{heartbeat_interval:50}}));ws.on('close',()=>{closed++;});ws.on('message',raw=>{const value=JSON.parse(String(raw));
   if(value.op===1){heartbeats++;return;}
   if(value.op===2){identifies++;assert.equal(value.d.token,'QQBot access');assert.equal(value.d.intents,33554432);assert.deepEqual(value.d.shard,[0,1]);assert.deepEqual(value.d.properties,{$os:'linux',$browser:'pilotdeck',$device:'pilotdeck'});
    if(identifies===1){ws.send(JSON.stringify({op:0,s:1,t:'READY',d:{session_id:'session'}}));for(const [seq,type,data] of [[2,'GROUP_AT_MESSAGE_CREATE',{id:'g1',group_openid:'blocked',author:{member_openid:'u'},content:'hi'}],[3,'GROUP_AT_MESSAGE_CREATE',{id:'g2',group_openid:'g',author:{member_openid:'u'},content:'/ask hello'}],[4,'C2C_MESSAGE_CREATE',{id:'c1',author:{user_openid:'u2'},content:'/new'}]])ws.send(JSON.stringify({op:0,s:seq,t:type,d:data}));}
    else {ws.send(JSON.stringify({op:0,s:10,t:'READY',d:{session_id:'session2'}}));setImmediate(()=>ws.close());}
   }
   if(value.op===6){resumes++;assert.equal(value.d.session_id,'session2');assert.equal(value.d.seq,10);assert.equal(value.d.token,'QQBot access');ws.send(JSON.stringify({op:0,s:11,t:'RESUMED',d:{}}));}
  });
 });
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const root=`http://127.0.0.1:${server.address().port}`;
const originalFetch=globalThis.fetch;
globalThis.fetch=(input,options)=>{const url=new URL(String(input));if(url.origin==='https://bots.qq.com'&&url.pathname==='/app/getAppAccessToken')return originalFetch(root+'/token',options);assert.equal(url.origin,'https://api.sgroup.qq.com','only fixed official QQ HTTP may be remapped');return originalFetch(root+url.pathname+url.search,options);};
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
 const {QQChannel}=load('src/adapters/channel/qq/QQChannel.ts');const {QQSessionMapper}=load('src/adapters/channel/qq/QQSessionMapper.ts');const mapper=new QQSessionMapper(undefined,()=> '01234567-89ab-4cde-8fab-0123456789ab');const requests=[];const logs=[];const states=[];
 const channel=new QQChannel({appId:'app',clientSecret:'fixture',allowGroups:['g'],mapper,onStateChange:state=>states.push(state)});const handle=await channel.start({gateway:{async *submitTurn(input){requests.push(input);yield {type:'assistant_text_delta',text:'QQ reply'};}},logger:{info:t=>logs.push(['info',t]),warn:t=>logs.push(['warn',t]),error:t=>logs.push(['error',t])}});
 async function until(predicate,label,ms=3000){const deadline=Date.now()+ms;while(!predicate()){assert.ok(Date.now()<deadline,label);await new Promise(r=>setImmediate(r));}}
 await until(()=>calls.filter(c=>c.path.includes('/messages')).length===2&&channel.activeChats.size===0,'initial group/C2C replies');
 sockets[0].send(JSON.stringify({op:0,s:5,t:'GROUP_AT_MESSAGE_CREATE',d:{id:'g3',group_openid:'g',author:{member_openid:'u'},content:'/ask /new'}}));await until(()=>states.length===1&&calls.filter(c=>c.path.includes('/messages')).length===3,'group new persistence');
 sockets[0].send(JSON.stringify({op:9,d:false}));await until(()=>identifies===2,'same-socket delayed identify',4000);await until(()=>resumes===1,'five-second reconnect and resume',7000);assert.ok(heartbeats>0);
 await handle.stop('fixture');await until(()=>closed===2,'actual QQ sockets closed');
 assert.equal(requests.length,2);assert.ok(requests.some(r=>r.sessionKey==='qq:group=g:user=u:general'&&r.message==='hello'));assert.ok(requests.some(r=>r.sessionKey==='qq:c2c:u2:general'&&r.message==='/new'));assert.equal(calls.filter(c=>c.path==='/token').length,1);
 const result={environment:{node:process.version,icu:process.versions.icu,wsVersion,diagnostic,formalAcceptance:false},baseline:commit,sourceSha256:hashes,requests,calls,logs,states,mapper:mapper.snapshot(),identifies,resumes,heartbeatObserved:heartbeats>0,closedSockets:closed};const serialized=JSON.stringify(result,null,2)+'\n';if(process.argv[2])await writeFile(process.argv[2],serialized,{flag:'wx'});else process.stdout.write(serialized);
}finally{globalThis.fetch=originalFetch;for(const ws of sockets)ws.terminate();await new Promise(r=>wss.close(r));await new Promise(r=>server.close(r));}
