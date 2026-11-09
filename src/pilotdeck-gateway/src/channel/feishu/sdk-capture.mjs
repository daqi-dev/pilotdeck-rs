// Actual fixed Feishu adapter + installed Lark SDK protobuf/WS, loopback only.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {createServer} from 'node:http';
import {createHash} from 'node:crypto';
import {resolve,join,dirname,posix} from 'node:path';
const [rootArg,outArg]=process.argv.slice(2);const root=resolve(rootArg),out=resolve(outArg);
await mkdir(out,{recursive:true});const fixed='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const require=createRequire(join(root,'package.json'));const sdkPath=require.resolve('@larksuiteoapi/node-sdk');const sdkSource=await readFile(sdkPath,'utf8');
const sdkModule={exports:{}};new Function('require','module','exports','__filename','__dirname',sdkSource+'\nexports.__frames=pbbp2;')(createRequire(sdkPath),sdkModule,sdkModule.exports,sdkPath,dirname(sdkPath));
const sdk=sdkModule.exports;const Frame=sdk.__frames.Frame;const {WebSocketServer}=require('ws');const {transformSync}=createRequire(require.resolve('tsx'))('esbuild');
const rows=[],sockets=new Set(),instances=[];let streamClosed=false;
const http=createServer(async(req,res)=>{let body='';for await(const chunk of req)body+=chunk;rows.push({http:{path:req.url,body:body?JSON.parse(body):null}});res.setHeader('content-type','application/json');res.end(JSON.stringify({code:0,data:{URL:'ws://127.0.0.1:'+http.address().port+'/stream?device_id=fixture&service_id=77',ClientConfig:{PingInterval:1,ReconnectCount:2,ReconnectInterval:0.01,ReconnectNonce:0}}}));});
const wss=new WebSocketServer({noServer:true});
http.on('upgrade',(req,socket,head)=>wss.handleUpgrade(req,socket,head,ws=>{
 sockets.add(ws);let sentEvent=false;ws.on('close',()=>{streamClosed=true;sockets.delete(ws);});
 ws.on('message',bytes=>{const frame=Frame.decode(bytes);const plain=Frame.toObject(frame,{longs:Number,bytes:String});rows.push({sdkOutbound:plain});if(frame.headers.some(h=>h.key==='type'&&h.value==='ping')){const pong={...frame,headers:[{key:'type',value:'pong'}],payload:Buffer.from(JSON.stringify({PingInterval:1,ReconnectCount:2,ReconnectInterval:0.01,ReconnectNonce:0}))};ws.send(Frame.encode(pong).finish());if(!sentEvent){sentEvent=true;sendEvent();}}});
 function sendEvent(){ const event=Buffer.from(JSON.stringify({schema:'2.0',header:{event_id:'wire-event',event_type:'im.message.receive_v1'},event:{message:{chat_id:'room',message_id:'message-1',message_type:'text',content:'{"text":"SDK fragmented input"}'}}}));
 const cut=17;
 for(const seq of [1,0]){const frame={SeqID:5,LogID:9,service:77,method:1,headers:[{key:'type',value:'event'},{key:'message_id',value:'wire-event'},{key:'sum',value:'2'},{key:'seq',value:String(seq)},{key:'trace_id',value:'trace-fixture'}],payload:seq===0?event.subarray(0,cut):event.subarray(cut)};rows.push({inboundBinary:Buffer.from(Frame.encode(frame).finish()).toString('base64')});ws.send(Frame.encode(frame).finish());}
 }

}));
await new Promise(r=>http.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+http.address().port;
const hashes={},cache=new Map();
function load(path){
 if(cache.has(path))return cache.get(path).exports;
 const source=execFileSync('git',['-C',root,'show',fixed+':'+path],{encoding:'utf8',maxBuffer:8*1024*1024});hashes[path]=createHash('sha256').update(source).digest('hex');
 const module={exports:{}};cache.set(path,module);
 const code=transformSync(source,{loader:'ts',format:'cjs',target:'es2022',supported:{'dynamic-import':false}}).code;
 new Function('require','module','exports',code)(name=>{
 if(name==='@larksuiteoapi/node-sdk')return{...sdk,WSClient:class extends sdk.WSClient{constructor(options){super({...options,domain:origin});instances.push(this);}}};
 if(name.startsWith('node:'))return require(name);
 assert(name.startsWith('.'),'unexpected external '+name);
 return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));
 },module,module.exports);return module.exports;
}
const until=async(predicate,label)=>{const end=Date.now()+10000;while(!predicate()){assert(Date.now()<end,label);await new Promise(r=>setTimeout(r,5));}};
try{
 const {FeishuChannel}=load('src/adapters/channel/feishu/FeishuChannel.ts');const turns=[],sent=[];
 const channel=new FeishuChannel({appId:'cli_0123456789abcdef',appSecret:'mock-secret',connectionMode:'stream',send:async value=>{sent.push(value);}});
 const handle=await channel.start({gateway:{async *submitTurn(input){turns.push(input);yield{type:'assistant_text_delta',text:'Feishu SDK reply'};}},logger:Object.fromEntries(['info','warn','error','debug','trace'].map(level=>[level,message=>rows.push({log:{level,message:String(message)}})]))});
 await until(()=>turns.length===1,'real SDK fragmented event dispatched once');await until(()=>rows.some(r=>r.sdkOutbound?.payload&&Buffer.from(r.sdkOutbound.payload,'base64').toString().includes('"code":200')),'actual SDK protobuf ACK');
 await until(()=>sent.some(s=>s.text?.includes('Feishu SDK reply')),'real adapter output');
 await handle.stop('fixture');await new Promise(r=>setTimeout(r,50));
 const result={fixed,sourceSha256:hashes,sdk:{version:require(join(dirname(sdkPath),'../package.json')).version,sourceSha256:createHash('sha256').update(sdkSource).digest('hex'),stopMethod:typeof sdk.WSClient.prototype.stop,closeMethod:typeof sdk.WSClient.prototype.close},environment:process.versions,formalAcceptance:false,turns,sent,rows,streamClosedAfterFixedAdapterStop:streamClosed};
 await writeFile(join(out,'oracle.json'),JSON.stringify(result,null,2)+'\n',{flag:'wx'});
 console.log(JSON.stringify({turns:turns.length,sent:sent.length,streamClosedAfterFixedAdapterStop:streamClosed}));
}catch(error){await writeFile(join(out,'failure.json'),JSON.stringify({error:String(error.stack),fixed,sourceSha256:hashes,rows,environment:process.versions},null,2)+'\n',{flag:'wx'});throw error;}finally{for(const instance of instances)instance.close({force:true});for(const socket of sockets)socket.terminate();wss.close();http.close();}
