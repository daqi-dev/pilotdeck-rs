// Owned interaction scenario derived from process.mjs transport fixture; original file remains unchanged.
// Non-frozen production consumer probe. Real executable/Server config/A7/provider.
// Reuses native-storage-stimulus process protocol; no mock TurnGateway.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {existsSync} from 'node:fs';
import {createServer} from 'node:http';
import {mkdir,writeFile,readFile,readdir} from 'node:fs/promises';
import {resolve,join} from 'node:path';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {createRequire} from 'node:module';
const require=createRequire(join(process.env.PDRS_BASELINE,'package.json'));
const WebSocket=require('ws');
const [binary,sidecar,out]=process.argv.slice(2).map(p=>resolve(p));
await mkdir(out,{recursive:true});
const kind=process.env.PDRS_CHANNEL??'matrix';assert(['matrix','signal','mattermost','homeassistant','whatsapp','email','bluebubbles'].includes(kind));
const probeSha=createHash('sha256').update(await readFile(fileURLToPath(import.meta.url))).digest('hex');
let releaseHeld;const held=new Promise(r=>releaseHeld=r);let heldEntered=false,heldAttempted=false,heldFinished=false,heldClosed=false;
const rows=[]; let child,ws; const sockets=new Set(); let generation=0;
const waMessages=new Map();
const stages=new Map(),pendingInputs=new Map(),initialInputs=new Set(),answerKeys=new Set(),answerTimers=new Set(),signalPeers=new Map(),wsPeers=new Map(),emailAnswers=new Map();let inputSeq=0;
const rawPush=rows.push.bind(rows);rows.push=(...items)=>{const n=rawPush(...items);for(const item of items)if(item.mailSent)observeOutgoing(JSON.stringify(item.mailSent));return n;};
function takeInput(){if(!initialInputs.has(generation)){initialInputs.add(generation);return 'channel-lifecycle-input-'+generation;}return pendingInputs.get(generation)?.shift();}
function signalEvent(res,text){res.write('data: '+JSON.stringify({envelope:{source:'+human',dataMessage:{message:text}}})+'\r\n\n');}
function wsEvent(client,text){const event=kind==='homeassistant'?{type:'event',event:{event_type:'state_changed',data:{entity_id:'conversation.lifecycle',old_state:{state:'old'},new_state:{state:text}}}}:{event:'posted',data:{post:JSON.stringify({id:'event-'+generation+'-'+(++inputSeq),user_id:'human',channel_id:'room',message:text})}};client.send(JSON.stringify(event));}
function queueAnswer(type,g){
 rows.push({answerSent:{type,generation:g,text:'1',delayMs:50}});
 if(g!==generation){rows.push({lateAnswer:true});return;}
 if(kind==='email'){emailAnswers.set(g,(emailAnswers.get(g)??0)+1);return;}
 if(kind==='signal'){const peers=[...signalPeers].filter(([,n])=>n===g);if(peers.length){for(const[res]of peers)signalEvent(res,'1');return;}}
 if(kind==='mattermost'||kind==='homeassistant'){const peers=[...wsPeers].filter(([,n])=>n===g);if(peers.length){for(const[client]of peers)wsEvent(client,'1');return;}}
 const queue=pendingInputs.get(g)??[];queue.push('1');pendingInputs.set(g,queue);
}
function observeOutgoing(text){
 const type=text.includes('回复 1 允许一次')?'permission':text.includes('Choose mock option')?'elicitation':null;if(!type)return;
 const g=generation,key=g+':'+type;if(answerKeys.has(key))return;answerKeys.add(key);
 if(type==='permission')rows.push({permissionGate:{generation:g,fileExistsBeforeAnswer:existsSync(join(project,'permission-'+g+'.txt'))}});
 const timer=setTimeout(()=>{answerTimers.delete(timer);queueAnswer(type,g);},50);answerTimers.add(timer);
}

