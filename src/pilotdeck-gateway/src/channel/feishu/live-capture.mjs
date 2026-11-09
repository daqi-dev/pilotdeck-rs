// Fixed Feishu media methods over real loopback fetch/FormData and real attachment store.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {mkdir,readFile,writeFile,stat} from 'node:fs/promises';
import {createServer} from 'node:http';
import {createHash} from 'node:crypto';
import {resolve,join,posix} from 'node:path';
const [baseline,outArg]=process.argv.slice(2);const out=resolve(outArg);await mkdir(out,{recursive:true});
const fixed='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';const require=createRequire(join(baseline,'package.json'));const {transformSync}=createRequire(require.resolve('tsx'))('esbuild');const sha=b=>createHash('sha256').update(b).digest('hex');
let currentCase='';let failEdit=false;
const wire=[],clients=new Set();const png=Buffer.from('89504e470d0a1a0a0000000000000000','hex');
const server=createServer(async(req,res)=>{const chunks=[];for await(const b of req)chunks.push(b);const bytes=Buffer.concat(chunks);const type=req.headers['content-type']??'';const row={case:currentCase,path:req.url,method:req.method,authorization:req.headers.authorization};
 if(type.startsWith('multipart/form-data')){
 const boundary=type.split('boundary=')[1],body=bytes.toString('latin1');row.parts=[];
 for(const part of body.split('--'+boundary).slice(1,-1)){const at=part.indexOf('\r\n\r\n');const header=part.slice(0,at);const raw=Buffer.from(part.slice(at+4,-2),'latin1');const name=/name="([^"]+)"/.exec(header)?.[1];const filename=/filename="([^"]+)"/.exec(header)?.[1];row.parts.push({name,...(filename?{filename,size:raw.length,sha256:sha(raw),mime:/Content-Type: ([^\r]+)/i.exec(header)?.[1]}:{value:raw.toString()})});}
 }else if(bytes.length)row.body=JSON.parse(bytes);
 wire.push(row);
 if(failEdit&&req.method==='PUT'&&row.body?.msg_type==='post'){failEdit=false;res.setHeader('content-type','application/json');res.end(JSON.stringify({code:400,msg:'mock post edit refused'}));return;}
 if(req.url.includes('/resources/')){res.setHeader('content-type',req.url.includes('type=image')?'image/png':'application/pdf');res.end(req.url.includes('type=image')?png:Buffer.from('file bytes'));return;}
 res.setHeader('content-type','application/json');
 if(req.url.includes('/tenant_access_token/'))res.end(JSON.stringify({code:0,tenant_access_token:'mock-token',expire:7200}));
 else if(req.url==='/open-apis/im/v1/images')res.end(JSON.stringify({code:0,data:{image_key:'uploaded-image'}}));
 else if(req.url==='/open-apis/im/v1/files')res.end(JSON.stringify({code:0,data:{file_key:'uploaded-file'}}));
 else res.end(JSON.stringify({code:0,data:{message_id:'sent-message-'+wire.length}}));
});
server.on('connection',s=>{clients.add(s);s.on('error',()=>{});s.on('close',()=>clients.delete(s));});await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
const hashes={},cache=new Map();function load(path){
 if(cache.has(path))return cache.get(path).exports;
 const source=execFileSync('git',['-C',baseline,'show',fixed+':'+path],{encoding:'utf8',maxBuffer:8*1024*1024});hashes[path]=sha(source);
 const module={exports:{}};cache.set(path,module);const code=transformSync(source,{loader:'ts',format:'cjs',target:'es2022',supported:{'dynamic-import':false}}).code;
 new Function('require','module','exports',code)(name=>{if(name.startsWith('node:'))return require(name);assert(name.startsWith('.'));return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));},module,module.exports);return module.exports;
}
const realFetch=globalThis.fetch;globalThis.fetch=(url,options)=>realFetch(String(url).replace('https://open.feishu.cn',origin),options);
try{
 const {FeishuChannel}=load('src/adapters/channel/feishu/FeishuChannel.ts');const cases=[];
 const until=async(predicate,label)=>{const end=Date.now()+5000;while(!predicate()){assert(Date.now()<end,label);await new Promise(r=>setTimeout(r,2));}};
 for(const editFailure of [false,true]){
 currentCase=editFailure?'post-edit-fallback':'stream-segments';failEdit=editFailure;const start=wire.length;const turns=[];
 const options={initialThrottleMs:0,initialBufferThreshold:1,throttleMs:0,bufferThreshold:1,activityDelayMs:100000,turnTimeoutMs:0};
 const channel=new FeishuChannel({appId:'cli_0123456789abcdef',appSecret:'mock-secret',connectionMode:'webhook',liveReplyOptions:options});
 const handle=await channel.start({gateway:{async *submitTurn(input){turns.push(input);yield{type:'turn_started',runId:'run-fixture'};yield{type:'assistant_text_delta',text:'first'};yield{type:'assistant_text_delta',text:' second'};yield{type:'tool_call_started',name:'read_file'};yield{type:'tool_call_finished',ok:true};yield{type:'assistant_text_delta',text:'tail'};}},logger:{info(){},warn(){},error(){}}});
 await channel.handleWebhook({}, {writeHead(){},end(){}},JSON.stringify({chatId:'room',text:'live input',eventId:currentCase}));
 await until(()=>wire.slice(start).some(r=>r.body?.content&&JSON.parse(r.body.content)?.zh_cn?.content?.[0]?.[0]?.text==='tail'),'final segment arrives');
 await until(()=>turns.length===1&&channel.activeChats.size===0,'actual adapter turn fully settled');
 await handle.stop('fixture');cases.push({name:currentCase,options,turns,wire:wire.slice(start)});
 }
 await writeFile(join(out,'oracle.json'),JSON.stringify({fixed,sourceSha256:hashes,environment:process.versions,formalAcceptance:false,cases},null,2)+'\n',{flag:'wx'});console.log(JSON.stringify({cases:cases.length,requests:wire.length}));

}catch(error){await writeFile(join(out,'failure.json'),JSON.stringify({error:String(error.stack),wire,sourceSha256:hashes},null,2)+'\n',{flag:'wx'});throw error;}
finally{globalThis.fetch=realFetch;for(const socket of clients)socket.destroy();server.close();}
