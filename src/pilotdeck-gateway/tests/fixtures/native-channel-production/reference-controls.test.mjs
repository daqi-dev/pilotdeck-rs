// Authored controls only. Run solely in an explicitly authorized test environment.
// These exercise the transport seam and prerequisite refusal, never a fake SDK.
import assert from 'node:assert/strict';
import test from 'node:test';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {mkdtemp,readFile,writeFile,stat,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {selectModelRoundInput,selectPersistedRound,selectLatestModelUser,selectPersistedOutputTurns} from './reference-round-context.mjs';

const preload=fileURLToPath(new URL('./media-ts-loopback.mjs',import.meta.url));
function childResult(args,env){return new Promise((resolve,reject)=>{
 const child=spawn(process.execPath,args,{env:{PATH:process.env.PATH,...env},stdio:['ignore','pipe','pipe']});let stdout='',stderr='';
 const timer=setTimeout(()=>{child.kill('SIGKILL');reject(Error('bounded reference control timed out'));},10000);
 child.on('error',error=>{clearTimeout(timer);reject(error);});
 child.stdout.on('data',data=>stdout+=data);child.stderr.on('data',data=>stderr+=data);
 child.on('exit',(code,signal)=>{clearTimeout(timer);resolve({code,signal,stdout,stderr});});
});}
async function loopback(t,channel){
 const dir=await mkdtemp(join(tmpdir(),'reference-channel-control-')),requests=[];
 const server=createServer(async(req,res)=>{const chunks=[];for await(const chunk of req)chunks.push(chunk);requests.push({url:req.url,method:req.method,headers:req.headers,hex:Buffer.concat(chunks).toString('hex')});res.writeHead(200,{'content-type':'application/json'});res.end('{"realTransport":true}');});
 await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
 t.after(async()=>{server.closeAllConnections();await new Promise(resolve=>server.close(resolve));await rm(dir,{recursive:true,force:true});});
 const receipt=join(dir,'redirects.jsonl'),origin='http://127.0.0.1:'+server.address().port;
 return {dir,requests,receipt,run:source=>childResult(['--import',preload,'--input-type=module','--eval',source],{PDRS_TEST_CHANNEL_KIND:channel,PDRS_TEST_CHANNEL_LOOPBACK_ORIGIN:origin,PDRS_TEST_CHANNEL_REDIRECT_RECEIPT:receipt})};
}

test('real HTTPS and fetch Request preserve payload, query and authentication',async t=>{
 const fixture=await loopback(t,'feishu');
 const result=await fixture.run(`
  import https from 'node:https';
  import assert from 'node:assert/strict';
  assert.equal(typeof globalThis.require,'undefined');
  await new Promise((resolve,reject)=>{const request=https.request('https://open.feishu.cn/ignored',{method:'POST',path:'/open-apis/upload?key=a%2Fb',headers:{authorization:'Bearer fixture-only','content-type':'application/octet-stream'}},response=>{response.resume();response.once('end',resolve);});request.once('error',reject);request.end(Buffer.from([0,255,1,13,10]));});
  const request=new Request('https://open.feishu.cn/open-apis/messages?key=x%2By',{method:'POST',headers:{authorization:'Bearer fixture-only'},body:'{"text":"same input"}'});
  assert.equal((await (await fetch(request)).json()).realTransport,true);
 `);
 assert.equal(result.code,0,result.stderr);assert.equal(fixture.requests.length,2);
 assert.deepEqual(fixture.requests.map(({url,method,hex})=>({url,method,hex})),[
  {url:'/open-apis/upload?key=a%2Fb',method:'POST',hex:'00ff010d0a'},
  {url:'/open-apis/messages?key=x%2By',method:'POST',hex:Buffer.from('{"text":"same input"}').toString('hex')},
 ]);
 for(const request of fixture.requests)assert.equal(request.headers.authorization,'Bearer fixture-only');
 const receipts=(await readFile(fixture.receipt,'utf8')).trim().split('\n').map(JSON.parse);
 assert.deepEqual(receipts.map(row=>row.transport),['https.request','fetch']);
 assert(receipts.every(row=>row.upstream==='https://open.feishu.cn'&&row.channel==='feishu'));
});

test('QQ origin substitution preserves production token and gateway paths',async t=>{
 const fixture=await loopback(t,'qq');
 const result=await fixture.run(`
  await fetch('https://bots.qq.com/app/getAppAccessToken',{method:'POST',body:'{"appId":"mock-app","clientSecret":"mock-secret"}'});
  await fetch('https://api.sgroup.qq.com/gateway');
 `);
 assert.equal(result.code,0,result.stderr);
 assert.deepEqual(fixture.requests.map(row=>row.url),['/app/getAppAccessToken','/gateway']);
});

test('Discord origin substitution preserves the versioned API root and reply bytes',async t=>{
 const fixture=await loopback(t,'discord');
 const result=await fixture.run(`
  import https from 'node:https';
  const headers={authorization:'Bot fixture-only','content-type':'application/json'};
  await new Promise((resolve,reject)=>{const request=https.request('https://discord.com/api/v10/gateway/bot',{headers},response=>{response.resume();response.once('end',resolve);});request.once('error',reject);request.end();});
  await fetch('https://discord.com/api/v10/users/@me',{headers});
  await fetch('https://discord.com/api/v10/channels/42',{headers});
  await fetch('https://discord.com/api/v10/channels/42/messages',{method:'POST',headers,body:'{"content":"same Discord reply"}'});
  await fetch('https://discord.com/api/v10/channels/42/typing',{method:'POST',headers});
 `);
 assert.equal(result.code,0,result.stderr);
 assert.deepEqual(fixture.requests.map(({url,method})=>({url,method})),[
  {url:'/api/v10/gateway/bot',method:'GET'},
  {url:'/api/v10/users/@me',method:'GET'},
  {url:'/api/v10/channels/42',method:'GET'},
  {url:'/api/v10/channels/42/messages',method:'POST'},
  {url:'/api/v10/channels/42/typing',method:'POST'},
 ]);
 assert.equal(fixture.requests[3].hex,Buffer.from('{"content":"same Discord reply"}').toString('hex'));
 assert(fixture.requests.every(row=>row.headers.authorization==='Bot fixture-only'));
 const receipts=(await readFile(fixture.receipt,'utf8')).trim().split('\n').map(JSON.parse);
 assert.deepEqual(receipts.map(row=>row.path),fixture.requests.map(row=>row.url));
 assert(receipts.every(row=>row.upstream==='https://discord.com'&&row.channel==='discord'));
});

test('cross-channel hosts, credentials, alternate ports and bypass transports fail closed',async t=>{
 const fixture=await loopback(t,'feishu');
 const result=await fixture.run(`
  import assert from 'node:assert/strict';import https from 'node:https';import net from 'node:net';import tls from 'node:tls';
  for(const url of ['https://bots.qq.com/app/getAppAccessToken','https://open.feishu.cn:444/x','https://user:pass@open.feishu.cn/x','http://open.feishu.cn/x'])assert.throws(()=>fetch(url),/unregistered/);
  assert.throws(()=>https.get({hostname:'bots.qq.com',path:'/x'}),/unregistered/);
  assert.throws(()=>net.connect({host:'example.invalid',port:443}),/unregistered/);
  assert.throws(()=>tls.connect({host:'open.feishu.cn',port:443}),/unsupported/);
 `);
 assert.equal(result.code,0,result.stderr);assert.equal(fixture.requests.length,0);
});

test('an unverified source tree cannot manufacture successful reference evidence',async t=>{
 const dir=await mkdtemp(join(tmpdir(),'reference-prerequisite-control-'));
 t.after(()=>rm(dir,{recursive:true,force:true}));
 await writeFile(join(dir,'package.json'),'{"type":"module"}\n');
 for(const [name,extra] of [['media.mjs',['feishu','image-file','ts']],['eight.mjs',['telegram','ts']]]){
  const out=join(dir,name.replace('.mjs',''));
  const result=await childResult([fileURLToPath(new URL('./'+name,import.meta.url)),dir,join(dir,'absent-native'),join(dir,'absent-sidecar'),out,...extra],{});
  assert.equal(result.code,1,result.stderr);
  const evidence=JSON.parse(await readFile(join(out,'evidence.json'),'utf8'));
  assert.equal(evidence.status,'BLOCKED');assert.equal(evidence.formalAcceptance,false);
  assert(evidence.prerequisiteFailures.some(row=>row.kind==='fixed-ts-source'));
  assert.deepEqual(evidence.steps,[]);await assert.rejects(stat(join(out,'home')),error=>error.code==='ENOENT');
 }
});

test('previous-round attachments cannot satisfy the selected model input',()=>{
 const messages=[{role:'user',content:[{type:'text',text:'MEDIA_INPUT_0 old-file'},{type:'image_url',image_url:{url:'data:image/png;base64,old-image'}}]},{role:'assistant',content:'MEDIA_OUTPUT_0'},{role:'user',content:'MEDIA_INPUT_1'}];
 const selected=selectModelRoundInput(messages,'MEDIA_INPUT_1');assert.equal(selected.index,2);
 assert(!JSON.stringify(selected.message).includes('old-file'));assert(!JSON.stringify(selected.message).includes('old-image'));
 assert.equal(selectModelRoundInput(messages.slice(0,2),'MEDIA_INPUT_1'),null);
});

test('persisted round selection requires the current input and output to share a durable turn',()=>{
 const message=(role,text,turnId,extra={})=>({source:'history',kind:'text',role,text,turnId,...extra});
 const messages=[message('user','MEDIA_INPUT_0 old-file','turn-0',{images:[{data:'old-image'}]}),message('assistant','MEDIA_OUTPUT_0','turn-0'),message('user','MEDIA_INPUT_1','turn-1'),message('assistant','MEDIA_OUTPUT_1','turn-0')];
 assert.equal(selectPersistedRound(messages,'MEDIA_INPUT_1','MEDIA_OUTPUT_1'),null,'an output on the previous turn is insufficient');
 messages.push(message('assistant','MEDIA_OUTPUT_1','turn-1'));
 const selected=selectPersistedRound(messages,'MEDIA_INPUT_1','MEDIA_OUTPUT_1');assert.equal(selected.turnId,'turn-1');assert.deepEqual(selected.userIndices,[2]);assert.deepEqual(selected.outputIndices,[4]);
 assert(!JSON.stringify(selected.users).includes('old-file'));assert(!JSON.stringify(selected.users).includes('old-image'));
 assert.equal(selectPersistedRound(messages.map(row=>({...row,source:'live'})),'MEDIA_INPUT_1','MEDIA_OUTPUT_1'),null);
});

test('media-only inputs select the latest model user without inventing a text marker',()=>{
 const messages=[{role:'user',content:'old media-message-1 old-file'},{role:'assistant',content:'MEDIA_OUTPUT_0'},{role:'user',content:[{type:'image_url',image_url:{url:'data:image/png;base64,current-image'}},{type:'text',text:'registered media-message-7/image.png'}]},{role:'tool',content:'old-file'}];
 const selected=selectLatestModelUser(messages);assert.equal(selected.index,2);assert(!JSON.stringify(selected.message).includes('old-file'));
 assert.equal(selectLatestModelUser([{role:'assistant',content:'no user'}]),null);
});

test('separate current media turns retain their own users and cannot borrow an earlier attachment',()=>{
 const message=(role,text,turnId)=>({source:'history',kind:'text',role,text,turnId});
 const messages=[message('user','media-message-1 old-file','old'),message('assistant','MEDIA_OUTPUT_0','old'),message('user','media-message-7 current-image','image'),message('assistant','MEDIA_OUTPUT_1','image'),message('user','media-message-8 current-file','file'),message('assistant','MEDIA_OUTPUT_1','file')];
 const selected=selectPersistedOutputTurns(messages,'MEDIA_OUTPUT_1',['old']);assert.deepEqual(selected.map(row=>row.turnId),['image','file']);assert.deepEqual(selected.flatMap(row=>row.userIndices),[2,4]);
 assert(!JSON.stringify(selected.flatMap(row=>row.users)).includes('old-file'));
 const missingFile=selectPersistedOutputTurns(messages.slice(0,4),'MEDIA_OUTPUT_1',['old']);assert(!JSON.stringify(missingFile.flatMap(row=>row.users)).includes('current-file'));
 assert.throws(()=>selectPersistedOutputTurns([...messages,message('assistant','MEDIA_OUTPUT_1','old')],'MEDIA_OUTPUT_1',['old']),/previous-round/);
});
