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
const hashes={};const cache=new Map();const clients=[];const calls=[];
const server=http.createServer(async(req,res)=>{
 let raw='';for await(const b of req)raw+=b;const body=JSON.parse(raw||'{}');calls.push({method:req.method,path:req.url,body});
 assert.equal(req.headers.authorization,'Bearer fixture');res.setHeader('content-type','application/json');
 res.end(JSON.stringify(req.url.endsWith('/account/whoami')?{user_id:'@bot:fixture'}:{}));
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const root=`http://127.0.0.1:${server.address().port}`;
class SimpleFsStorageProvider {constructor(path){this.path=path;}}
class MatrixClient {
 constructor(homeserver,token,storage){assert.equal(homeserver,root);assert.equal(token,'fixture');this.storage=storage;this.handlers={};clients.push(this);}
 async request(method,path,body){const response=await fetch(root+'/_matrix/client/v3'+path,{method,headers:{authorization:'Bearer fixture'},...(method==='GET'?{}:{body:JSON.stringify(body)})});return response.json();}
 async getUserId(){return (await this.request('GET','/account/whoami')).user_id;}
 on(name,fn){this.handlers[name]=fn;}
 async start(){this.started=true;}
 stop(){this.started=false;}
 async joinRoom(room){return this.request('POST','/join/'+encodeURIComponent(room),{});}
 async sendMessage(room,body){return this.request('PUT','/rooms/'+encodeURIComponent(room)+'/send/m.room.message/fixed-transaction',body);}
}
function load(path){
  if(cache.has(path))return cache.get(path).exports;
  const source=execFileSync('git',['-C',baseline,'show',`${commit}:${path}`],{encoding:'utf8'});
  hashes[path]=createHash('sha256').update(source).digest('hex');
  const compiled=transformSync(source,{loader:'ts',format:'cjs',target:'es2022'}).code;
  const module={exports:{}};cache.set(path,module);
  new Function('require','module','exports',compiled)(name=>{
    if(name==='matrix-bot-sdk')return {MatrixClient,SimpleFsStorageProvider};
    if(name==='../../../pilot/index.js')return {resolvePilotHome:()=>'/isolated-matrix-fixture'};
    if(name.startsWith('node:'))return require(name);
    assert.ok(name.startsWith('.'),`unapproved import ${name}`);
    return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));
  },module,module.exports);return module.exports;
}
try{
 const {MatrixChannel}=load('src/adapters/channel/matrix/MatrixChannel.ts');const {MatrixSessionMapper}=load('src/adapters/channel/matrix/MatrixSessionMapper.ts');const {renderMatrixEvent}=load('src/adapters/channel/matrix/matrix-render.ts');
 const mapper=new MatrixSessionMapper(undefined,()=> '01234567-89ab-4cde-8fab-0123456789ab');const requests=[];const logs=[];
 const channel=new MatrixChannel({accessToken:'fixture',homeserver:root,pilotHome:'/isolated-matrix-fixture',mapper});
 const handle=await channel.start({gateway:{async *submitTurn(input){requests.push(input);yield {type:'assistant_text_delta',text:'matrix reply'};}},logger:{info:t=>logs.push(['info',t]),warn:t=>logs.push(['warn',t]),error:t=>logs.push(['error',t])}});
 const client=clients[0];await client.handlers['room.invite']('!invite:fixture');
 for(const event of [{sender:'@bot:fixture',content:{body:'self'}},{sender:'@human:fixture',content:{body:'notice',msgtype:'m.notice'}},{sender:'@human:fixture',content:{body:'edit','m.relates_to':{rel_type:'m.replace'}}},{sender:'@human:fixture',content:{body:' hello ',msgtype:'m.text'}}])client.handlers['room.message']('!room:fixture',event);
 const deadline=Date.now()+3000;while(!calls.some(c=>c.body.body==='matrix reply')){assert.ok(Date.now()<deadline,'reply wire deadline');await new Promise(r=>setImmediate(r));}
 await handle.stop('fixture');assert.equal(requests.length,1);assert.equal(requests[0].channelKey,'matrix');assert.equal(requests[0].message,'hello');
 const result={environment:{node:process.version,icu:process.versions.icu,diagnostic,formalAcceptance:false,environmentMatchesFormal:!diagnostic},baseline:commit,sourceSha256:hashes,requests,calls,logs,mapper:mapper.snapshot(),storagePath:client.storage.path,render:[renderMatrixEvent({type:'tool_call_finished',ok:false,toolName:'Read'})]};
 const serialized=JSON.stringify(result,null,2)+'\n';if(process.argv[2])await writeFile(process.argv[2],serialized,{flag:'wx'});else process.stdout.write(serialized);
}finally{await new Promise(r=>server.close(r));}
