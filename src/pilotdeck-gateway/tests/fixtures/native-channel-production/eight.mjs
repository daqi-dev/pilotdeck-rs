// Ordinary real production Server/A7 loopback fixture. No test executable or real account.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdir,writeFile,readFile,stat} from 'node:fs/promises';
import {join,resolve,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createRequire} from 'node:module';
import {createHash} from 'node:crypto';
import {selectModelRoundInput,selectPersistedRound} from './reference-round-context.mjs';
const [baseline,gateway,sidecar,outArg,kind,engineArg,layoutArg]=process.argv.slice(2);
assert(['telegram','discord','slack','dingtalk','feishu','weixin','qq','wecom'].includes(kind));
const engine=engineArg??'native';assert(['native','ts'].includes(engine));
const fixedTS='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const out=resolve(outArg);await mkdir(out,{recursive:false,mode:0o700});
const require=createRequire(join(resolve(baseline),'package.json')),prerequisiteFailures=[];
try{assert.equal(execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim(),fixedTS);execFileSync('git',['-C',baseline,'diff','--exit-code','HEAD','--','src','packages/sdk','package.json','pnpm-lock.yaml'],{stdio:['ignore','pipe','pipe']});}catch(error){prerequisiteFailures.push({kind:'fixed-ts-source',detail:String(error.message)});}
const optionalSDK={telegram:'grammy',discord:'discord.js',slack:'@slack/bolt',dingtalk:'dingtalk-stream'}[kind];
for(const dependency of engine==='ts'?['ws','tsx','@pilotdeck/sdk','@larksuiteoapi/node-sdk','weixin-ilink','undici',...(optionalSDK?[optionalSDK]:[])]:['ws']){try{require.resolve(dependency);}catch{prerequisiteFailures.push({kind:'missing-real-dependency',dependency});}}
if(engine==='ts'){for(const file of ['src/cli/pilotdeck.ts','node_modules/tsx/dist/loader.mjs']){try{assert((await stat(join(resolve(baseline),file))).isFile());}catch{prerequisiteFailures.push({kind:'missing-real-entrypoint',file});}}const [major,minor]=process.versions.node.split('.').map(Number);if(major!==22||minor<13)prerequisiteFailures.push({kind:'unsupported-reference-runtime',required:'>=22.13.0 <23',actual:process.version});}
if(prerequisiteFailures.length){await writeFile(join(out,'evidence.json'),JSON.stringify({schemaVersion:2,profile:'native-eight-v2',channel:kind,engine,fixedTS,status:'BLOCKED',formalAcceptance:false,prerequisiteFailures,steps:[],failure:'Production prerequisites unavailable; no gateway or replacement SDK was executed.'},null,2)+'\n',{flag:'wx'});process.exit(1);}
const {WebSocket,WebSocketServer}=require('ws');
const layout=layoutArg?JSON.parse(await readFile(layoutArg,'utf8')):null;if(layout){assert.equal(layout.root,join(dirname(out),'cur'));for(const key of ['mockPort','controlPort'])assert(Number.isInteger(layout[key])&&layout[key]>0&&layout[key]<65536);assert.notEqual(layout.mockPort,layout.controlPort);await mkdir(layout.root,{mode:0o700});}
// The fixed TS server uses PILOT_HOME for unbound channel sessions; process.cwd() only selects its config/startup root.
const osHome=join(layout?.root??out,'os-home'),home=join(layout?.root??out,'home'),project=join(layout?.root??out,'project'),tmp=join(layout?.root??out,'tmp');for(const p of [osHome,home,project,tmp])await mkdir(p,{mode:0o700});
const phases=new Map(),permissionGate=[],answerSent=[];const models=[],http=[],wire=[],frames=[],inbound=[],steps=[],sockets=new Set(),peers=new Set(),pending=[],deliveries=[],persistedSessions=[],persistedMessages=[],roundContexts=[],modelRoundInputs=[];
let generation=0,seq=0,child,control,failure=null,exit=null,stdout='',stderr='',enabled=true;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function wait(fn,label,timeout=35000){const end=Date.now()+timeout;while(!await fn()){if(child?.spawnError)throw child.spawnError;if(engine==='ts'&&/not installed;|Cannot find (?:package|module)|ERR_MODULE_NOT_FOUND/.test(stderr))throw Error('real TS dependency/startup unavailable: '+stderr);const broken=http.find(row=>row.fixtureError);if(broken)throw Error(broken.fixtureError);if(child&&child.exitCode!==null)throw Error('gateway exited '+child.exitCode+' '+label);if(Date.now()>end)throw Error('timeout '+label);await delay(25)}}
const send=(ws,v)=>ws.send(typeof v==='object'&&!Buffer.isBuffer(v)?JSON.stringify(v):v);
function varint(n){const a=[];while(n>=128){a.push((n&127)|128);n=Math.floor(n/128)}a.push(n);return Buffer.from(a)}
function bytes(k,v){v=Buffer.isBuffer(v)?v:Buffer.from(v);return Buffer.concat([varint(k*8+2),varint(v.length),v])}
function feishuFrame(v,n){const fields=[];for(const [k,val] of [[1,n],[2,n+100],[3,77],[4,1]])fields.push(varint(k*8),varint(val));for(const [k,val] of [['type','event'],['message_id',`wire-${n}`],['sum','1'],['seq','0']])fields.push(bytes(5,Buffer.concat([bytes(1,k),bytes(2,val)])));fields.push(bytes(6,'utf8'),bytes(7,'json'),bytes(8,JSON.stringify(v)));return Buffer.concat(fields)}
function inputFrame(text){const n=++seq;if(kind==='discord')return {op:0,t:'MESSAGE_CREATE',s:n+1,d:{id:`event-${n}`,channel_id:'42',type:0,content:text,author:{id:'human',bot:false}}};if(kind==='qq')return {op:0,t:'GROUP_AT_MESSAGE_CREATE',s:n+1,d:{id:`event-${n}`,group_openid:'group',author:{member_openid:'human'},content:text}};if(kind==='dingtalk')return {type:'EVENT',headers:{topic:'/v1.0/im/bot/messages/get',messageId:`event-${n}`},data:JSON.stringify({msgId:`event-${n}`,conversationId:'room',msgtype:'text',text:{content:text},sessionWebhook:'https://api.dingtalk.com/lifecycle'})};if(kind==='slack')return {type:'events_api',envelope_id:`env-${n}`,accepts_response_payload:false,payload:{type:'event_callback',event:{type:'message',channel:'Droom',channel_type:'im',user:'HUMAN',ts:`1700000000.${n}`,text}}};if(kind==='wecom')return {cmd:'aibot_msg_callback',headers:{req_id:`in-${n}`},body:{msgid:`m-${n}`,from:{userid:'alice'},chatid:'alice',chattype:'single',msgtype:'text',text:{content:text}}};return feishuFrame({schema:'2.0',header:{event_id:`event-${n}`,event_type:'im.message.receive_v1'},event:{sender:{sender_id:{open_id:'human'},sender_type:'user'},message:{message_id:`message-${n}`,chat_id:'room',chat_type:'p2p',message_type:'text',content:JSON.stringify({text})}}},n)}
const service=createServer(async(req,res)=>{try{let raw=[];for await(const c of req)raw.push(c);raw=Buffer.concat(raw);const path=new URL(req.url,'http://127.0.0.1').pathname;let body;try{body=JSON.parse(raw)}catch{body={raw:raw.toString()}}const httpIndex=http.push({generation,time:Date.now(),method:req.method,path,body,authorization:req.headers.authorization??null})-1;res.setHeader('content-type','application/json');let value={};if(path.startsWith('/v1/')){models.push(body);const matches=JSON.stringify(body.messages??[]).match(/CHANNEL_INPUT_[01]/g)??[];const answer=matches.at(-1)?.replace('INPUT','OUTPUT')??'{"title":"Channel"}';if(body.stream){const phase=phases.get(generation)??0;phases.set(generation,phase+1);const approved=join(project,`permission-${generation}.txt`);const tool=phase===0?{id:`permission-${generation}`,type:'function',function:{name:'bash',arguments:JSON.stringify({command:"printf approved > '"+approved+"'"})}}:phase===1?{id:`question-${generation}`,type:'function',function:{name:'ask_user_question',arguments:JSON.stringify({questions:[{header:'Choice',question:'Choose mock option',options:[{label:'One',description:'First mock'},{label:'Two',description:'Second mock'}],multiSelect:false}]})}}:null;const delta=tool?{role:'assistant',tool_calls:[{index:0,...tool}]}:{role:'assistant',content:answer};res.setHeader('content-type','text/event-stream');res.end('data: '+JSON.stringify({id:'mock',choices:[{index:0,delta,finish_reason:null}]})+'\n\ndata: '+JSON.stringify({id:'mock',choices:[{index:0,delta:{},finish_reason:tool?'tool_calls':'stop'}]})+'\n\ndata: [DONE]\n\n');return}value={choices:[{message:{role:'assistant',content:answer},finish_reason:'stop'}]};}
// Discord's production API root includes /api/v10. Do not accept a stripped
// prefix through the generic message routes: both engines must retain it.
else if(kind==='discord'){
 if(path==='/api/v10/gateway/bot')value={url:mock.replace('http:','ws:')+'/discord/stream'};
 else if(path==='/api/v10/users/@me')value={id:'bot'};
 else if(path==='/api/v10/channels/42')value={id:'42',type:0};
 else if(path==='/api/v10/channels/42/messages'||path==='/api/v10/channels/42/typing')value={id:'outbound'};
 else {res.statusCode=404;value={error:'unknown Discord fixture path',path};}
}
else if(path.endsWith('/getMe'))value={ok:true,result:{id:1,username:'mock'}};
else if(path.endsWith('/getUpdates')){await delay(50);const queued=pending.shift(),text=queued?.text,id=++seq;value={ok:true,result:text?[{update_id:id,message:{message_id:id,chat:{id:42,type:'private'},from:{id:7},text}}]:[]};if(queued)deliveries[queued.deliveryIndex].frames.push({transport:'poll',httpIndex,frame:value.result[0]});}
else if(path.endsWith('/getupdates')){await delay(50);const queued=pending.shift(),text=queued?.text;value={ret:0,msgs:text?[{from_user_id:'chat',context_token:'ctx',message_type:1,item_list:[{type:1,text_item:{text}}]}]:[],get_updates_buf:'cursor'};if(queued)deliveries[queued.deliveryIndex].frames.push({transport:'poll',httpIndex,frame:value.msgs[0]});}
else if(path.endsWith('/sendmessage')||path.endsWith('/getconfig')||path.endsWith('/sendtyping'))value={ret:0};
else if(path.endsWith('/gettoken')||path.includes('tenant_access_token'))value={code:0,tenant_access_token:'token',expire:7200};
else if(kind==='qq'&&path==='/gateway')value={url:mock.replace('http:','ws:')+'/qq/stream'};
else if(path==='/app/getAppAccessToken')value={access_token:'mock-token',expires_in:'7200'};
else if(path==='/v1.0/gateway/connections/open')value={endpoint:mock.replace('http:','ws:')+'/dingtalk/stream',ticket:'mock-ticket'};
else if(path==='/api/apps.connections.open')value={ok:true,url:mock.replace('http:','ws:')+'/slack/stream'};
else if(path==='/api/auth.test')value={ok:true,user_id:'BOT',user:'mockbot'};
else if(path==='/api/chat.postMessage')value={ok:true,ts:'reply'};
else if(path==='/callback/ws/endpoint')value={code:0,data:{URL:mock.replace('http:','ws:')+'/feishu/stream?service_id=77&device_id=fixture',ClientConfig:{PingInterval:1,ReconnectCount:2,ReconnectInterval:0.01,ReconnectNonce:0}}};
else if(path.startsWith('/open-apis/'))value={code:0,data:{message_id:'reply',reaction_id:'reaction'}};
else if(path.endsWith('/sendMessage')||path.endsWith('/sendChatAction')||path.endsWith('/deleteWebhook'))value={ok:true,result:{}};
else if(path.endsWith('/messages')||path.endsWith('/typing'))value={id:'outbound'};
else if(kind==='dingtalk'&&path==='/lifecycle')value={errcode:0,errmsg:'ok'};
else {res.statusCode=404;value={error:'unknown fixture path',path};}
res.end(JSON.stringify(value));}catch(e){http.push({fixtureError:String(e.stack??e)});res.statusCode=500;res.end(JSON.stringify({error:String(e)}))}});
service.on('connection',s=>{sockets.add(s);s.on('error',()=>{});s.on('close',()=>sockets.delete(s))});
const wss=new WebSocketServer({server:service});wss.on('connection',ws=>{peers.add(ws);ws.readyForInput=['slack','dingtalk','feishu'].includes(kind);ws.on('close',()=>peers.delete(ws));ws.on('error',()=>{});if(['discord','qq'].includes(kind))send(ws,{op:10,d:{heartbeat_interval:1000}});ws.on('message',(b,binary)=>{if(binary){wire.push({binary:b.toString('base64')});return}let v;try{v=JSON.parse(b)}catch{return}wire.push(v);if(v.op===2){send(ws,{op:0,t:'READY',s:1,d:{session_id:'fixture',user:{id:'bot'},resume_gateway_url:mock.replace('http:','ws:')+'/'+kind+'/stream'}});ws.readyForInput=true;}if(v.op===1)send(ws,{op:11});if(kind==='wecom'){assert(v.headers?.req_id);if(v.cmd==='aibot_subscribe'){assert.equal(v.body.bot_id,'mock-bot');assert.equal(v.body.secret,'mock-secret');ws.readyForInput=true;}send(ws,{headers:{req_id:v.headers.req_id},body:{errcode:0}});}});});
await new Promise(r=>service.listen(layout?.mockPort??0,'127.0.0.1',r));const mock='http://127.0.0.1:'+service.address().port;
let ctrlPort=layout?.controlPort;if(!ctrlPort){const ctrlServer=createServer();await new Promise(r=>ctrlServer.listen(0,'127.0.0.1',r));ctrlPort=ctrlServer.address().port;await new Promise(r=>ctrlServer.close(r));}
const channelOptions=()=>({telegram:{token:'mock-token'},discord:{token:'mock-token'},slack:{token:'mock-bot-token',extra:{appToken:'mock-app-token'}},dingtalk:{extra:{clientId:'mock-id',clientSecret:'mock-secret'}},feishu:{appId:'cli_0123456789abcdef',appSecret:'mock-secret',connectionMode:'stream'},weixin:{},qq:{appId:'mock-app',clientSecret:'mock-secret',allowGroups:['*']},wecom:{token:'mock-bot',extra:{botSecret:'mock-secret',dmPolicy:'open',textBatchDelayMs:0,websocketUrl:mock.replace('http:','ws:')+'/wecom'}}}[kind]);
function config(on){return JSON.stringify({schemaVersion:1,extension:{builtinPluginsEnabled:{'browser-use':false,funasr:false,'windows-skills':false}},agent:{model:'fixture/mock'},model:{providers:{fixture:{protocol:'openai',url:mock+'/v1',apiKey:'mock-only',models:{mock:{}}}}},adapters:{[kind]:{enabled:on,...channelOptions()}}})}
const cfg=join(home,'pilotdeck.yaml'),tokenPath=join(home,'token');await writeFile(cfg,config(true));await writeFile(tokenPath,'fixture-gateway-token\n');if(kind==='weixin'){const credentials=JSON.stringify({baseUrl:mock,botToken:'mock-token',accountId:'mock-account'});await writeFile(join(home,'weixin-credentials.json'),credentials);await mkdir(join(osHome,'.pilotdeck'),{mode:0o700});await writeFile(join(osHome,'.pilotdeck','weixin-credentials.json'),credentials);}
function sent(g){const marker=`CHANNEL_OUTPUT_${g}`;return http.some(x=>JSON.stringify(x.body).includes(marker)&&!x.path.startsWith('/v1/'))||wire.some(x=>x.cmd==='aibot_respond_msg'&&JSON.stringify(x.body).includes(marker))}
function outboundReceipt(text,httpStart,wireStart){
 const httpIndex=http.findIndex((row,index)=>index>=httpStart&&!row.path?.startsWith('/v1/')&&JSON.stringify(row.body).includes(text));if(httpIndex>=0)return {source:'http',index:httpIndex};
 const wireIndex=wire.findIndex((row,index)=>index>=wireStart&&row.cmd==='aibot_respond_msg'&&JSON.stringify(row.body).includes(text));return wireIndex<0?null:{source:'wire',index:wireIndex};
}
async function deliver(text,g){
 const initial=text===`CHANNEL_INPUT_${g}`,row={generation:g,text,initial,time:Date.now(),frames:[]},deliveryIndex=deliveries.push(row)-1;
 if(initial)inbound.push(row);
 if(['telegram','weixin'].includes(kind)){pending.push({text,deliveryIndex});return deliveryIndex;}
 await wait(()=>[...peers].some(peer=>peer.readyForInput),'peer ready');
 for(const peer of peers)if(peer.readyForInput){
  const frame=inputFrame(text),record=Buffer.isBuffer(frame)?{rawBytesHex:frame.toString('hex')}:frame;
  row.frames.push({transport:'websocket',frame:record});row.frame=record;
  if(kind==='qq'||kind==='discord')row.messageId=frame.d.id;if(kind==='wecom')row.reqId=frame.headers.req_id;
  send(peer,frame);
 }
 return deliveryIndex;
}
async function incoming(g){
 const hs=http.length,ws=wire.length,modelStart=models.length;await deliver(`CHANNEL_INPUT_${g}`,g);
 let prompt;await wait(()=>prompt=outboundReceipt('回复 1 允许一次',hs,ws),'actual permission prompt '+g);
 const file=join(project,`permission-${g}.txt`),exists=await readFile(file).then(()=>true,()=>false);permissionGate.push({generation:g,fileExistsBeforeAnswer:exists,path:file,promptReceipt:prompt});assert.equal(exists,false);
 async function answerPermission(attempt,retryAfterReceipt){
  const fileExistsBeforeAnswer=await readFile(file).then(()=>true,()=>false);assert.equal(fileExistsBeforeAnswer,false,'permission side effect cannot precede the accepted answer');
  const receipt={generation:g,kind:'permission',text:'1',decision:'allow_once',attempt,httpStart:http.length,wireStart:wire.length,fileExistsBeforeAnswer,promptReceipt:prompt,...(retryAfterReceipt?{retryAfterReceipt}:{}),time:Date.now()};
  receipt.deliveryIndex=await deliver('1',g);answerSent.push(receipt);return receipt;
 }
 let answer=await answerPermission(1),question;
 await wait(async()=>{
  question=outboundReceipt('Choose mock option',hs,ws);if(question)return true;
  const accepted=outboundReceipt('已允许一次，继续执行。',answer.httpStart,answer.wireStart);if(accepted){answer.confirmationReceipt=accepted;return false;}
  assert(!outboundReceipt('权限请求已失效',answer.httpStart,answer.wireStart),'permission request must remain live until accepted');
  const rejected=outboundReceipt('权限提示发送中，请稍候',answer.httpStart,answer.wireStart);
  if(rejected){assert(answer.attempt<3,'bounded retry after the real channel rejects a premature answer');answer=await answerPermission(answer.attempt+1,rejected);}
  return false;
 },'actual permission acceptance and elicitation prompt '+g);
 assert.equal(await readFile(file,'utf8'),'approved');
 const elicitation={generation:g,kind:'elicitation',text:'1',promptReceipt:question,time:Date.now()};elicitation.deliveryIndex=await deliver('1',g);answerSent.push(elicitation);
 await wait(()=>sent(g),'actual outbound '+g);
 const requests=models.flatMap((body,index)=>index>=modelStart&&body.stream?[{body,index}]:[]);assert(requests.length,'actual current-round model requests');
 for(const {body,index}of requests){const selected=selectModelRoundInput(body.messages,`CHANNEL_INPUT_${g}`);assert(selected,'model user input belongs to this round');modelRoundInputs.push({generation:g,modelIndex:index,userMessageIndex:selected.index,marker:`CHANNEL_INPUT_${g}`});}
 steps.push({event:'actual-model',generation:g,time:Date.now()});steps.push({event:'actual-outbound',generation:g,time:Date.now()});
 await wait(()=>capturePersistedContext(g),'durable current channel round '+g);
}