const deadline=Date.now()+90000;
const wait=async(predicate,label)=>{while(!predicate()){assert(Date.now()<deadline,label);await new Promise(r=>setTimeout(r,20));}};
const provider=createServer(async(req,res)=>{
 let data='';for await(const chunk of req)data+=chunk;
 const body=JSON.parse(data);rows.push({provider:body});
 if(!body.stream){res.setHeader('content-type','application/json');res.end(JSON.stringify({choices:[{message:{role:'assistant',content:'{"title":"Channel lifecycle"}'},finish_reason:'stop'}]}));return;}
 const phase=stages.get(generation)??0;stages.set(generation,phase+1);rows.push({providerPhase:{generation,phase,toolResults:(body.messages??[]).filter(m=>m.role==='tool').length,hasInput:JSON.stringify(body).includes('channel-lifecycle-input-'+generation),hasOne:JSON.stringify(body).includes('One')}});
 if(generation===2&&phase===0){heldEntered=true;res.once('finish',()=>{heldFinished=true});res.once('close',()=>{heldClosed=true});await held;res.setHeader('content-type','text/event-stream');res.end('data: '+JSON.stringify({id:'held-old',choices:[{index:0,delta:{role:'assistant',content:'CHANNEL_OLD_LATE_FINAL_42'},finish_reason:null}]})+'\n\ndata: '+JSON.stringify({id:'held-old',choices:[{index:0,delta:{},finish_reason:'stop'}]})+'\n\ndata: [DONE]\n\n');heldAttempted=true;rows.push({heldOldResponseAttempted:true});return;}
 if(phase>2){rows.push({unexpectedModelPhase:phase});res.writeHead(500);res.end('unexpected extra business model call');return;}
 const delta=phase===0?{role:'assistant',tool_calls:[{index:0,id:'permission-'+generation,type:'function',function:{name:'bash',arguments:JSON.stringify({command:"printf approved > '"+join(project,'permission-'+generation+'.txt')+"'"})}}]}:phase===1?{role:'assistant',tool_calls:[{index:0,id:'question-'+generation,type:'function',function:{name:'ask_user_question',arguments:JSON.stringify({questions:[{header:'Choice',question:'Choose mock option',options:[{label:'One',description:'First mock'},{label:'Two',description:'Second mock'}],multiSelect:false}]})}}]}:{role:'assistant',content:'channel-lifecycle-output-'+generation};
 res.setHeader('content-type','text/event-stream');
 res.end('data: '+JSON.stringify({id:'channel-loopback',model:'mock',choices:[{index:0,delta,finish_reason:null}]})+'\n\ndata: '+JSON.stringify({id:'channel-loopback',choices:[{index:0,delta:{},finish_reason:phase<2?'tool_calls':'stop'}]})+'\n\ndata: [DONE]\n\n');
});
let sent=new Set();let activeReceivers=0;
const channel=createServer(async(req,res)=>{
 let data='';for await(const chunk of req)data+=chunk;
 const url=new URL(req.url,'http://loopback');rows.push({channel:{method:req.method,path:url.pathname,body:data}});
 if(['/v2/send','/posts','/send','/api/v1/message/text'].includes(url.pathname)||url.pathname.includes('/send/m.room.message/'))observeOutgoing(data);
 if(kind==='bluebubbles'&&url.pathname==='/api/v1/message'){const text=takeInput();res.setHeader('content-type','application/json');return res.end(JSON.stringify({data:text?[{guid:'event-'+generation+'-'+(++inputSeq),dateCreated:Date.now(),isFromMe:false,chatGuid:'room',text}]:[]}));}
 if(kind==='whatsapp'&&url.pathname==='/messages'){const text=takeInput(),messages=waMessages.get(generation)??[];if(text)messages.push({id:'event-'+generation+'-'+(++inputSeq),chatId:'room',text});waMessages.set(generation,messages);res.setHeader('content-type','application/json');return res.end(JSON.stringify({messages}));}
 if(kind==='signal'&&url.pathname.startsWith('/v1/receive/')){activeReceivers++;signalPeers.set(res,generation);res.once('close',()=>{signalPeers.delete(res);activeReceivers--;rows.push({receiveClosed:kind});});res.setHeader('content-type','text/event-stream');const text=takeInput();if(text)signalEvent(res,text);return;}
 res.setHeader('content-type','application/json');
 if(url.pathname==='/users/me')return res.end('{"id":"bot"}');
 if(url.pathname.endsWith('/account/whoami'))return res.end('{"user_id":"@bot:test"}');
 if(url.pathname.endsWith('/sync')){
  const input=takeInput();if(input){return res.end(JSON.stringify({next_batch:'batch-'+generation+'-'+(++inputSeq),rooms:{join:{'!room:test':{timeline:{events:[{type:'m.room.message',sender:'@user:test',event_id:'event-'+generation+'-'+inputSeq,content:{msgtype:'m.text',body:input}}]}}}}}));}
  const timer=setTimeout(()=>res.end(JSON.stringify({next_batch:'idle-'+generation,rooms:{}})),100);
  res.on('close',()=>clearTimeout(timer));return;
 }
 if(url.pathname.includes('/send/m.room.message/'))return res.end('{"event_id":"outbound"}');
 res.end('{}');
});
for(const server of [provider,channel]){server.on('connection',s=>{sockets.add(s);s.on('error',()=>{});s.on('close',()=>sockets.delete(s));});await new Promise(r=>server.listen(0,'127.0.0.1',r));}
const websocketServer=new (require('ws').WebSocketServer)({noServer:true});
channel.on('upgrade',(req,socket,head)=>websocketServer.handleUpgrade(req,socket,head,client=>{
 activeReceivers++;wsPeers.set(client,generation);client.once('close',()=>{wsPeers.delete(client);activeReceivers--;rows.push({receiveClosed:kind});});
 if(kind==='homeassistant'){
  client.send(JSON.stringify({type:'auth_required'}));
  client.on('message',raw=>{const message=JSON.parse(raw);rows.push({haFrame:message});if(message.type==='call_service')observeOutgoing(JSON.stringify(message));
   if(message.type==='auth')client.send(JSON.stringify({type:'auth_ok'}));
   else if(message.type==='subscribe_events'){client.send(JSON.stringify({id:message.id,type:'result',success:true}));const text=takeInput();if(text)wsEvent(client,text);}
   else if(message.type==='call_service')client.send(JSON.stringify({id:message.id,type:'result',success:true}));
  });return;
 }
 const text=takeInput();if(text)wsEvent(client,text);
 client.on('error',()=>{});
}));
const home=join(out,'home'),project=join(out,'project');await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});
const base='schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:'+provider.address().port+'/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n';
const matrixConfig=enabled=>base+'adapters:\n  matrix:\n    enabled: '+enabled+'\n    token: mock-token\n    extra:\n      userId: "@bot:test"\n      homeserver: http://127.0.0.1:'+channel.address().port+'\n';
let bridgePort;
if(kind==='whatsapp'){
 const {createServer}=await import('node:net');const reservation=createServer();
 await new Promise(r=>reservation.listen(0,'127.0.0.1',r));bridgePort=reservation.address().port;
 await new Promise(r=>reservation.close(r));
 await writeFile(join(home,'lifecycle-bridge-target'),'http://127.0.0.1:'+channel.address().port,{flag:'wx'});
}
const email=kind==='email'?await (await import('./interaction-email-mock.mjs')).startEmail(()=>generation*100+(emailAnswers.get(generation)??0),rows,sockets,()=>emailAnswers.get(generation)?'1':'channel-lifecycle-input-'+generation):null;
const config=enabled=>kind==='matrix'?matrixConfig(enabled):kind==='bluebubbles'?base+'adapters:\n  bluebubbles:\n    enabled: '+enabled+'\n    extra:\n      serverUrl: http://127.0.0.1:'+channel.address().port+'\n      password: mock-only\n':kind==='email'?base+'adapters:\n  email:\n    enabled: '+enabled+'\n    extra:\n'+email.config:kind==='whatsapp'?base+'adapters:\n  whatsapp:\n    enabled: '+enabled+'\n    extra:\n      bridgePath: '+join(fileURLToPath(new URL('.',import.meta.url)),'bridge.mjs')+'\n      bridgeUrl: http://127.0.0.1:'+bridgePort+'\n':kind==='homeassistant'?base+'adapters:\n  homeassistant:\n    enabled: '+enabled+'\n    token: mock-token\n    extra:\n      url: http://127.0.0.1:'+channel.address().port+'\n':base+'adapters:\n  '+kind+':\n    enabled: '+enabled+'\n    token: mock-token\n    extra:\n'+(kind==='signal'?'      restUrl: http://127.0.0.1:'+channel.address().port+'\n      account: "+123"\n':'      serverUrl: http://127.0.0.1:'+channel.address().port+'\n');
const path=join(home,'pilotdeck.yaml');await writeFile(path,config(true),{flag:'wx'});const token=join(home,'token');await writeFile(token,'channel-lifecycle-token\n',{flag:'wx'});
let stderr='';let exit; const outbound=()=>kind==='email'?rows.filter(r=>r.mailSent).map(r=>({channel:{body:JSON.stringify(r.mailSent)}})):kind==='homeassistant'?rows.filter(r=>r.haFrame?.type==='call_service').map(r=>({channel:{body:JSON.stringify(r.haFrame)}})):rows.filter(r=>(kind==='bluebubbles'?r.channel?.path==='/api/v1/message/text':kind==='matrix'?r.channel?.path.includes('/send/m.room.message/'):kind==='signal'?r.channel?.path==='/v2/send':kind==='whatsapp'?r.channel?.path==='/send':r.channel?.path==='/posts'));
try{
 child=spawn(binary,['--native','--home',home,'--project',project,'--listen','127.0.0.1:0','--token-file',token,'--sidecar',sidecar,'--drain-ms','1500'],{cwd:project,env:{PATH:process.env.PATH,HOME:home,PILOT_HOME:home,LANG:'en_US.UTF-8'},stdio:['ignore','pipe','pipe']});
 exit=new Promise(r=>child.once('exit',(code,signal)=>r({code,signal})));child.stderr.on('data',b=>stderr+=b);child.on('error',e=>stderr+=String(e));
 await wait(()=>/(?:^|\n)native listening (\S+:\d+)\r?\n/.test(stderr),'native readiness');
 const address=/(?:^|\n)native listening (\S+:\d+)\r?\n/.exec(stderr)[1];
 ws=new WebSocket('ws://'+address+'/ws');const frames=[];ws.on('message',b=>frames.push(JSON.parse(b)));
 await wait(()=>ws.readyState===1,'websocket open');
 ws.send(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'channel-lifecycle',token:'channel-lifecycle-token'}));
 await wait(()=>frames.some(f=>f.type==='hello_ok'),'authentication');
 await wait(()=>outbound().some(r=>r.channel.body.includes('channel-lifecycle-output-0')),'inbound -> actual A7/provider -> outbound');
 assert(rows.some(r=>r.provider?.stream&&JSON.stringify(r.provider).includes('channel-lifecycle-input-0')),'provider saw channel input');
 async function reload(id,enabled){await writeFile(path,config(enabled));ws.send(JSON.stringify({type:'request',id,method:'reload_config',params:{}}));await wait(()=>frames.some(f=>f.type==='response'&&f.id===id),'reload response');assert.equal(frames.find(f=>f.type==='response'&&f.id===id).ok,true);}
 await reload('disable',false);const count=outbound().length;
 const receives=()=>kind==='email'?rows.filter(r=>r.mailWire?.protocol==='imap'&&r.mailWire.line.includes('SEARCH')).length:rows.filter(r=>r.channel&&(kind==='matrix'?r.channel.path.endsWith('/sync'):kind==='signal'?r.channel.path.startsWith('/v1/receive/'):kind==='whatsapp'?r.channel.path==='/messages':kind==='bluebubbles'?r.channel.path==='/api/v1/message':false)).length;
 // Fixed TS reload queues adapter reconciliation; RPC completion is not a barrier.
 let previous=receives();let stable=Date.now();const stopDeadline=Date.now()+10000;
 while(Date.now()-stable<(kind==='bluebubbles'?3000:500)||(!['matrix','whatsapp','email','bluebubbles'].includes(kind)&&activeReceivers!==0)){
  assert(Date.now()<stopDeadline,'queued disable must stop inbound transport within 10 seconds');
  await new Promise(r=>setTimeout(r,50));const current=receives();if(current!==previous){stable=Date.now();previous=current;}
 }
 assert.equal(outbound().length,count,'disabled channel produces no response');
 generation=1;await reload('enable',true);
 if(kind==='matrix')await wait(()=>rows.filter(r=>r.channel?.path.endsWith('/account/whoami')).length===2,'reload constructs a fresh Matrix adapter');
 await wait(()=>outbound().some(r=>r.channel.body.includes('channel-lifecycle-output-1')),'reloaded channel enters actual A7 and replies');
 await reload('disable-held-setup',false);
 previous=receives();stable=Date.now();const setupDeadline=Date.now()+10000;
 while(Date.now()-stable<(kind==='bluebubbles'?3000:500)||(!['matrix','whatsapp','email','bluebubbles'].includes(kind)&&activeReceivers!==0)){
  assert(Date.now()<setupDeadline,'second disable stops transport');await new Promise(r=>setTimeout(r,50));const current=receives();if(current!==previous){stable=Date.now();previous=current;}
 }
 generation=2;await reload('enable-held',true);await wait(()=>heldEntered,'old generation reaches held real provider');
 async function transcriptEntries(dir){let all=[];for(const entry of await readdir(dir,{withFileTypes:true})){const file=join(dir,entry.name);if(entry.isDirectory())all.push(...await transcriptEntries(file));else if(entry.name.endsWith('.jsonl'))for(const line of (await readFile(file,'utf8')).split('\n')){try{all.push(JSON.parse(line));}catch{}}}return all;}
 let oldInput;const locateEnd=Date.now()+10000;
 while(!oldInput){assert(Date.now()<locateEnd,'old accepted input persisted');const entries=await transcriptEntries(join(home,'projects'));oldInput=entries.find(e=>e.type==='accepted_input'&&JSON.stringify(e).includes('channel-lifecycle-input-2'));if(!oldInput)await new Promise(r=>setTimeout(r,25));}
 const oldSession=oldInput.sessionId,oldTurn=oldInput.turnId;assert(oldSession&&oldTurn,'old input has session and turn identity');
 let snapshotSeq=0;async function activeSnapshot(){const id='old-turn-state-'+(++snapshotSeq);ws.send(JSON.stringify({type:'request',id,method:'active_turn_snapshot',params:{sessionKey:oldSession,includeEvents:false}}));await wait(()=>frames.some(f=>f.type==='response'&&f.id===id),'active turn snapshot response');const f=frames.find(f=>f.type==='response'&&f.id===id);assert.equal(f.ok,true,JSON.stringify(f));rows.push({oldTurnSnapshot:f.result});return f.result.active;}
 assert.equal(await activeSnapshot(),true,'held old turn is active before disable');
 await reload('disable-held',false);
 previous=receives();stable=Date.now();const heldDeadline=Date.now()+10000;
 while(Date.now()-stable<(kind==='bluebubbles'?3000:500)||(!['matrix','whatsapp','email','bluebubbles'].includes(kind)&&activeReceivers!==0)){
  assert(Date.now()<heldDeadline,'held generation transport stops');await new Promise(r=>setTimeout(r,50));const current=receives();if(current!==previous){stable=Date.now();previous=current;}
 }
 releaseHeld();
 await wait(()=>heldAttempted&&(heldFinished||heldClosed),'old provider response write attempted and finished or socket closed');
 const settleEnd=Date.now()+15000;let terminal=false,active=true;
 while(!terminal||active){assert(Date.now()<settleEnd,'old turn terminal and inactive barrier');const entries=await transcriptEntries(join(home,'projects'));terminal=entries.some(e=>e.sessionId===oldSession&&e.turnId===oldTurn&&e.type==='turn_result');active=await activeSnapshot();if(!terminal||active)await new Promise(r=>setTimeout(r,25));}
 rows.push({oldTurnTerminalBarrier:{sessionKey:oldSession,turnId:oldTurn,terminal,active}});
 generation=3;await reload('enable-after-held',true);
 await wait(()=>outbound().some(r=>r.channel.body.includes('channel-lifecycle-output-3')),'fresh generation runs after old turn terminal');
 assert(!outbound().some(r=>r.channel.body.includes('CHANNEL_OLD_LATE_FINAL_42')),'old generation must not send a late final after reload');
 ws.close();child.kill('SIGTERM');const stopped=await Promise.race([exit,new Promise((_,reject)=>setTimeout(()=>reject(Error('shutdown timeout')),10000))]);
 assert.equal(stopped.code,0,'graceful native shutdown');
 const after=rows.length;await new Promise(r=>setTimeout(r,kind==='bluebubbles'?3000:200));assert.equal(rows.length,after,'no transport work after shutdown');
 for(const g of [0,1,3]){
  assert.equal(stages.get(g),3,'exactly one permission + one elicitation + final model continuation per generation');
  assert.equal(await readFile(join(project,'permission-'+g+'.txt'),'utf8'),'approved','real tool ran only after IM approval');
  const phases=rows.filter(r=>r.providerPhase?.generation===g).map(r=>r.providerPhase);
  assert(phases[1].toolResults>=1&&phases[2].toolResults>=2&&phases.every(p=>p.hasInput)&&phases[2].hasOne,'same A7 model history carries tool results and chosen option');
 }
 assert.equal(answerKeys.size,6,'both interactions consumed in all three normal generations');
 assert(rows.filter(r=>r.permissionGate).every(r=>r.permissionGate.fileExistsBeforeAnswer===false),'permission file did not exist before answer');
 assert(!rows.some(r=>r.lateAnswer||r.unexpectedModelPhase),'no late answer or unexpected model call');
 rows.push({result:'PASS',scope:kind+' real Server/A7 permission+elicitation same-run continuation/outbound/reload/shutdown',formalAcceptance:false,environment:{node:process.version,icu:process.versions.icu}});
}finally{
 for(const timer of answerTimers)clearTimeout(timer);
 ws?.terminate();if(child&&child.exitCode===null)child.kill('SIGTERM');
 for(const client of websocketServer.clients)client.terminate();websocketServer.close();for(const s of sockets)s.destroy();for(const s of [provider,channel,email?.imap,email?.smtp].filter(Boolean))s.close();
 const digest=async p=>createHash('sha256').update(await readFile(p)).digest('hex');
 await writeFile(join(out,'evidence.json'),JSON.stringify({probe:{path:fileURLToPath(import.meta.url),sha256:probeSha,channel:kind},binary:{path:binary,sha256:await digest(binary)},sidecar:{path:sidecar,sha256:await digest(sidecar)},rows,stderr},null,2)+'\n',{flag:'wx'});
}

