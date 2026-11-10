// Non-frozen real Server -> Cron -> actual A7 -> production Weixin ILink consumer.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer as httpServer} from 'node:http';
import {createServer as netServer,connect as tcpConnect} from 'node:net';
import {mkdir,readFile,writeFile,realpath} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {createHash,randomUUID} from 'node:crypto';
import {fileURLToPath} from 'node:url';
assert.equal(process.versions.node,'22.22.0');assert.equal(process.versions.icu,'78.2');
const [gatewayArg,sidecarArg,outArg]=process.argv.slice(2),engine=process.env.PDRS_PROBE_ENGINE??'native';
assert(['native','ts'].includes(engine));const source=process.env.PDRS_SOURCE_MANIFEST_SHA256;assert.match(source??'',/^[0-9a-f]{64}$/);
const gateway=await realpath(gatewayArg),sidecar=await realpath(sidecarArg),out=resolve(outArg);
await mkdir(out);const home=join(out,'home'),project=join(out,'project'),tmp=join(out,'tmp');
for(const p of [home,project,tmp])await mkdir(p);
// Fixed TS web project catalog reads pilotHome/projects/*/.cwd.
// Register the same real project before any Cron recovery preflight.
await mkdir(join(home,'projects','fixture'),{recursive:true});
await writeFile(join(home,'projects','fixture','.cwd'),project+'\n',{flag:'wx'});

