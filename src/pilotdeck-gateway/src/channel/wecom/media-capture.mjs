// Non-frozen diagnostic, real fixed TS media methods with loopback network.
import {execFileSync} from 'node:child_process';
import {createRequire} from 'node:module';
import {mkdir,writeFile,readFile,stat} from 'node:fs/promises';
import {createServer} from 'node:http';
import {createHash,createCipheriv} from 'node:crypto';
import {resolve,join,relative} from 'node:path';
import {pathToFileURL} from 'node:url';
const [baseline,outArg]=process.argv.slice(2);const out=resolve(outArg);await mkdir(out,{recursive:true});
const fixed='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const show=p=>execFileSync('git',['-C',baseline,'show',fixed+':'+p],{maxBuffer:8*1024*1024}).toString();
const sourcePath='src/adapters/channel/wecom/WeComChannel.ts';const source=show(sourcePath);
const require=createRequire(join(baseline,'package.json'));const esbuild=createRequire(require.resolve('tsx'))('esbuild');
const bundle=join(out,'fixed-media.cjs');
await esbuild.build({stdin:{contents:source+'\nexport {decryptWeComBytes,extractWeComDeliverables};',sourcefile:sourcePath,resolveDir:join(baseline,'src/adapters/channel/wecom'),loader:'ts'},bundle:true,platform:'node',format:'cjs',outfile:bundle,nodePaths:[join(baseline,'node_modules')],plugins:[{name:'fixed-ts',setup(build){build.onLoad({filter:/\.[jt]s$/},args=>{if(args.path.startsWith(join(baseline,'src')+'/')){let p=relative(baseline,args.path);if(p.endsWith('.js'))p=p.slice(0,-3)+'.ts';return{contents:show(p),loader:'ts'};}});}}]});
const {WeComChannel,decryptWeComBytes,extractWeComDeliverables}=require(bundle);
const channel=new WeComChannel({botKey:'mock',extra:{secret:'mock'},uuid:()=> '00000000-0000-0000-0000-000000000001'});
const sha=b=>createHash('sha256').update(b).digest('hex');
const prepare=[];
for(const [size,mime,name]of [[1,'','a.png'],[10485761,'image/png','a.png'],[10485761,'video/mp4','a.mp4'],[1,'audio/ogg','a.ogg'],[2097153,'audio/amr','a.amr'],[20971521,'','a.bin']]){
 const p=channel.prepareOutboundMediaBytes(Buffer.alloc(size,65),mime,name);prepare.push({size,mime,name,...Object.fromEntries(Object.entries(p).filter(([k])=>k!=='data'))});
}
const key=Buffer.from(Array.from({length:32},(_,i)=>i));const plain=Buffer.from('WeCom encrypted attachment');const pad=32-plain.length%32;const cipher=createCipheriv('aes-256-cbc',key,key.subarray(0,16));cipher.setAutoPadding(false);const encrypted=Buffer.concat([cipher.update(Buffer.concat([plain,Buffer.alloc(pad,pad)])),cipher.final()]);
const decrypt={key:key.toString('base64').replace(/=+$/,''),encrypted:encrypted.toString('base64'),plain:plain.toString(),invalid:[]};
for(const [data,k]of [[encrypted,'YQ'],[Buffer.alloc(16),key.toString('base64')]]){try{decryptWeComBytes(data,k);decrypt.invalid.push('unexpected success');}catch(e){decrypt.invalid.push(String(e.message));}}
const wire=[];const server=createServer((req,res)=>{wire.push({path:req.url,ua:req.headers['user-agent'],accept:req.headers.accept});if(req.url==='/redirect'){res.writeHead(302,{location:'/bytes'});return res.end();}if(req.url==='/error'){res.writeHead(404);return res.end();}res.setHeader('content-type','image/png');res.setHeader('content-disposition','attachment; filename="remote.png"');res.end('remote-bytes');});
await new Promise(r=>server.listen(0,'127.0.0.1',r));const origin='http://127.0.0.1:'+server.address().port;
const inbound=[];
for(const [kind,media]of [['image',{base64:encrypted.toString('base64'),aeskey:decrypt.key,filename:'../测试 ?.png'}],['file',{url:origin+'/redirect'}],['image',{base64:'',url:origin+'/error'}]]){
 try{const result=await channel.cacheInboundMedia(kind,media);if(result){const bytes=await readFile(result.path);const mode=(await stat(result.path)).mode&0o777;inbound.push({kind,media:{...media,url:media.url?.replace(origin,'$MOCK')},result:{...result,path:'$CACHE',bytesSha256:sha(bytes),mode}});}else inbound.push({kind,result:null});}catch(e){inbound.push({kind,error:String(e.message)});}
}
const upload=[];
for(const fail of [null,'init','chunk','finish']){
 const requests=[];channel.sendRequest=async(cmd,body)=>{requests.push({cmd,body:body.base64_data?{...body,base64_data:undefined,chunkBytes:Buffer.from(body.base64_data,'base64').length,chunkSha256:sha(Buffer.from(body.base64_data,'base64'))}:body});if(fail&&cmd.endsWith('_'+fail))return{body:{errcode:3,errmsg:'mock fail'}};return cmd.endsWith('_init')?{body:{upload_id:'upload-1'}}:cmd.endsWith('_finish')?{body:{media_id:'media-1'}}:{};};
 try{const result=await channel.uploadMediaBytes(Buffer.alloc(524289,65),'file','sample.pdf');upload.push({fail,requests,result});}catch(e){upload.push({fail,requests,error:String(e.message)});}
}
const paths=join(out,'paths');await mkdir(join(paths,'.git'),{recursive:true});await writeFile(join(paths,'a.pdf'),'file');await writeFile(join(paths,'.git','a.pdf'),'secret');
const extract=[];for(const input of ['MEDIA:'+join(paths,'a.pdf'),'MEDIA:'+join(paths,'.git/a.pdf'),'MEDIA:'+join(paths,'missing.pdf'),'MEDIA:'+join(paths,'a.pdf')+'\n'+join(paths,'a.pdf'),'>'+join(paths,'a.pdf'),String.fromCharCode(96)+join(paths,'a.pdf')+String.fromCharCode(96)]){const result=await extractWeComDeliverables(input);extract.push({input:input.replaceAll(paths,'$PATHS'),result:JSON.parse(JSON.stringify(result).replaceAll(paths,'$PATHS'))});}
server.close();
await writeFile(join(out,'oracle.json'),JSON.stringify({fixed,sourceSha256:sha(source),environment:process.versions,formalAcceptance:false,prepare,decrypt,inbound,wire,upload,extract},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({prepare:prepare.length,inbound:inbound.length,upload:upload.length,extract:extract.length}));
