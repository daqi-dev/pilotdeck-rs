// Real fixed-baseline streamModel / undici over owned loopback sockets.
import assert from 'node:assert/strict';
import {createServer} from 'node:http';
import {createServer as createHttpsServer} from 'node:https';
import {connect} from 'node:net';
import {once} from 'node:events';
import {mkdirSync, readFileSync, writeFileSync} from 'node:fs';
import {resolve, join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindBaseline, installFixedClock} from '../_shared/generator_guard.mjs';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
assert.equal(process.version,'v22.22.0','#360 #860 calibrated Node');
const base=resolve(process.env.PDRS_BASELINE!), out=resolve(process.argv[2]);
const {sourceCommit,gitExecutable}=bindBaseline({baseline:base,commit:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'});
const entry='src/model/streaming/streamModel.ts';
const sourceSha256=sourceClosureSha256(base,sourceCommit,gitExecutable,[entry,'src/cli/proxy.ts']);
for(const key of ['PILOTDECK_PROXY','http_proxy','HTTP_PROXY','https_proxy','HTTPS_PROXY','no_proxy','NO_PROXY','ALL_PROXY','all_proxy','LITELLM_MAX_STREAMING_DURATION_SECONDS','PILOTDECK_DUMP_REQUEST'])delete process.env[key];
const {streamModel,resolveStreamIdleTimeout}=await import(pathToFileURL(join(base,entry)).href);
const undici=await import(pathToFileURL(join(base,'node_modules/undici/index.js')).href);
const {default:fastTimers}=await import(pathToFileURL(join(base,'node_modules/undici/lib/util/timers.js')).href);
const fixed=installFixedClock();const epoch=fixed.now();Math.random=()=>0.5;
const sse=(e:any)=>({text:`data: ${JSON.stringify(e)}\n\n`});
const delta=(text:string)=>sse({type:'content_block_delta',index:0,delta:{type:'text_delta',text}});
const done={text:sse({type:'message_delta',delta:{stop_reason:'end_turn'},usage:{output_tokens:2}}).text+sse({type:'message_stop'}).text};
const request={provider:'p',model:'m',messages:[{role:'user',content:[{type:'text',text:'hello'}]}]};
const provider={id:'p',protocol:'anthropic',url:'$MOCK/v1',apiKey:'fixture-key',retry:{streamMaxRetries:0},models:{m:{id:'m',capabilities:{supportsStreaming:true,supportsSystemPrompt:true,supportsToolUse:true,maxOutputTokens:32},multimodal:{input:['text','image']}}}};
const specs:any[]=[];
for(const n of [2,3,4])specs.push({name:`repeat-${n}`,provider:{retry:{repeatedChunkLimit:3}},reply:{chunks:[...Array.from({length:n},()=>delta('abc')),done]}});
specs.push({name:'repeat-short',provider:{retry:{repeatedChunkLimit:3}},reply:{chunks:[...Array.from({length:4},()=>delta('😀')),done]}});
specs.push({name:'repeat-reset',provider:{retry:{repeatedChunkLimit:3}},reply:{chunks:[delta('abc'),delta('abc'),sse({type:'content_block_delta',index:0,delta:{type:'thinking_delta',thinking:'reset'}}),delta('abc'),delta('abc'),done]}});
for(const ms of [9,10,11])specs.push({name:`duration-${ms}`,clockAfterFirst:ms,provider:{retry:{maxStreamingDurationMs:10}},reply:{chunks:[delta('first'),delta('second'),done]}});
specs.push({name:'zero-idle-fallback',streamTimeoutMs:0,provider:{retry:{streamIdleTimeoutMs:80}},reply:{chunks:[delta('first')],end:'hold'}});
specs.push({name:'zero-idle-default-cancel',streamTimeoutMs:0,abortAfterFirst:true,reply:{chunks:[delta('first')],end:'hold'}});
// Stable timer ordering away from the unmodelled near-tie window. These are
// timeout assertions, never wall-clock speed assertions.
specs.push({name:'idle-body-tie',timerReset:true,bodyTimeoutMs:1500,streamTimeoutMs:1500,reply:{chunks:[delta('first')],end:'hold'}});
specs.push({name:'body-before-idle',timerReset:true,bodyTimeoutMs:1500,streamTimeoutMs:2600,reply:{chunks:[delta('first')],end:'hold'}});
const messages=[
 'range of max_tokens should be [1, 32]','max_tokens available_tokens: 12','max_tokens available tokens: 12',
 'max_tokens: 99 > 32','max_tokens: 99 > context_window: 32','max_tokens: 99 exceeds context window 32',
 'max_output_tokens must be at most 32','max_completion_tokens <= 32',
 'maximum context length is 100; requested 50 output tokens; prompt contains 80 tokens',
 'maximum context length is 100; requested 50 output tokens; input_tokens: 80',
 'maximum context length is 100; requested 50 output tokens','maximum context length is 100; requested 50 output tokens; input_tokens: 120',
 'maximum context length is 100; 80 tokens in the output','maximum context length is 100; 120 tokens of output',
 'limit: 100 tokens','max_model_len: 100','maximum context length is 100','context_window: 100','context_length_exceeded: 100','最多支持100个token',
 'max_tokens at most 32; limit 100 tokens','range of max_tokens should be [1, 0]; limit 100 tokens',
 'max_tokens available_tokens: 0; limit 100 tokens','max_tokens available tokens: 0; limit 100 tokens',
 'max_tokens at most 0; limit 100 tokens','maximum context length is 0; requested 50 output tokens; input_tokens: 80',
 'maximum context length is 100; 0 tokens in output','limit 0 tokens; context_window: 100','provider rejected input',
];
for(const [i,message] of messages.entries())specs.push({name:`token-${i}`,reply:{status:400,headers:{'content-type':'application/json'},chunks:[{text:JSON.stringify({error:{type:'invalid_request_error',message}})}]}});
for(const protocol of ['anthropic','openai','openai-responses','google'])for(const variant of ['omitted','full','none','required','empty']){
 const req:any=structuredClone(request);
 if(variant!=='omitted')Object.assign(req,{systemPrompt:'system',tools:[{name:'lookup',description:'find',inputSchema:{type:'object',properties:{q:{type:'string'}}}}],metadata:{user_id:'u',trace:'t'},maxOutputTokens:17,toolChoice:variant==='full'?'auto':variant==='empty'?'auto':variant});
 if(variant==='empty')Object.assign(req,{systemPrompt:'',tools:[],metadata:{},maxOutputTokens:0});
 specs.push({name:`body-${protocol}-${variant}`,request:req,provider:{protocol,url:protocol==='google'?'$MOCK':'$MOCK/v1'},reply:{status:400,headers:{'content-type':'application/json'},chunks:[{text:JSON.stringify({error:{code:400,type:'invalid_request_error',message:'fixture rejection',status:'INVALID_ARGUMENT'}})}]}});
}
let current:any, seen:any[]=[];
const sockets=new Set<any>();
const server=createServer((req,res)=>{
 const chunks:Buffer[]=[];req.on('data',c=>chunks.push(c));req.on('end',()=>{
  seen.push({method:req.method,path:req.url,body:Buffer.concat(chunks).toString()});
  const reply=current.reply;res.writeHead(reply.status??200,reply.headers??{'content-type':'text/event-stream'});
  // One response write; the consumer's canonical-event boundary drives the
  // logical clock, so TCP segmentation and scheduler speed cannot set it.
  res.write((reply.chunks??[]).map((c:any)=>c.text).join(''));
  if(reply.end!=='hold')res.end();
 });
});
server.on('connection',s=>{sockets.add(s);s.on('close',()=>sockets.delete(s));});
server.listen(0,'127.0.0.1');await once(server,'listening');
const origin=`http://127.0.0.1:${(server.address() as any).port}`;
const project=(event:any)=>event.type==='error'?{type:event.type,error:Object.fromEntries(['code','message','retryable','maxOutputTokens','availableOutputTokens','maxContextTokens'].filter(k=>event.error[k]!==undefined).map(k=>[k,event.error[k]]))}:Object.fromEntries(['type','text'].filter(k=>event[k]!==undefined).map(k=>[k,event[k]]));
const cases:any[]=[];
const watchdog=setTimeout(()=>{throw Error('#360 fixture hard deadline');},45000);
try{
 for(const spec of specs){
  current=spec;seen=[];fixed.set(epoch);
  // Test-only isolation: every previous dispatcher is destroyed before this
  // reset. Wall-clock phase distributions are measured by probe.mts children.
  if(spec.timerReset)fastTimers.reset();
  const p={...provider,...spec.provider,retry:{...provider.retry,...spec.provider?.retry}};p.url=p.url.replace('$MOCK',origin);
  const req=spec.request??request,controller=new AbortController(),events:any[]=[];
  const previous=undici.getGlobalDispatcher();const agent=new undici.Agent({bodyTimeout:spec.bodyTimeoutMs??600000});undici.setGlobalDispatcher(agent);
  let thrown:any;
  try{for await(const event of streamModel(req,{providers:{p}},{signal:controller.signal,streamTimeoutMs:spec.streamTimeoutMs})){
   events.push(project(event));
   if(event.type==='text_delta'){
    if(spec.clockAfterFirst!==undefined)fixed.set(epoch+spec.clockAfterFirst);
    if(spec.abortAfterFirst)controller.abort();
   }
  }}catch(e:any){thrown={name:e.name,message:e.message};}
  finally{undici.setGlobalDispatcher(previous);await agent.destroy();}
  assert.equal(seen.length,1,`#360 ${spec.name} request count`);
  cases.push({...spec,provider:{...p,url:p.url.replace(origin,'$MOCK')},request:req,events,thrown,requests:structuredClone(seen),resolvedIdle:resolveStreamIdleTimeout(p,{streamTimeoutMs:spec.streamTimeoutMs})});
 }
 // Real EnvHttpProxyAgent routing; the proxy tunnels to the same owned server.
 // localhost. avoids PilotDeck's unconditional localhost bypass entry.
 current={reply:{chunks:[{text:'ok'}]}};
 const proxy=createServer();let tunnels=0;
 proxy.on('connect',(_req,client,head)=>{tunnels++;const upstream=connect((server.address() as any).port,'127.0.0.1',()=>{client.write('HTTP/1.1 200 Connection Established\r\n\r\n');if(head.length)upstream.write(head);client.pipe(upstream);upstream.pipe(client);});client.on('error',()=>upstream.destroy());upstream.on('error',()=>client.destroy());client.on('close',()=>upstream.destroy());});
 proxy.on('connection',s=>{sockets.add(s);s.on('close',()=>sockets.delete(s));});proxy.listen(0,'127.0.0.1');await once(proxy,'listening');
 const noProxy:any[]=[];
 try{for(const cp of [9,10,11,12,13,32,160,0x1680,...Array.from({length:11},(_,i)=>0x2000+i),0x2028,0x2029,0x202f,0x205f,0x3000,0xfeff,0x85,0x180e,0x200b,44]){
  const value=`unused${String.fromCodePoint(cp)}localhost.`;tunnels=0;
  const agent=new undici.EnvHttpProxyAgent({httpProxy:`http://127.0.0.1:${(proxy.address() as any).port}`,noProxy:value});
  try{const response=await undici.fetch(origin.replace('127.0.0.1','localhost.'),{dispatcher:agent});assert.equal(await response.text(),'ok','#860 TCP reply');noProxy.push({cp,value,proxy:tunnels===1});}
  finally{await agent.destroy();}
 }}finally{proxy.closeAllConnections();proxy.close();}
 const material=JSON.parse(readFileSync(new URL('./tls.json',import.meta.url),'utf8')),tls:any[]=[];
 for(const row of material.cases){
  const peer=createHttpsServer({key:material.key,cert:row.cert},(_req,res)=>res.end('ok'));peer.listen(0,'127.0.0.1');await once(peer,'listening');
  const agent=new undici.Agent({connect:{ca:material.ca}});let result:any;
  try{const response=await undici.fetch(`https://127.0.0.1:${(peer.address() as any).port}`,{dispatcher:agent});result={text:await response.text()};}
  catch(e:any){result={message:e.message,cause:e.cause?.code};}
  finally{await agent.destroy();peer.closeAllConnections();await new Promise<void>(r=>peer.close(()=>r()));}
  tls.push({name:row.name,result});
 }
 const oracle={sourceCommit,sourceSha256,node:process.version,undici:JSON.parse(readFileSync(join(base,'node_modules/undici/package.json'),'utf8')).version,cases,noProxy,tls};
 mkdirSync(out,{recursive:true});writeFileSync(join(out,'oracle.json'),JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
 console.log(JSON.stringify(oracle,null,2));
}finally{clearTimeout(watchdog);for(const socket of sockets)socket.destroy();server.closeAllConnections();server.close();}
