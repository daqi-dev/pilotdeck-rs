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
const hashes={}; const cache=new Map(); const bots=[]; const calls=[];
const server=http.createServer(async(req,res)=>{
  let raw='';for await(const b of req)raw+=b;
  const method=req.url.split('/').at(-1);const body=JSON.parse(raw||'{}'); calls.push({method,body});
  res.setHeader('content-type','application/json');
  res.end(JSON.stringify({ok:true,result:method==='getMe'?{username:'fixture'}:true}));
});
await new Promise(r=>server.listen(0,'127.0.0.1',r));
const root=`http://127.0.0.1:${server.address().port}`;
class Bot {
  constructor(token){assert.equal(token,'fixture');this.handlers={};bots.push(this);this.api={};for(const m of ['setWebhook','deleteWebhook','getMe','sendMessage','sendChatAction'])this.api[m]=async(...args)=>{
    const body=m==='setWebhook'?{url:args[0]}:m==='sendMessage'?{chat_id:args[0],text:args[1]}:m==='sendChatAction'?{chat_id:args[0],action:args[1]}:{};
    const response=await fetch(`${root}/botfixture/${m}`,{method:'POST',body:JSON.stringify(body)});return (await response.json()).result;
  };}
  on(name,fn){this.handlers[name]=fn;}
  catch(fn){this.errorHandler=fn;}
  start(options){assert.deepEqual(options,{drop_pending_updates:false});this.started=true;}
  async stop(){this.started=false;}
}
function load(path){
  if(cache.has(path))return cache.get(path).exports;
  const source=execFileSync('git',['-C',baseline,'show',`${commit}:${path}`],{encoding:'utf8'});
  hashes[path]=createHash('sha256').update(source).digest('hex');
  const compiled=transformSync(source,{loader:'ts',format:'cjs',target:'es2022'}).code;
  const module={exports:{}};cache.set(path,module);
  new Function('require','module','exports',compiled)(name=>{
    if(name==='grammy')return {Bot};
    if(name.startsWith('node:'))return require(name);
    assert.ok(name.startsWith('.'),`unapproved import ${name}`);
    return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));
  },module,module.exports);return module.exports;
}
try{
  const {TelegramChannel}=load('src/adapters/channel/telegram/TelegramChannel.ts');
  const {TelegramSessionMapper}=load('src/adapters/channel/telegram/TelegramSessionMapper.ts');
  const {renderTelegramEvent}=load('src/adapters/channel/telegram/telegram-render.ts');
  const mapper=new TelegramSessionMapper(undefined,()=> '01234567-89ab-4cde-8fab-0123456789ab');
  const requests=[];const logs=[];
  const channel=new TelegramChannel({token:'fixture',mapper});
  const handle=await channel.start({gateway:{async *submitTurn(request){requests.push(request);yield {type:'assistant_text_delta',text:'answer'};}},logger:{info:t=>logs.push(['info',t]),warn:t=>logs.push(['warn',t]),error:t=>logs.push(['error',t])}});
  await bots[0].handlers['message:text']({message:{chat:{id:12},text:' hello '}});
  await bots[0].handlers['message:text']({message:{chat:{id:12},text:'/new'}});
  await handle.stop('fixture');
  // A typing send is intentionally fire-and-forget; wait for its real wire completion.
  await new Promise(r=>setImmediate(r));
  const result={environment:{node:process.version,icu:process.versions.icu,diagnostic,formalAcceptance:false,environmentMatchesFormal:!diagnostic},baseline:commit,sourceSha256:hashes,requests,calls,logs,mapper:mapper.snapshot(),render:[renderTelegramEvent({type:'tool_call_finished',ok:false,toolName:'Read'}),renderTelegramEvent({type:'error',message:'bad'})]};
  assert.equal(requests.length,1);assert.equal(requests[0].channelKey,'telegram');assert.ok(calls.some(c=>c.method==='sendMessage'&&c.body.text==='answer'));assert.ok(calls.some(c=>c.body.text==='已创建新会话。'));
  const serialized=JSON.stringify(result,null,2)+'\n';
  if(process.argv[2])await writeFile(process.argv[2],serialized,{flag:'wx'});else process.stdout.write(serialized);
} finally {await new Promise(r=>server.close(r));}
