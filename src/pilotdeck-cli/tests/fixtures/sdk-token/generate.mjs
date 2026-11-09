// Real fixed pilotdeck sessions list against an owned loopback WS peer.
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {spawn,execFileSync} from 'node:child_process';
import {pathToFileURL} from 'node:url';
const [baselineArg,outputArg]=process.argv.slice(2);
if(!baselineArg||!outputArg)throw Error('usage: node generate.mjs <fixed-baseline> <new-output>');
const baseline=fs.realpathSync(baselineArg),output=path.resolve(outputArg),commit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw Error('Node/ICU mismatch');
if(execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8'}).trim()!==commit)throw Error('wrong baseline');
const hash=bytes=>createHash('sha256').update(bytes).digest('hex'),sources={};
for(const name of ['src/cli/sdkCli.ts','src/cli/pilotdeck.ts','src/gateway/server/authToken.ts']){
 const bytes=fs.readFileSync(path.join(baseline,name));if(!bytes.equals(execFileSync('git',['-C',baseline,'show',`${commit}:${name}`])))throw Error('modified '+name);sources[name]=hash(bytes);
}
if(sources['src/cli/sdkCli.ts']!=='d41c08ce59380f85bb81ec8bd2d9bc876c778f9b2ebaea3fd25b1b5f08e288ec')throw Error('SDK CLI source changed');
for(const name of ['packages/sdk/dist/client.js','packages/sdk/dist/transport.js','packages/sdk/dist/index.js'])sources[name]=hash(fs.readFileSync(path.join(baseline,name)));
const sdkVersion=JSON.parse(fs.readFileSync(path.join(baseline,'packages/sdk/package.json'))).version;
if(sdkVersion!=='0.1.0-alpha.0')throw Error('SDK version mismatch');
const {WebSocketServer}=await import(pathToFileURL(path.join(baseline,'node_modules/ws/wrapper.mjs')));
fs.mkdirSync(output);
const cases=[
 {id:'regular-trim',file:{text:'\ufeff \tfixture token\r\n\u3000'}},
 {id:'invalid-utf8',file:{bytes:[255,97,226,130]}},
 {id:'whitespace-file',file:{text:'\ufeff \t\r\n\u3000'}},
 {id:'missing-file',file:{kind:'missing'}},
 {id:'directory-file',file:{kind:'directory'}},
 {id:'permission-file',file:{text:'permission token',mode:0}},
 {id:'exact-limit',file:{byte:120,length:65536}},
 {id:'over-limit',file:{byte:120,length:65537},safety:'limit'},
 {id:'multibyte-limit',file:{text:'中'.repeat(21845)+'a'}},
 {id:'fifo-no-writer',file:{kind:'fifo'},safety:'non_regular'},
 {id:'fifo-with-writer',file:{kind:'fifo'},writer:{text:'pipe token\r\n',delay:200},safety:'non_regular'},
 {id:'fifo-delayed-writer',file:{kind:'fifo'},writer:{text:'delayed pipe token\n',delay:3000},safety:'non_regular'},
 {id:'flag-overrides-file-and-env',file:{kind:'fifo'},argv:['--auth-token','flag-token'],env:{PILOTDECK_SDK_AUTH_TOKEN:'sdk-token',PILOTDECK_TOKEN:'generic-token'}},
 {id:'empty-flag-falls-to-env',file:{kind:'fifo'},argv:['--auth-token',''],env:{PILOTDECK_SDK_AUTH_TOKEN:'sdk-token'}},
 {id:'sdk-env-overrides-file',file:{kind:'fifo'},env:{PILOTDECK_SDK_AUTH_TOKEN:'sdk-token',PILOTDECK_TOKEN:'generic-token'}},
 {id:'empty-sdk-env-blocks-fallback',file:{kind:'fifo'},env:{PILOTDECK_SDK_AUTH_TOKEN:'',PILOTDECK_TOKEN:'generic-token'}},
 {id:'generic-env-overrides-file',file:{kind:'fifo'},env:{PILOTDECK_TOKEN:'generic-token'}},
 {id:'empty-generic-env-blocks-file',file:{text:'file-token'},env:{PILOTDECK_TOKEN:''}},
 {id:'env-whitespace-stays-untrimmed',file:{kind:'fifo'},env:{PILOTDECK_SDK_AUTH_TOKEN:' \t '}}
];
const bytesFor=file=>file.bytes?Buffer.from(file.bytes):file.length!==undefined?Buffer.alloc(file.length,file.byte):Buffer.from(file.text??'');
function metadata(file){try{const m=fs.lstatSync(file,{bigint:true});return {kind:m.isFIFO()?'fifo':m.isDirectory()?'directory':'file',mode:Number(m.mode&0o777n),size:Number(m.size),ino:String(m.ino),mtimeNs:String(m.mtimeNs)};}catch(error){if(error.code==='ENOENT')return null;throw error;}}
const rows=[];
for(const test of cases){
 const root=path.join(output,test.id),home=path.join(root,'home'),project=path.join(root,'project'),tmp=path.join(root,'tmp');fs.mkdirSync(root);for(const p of [home,project,tmp])fs.mkdirSync(p,{mode:0o700});const file=path.join(home,'server-token');let holder=null,inputBytes;
 if(test.file.kind==='fifo')execFileSync('/usr/bin/mkfifo',[file]);else if(test.file.kind==='directory')fs.mkdirSync(file);else if(test.file.kind!=='missing'){inputBytes=bytesFor(test.file);fs.writeFileSync(file,inputBytes,{mode:0o600});holder=fs.openSync(file,'r');if(test.file.mode!==undefined)fs.chmodSync(file,test.file.mode);}
 const before=metadata(file),wire=[],rawHashes=[],connections=[];let writerDone=false,heldBeforeRelease=null;
 const server=new WebSocketServer({host:'127.0.0.1',port:0});await new Promise((resolve,reject)=>{server.once('listening',resolve);server.once('error',reject);});
 server.on('connection',socket=>{connections.push({closed:false});const connection=connections.at(-1);socket.on('close',()=>connection.closed=true);socket.on('message',data=>{
  const frame=JSON.parse(data.toString());rawHashes.push({bytes:data.length,sha256:hash(data)});const recorded=structuredClone(frame);
  if(frame.type==='hello'){if(typeof frame.token!=='string')throw Error('hello token must be string');recorded.token='<token>';recorded.tokenSummary={utf8:Buffer.byteLength(frame.token),utf16:frame.token.length,sha256:hash(Buffer.from(frame.token))};wire.push(recorded);socket.send(JSON.stringify({type:'hello_ok',protocolVersion:'1.2',serverVersion:'fixture',serverInfo:{capabilities:[]}}));}
  else if(frame.type==='request'){if(frame.method!=='list_sessions')throw Error('unexpected SDK request');wire.push(recorded);socket.send(JSON.stringify({type:'response',id:frame.id,ok:true,result:{sessions:[]}}));}
  else throw Error('unexpected SDK frame');
 });});
 const url=`ws://127.0.0.1:${server.address().port}/ws`;const env={HOME:home,USERPROFILE:home,CFFIXED_USER_HOME:home,PILOT_HOME:home,PATH:'/usr/bin:/bin',TMPDIR:tmp,TMP:tmp,TEMP:tmp,LANG:'C',LC_ALL:'C',TZ:'UTC',NODE_NO_WARNINGS:'1',...test.env};
 const argv=['sessions','list','--gateway-url',url,...test.argv??[]],started=Date.now();let stdout='',stderr='',timedOut=false;
 const child=spawn(process.execPath,['--import',path.join(baseline,'node_modules/tsx/dist/loader.mjs'),path.join(baseline,'src/cli/pilotdeck.ts'),...argv],{cwd:project,env,stdio:['ignore','pipe','pipe']});
 child.stdout.on('data',bytes=>stdout+=bytes);child.stderr.on('data',bytes=>stderr+=bytes);
 const bound=setTimeout(()=>{timedOut=true;child.kill('SIGKILL');},test.id==='fifo-no-writer'?2500:10000);
 const writer=test.writer?setInterval(()=>{if(writerDone||Date.now()-started<test.writer.delay)return;if(test.writer.delay>=3000&&heldBeforeRelease===null)heldBeforeRelease=child.exitCode===null&&connections.length===0;
  let fd;try{fd=fs.openSync(file,fs.constants.O_WRONLY|fs.constants.O_NONBLOCK);fs.writeSync(fd,Buffer.from(test.writer.text));writerDone=true;}catch(error){if(!['ENXIO','EPIPE'].includes(error.code))throw error;}finally{if(fd!==undefined)fs.closeSync(fd);}
 },20):null;
 const exit=await new Promise(resolve=>child.once('close',(code,signal)=>resolve({code,signal})));clearTimeout(bound);if(writer)clearInterval(writer);
 for(const socket of server.clients)socket.terminate();await new Promise(resolve=>server.close(resolve));
 const after=metadata(file);
 // An owned FIFO producer can change mtime; preserve its identity/type/mode,
 // while regular file bodies and all their original metadata remain exact.
 const identity = value => value?{kind:value.kind,mode:value.mode,size:value.size,ino:value.ino}:null;
 let fileUnchanged=JSON.stringify(before?.kind==='fifo'?identity(before):before)===JSON.stringify(after?.kind==='fifo'?identity(after):after);
 if(holder!==null){fileUnchanged&&=fs.readFileSync(holder).equals(inputBytes);fs.closeSync(holder);}
 if(!fileUnchanged)throw Error('token input changed');
 const raw={...test,argv,gatewayUrl:url,stdout,stderr,exit,timedOut,writerDone,heldBeforeRelease,connections,wire,rawHashes,before,after,fileUnchanged,elapsedMs:Date.now()-started};fs.writeFileSync(path.join(root,'raw.json'),JSON.stringify(raw,null,2)+'\n',{flag:'wx'});
 if(test.id==='fifo-no-writer'){if(!timedOut||wire.length||exit.signal!=='SIGKILL')throw Error('FIFO baseline did not remain pending');}
 else if(timedOut||exit.signal)throw Error('baseline child did not settle');
 if(test.id==='over-limit'&&(exit.code!==0||wire[0]?.tokenSummary.utf8!==65537))throw Error('oversized token was not actually sent');
 if(test.id==='fifo-delayed-writer'&&(!heldBeforeRelease||!writerDone||exit.code!==0))throw Error('FIFO delayed-writer positive control failed');
 const normalizedWire=wire.map(frame=>{const copy=structuredClone(frame);if(copy.type==='request'){if(!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(copy.id))throw Error('request id not UUID');copy.id='<uuid>';}return copy;});
 rows.push({...test,stdout,stderr,exit,timedOut,writerDone,heldBeforeRelease,connections:connections.length,wire:normalizedWire,helloRaw:wire[0]?.type==='hello'?rawHashes[0]:null,fileUnchanged,inputMetadata:before?{kind:before.kind,mode:before.mode,size:before.size}:null});
}
fs.writeFileSync(path.join(output,'oracle.json'),JSON.stringify({baseline:commit,node:process.version,icu:process.versions.icu,euid:process.geteuid(),sdkVersion,sources,NODE_NO_WARNINGS:'1',cases:rows},null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify({cases:rows.length,output}));
