// #867: real Node fetch over loopback; no copied fetch/decoder implementation.
import assert from 'node:assert/strict';
import http from 'node:http';
import z from 'node:zlib';
import {once} from 'node:events';
import {mkdirSync, writeFileSync, realpathSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
assert.equal(process.version, 'v22.22.0');
const base=resolve(process.env.PDRS_BASELINE!), out=resolve(process.argv[2]);
const git=realpathSync(execFileSync('which',['git'],{encoding:'utf8'}).trim());
const sourceCommit=bindBaseline({ baseline: base, commit: 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', gitExecutable: git }).provenance.ref;
const sourceSha256=sourceClosureSha256(base,sourceCommit,git,['src/model/streaming/streamModel.ts','src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts']);
const payload='{"choices":[{"message":{"role":"assistant","content":"ok gzip deflate br identity é"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":2}}';
const bytes=Buffer.from(payload);
const specs:any[]=[
 ['gzip','gzip',z.gzipSync(bytes)],['deflate','deflate',z.deflateSync(bytes)],['br','br',z.brotliCompressSync(bytes)],['identity','identity',bytes],
 ['raw-deflate','deflate',z.deflateRawSync(bytes)],['x-gzip','x-gzip',z.gzipSync(bytes)],['x-deflate','x-deflate',z.deflateSync(bytes)],
 ['stack','gzip, br',z.brotliCompressSync(z.gzipSync(bytes))],['upper','GZip',z.gzipSync(bytes)],
 ['unknown','custom',bytes],['mixed-unknown','gzip, custom',z.gzipSync(bytes)],
 ['multi-gzip','gzip',Buffer.concat([z.gzipSync(bytes.subarray(0,20)),z.gzipSync(bytes.subarray(20))])],
 ['truncated-gzip','gzip',z.gzipSync(bytes).subarray(0,-6)],['truncated-deflate','deflate',z.deflateSync(bytes).subarray(0,-2)],
 ['bad-gzip','gzip',Buffer.from('not gzip')],['bad-br','br',Buffer.from('not brotli')],
 ['empty-gzip','gzip',Buffer.alloc(0)],['empty-deflate','deflate',Buffer.alloc(0)],['empty-br','br',Buffer.alloc(0)],
 ['reset-content','gzip',Buffer.from('not gzip'),'GET',205],
 ['head','gzip',Buffer.from('not gzip'),'HEAD',200],['no-content','gzip',Buffer.alloc(0),'GET',204],
];
for(const [name,encoding,wire] of specs.slice(0,3)) {
 for(const cut of [1,Math.floor(wire.length/2)]) specs.push([`${name}-cut-${cut}`,encoding,wire.subarray(0,cut)]);
}
const cases:any[]=[];
let current:any, seen:string|undefined, release:((destroy?:boolean)=>void)|undefined;
let redirectPredecessor:http.ServerResponse|undefined;
const server=http.createServer((req,res)=>{
 if(current.followBoundary)current.requests++;
 if(current.redirectCase && req.url==='/target'){
  if(current.bodyState==='disconnect')redirectPredecessor?.destroy();
  res.writeHead(200,current.targetSix?{'content-encoding':'gzip,gzip,gzip,gzip,gzip,gzip'}:{});
  res.end('target-ok');return;
 }
 seen=req.headers['x-test'] as string|undefined;
 res.writeHead(current.status??200,{'content-encoding':current.encoding,'connection':current.stream?'keep-alive':'close',...(current.location===undefined?{}:{location:current.location}),...((current.stream||current.heartbeat||current.bodyState)?{'transfer-encoding':'chunked'}:{'content-length':current.wire.length})});
 if(current.bodyState){redirectPredecessor=res;res.flushHeaders();}
 else if(current.heartbeat){
  const wire=current.wire;let at=1;res.write(wire.subarray(0,at));
  const timer=setInterval(()=>{if(at===4){clearInterval(timer);res.end(wire.subarray(at));}else{res.write(wire.subarray(at,++at));}},400);
  res.on('close',()=>clearInterval(timer));
 } else if(current.stream){res.write(current.prefix); release=(destroy=false)=>{if(destroy)res.destroy();else res.end(current.tail);};} else res.end(current.wire);
});
server.listen(0,'127.0.0.1'); await once(server,'listening');
const url=`http://127.0.0.1:${(server.address() as any).port}/`;
const watchdog=setTimeout(()=>{throw Error('#867 Node loopback watchdog');},10000);
try {
 for(const [name,encoding,wire,method='GET',status=200] of specs){
  current={encoding,wire,status};
  let result:any;
  try{const r=await fetch(url,{method});result={hex:Buffer.from(await r.arrayBuffer()).toString('hex'),encoding:r.headers.get('content-encoding'),length:r.headers.get('content-length')};}
  catch(e:any){result={error:e.message,name:e.name};}
  cases.push({name,encoding,wire:wire.toString('hex'),method,status,result});
 }
 const layers:any[]=[];
 for(const encoding of ['gzip','br','identity','custom'])for(const depth of [5,6]){
  let wire=Buffer.from('layer probe');
  for(let i=0;i<depth;i++)wire=encoding==='gzip'?z.gzipSync(wire):encoding==='br'?z.brotliCompressSync(wire):wire;
  const coding=Array(depth).fill(encoding).join(', ');
  for(const method of ['GET','HEAD']){
   current={encoding:coding,wire};let result:any;
   try{const r=await fetch(url,{method});result={text:await r.text()};}
   catch(e:any){result={error:e.message,name:e.name,cause:e.cause?.message};}
   layers.push({encoding:coding,depth,method,wire:wire.toString('hex'),result});
  }
 }
 const tokenBoundaries:any[]=[];
 const gzipLayers=(depth:number)=>{let wire=Buffer.from('ok');for(let i=0;i<depth;i++)wire=z.gzipSync(wire);return wire;};
 for(const [name,encoding,wire,status=200] of [
  ['five-trailing-comma','gzip,gzip,gzip,gzip,gzip,',gzipLayers(5)],
  ['double-comma','gzip,,gzip',gzipLayers(2)],
  ['space-token','gzip, \t ,gzip',gzipLayers(2)],
  ['empty-value','',Buffer.from('ok')],
  ['six-empty-tokens',', , , , ,',Buffer.from('ok')],
  ['multi-line-five',['gzip,gzip,gzip','gzip,gzip'],gzipLayers(5)],
  ['multi-line-six',['gzip,gzip,gzip','gzip,gzip,gzip'],gzipLayers(6)],
  ['no-content-six','gzip,gzip,gzip,gzip,gzip,gzip',Buffer.alloc(0),204],
  ['not-modified-six','gzip,gzip,gzip,gzip,gzip,gzip',Buffer.alloc(0),304],
 ] as any[]){
  current={encoding,wire,status};let result:any;
  try{const r=await fetch(url);result={status:r.status,hex:Buffer.from(await r.arrayBuffer()).toString('hex')};}
  catch(e:any){result={error:e.message,name:e.name,cause:e.cause?.message};}
  tokenBoundaries.push({name,encoding,wire:wire.toString('hex'),status,result});
 }
 const redirects:any[]=[];
 const redirectSpecs:any[]=[];
 for(const status of [301,302,303,307,308])for(const redirect of ['follow','manual'])redirectSpecs.push({name:`${status}-${redirect}`,status,redirect,location:'/target'});
 redirectSpecs.push(
  {name:'no-location',status:302,redirect:'follow'},
  {name:'empty-location',status:302,redirect:'follow',location:''},
  {name:'non-redirect-status',status:200,redirect:'follow',location:'/target'},
  {name:'target-six',status:302,redirect:'follow',location:'/target',targetSix:true},
  {name:'pending-predecessor',status:302,redirect:'follow',location:'/target',bodyState:'pending'},
  {name:'disconnected-predecessor',status:302,redirect:'follow',location:'/target',bodyState:'disconnect'},
 );
 for(const spec of redirectSpecs){
  current={...spec,redirectCase:true,encoding:'gzip,gzip,gzip,gzip,gzip,gzip',wire:Buffer.from('x')};let result:any;
  try{const r=await fetch(url,{redirect:spec.redirect});result={status:r.status,text:await r.text()};}
  catch(e:any){result={error:e.message,name:e.name,cause:e.cause?.message};}
  finally{redirectPredecessor?.destroy();redirectPredecessor=undefined;}
  redirects.push({...spec,encoding:current.encoding,wire:current.wire.toString('hex'),result});
 }
 // Legal codings make the follow loop observable: excess codings would fail
 // before httpRedirectFetch can distinguish null Location from empty string.
 const followBoundaries:any[]=[];
 for(const [name,status,location] of [
  ['empty-location',302,''],['no-location',302,undefined],
  ['ows-location',302,' \t '],['non-redirect-status',200,'/target'],
  ['multiple-choice-status',300,'/target'],
 ] as const)for(const redirect of ['follow','manual'] as const){
  current={followBoundary:true,requests:0,status,location,encoding:'gzip',wire:z.gzipSync(Buffer.from('ok'))};
  let result:any;
  try{const r=await fetch(url,{redirect});result={status:r.status,text:await r.text()};}
  catch(e:any){result={error:e.message,name:e.name,cause:e.cause?.message};}
  followBoundaries.push({name,redirect,status,location,encoding:current.encoding,wire:current.wire.toString('hex'),requests:current.requests,result});
 }
 const sizes:any[]=[];
 for(const encoding of ['gzip','br'])for(const size of [16384,16385]){
  const input=Buffer.alloc(size,97);
  const wire=encoding==='gzip'?z.gzipSync(input):z.brotliCompressSync(input);
  current={encoding,wire};const r=await fetch(url);const decoded=await r.text();
  assert.equal(decoded,'a'.repeat(size),'#867 boundary bytes');
  sizes.push({encoding,limit:16384,size,wire:wire.toString('hex'),result:{length:decoded.length,byte:97}});
 }
 const headers:any[]=[];
 for(const [name,value] of [['latin1','éÿ'],['wide','中'],['surrogate','\ud800'],['trim',' \té\t '],['duplicate','é'],['control','a\u0001b'],['newline','a\nb'],['invalid-name','ok'],['wide-name','ok'],['surrogate-name','ok'],['conversion-order','中']] as const){
  current={encoding:'identity',wire:Buffer.from('ok')}; seen=undefined;
  let result:any;
  const key=name==='invalid-name'?'bad name':name==='wide-name'?'x-中':name==='surrogate-name'?'x-\ud800':'x-test';
  const requestHeaders=name==='duplicate'?{'X-Test':value,'x-test':'ÿ'}:name==='conversion-order'?{'bad name':'ok','x-test':value}:{[key]:value};
  try{const r=await fetch(url,{headers:requestHeaders});await r.text();result={hex:Buffer.from(seen??'','latin1').toString('hex')};}
  catch(e:any){result={error:e.message,name:e.name};}
  headers.push({name,keyUnits:Array.from({length:key.length},(_,i)=>key.charCodeAt(i)),units:[...value].flatMap(c=>Array.from({length:c.length},(_,i)=>c.charCodeAt(i))),result});
 }
 const lifecycle:any[]=[];
 for(const encoding of ['gzip','deflate','br']){
  const compressor=encoding==='gzip'?z.createGzip():encoding==='deflate'?z.createDeflate():z.createBrotliCompress();
  const chunks:Buffer[]=[];compressor.on('data',c=>chunks.push(c));
  compressor.write('first');
  await new Promise<void>((resolve,reject)=>compressor.flush(encoding==='br'?z.constants.BROTLI_OPERATION_FLUSH:z.constants.Z_SYNC_FLUSH,e=>e?reject(e):resolve()));
  const prefix=Buffer.concat(chunks);chunks.length=0;
  compressor.end('last');await once(compressor,'end'); const tail=Buffer.concat(chunks);
  for(const cancel of [false,true]){
   current={encoding,stream:true,prefix,tail};const controller=new AbortController();
   const r=await fetch(url,{signal:controller.signal});const reader=r.body!.getReader();
   const first=await reader.read(); let result:any={first:Buffer.from(first.value!).toString(),beforeEnd:!first.done};
   if(cancel){controller.abort();await assert.rejects(reader.read(),(e:any)=>{assert.equal(e.name,'AbortError');result.error=e.message;result.name=e.name;return true;});}
   else{release!();let rest='';while(true){const c=await reader.read();if(c.done)break;rest+=Buffer.from(c.value!).toString();}result.rest=rest;}
   lifecycle.push({encoding,cancel,prefix:prefix.toString('hex'),tail:tail.toString('hex'),result});
  }
 }
 const endings:any[]=[];
 for(const encoding of ['gzip','deflate','br']) for(const finish of ['release','cancel','disconnect']){
  const prefix=encoding==='gzip'?z.gzipSync(Buffer.from('first')):encoding==='deflate'?z.deflateSync(Buffer.from('first')):z.brotliCompressSync(Buffer.from('first'));
  current={encoding,stream:true,prefix,tail:Buffer.alloc(0)};const controller=new AbortController();
  const r=await fetch(url,{signal:controller.signal});const reader=r.body!.getReader();
  const first=await reader.read(); let settled=false;
  const pending=reader.read().then(value=>{settled=true;return {done:value.done};},e=>{settled=true;return {error:e.message,name:e.name};});
  for(let i=0;i<20;i++) await new Promise(resolve=>setImmediate(resolve));
  const pendingBefore=!settled;assert.equal(pendingBefore,true,'compression EOF must not end HTTP');
  if(finish==='cancel')controller.abort();else release!(finish==='disconnect');
  endings.push({encoding,finish,prefix:prefix.toString('hex'),tail:'',result:{first:Buffer.from(first.value!).toString(),pendingBefore,...await pending}});
 }
 const interrupted:any[]=[];
 for(const encoding of ['identity','gzip','deflate','br']) {
  const full=encoding==='identity'?Buffer.from('firstlast'):encoding==='gzip'?z.gzipSync(Buffer.from('firstlast')):encoding==='deflate'?z.deflateSync(Buffer.from('firstlast')):z.brotliCompressSync(Buffer.from('firstlast'));
  const prefix=full.subarray(0,Math.floor(full.length/2));current={encoding,stream:true,prefix,tail:Buffer.alloc(0)};
  const r=await fetch(url);const pending=r.text().then(text=>({text}),e=>({error:e.message,name:e.name}));
  for(let i=0;i<20;i++)await new Promise(resolve=>setImmediate(resolve));release!(true);
  interrupted.push({encoding,prefix:prefix.toString('hex'),result:await pending});
 }
 const progress:any[]=[];
 const Agent=(globalThis as any)[Symbol.for('undici.globalDispatcher.1')].constructor;
 for(const watchdogMs of [null,900]){
  const agent=new Agent({bodyTimeout:1000,headersTimeout:1000});const controller=new AbortController();
  current={encoding:'gzip',heartbeat:true,wire:z.gzipSync(Buffer.from('first'))};
  let timer:any;
  try{
   const r=await fetch(url,{signal:controller.signal,dispatcher:agent} as any);
   const read=r.text();
   const value=watchdogMs===null?await read:await Promise.race([read,new Promise(resolve=>{timer=setTimeout(()=>resolve('application idle'),watchdogMs);})]);
   progress.push({watchdogMs,result:value});controller.abort();await read.catch(()=>{});
  }finally{clearTimeout(timer);await agent.destroy();}
 }
 // Isolate the real 512 MiB text conversions from the small replay fixtures.
 // This child uses actual fetch and emits only lengths/error shapes.
 const sizeProbe=execFileSync(process.execPath,['--expose-gc',fileURLToPath(new URL('./size-probe.mjs',import.meta.url))],{encoding:'utf8',timeout:120000}).trim().split('\n').map(line=>JSON.parse(line));
 const oracle={sourceCommit,sourceSha256,node:process.version,payload,cases,headers,lifecycle,endings,interrupted,progress,layers,tokenBoundaries,redirects,followBoundaries,sizes,sizeProbe};
 mkdirSync(out,{recursive:true});writeFileSync(resolve(out,'oracle.json'),JSON.stringify(oracle,null,2)+'\n',{flag:'wx'});
 console.log(JSON.stringify(oracle,null,2));
}finally{clearTimeout(watchdog);server.closeAllConnections();server.close();}