const hash=async p=>createHash('sha256').update(await readFile(p)).digest('hex');
const binaries={gateway:await hash(gateway),sidecar:await hash(sidecar)};
const rows=[],models=[],wire=[],checks=[],pending=[],sockets=new Set();
let child,ws,exitPromise,stderr='',stdout='',failure,passed=false,serverFailure,retryAttempts=0,polls=0,modelCount=0,eventSequence=0;
const deadline=Date.now()+90000,delay=ms=>new Promise(r=>setTimeout(r,ms));
async function wait(predicate,label){while(!await predicate()){if(serverFailure)throw serverFailure;assert(Date.now()<deadline,label);assert(!child||child.exitCode===null&&child.signalCode===null,'host exited '+stderr);await delay(20);}}
const peer=httpServer(async(req,res)=>{try{
 let raw=Buffer.alloc(0);for await(const chunk of req){raw=Buffer.concat([raw,chunk]);assert(raw.length<=2*1024*1024);}
 const path=new URL(req.url,'http://127.0.0.1').pathname,body=raw.length?JSON.parse(raw):{};
 if(path==='/v1/chat/completions'){
  assert(++modelCount<=20);models.push({path,body});
  res.setHeader('Content-Type',body.stream?'text/event-stream':'application/json');
  if(!body.stream){res.end(JSON.stringify({choices:[{message:{role:'assistant',content:'{"title":"Cron delivery"}'},finish_reason:'stop'}]}));return;}
  const text=JSON.stringify(body.messages).includes('WX_CRON_DISABLED_ACTUAL')?'WX_CRON_DISABLED_RESULT':JSON.stringify(body.messages).includes('WX_CRON_RETRY_ACTUAL')?'WX_CRON_RETRY_RESULT':'WX_PRIME_RESULT';
  for(const [delta,finish_reason] of [[{role:'assistant',content:text},null],[{},'stop']])res.write('data: '+JSON.stringify({id:'mock',choices:[{index:0,delta,finish_reason}]})+'\n\n');
  res.end('data: [DONE]\n\n');return;
 }
 const entry={path,body,requestSequence:++eventSequence};rows.push(entry);res.setHeader('Content-Type','application/json');
 if(path.endsWith('/getupdates')){polls++;await delay(40);entry.pollResponseSequence=++eventSequence;res.end(JSON.stringify({ret:0,msgs:pending.splice(0),get_updates_buf:'cursor'}));return;}
 if(path.endsWith('/sendmessage')){
  if(JSON.stringify(body).includes('WX_CRON_RETRY_RESULT')){entry.attempt=++retryAttempts;entry.responseSequence=++eventSequence;entry.simulatedFailure=retryAttempts===1;entry.httpStatus=retryAttempts===1?500:200;res.statusCode=entry.httpStatus;res.end(JSON.stringify(retryAttempts===1?{ret:1,errmsg:'fixture first delivery failure'}:{ret:0}));}
  else res.end('{"ret":0}');return;
 }
 if(['/getconfig','/sendtyping'].some(s=>path.endsWith(s))){res.end('{"ret":0}');return;}
 throw Error('unexpected loopback '+path);
}catch(error){serverFailure=error;res.writeHead(500);res.end('fixture failure');}});
peer.on('connection',socket=>{sockets.add(socket);socket.on('error',()=>{});socket.on('close',()=>sockets.delete(socket));});
let timer;
try{
 await new Promise((done,reject)=>{peer.once('error',reject);peer.listen(0,'127.0.0.1',done);});const origin='http://127.0.0.1:'+peer.address().port;
 const config=enabled=>'schemaVersion: 1\nagent: {model: fixture/mock, maxOutputTokens: 128}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: '+origin+'/v1\n      apiKey: mock-only\n      models: {mock: {}}\ntelemetry: {enabled: false}\ncron: {enabled: true, timezone: UTC}\nadapters:\n  weixin:\n    enabled: '+enabled+'\n';
 await writeFile(join(home,'pilotdeck.yaml'),config(true),{flag:'wx'});
 const credentialsPath=engine==='ts'?join(home,'.pilotdeck','weixin-credentials.json'):join(home,'weixin-credentials.json');
 if(engine==='ts')await mkdir(join(home,'.pilotdeck'));
 await writeFile(credentialsPath,JSON.stringify({baseUrl:origin,botToken:'mock-only',accountId:'fixture'}),{flag:'wx',mode:0o600});
 const recordedCredentials=JSON.parse(await readFile(credentialsPath,'utf8'));
 assert.equal(recordedCredentials.baseUrl,origin);assert(recordedCredentials.botToken&&recordedCredentials.accountId);
 const token='cron-'+randomUUID(),tokenFile=join(home,'token');await writeFile(tokenFile,token+'\n',{flag:'wx',mode:0o600});
 const reservation=netServer();await new Promise(r=>reservation.listen(0,'127.0.0.1',r));const port=reservation.address().port;await new Promise(r=>reservation.close(r));
 const env={PATH:process.env.PATH,HOME:home,PILOT_HOME:home,TMPDIR:tmp,LANG:'en_US.UTF-8',PILOTDECK_GATEWAY_HOST:'127.0.0.1',PILOTDECK_GATEWAY_PORT:String(port),PILOTDECK_GATEWAY_TOKEN:token,PILOTDECK_GATEWAY_TOKEN_PATH:tokenFile,PILOTDECK_SKIP_BROWSER_OPEN:'1'};
 let command=gateway,args=['--native','--home',home,'--project',project,'--listen','127.0.0.1:'+port,'--token-file',tokenFile,'--sidecar',sidecar,'--drain-ms','2000'];
 if(engine==='ts'){
  const baseline=process.env.PDRS_BASELINE;assert(baseline);
  const guard=await import('../_shared/baseline.mts');
  assert.equal(guard.assertBaseline(baseline),'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
  env.NODE_OPTIONS='--import '+JSON.stringify(join(fileURLToPath(new URL('.',import.meta.url)),'loopback-network-guard.mjs'));
  env.PDRS_LOOPBACK_GUARD_LOG=join(out,'loopback-guard.jsonl');
  env.PDRS_FIXTURE_CREDENTIALS_PATH=credentialsPath;
  env.PDRS_FIXTURE_ORIGIN=origin;
  command=process.execPath;args=['--import',join(baseline,'node_modules/tsx/dist/loader.mjs'),join(baseline,'src/cli/pilotdeck.ts'),'server'];
 }
 child=spawn(command,args,{cwd:project,env,stdio:['ignore','pipe','pipe']});
 child.on('error',e=>serverFailure=e);exitPromise=new Promise(done=>child.once('exit',(code,signal)=>done({code,signal})));
 child.stderr.on('data',b=>stderr+=b);child.stdout.on('data',b=>stdout+=b);
 timer=setTimeout(()=>{serverFailure=Error('90 second deadline');child?.kill('SIGTERM');},90000);
 await wait(()=>new Promise(done=>{const s=tcpConnect(port,'127.0.0.1');s.once('connect',()=>{s.destroy();done(true);});s.once('error',()=>done(false));}),'listener');
 ws=new WebSocket('ws://127.0.0.1:'+port+'/ws');const frames=[];let wireBytes=0;
 const capture=(direction,raw)=>{wireBytes+=Buffer.byteLength(raw);assert(wireBytes<=4*1024*1024);wire.push({direction,raw});};
 ws.addEventListener('message',e=>{try{capture('receive',String(e.data));frames.push(JSON.parse(String(e.data)));}catch(error){serverFailure=error;}});
 ws.addEventListener('error',()=>serverFailure=Error('WS error'));
 await wait(()=>ws.readyState===WebSocket.OPEN,'WS open');
 const send=frame=>{const raw=JSON.stringify(frame);capture('send',raw);ws.send(raw);};
 send({type:'hello',protocolVersion:'1.2',token,clientName:'web',clientVersion:'weixin-cron-consumer'});
 await wait(()=>frames.some(f=>f.type==='hello_ok'),'hello');
 async function rpc(method,params){const id=randomUUID();send({type:'request',id,method,params});await wait(()=>frames.some(f=>f.id===id&&f.type==='response'),method);const f=frames.find(f=>f.id===id&&f.type==='response');assert.equal(f.ok,true,JSON.stringify(f));return f.result;}
 async function createAndRun(message){
  const c=await rpc('cron_create',{projectKey:project,message,sessionKey:'weixin:chat=chat:general',channelKey:'weixin',mode:'default',schedule:{type:'once',runAt:'2036-01-01T00:00:00.000Z'}});
  const start=await rpc('cron_run_now',{projectKey:project,taskId:c.task.taskId});assert.equal(start.started,true);
  let run;await wait(async()=>{const list=await rpc('cron_list',{projectKey:project,includeHistory:true,limit:100});run=list.recentRuns?.find(r=>r.taskId===start.taskId);return !!run;},'persisted Cron run');assert.equal(run.outcome,'completed');return {created:c,started:start,run};
 }
 const registered=await rpc('list_projects',{});
 assert(registered.projects?.some(p=>resolve(p.projectKey)===project),'public project catalog must contain the actual Cron project');
 checks.push({case:'registered-project',projectKey:project});
 pending.push({from_user_id:'chat',context_token:'ctx',message_type:1,message_id:'prime',item_list:[{type:1,text_item:{text:'WX_PRIME_CHAT'}}]});
 await wait(()=>rows.some(r=>r.path.endsWith('/sendmessage')&&JSON.stringify(r.body).includes('WX_PRIME_RESULT')),'real inbound establishes ILink context and A7');
 if(engine==='ts'){
  const guardRows=(await readFile(join(out,'loopback-guard.jsonl'),'utf8')).trim().split('\n').map(l=>JSON.parse(l));
  assert(guardRows.some(r=>r.type==='credentials-read'&&r.valid),'actual TS loadCredentials must read the exact mock path');
  assert(!guardRows.some(r=>r.type==='denied'),'any non-loopback attempt fails this fixture');
 }
 const positive=await createAndRun('WX_CRON_RETRY_ACTUAL');
 await wait(()=>retryAttempts>=2,'failed Cron reply queued then resent on poll');
 const attempts=rows.filter(r=>r.path.endsWith('/sendmessage')&&JSON.stringify(r.body).includes('WX_CRON_RETRY_RESULT'));
 assert.equal(attempts.length,2);assert(attempts[0].simulatedFailure);assert.equal(attempts[0].httpStatus,500);assert.equal(attempts[1].httpStatus,200);assert.equal(attempts[1].simulatedFailure,false);
 assert(rows.some(r=>r.path.endsWith('/getupdates')&&r.pollResponseSequence>attempts[0].responseSequence&&r.pollResponseSequence<attempts[1].requestSequence),'retry follows a fresh actual poll completion');
 const actual=models.filter(r=>r.body.stream&&JSON.stringify(r.body.messages).includes('WX_CRON_RETRY_ACTUAL'));assert.equal(actual.length,1,'delivery retry must not rerun A7');
 checks.push({case:'cron-actual-a7-delivery-failure-queue-retry',...positive,modelRequests:actual.length,attempts});
 await writeFile(join(home,'pilotdeck.yaml'),config(false));await rpc('reload_config',{});
 await wait(async()=>{try{return ['stopped','disabled'].includes(JSON.parse(await readFile(join(home,'channels/runtime-status.json'),'utf8')).channels?.weixin?.state);}catch{return false;}},'configured Weixin stopped');
 const count=rows.filter(r=>r.path.endsWith('/sendmessage')).length;
 const disabled=await createAndRun('WX_CRON_DISABLED_ACTUAL');
 const disabledModels=models.filter(r=>r.body.stream&&JSON.stringify(r.body.messages).includes('WX_CRON_DISABLED_ACTUAL'));assert.equal(disabledModels.length,1,'disabled-source Cron still executes actual A7');
 assert.equal(await hash(gateway),binaries.gateway);assert.equal(await hash(sidecar),binaries.sidecar);
 ws.close();child.kill('SIGTERM');const stop=await Promise.race([exitPromise,delay(10000).then(()=>{throw Error('shutdown timeout');})]);assert.equal(stop.code,0);
 assert.equal(rows.filter(r=>r.path.endsWith('/sendmessage')).length,count,'disabled source cannot deliver after owned shutdown');
 checks.push({case:'disabled-origin-no-send',...disabled,modelRequests:disabledModels.length,sendCountUnchanged:true});checks.push({case:'shutdown',...stop});
 if(engine==='ts'){const guardRows=(await readFile(join(out,'loopback-guard.jsonl'),'utf8')).trim().split('\n').map(l=>JSON.parse(l));assert(!guardRows.some(r=>r.type==='denied'),'non-loopback attempt');}
 assert(!serverFailure,String(serverFailure));passed=true;
}catch(error){failure=String(error.stack??error);}
finally{
 clearTimeout(timer);ws?.close();
 if(child&&child.exitCode===null&&child.signalCode===null){child.kill('SIGTERM');const stopped=await Promise.race([exitPromise,delay(10000).then(()=>null)]);if(!stopped){child.kill('SIGKILL');failure=(failure??'')+'\nforced cleanup descendants NOT VERIFIED';await exitPromise;passed=false;}}
 for(const s of sockets)s.destroy();await new Promise(r=>peer.close(r));
 await writeFile(join(out,'evidence.json'),JSON.stringify({pass:passed,engine,sourceManifestSha256:source,binaries,node:process.versions,formalAcceptance:false,checks,rows,models,wire,stdout,stderr,failure,serverFailure:String(serverFailure??''),polls},null,2)+'\n',{flag:'wx'});
}
assert(passed,failure??'Weixin Cron consumer failed');
