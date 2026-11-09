// Fixed TS real outbound media pipeline and real installed weixin-ilink HTTP client.
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {mkdtemp,mkdir,writeFile,readFile} from 'node:fs/promises';
import {createServer} from 'node:http';
import * as crypto from 'node:crypto';
import {tmpdir} from 'node:os';
import {join,resolve,posix} from 'node:path';
import {pathToFileURL} from 'node:url';
const [base,outArg]=process.argv.slice(2),out=resolve(outArg);await mkdir(out,{recursive:true});assert.equal(process.version,'v22.22.0');assert.equal(process.versions.icu,'78.2');
const fixed='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',home=await mkdtemp(join(tmpdir(),'weixin-out-ts-')),require=createRequire(join(base,'package.json')),{transformSync}=createRequire(require.resolve('tsx'))('esbuild');
const ilink=await import(pathToFileURL(join(base,'node_modules/weixin-ilink/dist/index.js')).href),sources={},cache=new Map();let entropy=0,current;
function load(path){if(cache.has(path))return cache.get(path).exports;const source=execFileSync('git',['-C',base,'show',fixed+':'+path],{encoding:'utf8'});sources[path]=crypto.createHash('sha256').update(source).digest('hex');const module={exports:{}};cache.set(path,module);
const code=transformSync(source,{loader:'ts',format:'cjs',target:'es2022',supported:{'dynamic-import':false},define:{'import.meta.url':JSON.stringify(pathToFileURL(join(base,path)).href)}}).code;
new Function('require','module','exports',code)(name=>{if(name==='weixin-ilink')return ilink;if(name==='node:crypto')return{...crypto,randomBytes:n=>Buffer.alloc(n,++entropy===1?0x11:0x22)};if(name==='node:os')return{...require(name),homedir:()=>home};if(name.startsWith('node:'))return require(name);assert(name.startsWith('.'));return load(posix.normalize(posix.join(posix.dirname(path),name)).replace(/\.js$/,'.ts'));},module,module.exports);return module.exports;}
let origin;const wire=[];
const server=createServer(async(req,res)=>{const parts=[];for await(const b of req)parts.push(b);const data=Buffer.concat(parts),url=new URL(req.url,'http://mock');let body;try{body=JSON.parse(data.toString('utf8'));}catch{}
if(body?.msg?.client_id){assert.equal(typeof body.msg.client_id,'string');body.msg.client_id='$CLIENT';}
wire.push({method:req.method,path:url.pathname+url.search,contentType:req.headers['content-type']??null,authorization:req.headers.authorization??null,...(body?{body}:{bytesBase64:data.toString('base64')})});
if(url.pathname.endsWith('/getuploadurl')){res.setHeader('content-type','application/json');if(current==='getupload-ret')return res.end(JSON.stringify({ret:7,errmsg:'mock refused'}));return res.end(JSON.stringify(current==='upload-param'?{ret:0,upload_param:'mock param /+'}:{ret:0,upload_full_url:origin+(current==='redirect307'?'/cdn-redirect':'/cdn-upload')}));}
if(url.pathname==='/cdn-redirect'){res.writeHead(307,{location:'/cdn-upload'});return res.end();}
if(url.pathname==='/cdn-upload'||url.pathname==='/c2c/upload'){if(current==='cdn-error'){res.writeHead(503,{'x-error-message':'mock CDN denied'});return res.end('ignored body');}if(current!=='missing-header')res.setHeader('x-encrypted-param','download-param');return res.end('');}
res.setHeader('content-type','application/json');res.end(JSON.stringify({ret:0}));});
await new Promise(r=>server.listen(0,'127.0.0.1',r));origin='http://127.0.0.1:'+server.address().port;
const nativeFetch=globalThis.fetch;globalThis.fetch=(input,options)=>{const url=new URL(typeof input==='string'?input:input.url??String(input));if(url.hostname==='novac2c.cdn.weixin.qq.com')return nativeFetch(origin+url.pathname+url.search,options);assert.equal(url.origin,origin,'all external requests must be loopback');return nativeFetch(input,options);};
const {WeixinChannel}=load('src/adapters/channel/weixin/WeixinChannel.ts'),cases=[];
const png='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==';
try{
for(const name of ['file','image','upload-param','getupload-ret','cdn-error','missing-header','redirect307','local-path','empty-input','client-missing','context-missing']){
current=name;entropy=0;const start=wire.length,logs=[];const c=new WeixinChannel({credentialsPath:join(home,name+'.json')});c.client=name==='client-missing'?null:new ilink.ILinkClient({baseUrl:origin,token:'mock-token'});if(name!=='context-missing')c.contextTokens.set('alice','ctx');c.logger={info:s=>logs.push(['info',s]),warn:s=>logs.push(['warn',s]),error:s=>logs.push(['error',s])};
const input=name==='image'?{type:'image',name:'pixel.png',mimeType:'image/png',content:png}:name==='local-path'?{type:'file',name:'reply.txt',path:'/fixture/reply.txt',source:'local_path'}:name==='empty-input'?{type:'file',name:'reply.txt'}:{type:'file',name:'reply.txt',mimeType:'text/plain',content:Buffer.from('weixin-file-marker').toString('base64')};
const result=await c.sendAttachment('alice',input);cases.push({name,input,result,wire:wire.slice(start).map(row=>JSON.parse(JSON.stringify(row).split(origin).join('$MOCK'))),logs,pendingReplies:[...c.pendingReplies]});
}
const sdk={};for(const f of ['client.js','api.js'])sdk[f]=crypto.createHash('sha256').update(await readFile(join(base,'node_modules/weixin-ilink/dist',f))).digest('hex');
await writeFile(join(out,'oracle.json'),JSON.stringify({fixed,sourceSha256:sources,sdk,environment:process.versions,formalAcceptance:false,declaredMocks:['deterministic entropy for AES/filekey only','homedir isolated','official CDN origin routed to loopback'],cases},null,2)+String.fromCharCode(10),{flag:'wx'});console.log(JSON.stringify({cases:cases.length}));
}finally{globalThis.fetch=nativeFetch;await new Promise(r=>server.close(r));}