let rpcSequence=0;
async function rpc(method,params){
 const id=`channel-history-${++rpcSequence}`,request={type:'request',id,method,params};send(control,request);
 await wait(()=>frames.some(frame=>frame.type==='response'&&frame.id===id),method);
 const frameIndex=frames.findIndex(frame=>frame.type==='response'&&frame.id===id),response=frames[frameIndex];assert.equal(response.ok,true,`${method}: ${JSON.stringify(response.error)}`);
 return {request,frameIndex,response};
}
async function capturePersistedContext(g){
 const matches=[];
 for(const projectKey of [home,project]){
  let cursor;const seen=new Set();
  do{
   const receipt=await rpc('list_sessions',{projectKey,limit:100,...(cursor?{cursor}:{})}),listIndex=persistedSessions.push({generation:g,...receipt})-1,result=receipt.response.result;assert(Array.isArray(result?.sessions));
   for(const entry of result.sessions){
    const sessionKey=entry.sessionKey??entry.sessionId;if(typeof sessionKey!=='string'||!sessionKey.startsWith(kind+':'))continue;
    const history=await rpc('read_session_messages',{projectKey,sessionKey,limit:0}),messageIndex=persistedMessages.push({generation:g,listIndex,catalogSession:entry,...history})-1,value=history.response.result;
    assert.equal(value.session?.sessionKey,sessionKey,'history identity comes from the real catalog');
    const selected=selectPersistedRound(value.messages,`CHANNEL_INPUT_${g}`,`CHANNEL_OUTPUT_${g}`);if(!selected)continue;
    assert.equal(projectKey,home,'unbound channel history uses PILOT_HOME');assert.equal(value.session?.cwd,home,'durable transcript records the actual default project');
    if(g>0){
     const previous=roundContexts.find(row=>row.generation===g-1);assert(previous,'previous round was captured');
     assert.equal(projectKey,previous.projectKey);assert.equal(sessionKey,previous.sessionKey,'reload continues the same durable session');assert.notEqual(selected.turnId,previous.turnId,'each input owns a new turn');
     const earlier=selectPersistedRound(value.messages,`CHANNEL_INPUT_${g-1}`,`CHANNEL_OUTPUT_${g-1}`);assert(earlier,'reload retains the first input and output');assert.equal(earlier.turnId,previous.turnId);
    }
    matches.push({generation:g,projectKey,sessionKey,turnId:selected.turnId,listIndex,messageIndex,anchorIndices:selected.anchorIndices,userIndices:selected.userIndices,outputIndices:selected.outputIndices});
   }
   cursor=result.nextCursor;if(cursor){assert(!seen.has(cursor),'catalog cursor progresses');seen.add(cursor);assert(seen.size<20,'bounded channel catalog');}
  }while(cursor);
 }
 if(!matches.length)return false;assert.equal(matches.length,1,'one durable channel session contains the current round');roundContexts.push(matches[0]);steps.push({event:'persisted-channel-context',generation:g,matches,time:Date.now()});return true;
}

