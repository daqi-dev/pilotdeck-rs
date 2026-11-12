// Ordinary production Server media flow. Protocols are migrated from the existing
// Feishu/Weixin/WeCom actual Server fixtures; this script starts the real CLI.
import assert from 'node:assert/strict';
import {spawn,execFileSync} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdir,writeFile,readFile,readdir,stat} from 'node:fs/promises';
import {resolve,join,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createRequire} from 'node:module';
import {createCipheriv,createDecipheriv,createHash} from 'node:crypto';
import {selectModelRoundInput,selectPersistedRound,selectLatestModelUser,selectPersistedOutputTurns} from './reference-round-context.mjs';
const [baseline,gateway,sidecar,outArg,channel,caseArg,engineArg,layoutArg]=process.argv.slice(2);
assert(['feishu','weixin','wecom'].includes(channel));
const engine=engineArg??'native';assert(['native','ts'].includes(engine));const [mediaCase,failureStage='init']=(caseArg??'image-file').split(':');
const cases={feishu:['image-file','missing-image-key','missing-message-id','download404','upload-failure'],weixin:['image-file','voice','video-diagnostic','missing-url','plaintext-file','download404','decrypt-invalid','upload-failure'],wecom:['image-file','voice','video','quote-image','quote-file','quote-voice','appmsg','download404','decrypt-invalid','upload-failure']};assert(cases[channel].includes(mediaCase),'applicable media case');
const fixedTS='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const out=resolve(outArg);await mkdir(out,{mode:0o700});
const require=createRequire(join(resolve(baseline),'package.json')),prerequisiteFailures=[];
try{assert.equal(execFileSync('git',['-C',baseline,'rev-parse','HEAD'],{encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim(),fixedTS);execFileSync('git',['-C',baseline,'diff','--exit-code','HEAD','--','src','packages/sdk','package.json','pnpm-lock.yaml'],{stdio:['ignore','pipe','pipe']});}catch(error){prerequisiteFailures.push({kind:'fixed-ts-source',detail:String(error.message)});}
for(const dependency of engine==='ts'?['ws','tsx','@pilotdeck/sdk','@larksuiteoapi/node-sdk','weixin-ilink','undici']:['ws']){try{require.resolve(dependency);}catch{prerequisiteFailures.push({kind:'missing-real-dependency',dependency});}}
if(engine==='ts'){for(const file of ['src/cli/pilotdeck.ts','node_modules/tsx/dist/loader.mjs']){try{assert((await stat(join(resolve(baseline),file))).isFile());}catch{prerequisiteFailures.push({kind:'missing-real-entrypoint',file});}}const [major,minor]=process.versions.node.split('.').map(Number);if(major!==22||minor<13)prerequisiteFailures.push({kind:'unsupported-reference-runtime',required:'>=22.13.0 <23',actual:process.version});}
if(prerequisiteFailures.length){await writeFile(join(out,'evidence.json'),JSON.stringify({schemaVersion:2,profile:'native-media-v2',channel,mediaCase,failureStage,engine,fixedTS,status:'BLOCKED',formalAcceptance:false,prerequisiteFailures,steps:[],failure:'Production prerequisites unavailable; no gateway or replacement SDK was executed.'},null,2)+'\n',{flag:'wx'});process.exit(1);}
const {WebSocket,WebSocketServer}=require('ws');
const layout=layoutArg?JSON.parse(await readFile(layoutArg,'utf8')):null;if(layout){assert.equal(layout.root,join(dirname(out),'cur'));for(const key of ['mockPort','controlPort'])assert(Number.isInteger(layout[key])&&layout[key]>0&&layout[key]<65536);assert.notEqual(layout.mockPort,layout.controlPort);await mkdir(layout.root,{mode:0o700});}
// The fixed TS server uses PILOT_HOME for unbound channel sessions; process.cwd() only selects its config/startup root.
const osHome=join(layout?.root??out,'os-home'),home=join(layout?.root??out,'home'),project=join(layout?.root??out,'project'),tmp=join(layout?.root??out,'tmp');
for(const dir of [osHome,home,project,tmp])await mkdir(dir,{mode:0o700});
const PNG='iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==';
const image=Buffer.from(PNG,'base64'),fileIn=Buffer.from(`${channel}-inbound-file-marker`),fileOut=Buffer.from(`${channel}-outbound-file-marker`);
const voice=Buffer.concat([Buffer.from('OggS'),Buffer.alloc(28,0x42)]),video=Buffer.from('000000186674797069736f6d0000020069736f6d69736f32','hex');
const outputVoice=join(project,'reply.ogg'),outputVideo=join(project,'reply.mp4');await writeFile(outputVoice,voice);await writeFile(outputVideo,video);
const outputFile=join(project,'reply.txt'),outputImage=join(project,'reply.png');await writeFile(outputFile,fileOut);await writeFile(outputImage,image);
const sha=raw=>createHash('sha256').update(raw).digest('hex');
const digest=async file=>sha(await readFile(file));
const key128=Buffer.alloc(16,14),key256=Buffer.from(Array.from({length:32},(_,i)=>i));
function encrypt(plain){if(channel==='weixin'){const cipher=createCipheriv('aes-128-ecb',key128,null);return Buffer.concat([cipher.update(plain),cipher.final()]);}const pad=32-plain.length%32,cipher=createCipheriv('aes-256-cbc',key256,key256.subarray(0,16));cipher.setAutoPadding(false);return Buffer.concat([cipher.update(Buffer.concat([plain,Buffer.alloc(pad,pad)])),cipher.final()]);}
const ciphertextImage=encrypt(image),ciphertextFile=encrypt(fileIn),ciphertextVoice=encrypt(voice),ciphertextVideo=encrypt(video);
const expectedOutgoing=type=>({image,voice,video,file:fileOut}[type]);
const models=[],http=[],wire=[],inbound=[],frames=[],steps=[],mediaChecks=[],pollQueue=[],persistedSessions=[],persistedMessages=[],permissionContinuations=[],modelRoundInputs=[],roundContexts=[];
const peers=new Set(),sockets=new Set(),phases=new Map(),uploadParameters=new Map();
let generation=0,sequence=0,child,control,stdout='',stderr='',failure=null,exit=null;
const delay=ms=>new Promise(r=>setTimeout(r,ms));
async function wait(test,label,timeout=40000){const end=Date.now()+timeout;while(!await test()){const broken=http.find(row=>row.fixtureError);if(broken)throw Error(broken.fixtureError);if(child?.spawnError)throw child.spawnError;if(engine==='ts'&&/not installed;|Cannot find (?:package|module)|ERR_MODULE_NOT_FOUND/.test(stderr))throw Error('real TS dependency/startup unavailable: '+stderr);if(child&&child.exitCode!==null)throw Error(`Gateway exited ${child.exitCode}: ${label}`);assert(Date.now()<end,`timeout ${label}`);await delay(20);}}
const send=(ws,frame)=>ws.send(Buffer.isBuffer(frame)?frame:JSON.stringify(frame));
function varint(n){const out=[];while(n>=128){out.push((n&127)|128);n=Math.floor(n/128);}return Buffer.from([...out,n]);}
function bytes(k,value){value=Buffer.from(value);return Buffer.concat([varint(k*8+2),varint(value.length),value]);}
function feishuFrame(event,n){const fields=[];for(const[k,v]of [[1,n],[2,n+100],[3,77],[4,1]])fields.push(varint(k*8),varint(v));for(const[k,v]of [['type','event'],['message_id',`media-wire-${n}`],['sum','1'],['seq','0']])fields.push(bytes(5,Buffer.concat([bytes(1,k),bytes(2,v)])));fields.push(bytes(6,'utf8'),bytes(7,'json'),bytes(8,JSON.stringify(event)));return Buffer.concat(fields);}
function respond(res,status,mime,data,headers={}){res.writeHead(status,{'content-type':mime,...headers});res.end(data);}
const service=createServer(async(req,res)=>{try{
 const chunks=[];for await(const c of req)chunks.push(c);const raw=Buffer.concat(chunks),url=new URL(req.url,'http://127.0.0.1'),pathname=url.pathname;
 let body;try{body=JSON.parse(raw);}catch{body=null;}
 const row={generation,time:Date.now(),method:req.method,path:pathname,query:url.search,headers:req.headers,rawBytesHex:raw.toString('hex'),body};http.push(row);
 if(pathname==='/missing'||pathname.includes('/resources/img-bad')){row.actualResponse={status:404,body:'missing media fixture'};respond(res,404,'text/plain','missing media fixture');return;}
 if(pathname==='/invalid-cipher'){row.actualResponse={status:200,rawBytesHex:'010203'};respond(res,200,'application/octet-stream',Buffer.from([1,2,3]));return;}
 if(pathname==='/plaintext.txt'){respond(res,200,'text/plain',fileIn);return;}
 if(pathname==='/incoming.ogg'||pathname==='/incoming.mp4'){respond(res,200,'application/octet-stream',pathname.endsWith('.ogg')?ciphertextVoice:ciphertextVideo);return;}
 if(pathname==='/incoming.png'||pathname==='/incoming.txt'){const plain=pathname.endsWith('.png')?image:fileIn,data=channel==='feishu'?plain:(pathname.endsWith('.png')?ciphertextImage:ciphertextFile);row.mediaResponse={plainSha256:sha(plain),cipherSha256:sha(data),bytes:data.length};respond(res,200,channel==='feishu'?(pathname.endsWith('.png')?'image/png':'text/plain'):'application/octet-stream',data);return;}
 if(pathname.includes('/resources/img-key')){respond(res,200,'image/png',image);return;}
 if(pathname.includes('/resources/file-key')){respond(res,200,'text/plain',fileIn);return;}
 if(pathname==='/callback/ws/endpoint'){respond(res,200,'application/json',JSON.stringify({code:0,data:{URL:mock.replace('http:','ws:')+'/feishu/stream?service_id=77&device_id=media',ClientConfig:{PingInterval:1,ReconnectCount:2,ReconnectInterval:0.01,ReconnectNonce:0}}}));return;}
 if(pathname.includes('tenant_access_token')){respond(res,200,'application/json','{"code":0,"tenant_access_token":"media-tenant-token","expire":7200}');return;}
 if(pathname.endsWith('/getupdates')){await delay(50);respond(res,200,'application/json',JSON.stringify({ret:0,msgs:pollQueue.length?[pollQueue.shift()]:[],get_updates_buf:'cursor'}));return;}
 if(pathname.endsWith('/getuploadurl')){if(mediaCase==='upload-failure'&&failureStage==='getuploadurl'){row.actualResponse={status:200,body:{ret:1,errmsg:'mock upload rejected'}};respond(res,200,'application/json',JSON.stringify(row.actualResponse.body));return;}assert(body.aeskey);const id=`upload-${generation}-${uploadParameters.size}`;uploadParameters.set(id,{generation,params:body});respond(res,200,'application/json',JSON.stringify({ret:0,upload_full_url:mock+'/cdn-upload/'+id}));return;}
 if(pathname.startsWith('/cdn-upload/')){if(mediaCase==='upload-failure'&&failureStage==='cdn-http'){row.actualResponse={status:503,body:'mock CDN rejected'};respond(res,503,'text/plain',row.actualResponse.body);return;}const id=pathname.split('/').at(-1),entry=uploadParameters.get(id);assert(entry,'known upload URL');const params=entry.params,key=Buffer.from(params.aeskey,'hex'),decipher=createDecipheriv('aes-128-ecb',key,null),plain=Buffer.concat([decipher.update(raw),decipher.final()]);const expected=params.media_type===1?image:fileOut;assert.deepEqual(plain,expected);assert.equal(params.rawfilemd5,createHash('md5').update(expected).digest('hex'));assert.equal(params.filesize,(Math.floor(expected.length/16)+1)*16);row.decryption={uploadId:id,plainSha256:sha(plain),expectedSha256:sha(expected),rawfilemd5:params.rawfilemd5,mediaType:params.media_type};mediaChecks.push({generation,type:'cdn-upload-bytes',actual:{httpIndex:http.indexOf(row),field:'rawBytesHex'},expected:{field:'inputMaterials.outgoingByType.'+(params.media_type===1?'image':'file')},...row.decryption});if(mediaCase==='upload-failure'&&failureStage==='cdn-header'){row.actualResponse={status:200,body:'',missingHeader:'x-encrypted-param'};respond(res,200,'text/plain','');return;}respond(res,200,'text/plain','',{'x-encrypted-param':'download-param'});return;}
 if(pathname.endsWith('/sendmessage')||pathname.endsWith('/getconfig')||pathname.endsWith('/sendtyping')){respond(res,200,'application/json','{"ret":0}');return;}
 if(pathname==='/open-apis/im/v1/files'||pathname==='/open-apis/im/v1/images'){if(mediaCase==='upload-failure'&&pathname.endsWith('/'+(failureStage==='image'?'images':'files'))){row.actualResponse={status:200,body:{code:19001,msg:'mock upload rejected'}};respond(res,200,'application/json',JSON.stringify(row.actualResponse.body));return;}const isImage=pathname.endsWith('/images'),expected=isImage?image:fileOut;assert.equal(req.headers.authorization,'Bearer media-tenant-token');assert(raw.includes(expected),'actual multipart contains expected file bytes');mediaChecks.push({generation,type:isImage?'multipart-image':'multipart-file',actual:{httpIndex:http.indexOf(row),field:'rawBytesHex'},expected:{field:'inputMaterials.outgoingByType.'+(isImage?'image':'file')},plainSha256:sha(expected),rawSha256:sha(raw)});respond(res,200,'application/json',JSON.stringify({code:0,data:isImage?{image_key:'uploaded-image-key'}:{file_key:'uploaded-file-key'}}));return;}
 if(pathname.startsWith('/open-apis/im/v1/')){respond(res,200,'application/json','{"code":0,"data":{"message_id":"reply","reaction_id":"reaction"}}');return;}
 if(pathname.startsWith('/v1/')){models.push({generation,body});if(!body.stream){respond(res,200,'application/json',JSON.stringify({choices:[{message:{role:'assistant',content:JSON.stringify({title:'Media'})},finish_reason:'stop'}]}));return;}
 const phase=phases.get(generation)??0;phases.set(generation,phase+1);
 const tool=(name,file,mime,index)=>({index,id:`${channel}-${name}-${generation}`,type:'function',function:{name:'send_attachment',arguments:JSON.stringify({file_path:file,name:name==='file'?'reply.txt':'reply.png',mime_type:mime})}});
 const delta=channel==='wecom'?{role:'assistant',content:`MEDIA_OUTPUT_${generation}\nMEDIA:${outputFile}\nMEDIA:${outputImage}\nMEDIA:${outputVoice}\nMEDIA:${outputVideo}`}:(phase===0?{role:'assistant',tool_calls:[tool('file',outputFile,'text/plain',0),tool('image',outputImage,'image/png',1)]}:{role:'assistant',content:`MEDIA_OUTPUT_${generation}`});
 const finish=delta.tool_calls?'tool_calls':'stop';respond(res,200,'text/event-stream','data: '+JSON.stringify({id:'media',choices:[{index:0,delta,finish_reason:null}]})+'\n\ndata: '+JSON.stringify({id:'media',choices:[{index:0,delta:{},finish_reason:finish}]})+'\n\ndata: [DONE]\n\n');return;}
 respond(res,404,'application/json',JSON.stringify({error:'unhandled media fixture path',path:pathname}));
 }catch(error){http.push({fixtureError:String(error.stack??error)});respond(res,500,'text/plain',String(error.stack??error));}});
service.on('connection',socket=>{sockets.add(socket);socket.on('error',()=>{});socket.on('close',()=>sockets.delete(socket));});
const wss=new WebSocketServer({server:service});wss.on('connection',ws=>{peers.add(ws);ws.ready=channel==='feishu';ws.on('close',()=>peers.delete(ws));ws.on('error',()=>{});ws.on('message',(data,binary)=>{try{if(binary){wire.push({generation,direction:'gateway-to-peer',binary:true,rawBytesHex:data.toString('hex')});return;}const value=JSON.parse(data);wire.push({generation,direction:'gateway-to-peer',frame:value});if(channel!=='wecom')return;const id=value.headers?.req_id;assert(id);let response={headers:{req_id:id},body:{errcode:0}};if(value.cmd==='aibot_subscribe')ws.ready=true;if(value.cmd==='aibot_upload_media_init'&&mediaCase==='upload-failure'&&failureStage==='init'){response.body={errcode:19001,errmsg:'mock upload rejected'};}else if(value.cmd==='aibot_upload_media_init'){const uploadId=`wecom-${generation}-${sequence++}`;uploadParameters.set(uploadId,{generation,params:value.body,initWireIndex:wire.length-1,chunks:[]});response.body.upload_id=uploadId;}if(value.cmd==='aibot_upload_media_chunk'){const entry=uploadParameters.get(value.body.upload_id);assert(entry);entry.chunks.push({wireIndex:wire.length-1,index:value.body.chunk_index,rawBytesHex:Buffer.from(value.body.base64_data,'base64').toString('hex')});}if(value.cmd==='aibot_upload_media_finish'){const entry=uploadParameters.get(value.body.upload_id);assert(entry);const bytes=Buffer.concat(entry.chunks.map(c=>Buffer.from(c.rawBytesHex,'hex'))),inputType=({'reply.txt':'file','reply.png':'image','reply.ogg':'voice','reply.mp4':'video'})[entry.params.filename];assert(inputType,'upload filename identifies the supplied output material');const expected=expectedOutgoing(inputType);assert.deepEqual(bytes,expected,'upload filename and original bytes agree');assert.equal(entry.params.type,inputType==='voice'?'file':inputType,'OGG is delivered as file, preserving the original bytes');assert.equal(entry.params.total_size,bytes.length);assert.equal(entry.params.total_chunks,entry.chunks.length);assert.equal(entry.params.md5,createHash('md5').update(bytes).digest('hex'));assert.deepEqual(entry.chunks.map(c=>c.index),entry.chunks.map((_,i)=>i));const mediaId='media-'+value.body.upload_id;entry.mediaId=mediaId;mediaChecks.push({generation,type:'wecom-upload',actual:{initWireIndex:entry.initWireIndex,finishWireIndex:wire.length-1,chunkWireIndices:entry.chunks.map(c=>c.wireIndex)},expected:{field:'inputMaterials.outgoingByType.'+inputType},inputType,mediaType:entry.params.type,mediaId,plainSha256:sha(bytes),expectedSha256:sha(expected),chunks:entry.chunks});response.body.media_id=mediaId;}if(mediaCase==='upload-failure'&&((failureStage==='chunk'&&value.cmd==='aibot_upload_media_chunk')||(failureStage==='finish'&&value.cmd==='aibot_upload_media_finish')))response.body={errcode:19001,errmsg:'mock '+failureStage+' rejected'};wire.push({generation,direction:'peer-to-gateway',frame:response});send(ws,response);}catch(error){http.push({fixtureError:String(error.stack??error)});ws.close();}});});
await new Promise(r=>service.listen(layout?.mockPort??0,'127.0.0.1',r));const mock='http://127.0.0.1:'+service.address().port;
let controlPort=layout?.controlPort;if(!controlPort){const reserve=createServer();await new Promise(r=>reserve.listen(0,'127.0.0.1',r));controlPort=reserve.address().port;await new Promise(r=>reserve.close(r));}
const options={feishu:{appId:'cli_0123456789abcdef',appSecret:'mock-secret',connectionMode:'stream'},weixin:{},wecom:{token:'mock-bot',extra:{botSecret:'mock-secret',dmPolicy:'open',textBatchDelayMs:0,websocketUrl:mock.replace('http:','ws:')+'/wecom'}}};
const config=on=>JSON.stringify({schemaVersion:1,extension:{builtinPluginsEnabled:{'browser-use':false,funasr:false,'windows-skills':false}},agent:{model:'fixture/mock'},model:{providers:{fixture:{protocol:'openai',url:mock+'/v1',apiKey:'mock-only',models:{mock:{multimodal:{input:['text','image']}}}}}},adapters:{[channel]:{enabled:on,...options[channel]}}});
const configPath=join(home,'pilotdeck.yaml'),tokenPath=join(home,'token');await writeFile(configPath,config(true));await writeFile(tokenPath,'media-fixture-token\n');if(channel==='weixin'){const credentials=JSON.stringify({baseUrl:mock,botToken:'mock-token',accountId:'mock-account'});await writeFile(join(home,'weixin-credentials.json'),credentials);await mkdir(join(osHome,'.pilotdeck'),{mode:0o700});await writeFile(join(osHome,'.pilotdeck','weixin-credentials.json'),credentials);}
function finalSent(g){return http.some(row=>row.generation===g&&!row.path.startsWith('/v1/')&&JSON.stringify(row.body).includes(`MEDIA_OUTPUT_${g}`))||wire.some(row=>row.generation===g&&row.frame?.cmd==='aibot_respond_msg'&&JSON.stringify(row.frame.body).includes(`MEDIA_OUTPUT_${g}`));}
function mediaSent(g){if(mediaCase==='upload-failure'){return channel==='wecom'?wire.some(row=>row.generation===g&&row.direction==='peer-to-gateway'&&row.frame?.body?.errcode===19001):http.some(row=>row.generation===g&&row.actualResponse&&(row.actualResponse.status===503||row.actualResponse.missingHeader==='x-encrypted-param'||row.actualResponse.body?.ret===1||row.actualResponse.body?.code===19001));}if(channel==='feishu'){return ['file','image'].every(type=>http.some(row=>row.generation===g&&row.body?.msg_type===type&&String(row.body.content).includes(`uploaded-${type}-key`)));}if(channel==='weixin'){return [2,4].every(type=>http.some(row=>row.generation===g&&row.path.endsWith('/sendmessage')&&row.body?.msg?.item_list?.some(item=>item.type===type)));}return ['file','image','voice','video'].every(inputType=>{const mediaType=inputType==='voice'?'file':inputType;return mediaChecks.some(check=>check.generation===g&&check.type==='wecom-upload'&&check.inputType===inputType&&check.mediaType===mediaType&&wire.some(row=>row.generation===g&&row.frame?.cmd==='aibot_respond_msg'&&row.frame.headers.req_id===`media-in-${g}`&&row.frame.body?.[mediaType]?.media_id===check.mediaId));});}
function wecomFrame(g,content){return {cmd:'aibot_msg_callback',headers:{req_id:`media-in-${g}`},body:{msgid:`media-msg-${g}`,from:{userid:'alice'},chatid:'alice',chattype:'single',...content}};}
async function deliver(g){
 const imageRef={url:mock+'/incoming.png',aeskey:key256.toString('base64'),content_type:'image/png'},fileRef={url:mock+'/incoming.txt',aeskey:key256.toString('base64'),filename:'inbound.txt'};
 if(channel==='weixin'){
  const items=mediaCase==='voice'?[{type:3,voice_item:{text:`MEDIA_VOICE_${g}`}}]:mediaCase==='video-diagnostic'?[{type:5,video_item:{}}]:mediaCase==='missing-url'?[{type:2,image_item:{}},{type:4,file_item:{file_name:'missing.txt'}}]:mediaCase==='plaintext-file'?[{type:4,file_item:{file_name:'plain.txt',file_size:fileIn.length,cdn_url:mock+'/plaintext.txt'}}]:mediaCase==='download404'?[{type:2,image_item:{cdn_url:mock+'/missing'}}]:mediaCase==='decrypt-invalid'?[{type:2,image_item:{cdn_url:mock+'/invalid-cipher',aeskey:key128.toString('hex')}}]:[{type:2,image_item:{cdn_url:mock+'/incoming.png',aeskey:key128.toString('hex')}},{type:4,file_item:{file_name:'inbound.txt',file_size:fileIn.length,cdn_url:mock+'/incoming.txt',aeskey:key128.toString('hex')}},{type:3,voice_item:{text:`MEDIA_VOICE_${g}`}}];
  items.unshift({type:1,text_item:{text:`MEDIA_INPUT_${g}`}});
  const frame={from_user_id:'chat',context_token:`media-context-${g}`,message_type:1,message_id:`media-in-${g}`,item_list:items};inbound.push({generation:g,mediaCase,frame});pollQueue.push(frame);return;
 }
 await wait(()=>[...peers].some(ws=>ws.ready),'ready media peer');const ws=[...peers].find(ws=>ws.ready);
 if(channel==='wecom'){
  let content={msgtype:'mixed',mixed:{msg_item:[{msgtype:'text',text:{content:`MEDIA_INPUT_${g}`}},{msgtype:'image',image:imageRef},{msgtype:'file',file:fileRef}]},quote:{msgtype:'text',text:{content:`MEDIA_QUOTE_${g}`}}};
  if(mediaCase==='voice')content={msgtype:'voice',voice:{url:mock+'/incoming.ogg',aeskey:key256.toString('base64'),filename:'inbound.ogg',content:`MEDIA_VOICE_${g}`}};
  if(mediaCase==='video')content={msgtype:'video',video:{url:mock+'/incoming.mp4',aeskey:key256.toString('base64'),filename:'inbound.mp4'}};
  if(mediaCase==='quote-image'||mediaCase==='quote-file')content={msgtype:'text',text:{content:`MEDIA_INPUT_${g}`},quote:mediaCase==='quote-image'?{msgtype:'image',image:imageRef}:{msgtype:'file',file:fileRef}};
  if(mediaCase==='quote-voice')content={msgtype:'text',text:{content:''},quote:{msgtype:'voice',voice:{content:`MEDIA_INPUT_${g}\nMEDIA_QUOTE_VOICE_${g}`}}};
  if(mediaCase==='appmsg')content={msgtype:'appmsg',appmsg:{title:`MEDIA_APPMSG_${g}`,file:fileRef,image:imageRef}};
  if(mediaCase==='download404'||mediaCase==='decrypt-invalid')content={msgtype:'file',file:{url:mock+(mediaCase==='download404'?'/missing':'/invalid-cipher'),aeskey:key256.toString('base64'),filename:'invalid.txt'}};
  if(content.msgtype!=='mixed'&&!['quote-voice','download404','decrypt-invalid'].includes(mediaCase))content.text={content:`MEDIA_INPUT_${g}`};
  const frame=wecomFrame(g,content);if(['download404','decrypt-invalid'].includes(mediaCase))frame.headers.req_id=`media-bad-${g}`;inbound.push({generation:g,mediaCase,reqId:frame.headers.req_id,frame});send(ws,frame);return;
 }
 const parts=mediaCase==='download404'?[['image',{image_key:'img-bad'}]]:mediaCase==='missing-image-key'?[['image',{}],['text',{text:`MEDIA_RECOVERY_${g}`}]]:mediaCase==='missing-message-id'?[['image',{image_key:'img-key'}],['text',{text:`MEDIA_RECOVERY_${g}`}]]:[['image',{image_key:'img-key'}],['file',{file_key:'file-key',file_name:'inbound.txt',size:fileIn.length}]];
 for(const[type,content]of parts){const n=++sequence,event={schema:'2.0',header:{event_id:`media-event-${n}`,event_type:'im.message.receive_v1'},event:{sender:{sender_id:{open_id:'human'},sender_type:'user'},message:{message_id:`media-message-${n}`,chat_id:'room',chat_type:'p2p',message_type:type,content:JSON.stringify(content)}}};if(mediaCase==='missing-message-id'&&type==='image')delete event.event.message.message_id;const frame=feishuFrame(event,n);inbound.push({generation:g,mediaCase,type,event,rawBytesHex:frame.toString('hex')});send(ws,frame);}
}
async function round(g){
 const before=models.length;await deliver(g);
 if(channel==='wecom'&&['download404','decrypt-invalid'].includes(mediaCase)){
  await wait(()=>http.some(row=>row.generation===g&&(row.path==='/missing'||row.path==='/invalid-cipher')),'actual failed media resource');
  // Fixed TS skips a failed media message. A later valid ordinary message must
  // reach the real model; the consumer checks exactly this observed model delta.
  const ws=[...peers].find(ws=>ws.ready),frame=wecomFrame(g,{msgtype:'text',text:{content:`MEDIA_INPUT_${g}\nMEDIA_RECOVERY_${g}`}});frame.body.msgid+='-recovery';frame.headers.req_id=`media-in-${g}`;inbound.push({generation:g,recovery:true,frame});send(ws,frame);
 }
 await wait(async()=>{await continueAttachmentPermission(g);return finalSent(g);},'real media final '+g);await wait(()=>capturePersistedContext(g),'persisted media context '+g);await wait(()=>mediaSent(g),'actual upload success or explicit failure receipt '+g);
 const modelEnd=models.length,actual=models.slice(before,modelEnd).flatMap((row,offset)=>row.generation===g&&row.body.stream?[{row,index:before+offset}]:[]);assert(actual.length,'current media round reaches the real model');
 const selectedUsers=[];
 for(const {row,index}of actual){
  const selected=channel==='feishu'?selectLatestModelUser(row.body.messages):selectModelRoundInput(row.body.messages,`MEDIA_INPUT_${g}`);assert(selected,'model contains this round\'s actual user input');
  if(channel!=='feishu')assertRoundMediaContext(JSON.stringify(selected.message),g,'model user input');
  selectedUsers.push(selected.message);
  modelRoundInputs.push({generation:g,modelIndex:index,userMessageIndex:selected.index,modelWindow:{start:before,end:modelEnd},...(channel==='feishu'?{binding:'latest-user+inbound-message-id'}:{marker:`MEDIA_INPUT_${g}`})});
 }
 if(channel==='feishu'){const bound=feishuMediaBindings(selectedUsers,g);assert(bound.complete,'this round\'s model users contain every expected media input');assert(selectedUsers.every((_,index)=>bound.bindings.some(binding=>binding.userIndex===index)),'each selected model user belongs to a current media or recovery input');steps.push({event:'current-model-media-bindings',generation:g,modelWindow:{start:before,end:modelEnd},bindings:bound.bindings.map(binding=>({...binding,modelIndex:actual[binding.userIndex].index}))});}
 if(channel==='wecom'&&['download404','decrypt-invalid'].includes(mediaCase)){assert.equal(models.slice(before).filter(row=>row.body.stream).length,1,'failed media did not start an extra model turn');}
 steps.push({event:'media-case-complete',generation:g,mediaCase,models:actual.length,time:Date.now()});
}

function feishuMediaBindings(users,g){
 const texts=users.map(user=>JSON.stringify(user)),sources=inbound.flatMap((row,index)=>row.generation===g&&!row.permissionAnswer&&row.event?[{row,index}]:[]),bindings=[];
 if(['image-file','upload-failure'].includes(mediaCase)){
  const media=sources.filter(({row})=>row.type==='image'||row.type==='file');
  for(const {row,index}of media){
   const messageId=row.event.event.message.message_id,bytes=row.type==='image'?PNG:fileIn.toString();
   for(let userIndex=0;userIndex<texts.length;userIndex++)if(texts[userIndex].includes('/'+messageId+'/')&&texts[userIndex].includes(bytes))bindings.push({inboundIndex:index,userIndex,messageId,mediaType:row.type});
  }
  return {complete:media.length===2&&media.every(({index})=>bindings.some(binding=>binding.inboundIndex===index)),bindings};
 }
 const recovery=sources.find(({row})=>row.type==='text');
 for(let userIndex=0;userIndex<texts.length;userIndex++){
  if(recovery&&texts[userIndex].includes(`MEDIA_RECOVERY_${g}`))bindings.push({inboundIndex:recovery.index,userIndex,recoveryMarker:`MEDIA_RECOVERY_${g}`});
  else if(texts[userIndex].includes('Attachment diagnostics'))bindings.push({inboundIndex:sources.find(({row})=>row.type==='image')?.index,userIndex,diagnostic:true});
 }
 return {complete:mediaCase==='download404'?bindings.some(binding=>binding.diagnostic):bindings.some(binding=>binding.recoveryMarker),bindings};
}

function assertRoundMediaContext(text,g,label){
 assert(text.includes(`MEDIA_INPUT_${g}`),`${label} carries the current round marker`);
 if(['image-file','upload-failure','quote-image','appmsg'].includes(mediaCase))assert(text.includes(PNG),`${label} contains this round's inbound image`);
 if(['image-file','upload-failure','quote-file','appmsg','plaintext-file'].includes(mediaCase))assert(text.includes(fileIn.toString()),`${label} contains this round's inbound file`);
 if(channel==='weixin'&&['image-file','upload-failure','voice'].includes(mediaCase)||channel==='wecom'&&mediaCase==='voice')assert(text.includes(`MEDIA_VOICE_${g}`),`${label} contains this round's voice transcript`);
 if(mediaCase==='quote-voice')assert(text.includes(`MEDIA_QUOTE_VOICE_${g}`),`${label} contains this round's quoted voice`);
 if(channel==='wecom'&&['download404','decrypt-invalid'].includes(mediaCase))assert(text.includes(`MEDIA_RECOVERY_${g}`),`${label} identifies the recovery input`);
 if(channel==='feishu'&&mediaCase==='download404')assert(text.includes('Attachment diagnostics'),`${label} contains this round's failed attachment diagnostic`);
 if(channel==='weixin'&&mediaCase==='video-diagnostic')assert(text.includes('视频'),`${label} contains this round's video diagnostic`);
}

let rpcSequence=0;
async function rpc(method,params){
 const id=`media-history-${++rpcSequence}`,request={type:'request',id,method,params};send(control,request);
 await wait(()=>frames.some(frame=>frame.type==='response'&&frame.id===id),method);
 const frameIndex=frames.findIndex(frame=>frame.type==='response'&&frame.id===id),response=frames[frameIndex];
 assert.equal(response.ok,true,`${method}: ${JSON.stringify(response.error)}`);
 return {request,frameIndex,response};
}
const permissionAnswers=new Map(),answeredPromptIndices=new Set();
async function continueAttachmentPermission(g){
 if(channel==='wecom')return; // WeCom's MEDIA output uses its production deliverable pipeline.
 for(const projectKey of [home,project]){
  const catalog=await rpc('list_sessions',{projectKey,limit:100});assert(Array.isArray(catalog.response.result?.sessions));
  assert(!catalog.response.result.nextCursor,'isolated media catalog fits one page');
  for(const session of catalog.response.result.sessions){
   const sessionKey=session.sessionKey??session.sessionId;if(typeof sessionKey!=='string'||!sessionKey.startsWith(channel+':'))continue;
   const pending=await rpc('permission_list',{projectKey,sessionKey});assert(Array.isArray(pending.response.result?.requests));
   for(const request of pending.response.result.requests){
    const previous=permissionAnswers.get(request.requestId);
    const retryIndex=previous?http.findIndex((row,index)=>index>=previous.httpLength&&row.generation===g&&!row.path?.startsWith('/v1/')&&JSON.stringify(row.body).includes('权限提示发送中，请稍候')):-1;
    if(previous&&retryIndex<0)continue;
    assert(!previous||previous.attempts<3,'bounded retry of an explicitly premature permission answer');
    assert.equal(request.toolName,'send_attachment','only the fixture attachment permission may be answered');
    const payload=request.payload?.payload??request.payload,filePath=payload?.file_path;
    assert([outputFile,outputImage].includes(filePath),'permission is bound to an exact fixture output path');
    const promptIndex=previous?.promptIndex??http.findIndex((row,index)=>!answeredPromptIndices.has(index)&&row.generation===g&&!row.path?.startsWith('/v1/')&&JSON.stringify(row.body).includes('回复 1 允许一次')&&JSON.stringify(row.body).includes(filePath));
    if(promptIndex<0)continue; // Wait for the real channel prompt to be delivered.
    const n=++sequence;
    if(channel==='weixin'){
     const frame={from_user_id:'chat',context_token:`media-context-${g}`,message_type:1,message_id:`media-permission-${g}-${n}`,item_list:[{type:1,text_item:{text:'1'}}]};
     inbound.push({generation:g,permissionAnswer:true,decision:'allow_once',frame});pollQueue.push(frame);
    }else{
     const event={schema:'2.0',header:{event_id:`media-permission-${n}`,event_type:'im.message.receive_v1'},event:{sender:{sender_id:{open_id:'human'},sender_type:'user'},message:{message_id:`media-permission-${n}`,chat_id:'room',chat_type:'p2p',message_type:'text',content:JSON.stringify({text:'1'})}}};
     const frame=feishuFrame(event,n),peer=[...peers].find(ws=>ws.ready);assert(peer,'actual permission channel connected');
     inbound.push({generation:g,permissionAnswer:true,decision:'allow_once',event,rawBytesHex:frame.toString('hex')});send(peer,frame);
    }
    permissionAnswers.set(request.requestId,{httpLength:http.length,promptIndex,attempts:(previous?.attempts??0)+1});answeredPromptIndices.add(promptIndex);
    permissionContinuations.push({generation:g,projectKey,sessionKey,catalog,pending,requestId:request.requestId,filePath,promptHttpIndex:promptIndex,...(retryIndex>=0?{retryAfterHttpIndex:retryIndex}:{}),text:'1',decision:'allow_once',inboundIndex:inbound.length-1});
    return; // The production helper delivers the next queued prompt after this answer.
   }
  }
 }
}
async function capturePersistedContext(g){
 // Read the disk-backed catalog and history over the public API. A live stream
 // or guessed session key is insufficient evidence of persisted IM context.
 const matches=[];
 for(const projectKey of [home,project]){
  let cursor;const seen=new Set();
  do{
   const receipt=await rpc('list_sessions',{projectKey,limit:100,...(cursor?{cursor}:{})});
   const listIndex=persistedSessions.push({generation:g,...receipt})-1,result=receipt.response.result;
   assert(Array.isArray(result?.sessions),'actual durable session list');
   for(const entry of result.sessions){
    const sessionKey=entry.sessionKey??entry.sessionId;
    if(typeof sessionKey!=='string'||!sessionKey.startsWith(channel+':'))continue;
    const history=await rpc('read_session_messages',{projectKey,sessionKey,limit:0});
    const messageIndex=persistedMessages.push({generation:g,listIndex,catalogSession:entry,...history})-1;
    const value=history.response.result;assert(Array.isArray(value?.messages),'actual persisted messages');
    assert.equal(value.session?.sessionKey,sessionKey,'history identity comes from the catalog');
    const previous=g>0?roundContexts.find(row=>row.generation===g-1):null,previousTurnIds=previous?.turnIds??[];
    const marked=channel==='feishu'?null:selectPersistedRound(value.messages,`MEDIA_INPUT_${g}`,`MEDIA_OUTPUT_${g}`);
    const selected=channel==='feishu'?selectPersistedOutputTurns(value.messages,`MEDIA_OUTPUT_${g}`,previousTurnIds):marked?[marked]:[];if(!selected.length)continue;
    const currentUsers=selected.flatMap(turn=>turn.users),currentUserIndices=selected.flatMap(turn=>turn.userIndices);
    const mediaBindings=channel==='feishu'?feishuMediaBindings(currentUsers,g):null;
    // Separate media events may finish in different real turns. Wait for all
    // current inputs; never fill a missing attachment from older history.
    if(mediaBindings&&!mediaBindings.complete)continue;
    assert.equal(projectKey,home,'unbound channel history belongs to PILOT_HOME, not OS HOME or startup cwd');
    assert.equal(value.session?.cwd,home,'persisted transcript records the actual default project');
    if(channel!=='feishu')assertRoundMediaContext(JSON.stringify(currentUsers),g,'persisted current-turn user input');
    const turnIds=selected.map(turn=>turn.turnId);
    if(g>0){
     assert(previous,'previous round was captured');
     assert.equal(projectKey,previous.projectKey);assert.equal(sessionKey,previous.sessionKey,'reload keeps the same channel session');
     assert(turnIds.every(turnId=>!previousTurnIds.includes(turnId)),'new media inputs own new durable turns');
     const earlier=channel==='feishu'?selectPersistedOutputTurns(value.messages,`MEDIA_OUTPUT_${g-1}`):[selectPersistedRound(value.messages,`MEDIA_INPUT_${g-1}`,`MEDIA_OUTPUT_${g-1}`)].filter(Boolean);
     assert.deepEqual(earlier.map(turn=>turn.turnId),previousTurnIds,'all previous media turns remain in durable history');
    }
    matches.push({generation:g,projectKey,sessionKey,listIndex,messageIndex,turnId:turnIds.length===1?turnIds[0]:null,turnIds,userIndices:currentUserIndices,anchorIndices:selected.flatMap(turn=>turn.anchorIndices??turn.outputIndices),outputIndices:selected.flatMap(turn=>turn.outputIndices),binding:channel==='feishu'?'output-turn+inbound-message-id':'input-marker+turn-id',...(mediaBindings?{mediaBindings:mediaBindings.bindings.map(binding=>({...binding,userMessageIndex:currentUserIndices[binding.userIndex]}))}:{})});
   }
   cursor=result.nextCursor;if(cursor){assert(!seen.has(cursor),'session cursor progresses');seen.add(cursor);assert(seen.size<20,'bounded session catalog');}
  }while(cursor);
 }
 if(!matches.length)return false;
 assert.equal(matches.length,1,'one durable channel session contains the current round');roundContexts.push(matches[0]);
 steps.push({event:'persisted-channel-context',generation:g,matches,time:Date.now()});return true;
}

try{
 if(engine==='ts')await writeFile(join(out,'ts-redirects.jsonl'),'',{flag:'wx'});
const preload=fileURLToPath(new URL('./media-ts-loopback.mjs',import.meta.url)),executable=engine==='ts'?process.execPath:gateway,args=engine==='ts'?['--import',preload,'--import',join(resolve(baseline),'node_modules/tsx/dist/loader.mjs'),join(resolve(baseline),'src/cli/pilotdeck.ts'),'server']:['--native','--home',home,'--project',home,'--listen',`127.0.0.1:${controlPort}`,'--token-file',tokenPath,'--sidecar',sidecar,'--drain-ms','2000'];child=spawn(executable,args,{cwd:project,env:{PATH:process.env.PATH,LD_LIBRARY_PATH:process.env.LD_LIBRARY_PATH,LANG:'C.UTF-8',HOME:osHome,PILOT_HOME:home,TMPDIR:tmp,PILOTDECK_GATEWAY_HOST:'127.0.0.1',PILOTDECK_GATEWAY_PORT:String(controlPort),PILOTDECK_GATEWAY_TOKEN:'media-fixture-token',PILOTDECK_GATEWAY_TOKEN_PATH:tokenPath,PILOTDECK_SKIP_BROWSER_OPEN:'1',DATABASE_PATH:join(home,'auth.db'),WORKSPACES_ROOT:join(layout?.root??out,'workspaces'),PDRS_TEST_CHANNEL_KIND:channel,PDRS_TEST_CHANNEL_LOOPBACK_ORIGIN:mock,PDRS_TEST_CHANNEL_REDIRECT_RECEIPT:join(out,'ts-redirects.jsonl')},stdio:['ignore','pipe','pipe']});child.once('error',error=>{child.spawnError=error;});const stopped=new Promise(r=>child.once('exit',(code,signal)=>r({code,signal})));child.stdout.on('data',b=>stdout+=b);child.stderr.on('data',b=>stderr+=b);
 await wait(()=>channel==='weixin'?http.some(row=>row.path.endsWith('/getupdates')):[...peers].some(ws=>ws.ready),'actual channel startup');
 control=new WebSocket(`ws://127.0.0.1:${controlPort}/ws`);control.on('error',error=>{http.push({fixtureError:String(error)});});control.on('message',data=>frames.push(JSON.parse(data)));await wait(()=>control.readyState===1,'control open');send(control,{type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'media-fixture',token:'media-fixture-token'});await wait(()=>frames.some(frame=>frame.type==='hello_ok'),'control authenticated');await round(0);
 async function reload(on,id){await writeFile(configPath,config(on));send(control,{type:'request',id,method:'reload_config',params:{}});await wait(()=>frames.some(frame=>frame.type==='response'&&frame.id===id),'reload response '+id);assert.equal(frames.find(frame=>frame.type==='response'&&frame.id===id).ok,true);steps.push({event:'reload',enabled:on,time:Date.now()});}
 await reload(false,'media-disable');await wait(()=>peers.size===0,'disabled socket closed');generation=1;await reload(true,'media-enable');await round(1);control.close();child.kill('SIGTERM');exit=await Promise.race([stopped,delay(12000).then(()=>({code:null,signal:'timeout'}))]);assert.equal(exit.code,0);await wait(()=>peers.size===0,'stopped channel socket');steps.push({event:'actual-process-stop',exit,time:Date.now()});
}catch(error){failure=String(error.stack??error);process.exitCode=1;}finally{control?.terminate();if(child&&child.exitCode===null)child.kill('SIGTERM');for(const ws of peers)ws.terminate();for(const socket of sockets)socket.destroy();wss.close();await new Promise(r=>service.close(r));
 const saved=[];async function walk(dir){for(const entry of await readdir(dir,{withFileTypes:true}).catch(()=>[])){const file=join(dir,entry.name);if(entry.isDirectory())await walk(file);else if(entry.isFile()){const raw=await readFile(file);saved.push({path:file,sha256:sha(raw),bytes:raw.length,matchesInboundFile:raw.equals(fileIn),matchesInboundImage:raw.equals(image)});}}}await walk(join(home,'im-attachments'));await walk(join(osHome,'.pilotdeck','im-attachments'));
 await writeFile(join(out,'evidence.json'),JSON.stringify({schemaVersion:2,profile:'native-media-v2',channel,mediaCase,failureStage,engine,layout:layout?{...layout,path:resolve(layoutArg),sha256:await digest(layoutArg)}:null,fixedTS,sourceManifestSha256:process.env.PDRS_SOURCE_MANIFEST_SHA256,actualExecutable:{path:engine==='ts'?process.execPath:gateway,sha256:await digest(engine==='ts'?process.execPath:gateway)},tsPreload:engine==='ts'?{path:fileURLToPath(new URL('./media-ts-loopback.mjs',import.meta.url)),sha256:await digest(new URL('./media-ts-loopback.mjs',import.meta.url))}:null,tsEntrypoint:engine==='ts'?{path:join(resolve(baseline),'src/cli/pilotdeck.ts'),sha256:await digest(join(resolve(baseline),'src/cli/pilotdeck.ts'))}:null,tsLoader:engine==='ts'?{path:join(resolve(baseline),'node_modules/tsx/dist/loader.mjs'),sha256:await digest(join(resolve(baseline),'node_modules/tsx/dist/loader.mjs'))}:null,runtime:{node:process.version,osHome,pilotHome:home,home,project,defaultProject:home,cwd:project,tmp,mockPort:service.address()?.port??Number(new URL(mock).port),controlPort},status:failure?(engine==='ts'&&/not installed;|Cannot find (?:package|module)|ERR_MODULE_NOT_FOUND|unsupported .* fixture TLS transport/.test(stderr)?'BLOCKED':'FAIL'):'CAPTURED',gatewaySha256:engine==='native'?await digest(gateway):await digest(gateway).catch(()=>null),sidecarSha256:engine==='native'?await digest(sidecar):await digest(sidecar).catch(()=>null),fixtureSha256:await digest(new URL(import.meta.url)),contextHelperSha256:await digest(new URL('./reference-round-context.mjs',import.meta.url)),scope:'actual production media subtype selected by mediaCase, two rounds and public reload/stop; independent registry determines coverage',formalAcceptance:false,inputMaterials:{roundMarkers:channel==='feishu'?[]:['MEDIA_INPUT_0','MEDIA_INPUT_1'],outputMarkers:['MEDIA_OUTPUT_0','MEDIA_OUTPUT_1'],voiceHex:voice.toString('hex'),videoHex:video.toString('hex'),outgoingByType:Object.fromEntries(['file','image','voice','video'].map(type=>[type,{rawBytesHex:expectedOutgoing(type).toString('hex'),sha256:sha(expectedOutgoing(type))}])),imageBase64:PNG,fileIn:fileIn.toString(),fileOut:fileOut.toString(),key128Hex:key128.toString('hex'),key256Base64:key256.toString('base64'),ciphertextImageHex:ciphertextImage.toString('hex'),ciphertextFileHex:ciphertextFile.toString('hex')},steps,models,modelRoundInputs,roundContexts,http,wire,inbound,persistedSessions,persistedMessages,permissionContinuations,mediaChecks,uploadParameters:[...uploadParameters],savedAttachments:saved,controlFrames:frames,stdout,stderr,exit,failure},null,2)+'\n',{flag:'wx'});
}
