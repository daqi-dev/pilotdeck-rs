// Non-frozen product probe; no Cargo, oracle generation or real account access.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdir,readFile,writeFile,readdir,realpath} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {createCipheriv,createHash,randomUUID} from 'node:crypto';
import {fileURLToPath} from 'node:url';

assert.equal(process.version,'v22.22.0','use the pinned Node 22');
const [gatewayArg,sidecarArg,outArg]=process.argv.slice(2);
const sourceManifestSha256=process.env.PDRS_SOURCE_MANIFEST_SHA256;
assert.match(sourceManifestSha256??'',/^[0-9a-f]{64}$/, 'record the actual frozen build source manifest');
assert(gatewayArg&&sidecarArg&&outArg,'driver.mjs GATEWAY SIDECAR NEW_OUTPUT_DIRECTORY');
const gateway=await realpath(gatewayArg),sidecar=await realpath(sidecarArg),out=resolve(outArg);
await mkdir(out,{recursive:false}); // Exclusive run; never overwrite earlier evidence.
const home=join(out,'home'),project=join(out,'project'),tmp=join(out,'tmp');
for(const p of [home,project,tmp])await mkdir(p);
const secret='PRIVATE_CACHE_'+randomUUID(),typedMarker='TYPED_'+randomUUID();
const key=Buffer.alloc(16,14),cipher=createCipheriv('aes-128-ecb',key,null);
const encrypted=Buffer.concat([cipher.update(secret,'utf8'),cipher.final()]);
const rows=[],wire=[],checks=[],sockets=new Set(),pending=[];
let child,ws,exitPromise,stderr='',stdout='',passed=false,failure,serverFailure;
let requests=0,downloaded=0,wireBytes=0;
const deadline=Date.now()+90000,delay=ms=>new Promise(r=>setTimeout(r,ms));
async function wait(predicate,label){
  while(!await predicate()){
    if(serverFailure)throw serverFailure;
    assert(Date.now()<deadline,label);
    assert(!child||child.exitCode===null&&child.signalCode===null,'host exited: '+stderr);
    await delay(20);
  }
}
const digest=async p=>createHash('sha256').update(await readFile(p)).digest('hex');
const inputs={gateway:await digest(gateway),sidecar:await digest(sidecar)};
const peer=createServer(async(req,res)=>{
  try{
    let raw=Buffer.alloc(0);
    for await(const chunk of req){raw=Buffer.concat([raw,chunk]);assert(raw.length<=2*1024*1024,'HTTP body cap');}
    const path=new URL(req.url,'http://127.0.0.1').pathname;
    if(path==='/attachment.txt'){
      downloaded++;rows.push({download:{path,bytes:encrypted.length}});
      res.writeHead(200,{'content-type':'application/octet-stream'});res.end(encrypted);return;
    }
    const body=raw.length?JSON.parse(raw):{};
    if(path==='/v1/chat/completions'){
      assert(++requests<=16,'model request cap');
      rows.push({provider:{path,body}});
      if(!body.stream){
        res.writeHead(200,{'content-type':'application/json'});
        res.end(JSON.stringify({choices:[{message:{role:'assistant',content:'{"title":"Attachment boundary"}'},finish_reason:'stop'}]}));return;
      }
      res.writeHead(200,{'content-type':'text/event-stream','connection':'close'});
      for(const [delta,finish_reason] of [[{role:'assistant',content:'boundary-output'},null],[{},'stop']])
        res.write('data: '+JSON.stringify({id:'boundary',model:'mock',choices:[{index:0,delta,finish_reason}]})+'\n\n');
      res.end('data: [DONE]\n\n');return;
    }
    rows.push({channel:{path,body}});
    res.setHeader('content-type','application/json');
    if(path.endsWith('/getupdates')){
      await delay(40);res.end(JSON.stringify({ret:0,msgs:pending.splice(0),get_updates_buf:'cursor'}));
    }else if(['/sendmessage','/getconfig','/sendtyping'].some(s=>path.endsWith(s)))res.end('{"ret":0}');
    else throw Error('unexpected loopback endpoint '+path);
  }catch(error){serverFailure=error;res.writeHead(500);res.end('fixture failure');}
});
peer.on('connection',socket=>{sockets.add(socket);socket.on('error',()=>{});socket.on('close',()=>sockets.delete(socket));});
let timer;
try{
  await new Promise((done,reject)=>{peer.once('error',reject);peer.listen(0,'127.0.0.1',done);});
  const origin='http://127.0.0.1:'+peer.address().port;
  await writeFile(join(home,'pilotdeck.yaml'),'schemaVersion: 1\nagent: {model: fixture/mock, maxOutputTokens: 128}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: '+origin+'/v1\n      apiKey: mock-only\n      models:\n        mock: {}\nadapters:\n  weixin:\n    enabled: true\n',{flag:'wx'});
  // Synthetic, owned credentials select only this run's loopback ILink fixture.
  await writeFile(join(home,'weixin-credentials.json'),JSON.stringify({baseUrl:origin,botToken:'mock-only',accountId:'fixture'}),{flag:'wx',mode:0o600});
  const token='boundary-'+randomUUID(),tokenFile=join(home,'token');
  await writeFile(tokenFile,token+'\n',{flag:'wx',mode:0o600});
  child=spawn(gateway,['--native','--home',home,'--project',project,'--listen','127.0.0.1:0','--token-file',tokenFile,'--sidecar',sidecar,'--drain-ms','2000'],{
    cwd:project,env:{PATH:process.env.PATH,HOME:home,PILOT_HOME:home,TMPDIR:tmp,TMP:tmp,TEMP:tmp,LANG:'en_US.UTF-8'},stdio:['ignore','pipe','pipe']});
  child.on('error',error=>{serverFailure=error;});
  exitPromise=new Promise(done=>child.once('exit',(code,signal)=>done({code,signal})));
  child.stderr.on('data',b=>{stderr+=b;});child.stdout.on('data',b=>{stdout+=b;});
  timer=setTimeout(()=>{serverFailure=Error('90 second probe deadline');ws?.close();child?.kill('SIGTERM');},90000);
  await wait(()=>/(?:^|\n)native listening (127\.0\.0\.1:\d+)\r?\n/.test(stderr),'native listener');
  const address=/(?:^|\n)native listening (127\.0\.0\.1:\d+)\r?\n/.exec(stderr)[1];
  ws=new WebSocket('ws://'+address+'/ws');
  const frames=[];
  function record(direction,raw){wireBytes+=Buffer.byteLength(raw);assert(wireBytes<=4*1024*1024,'WS evidence cap');wire.push({direction,raw});}
  ws.addEventListener('error',()=>{serverFailure=Error('WS transport error');});
  ws.addEventListener('message',event=>{
    try{record('receive',String(event.data));frames.push(JSON.parse(String(event.data)));}
    catch(error){serverFailure=error;}
  });
  const send=frame=>{const raw=JSON.stringify(frame);record('send',raw);ws.send(raw);};
  await wait(()=>ws.readyState===WebSocket.OPEN,'ordinary WS open');
  send({type:'hello',protocolVersion:'1.2',clientName:'web',clientVersion:'attachment-boundary',token});
  await wait(()=>frames.some(f=>f.type==='hello_ok'),'ordinary WS authenticated');
  pending.push({from_user_id:'chat',context_token:'ctx',message_type:1,message_id:'boundary-inbound',item_list:[
    {type:1,text_item:{text:typedMarker}},
    {type:4,file_item:{file_name:'attachment.txt',file_size:Buffer.byteLength(secret),cdn_url:origin+'/attachment.txt',aeskey:key.toString('hex')}}
  ]});
  const models=()=>rows.filter(r=>r.provider?.body.stream).map(r=>r.provider.body);
  await wait(()=>rows.some(r=>r.channel?.path.endsWith('/sendmessage')&&JSON.stringify(r.channel.body).includes('boundary-output')),'typed channel final outbound');
  const positive=models().filter(b=>JSON.stringify(b.messages).includes(typedMarker));
  assert(positive.length>0&&positive.some(b=>JSON.stringify(b.messages).includes(secret)),'typed attachment content must reach actual model HTTP');
  assert.equal(downloaded,1,'typed channel must actually download attachment');
  async function leaves(dir){
    const result=[];
    for(const entry of await readdir(dir,{withFileTypes:true})){
      const path=join(dir,entry.name);
      if(entry.isDirectory())result.push(...await leaves(path));else if(entry.isFile())result.push(path);
    }
    return result;
  }
  const cache=join(home,'im-attachments/weixin'),matches=[];
  for(const p of await leaves(cache))if((await readFile(p)).equals(Buffer.from(secret)))matches.push(p);
  assert.equal(matches.length,1,'locate actual typed cache bytes, not a guessed path');
  const attachmentPath=matches[0];
  checks.push({case:'typed-positive',modelRequests:positive.length,cachePath:attachmentPath,downloaded});
  for(const [name,channelKey,metadata] of [
    ['forged-channel-key','weixin',undefined],
    ['forged-metadata','web',{channelKey:'weixin'}],
    ['forged-both','weixin',{channelKey:'weixin'}]
  ]){
    const id=randomUUID(),marker='WS_'+randomUUID(),before=models().length;
    const attachment={type:'file',name:'attachment.txt',path:attachmentPath,mimeType:'text/plain',bytes:Buffer.byteLength(secret),...(metadata?{metadata}:{})};
    send({type:'request',id,method:'submit_turn',params:{projectKey:project,sessionKey:'web:'+randomUUID(),runId:randomUUID(),channelKey,message:marker,attachments:[attachment],canPrompt:false,maxTurns:1}});
    await wait(()=>frames.some(f=>f.id===id&&(f.type==='event'&&f.final||f.type==='response'&&f.ok===false)),name+' terminal');
    const received=frames.filter(f=>f.id===id);
    assert(!received.some(f=>f.ok===false||f.event?.type==='error'),JSON.stringify(received));
    assert(received.some(f=>f.event?.type==='turn_completed'&&f.event.finishReason==='completed'),'negative must complete, not silently reject');
    const actual=models().slice(before);
    assert(actual.length>0&&actual.every(b=>JSON.stringify(b.messages).includes(marker)),'negative must reach the model in its own fresh session');
    assert(actual.every(b=>!JSON.stringify(b.messages).includes(secret)),name+' leaked cache secret to model');
    checks.push({case:name,id,modelRequests:actual.length,secretAbsent:true});
  }
  assert.equal(await readFile(attachmentPath,'utf8'),secret,'cache bytes unchanged');
  assert.equal(await digest(gateway),inputs.gateway);assert.equal(await digest(sidecar),inputs.sidecar);
  assert(!serverFailure,String(serverFailure));
  ws.close();child.kill('SIGTERM');
  const stopped=await Promise.race([exitPromise,delay(10000).then(()=>{throw Error('graceful shutdown timeout');})]);
  assert.equal(stopped.code,0,'native shutdown must reap its sidecar normally');
  checks.push({case:'shutdown',...stopped});passed=true;
}catch(error){failure=String(error.stack??error);}
finally{
  clearTimeout(timer);ws?.close();
  if(child&&child.exitCode===null&&child.signalCode===null){
    child.kill('SIGTERM');
    const stopped=await Promise.race([exitPromise,delay(10000).then(()=>null)]);
    if(!stopped){child.kill('SIGKILL');passed=false;failure=(failure??'')+'\nforced host termination; descendant cleanup NOT VERIFIED';await exitPromise;}
  }
  for(const socket of sockets)socket.destroy();
  await new Promise(done=>peer.close(done));
  await writeFile(join(out,'evidence.json'),JSON.stringify({pass:passed,failure,serverFailure:String(serverFailure??''),formalAcceptance:false,providerWitness:'local HTTP fixture only',sourceManifestSha256,probeSha256:await digest(fileURLToPath(import.meta.url)),inputs,node:process.versions,checks,rows,wire,stderr,stdout},null,2)+'\n',{flag:'wx'});
}
assert(passed,failure??'attachment boundary failed');