try{
if(engine==='ts')await writeFile(join(out,'ts-redirects.jsonl'),'',{flag:'wx'});
const preload=fileURLToPath(new URL('./media-ts-loopback.mjs',import.meta.url)),executable=engine==='ts'?process.execPath:gateway,args=engine==='ts'?['--import',preload,'--import',join(resolve(baseline),'node_modules/tsx/dist/loader.mjs'),join(resolve(baseline),'src/cli/pilotdeck.ts'),'server']:['--native','--home',home,'--project',home,'--listen',`127.0.0.1:${ctrlPort}`,'--token-file',tokenPath,'--sidecar',sidecar,'--drain-ms','2000'];
child=spawn(executable,args,{cwd:project,env:{PATH:process.env.PATH,LD_LIBRARY_PATH:process.env.LD_LIBRARY_PATH,LANG:'C.UTF-8',HOME:osHome,PILOT_HOME:home,TMPDIR:tmp,PILOTDECK_GATEWAY_HOST:'127.0.0.1',PILOTDECK_GATEWAY_PORT:String(ctrlPort),PILOTDECK_GATEWAY_TOKEN:'fixture-gateway-token',PILOTDECK_GATEWAY_TOKEN_PATH:tokenPath,PILOTDECK_SKIP_BROWSER_OPEN:'1',DATABASE_PATH:join(home,'auth.db'),WORKSPACES_ROOT:join(layout?.root??out,'workspaces'),PDRS_TEST_CHANNEL_KIND:kind,PDRS_TEST_CHANNEL_LOOPBACK_ORIGIN:mock,PDRS_TEST_CHANNEL_REDIRECT_RECEIPT:join(out,'ts-redirects.jsonl')},stdio:['ignore','pipe','pipe']});child.once('error',error=>{child.spawnError=error;});const stopped=new Promise(r=>child.once('exit',(code,signal)=>r({code,signal})));child.stdout.on('data',b=>stdout+=b);child.stderr.on('data',b=>stderr+=b);
await wait(()=>['telegram','weixin'].includes(kind)?http.some(x=>x.path.endsWith(kind==='telegram'?'/getUpdates':'/getupdates')):[...peers].some(p=>p.readyForInput),'actual channel startup');steps.push({event:'actual-channel-startup',time:Date.now()});control=new WebSocket(`ws://127.0.0.1:${ctrlPort}/ws`);control.on('error',error=>http.push({fixtureError:String(error)}));control.on('message',b=>frames.push(JSON.parse(b)));await wait(()=>control.readyState===1,'control open');send(control,{type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'production-channel-fixture',token:'fixture-gateway-token'});await wait(()=>frames.some(x=>x.type==='hello_ok'),'control auth');await incoming(0);
async function reload(on,id){await writeFile(cfg,config(on));send(control,{type:'request',id,method:'reload_config',params:{}});await wait(()=>frames.some(x=>x.type==='response'&&x.id===id),'reload '+id);assert.equal(frames.find(x=>x.type==='response'&&x.id===id).ok,true);steps.push({event:'public-reload-response',enabled:on,time:Date.now()});}
await reload(false,'disable');await wait(()=>peers.size===0,'disabled sockets');steps.push({event:'disabled-sockets-closed',time:Date.now()});await delay(150);generation=1;await reload(true,'enable');await incoming(1);control.close();child.kill('SIGTERM');exit=await Promise.race([stopped,delay(12000).then(()=>({code:null,signal:'timeout'}))]);assert.equal(exit.code,0);await wait(()=>peers.size===0,'shutdown sockets');steps.push({event:'actual-process-stop',exit,time:Date.now()});steps.push({event:'complete',status:'CAPTURED'});
}catch(e){failure=String(e.stack??e);process.exitCode=1;}finally{control?.terminate();if(child&&child.exitCode===null)child.kill('SIGTERM');for(const p of peers)p.terminate();for(const s of sockets)s.destroy();wss.close();await new Promise(r=>service.close(r));const sha=async p=>createHash('sha256').update(await readFile(p)).digest('hex');await writeFile(join(out,'evidence.json'),JSON.stringify({schemaVersion:2,profile:'native-eight-v2',channel:kind,engine,layout:layout?{...layout,path:resolve(layoutArg),sha256:await sha(layoutArg)}:null,fixedTS,actualExecutable:{path:engine==='ts'?process.execPath:gateway,sha256:await sha(engine==='ts'?process.execPath:gateway)},tsPreload:engine==='ts'?{path:fileURLToPath(new URL('./media-ts-loopback.mjs',import.meta.url)),sha256:await sha(new URL('./media-ts-loopback.mjs',import.meta.url))}:null,tsEntrypoint:engine==='ts'?{path:join(resolve(baseline),'src/cli/pilotdeck.ts'),sha256:await sha(join(resolve(baseline),'src/cli/pilotdeck.ts'))}:null,tsLoader:engine==='ts'?{path:join(resolve(baseline),'node_modules/tsx/dist/loader.mjs'),sha256:await sha(join(resolve(baseline),'node_modules/tsx/dist/loader.mjs'))}:null,runtime:{node:process.version,osHome,pilotHome:home,home,project,defaultProject:home,cwd:project,tmp,mockPort:Number(new URL(mock).port),controlPort:ctrlPort},status:failure?(engine==='ts'&&/not installed;|Cannot find (?:package|module)|ERR_MODULE_NOT_FOUND|unsupported .* fixture TLS transport/.test(stderr)?'BLOCKED':'FAIL'):'CAPTURED',sourceManifestSha256:process.env.PDRS_SOURCE_MANIFEST_SHA256,gatewaySha256:engine==='native'?await sha(gateway):await sha(gateway).catch(()=>null),sidecarSha256:engine==='native'?await sha(sidecar):await sha(sidecar).catch(()=>null),fixtureSha256:await sha(new URL(import.meta.url)),contextHelperSha256:await sha(new URL('./reference-round-context.mjs',import.meta.url)),steps,permissionGate,answerSent,deliveries,models,modelRoundInputs,persistedSessions,persistedMessages,roundContexts,http,wire,inbound,controlFrames:frames,stdout,stderr,exit,failure,formalAcceptance:false,scope:'production Server permission and elicitation continuation, two text rounds, public reload and shutdown; media remains separate'},null,2)+'\n',{flag:'wx'});}
