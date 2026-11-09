// Non-frozen production consumer probe. Real executable/Server config/A7/provider.
// Reuses native-storage-stimulus process protocol; no mock TurnGateway.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
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
const rows=[]; let child,ws; const sockets=new Set(); let generation=0;
const deadline=Date.now()+90000;
const wait=async(predicate,label)=>{while(!predicate()){assert(Date.now()<deadline,label);await new Promise(r=>setTimeout(r,20));}};
const provider=createServer(async(req,res)=>{
 let data='';for await(const chunk of req)data+=chunk;
 const body=JSON.parse(data);rows.push({provider:body});
 if(!body.stream){res.setHeader('content-type','application/json');res.end(JSON.stringify({choices:[{message:{role:'assistant',content:'{"title":"Channel lifecycle"}'},finish_reason:'stop'}]}));return;}
 res.setHeader('content-type','text/event-stream');
 res.end('data: '+JSON.stringify({id:'channel-loopback',model:'mock',choices:[{index:0,delta:{role:'assistant',content:'channel-lifecycle-output-'+generation},finish_reason:null}]})+'\n\ndata: '+JSON.stringify({id:'channel-loopback',choices:[{index:0,delta:{},finish_reason:'stop'}]})+'\n\ndata: [DONE]\n\n');
});
let sent=new Set();let activeReceivers=0;
const channel=createServer(async(req,res)=>{
 let data='';for await(const chunk of req)data+=chunk;
 const url=new URL(req.url,'http://loopback');rows.push({channel:{method:req.method,path:url.pathname,body:data}});
 if(kind==='bluebubbles'&&url.pathname==='/api/v1/message'){res.setHeader('content-type','application/json');return res.end(JSON.stringify({data:[{guid:'event-'+generation,dateCreated:Date.now(),isFromMe:false,chatGuid:'room',text:'channel-lifecycle-input-'+generation}]}));}
 if(kind==='whatsapp'&&url.pathname==='/messages'){res.setHeader('content-type','application/json');return res.end(JSON.stringify({messages:[{id:'event-'+generation,chatId:'room',text:'channel-lifecycle-input-'+generation}]}));}
 if(kind==='signal'&&url.pathname.startsWith('/v1/receive/')){activeReceivers++;res.once('close',()=>{activeReceivers--;rows.push({receiveClosed:kind});});res.setHeader('content-type','text/event-stream');res.write('data: '+JSON.stringify({envelope:{source:'+human',dataMessage:{message:'channel-lifecycle-input-'+generation}}})+'\r\n\n');return;}
 res.setHeader('content-type','application/json');
 if(url.pathname==='/users/me')return res.end('{"id":"bot"}');
 if(url.pathname.endsWith('/account/whoami'))return res.end('{"user_id":"@bot:test"}');
 if(url.pathname.endsWith('/sync')){
  if(!sent.has(generation)){sent.add(generation);return res.end(JSON.stringify({next_batch:'batch-'+generation,rooms:{join:{'!room:test':{timeline:{events:[{type:'m.room.message',sender:'@user:test',event_id:'event-'+generation,content:{msgtype:'m.text',body:'channel-lifecycle-input-'+generation}}]}}}}}));}
  const timer=setTimeout(()=>res.end(JSON.stringify({next_batch:'idle-'+generation,rooms:{}})),100);
  res.on('close',()=>clearTimeout(timer));return;
 }
 if(url.pathname.includes('/send/m.room.message/'))return res.end('{"event_id":"outbound"}');
 res.end('{}');
});
for(const server of [provider,channel]){server.on('connection',s=>{sockets.add(s);s.on('error',()=>{});s.on('close',()=>sockets.delete(s));});await new Promise(r=>server.listen(0,'127.0.0.1',r));}
const websocketServer=new (require('ws').WebSocketServer)({noServer:true});
channel.on('upgrade',(req,socket,head)=>websocketServer.handleUpgrade(req,socket,head,client=>{
 activeReceivers++;client.once('close',()=>{activeReceivers--;rows.push({receiveClosed:kind});});
 if(kind==='homeassistant'){
  client.send(JSON.stringify({type:'auth_required'}));
  client.on('message',raw=>{const message=JSON.parse(raw);rows.push({haFrame:message});
   if(message.type==='auth')client.send(JSON.stringify({type:'auth_ok'}));
   else if(message.type==='subscribe_events'){client.send(JSON.stringify({id:message.id,type:'result',success:true}));client.send(JSON.stringify({type:'event',event:{event_type:'state_changed',data:{entity_id:'conversation.lifecycle',old_state:{state:'old'},new_state:{state:'channel-lifecycle-input-'+generation}}}}));}
   else if(message.type==='call_service')client.send(JSON.stringify({id:message.id,type:'result',success:true}));
  });return;
 }
 client.send(JSON.stringify({event:'posted',data:{post:JSON.stringify({id:'event-'+generation,user_id:'human',channel_id:'room',message:'channel-lifecycle-input-'+generation})}}));
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
const email=kind==='email'?await (await import('./email-mock.mjs')).startEmail(()=>generation,rows,sockets):null;
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
 ws.close();child.kill('SIGTERM');const stopped=await Promise.race([exit,new Promise((_,reject)=>setTimeout(()=>reject(Error('shutdown timeout')),10000))]);
 assert.equal(stopped.code,0,'graceful native shutdown');
 const after=rows.length;await new Promise(r=>setTimeout(r,kind==='bluebubbles'?3000:200));assert.equal(rows.length,after,'no transport work after shutdown');
 rows.push({result:'PASS',scope:kind+' real Server/A7 outbound/reload/shutdown',formalAcceptance:false,environment:{node:process.version,icu:process.versions.icu}});
}finally{
 ws?.terminate();if(child&&child.exitCode===null)child.kill('SIGTERM');
 for(const client of websocketServer.clients)client.terminate();websocketServer.close();for(const s of sockets)s.destroy();for(const s of [provider,channel,email?.imap,email?.smtp].filter(Boolean))s.close();
 const digest=async p=>createHash('sha256').update(await readFile(p)).digest('hex');
 await writeFile(join(out,'evidence.json'),JSON.stringify({probe:{path:fileURLToPath(import.meta.url),sha256:probeSha,channel:kind},binary:{path:binary,sha256:await digest(binary)},sidecar:{path:sidecar,sha256:await digest(sidecar)},rows,stderr},null,2)+'\n',{flag:'wx'});
}
