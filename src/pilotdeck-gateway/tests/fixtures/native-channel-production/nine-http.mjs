// Non-frozen actual executable probes for HTTP-facing channels.
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {createServer} from 'node:http';
import {createServer as reserveServer} from 'node:net';
import {mkdir,writeFile,readFile,readdir} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {createHash,createHmac} from 'node:crypto';
import {createRequire} from 'node:module';
import {fileURLToPath} from 'node:url';
const [binary,sidecar,outArg]=process.argv.slice(2),out=resolve(outArg),kind=process.env.PDRS_CHANNEL??'apiServer';
assert(['apiServer','webhook'].includes(kind));await mkdir(out,{recursive:true});const rows=[],sockets=new Set();let child,ws,stderr='',stdout='',generation=0;
const deadline=Date.now()+90000,delay=ms=>new Promise(r=>setTimeout(r,ms)),wait=async(fn,label)=>{while(!await fn()){assert(Date.now()<deadline,label);await delay(20);}};
const require=createRequire(join(process.env.PDRS_BASELINE,'package.json')),WebSocket=require('ws');
const provider=createServer(async(req,res)=>{let raw='';for await(const b of req)raw+=b;const body=JSON.parse(raw);rows.push({provider:body});res.setHeader('content-type',body.stream?'text/event-stream':'application/json');if(!body.stream){res.end(JSON.stringify({choices:[{message:{role:'assistant',content:'{"title":"HTTP channel"}'},finish_reason:'stop'}]}));return;}res.end('data: '+JSON.stringify({id:'mock',choices:[{index:0,delta:{role:'assistant',content:'http-channel-output-'+generation},finish_reason:null}]})+'\n\ndata: '+JSON.stringify({id:'mock',choices:[{index:0,delta:{},finish_reason:'stop'}]})+'\n\ndata: [DONE]\n\n');});
provider.on('connection',s=>{sockets.add(s);s.on('error',()=>{});s.on('close',()=>sockets.delete(s));});await new Promise(r=>provider.listen(0,'127.0.0.1',r));
const reservation=reserveServer();await new Promise(r=>reservation.listen(0,'127.0.0.1',r));const port=reservation.address().port;await new Promise(r=>reservation.close(r));
const home=join(out,'home'),project=join(out,'project');await mkdir(home,{recursive:true});await mkdir(project,{recursive:true});
const base='schemaVersion: 1\nextension: {builtinPluginsEnabled: {browser-use: false, funasr: false, windows-skills: false}}\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:'+provider.address().port+'/v1\n      apiKey: mock-only\n      models:\n        mock: {}\n';
const config=enabled=>base+'adapters:\n  '+kind+':\n    enabled: '+enabled+'\n'+(kind==='apiServer'?'    apiKey: mock-channel-key\n':'')+'    extra:\n      port: '+port+'\n'+(kind==='webhook'?'      secret: mock-signing-key\n      routes:\n        main: {deliver: log}\n':'');
const path=join(home,'pilotdeck.yaml'),token=join(home,'token');await writeFile(path,config(true),{flag:'wx'});await writeFile(token,'channel-lifecycle-token\n',{flag:'wx'});
const origin='http://127.0.0.1:'+port,headers={authorization:'Bearer mock-channel-key',connection:'close'};
async function healthy(){try{return (await fetch(origin+'/health',{headers,signal:AbortSignal.timeout(500)})).ok;}catch{return false;}}
async function ingress(){
if(kind==='webhook'){const wrongMethod=await fetch(origin+'/webhooks/main',{method:'GET'});assert.equal(wrongMethod.status,404);rows.push({dispatch:{path:'/webhooks/main',method:'GET',status:wrongMethod.status,body:await wrongMethod.text()}});const unknown=await fetch(origin+'/webhooks/unknown',{method:'POST',headers:{'content-type':'application/json'},body:'{}'});assert.equal(unknown.status,404);rows.push({dispatch:{path:'/webhooks/unknown',method:'POST',status:unknown.status,body:await unknown.text()}});}
const text='http-channel-input-'+generation;const body=JSON.stringify(kind==='apiServer'?{model:'fixture/mock',messages:[{role:'user',content:text}],stream:false}:{text});
const h={...headers,'content-type':'application/json',...(kind==='apiServer'?{'x-hermes-session-id':'http-channel-room'}:{'x-delivery-id':'event-'+generation,'x-hub-signature-256':'sha256='+createHmac('sha256','mock-signing-key').update(body).digest('hex')})};
const response=await fetch(origin+(kind==='apiServer'?'/v1/chat/completions':'/webhooks/main'),{method:'POST',headers:h,body,signal:AbortSignal.timeout(30000)});const answer=await response.text();rows.push({inbound:{generation,status:response.status,body:answer}});assert.equal(response.status,200,answer);
if(kind==='apiServer')assert(answer.includes('http-channel-output-'+generation),'actual API output response');else await wait(()=>stdout.includes('webhook: response for ')&&stdout.includes('http-channel-output-'+generation),'actual webhook log deliverer');
assert(rows.some(r=>r.provider?.stream&&JSON.stringify(r.provider).includes(text)),'provider consumed actual channel input');
}
try{
child=spawn(binary,['--native','--home',home,'--project',project,'--listen','127.0.0.1:0','--token-file',token,'--sidecar',sidecar,'--drain-ms','1500'],{cwd:project,env:{PATH:process.env.PATH,HOME:home,PILOT_HOME:home,LANG:'en_US.UTF-8'},stdio:['ignore','pipe','pipe']});const exit=new Promise(r=>child.once('exit',(code,signal)=>r({code,signal})));child.stdout.on('data',b=>stdout+=b);child.stderr.on('data',b=>stderr+=b);
await wait(()=>/(?:^|\n)native listening (\S+:\d+)\r?\n/.test(stderr),'Gateway readiness');await wait(healthy,'configured channel HTTP listener');
const address=/(?:^|\n)native listening (\S+:\d+)\r?\n/.exec(stderr)[1],frames=[];ws=new WebSocket('ws://'+address+'/ws');ws.on('message',b=>frames.push(JSON.parse(b)));await wait(()=>ws.readyState===1,'control ws');ws.send(JSON.stringify({type:'hello',protocolVersion:'1.2',clientName:'ui',clientVersion:'http-channel-lifecycle',token:'channel-lifecycle-token'}));await wait(()=>frames.some(f=>f.type==='hello_ok'),'control auth');
await ingress();
async function reload(enabled,id){await writeFile(path,config(enabled));ws.send(JSON.stringify({type:'request',id,method:'reload_config',params:{}}));await wait(()=>frames.some(f=>f.type==='response'&&f.id===id),'reload RPC');assert.equal(frames.find(f=>f.id===id).ok,true);}
await reload(false,'disable-http');await wait(async()=>!await healthy(),'public reload closes HTTP channel listener');const before=rows.filter(r=>r.provider?.stream).length;await delay(300);assert.equal(rows.filter(r=>r.provider?.stream).length,before);
generation=1;await reload(true,'enable-http');await wait(healthy,'public reload reconstructs channel');await ingress();
ws.close();child.kill('SIGTERM');const stopped=await Promise.race([exit,delay(10000).then(()=>{throw Error('shutdown timeout');})]);assert.equal(stopped.code,0);assert.equal(await healthy(),false,'shutdown closes channel listener');const count=rows.length;await delay(250);assert.equal(rows.length,count,'no work after shutdown');
async function transcripts(path){let all='';for(const row of await readdir(path,{withFileTypes:true})){const p=join(path,row.name);if(row.isDirectory())all+=await transcripts(p);else if(row.name.endsWith('.jsonl'))all+=await readFile(p,'utf8');}return all;}
const log=await transcripts(join(home,'projects'));for(const marker of ['http-channel-output-0','http-channel-output-1'])assert(log.includes(marker),'durable '+marker);
rows.push({result:'PASS',scope:kind+' actual Server/A7/output/public reload/shutdown',formalAcceptance:false,environment:process.versions,portReservation:'bind/drop; collision fails readiness'});
}finally{
ws?.terminate();if(child&&child.exitCode===null)child.kill('SIGTERM');for(const s of sockets)s.destroy();provider.close();
const sha=async p=>createHash('sha256').update(await readFile(p)).digest('hex');await writeFile(join(out,'evidence.json'),JSON.stringify({probe:{path:fileURLToPath(import.meta.url),sha256:await sha(fileURLToPath(import.meta.url)),channel:kind},binary:{path:binary,sha256:await sha(binary)},sidecar:{path:sidecar,sha256:await sha(sidecar)},rows,stdout,stderr},null,2)+String.fromCharCode(10),{flag:'wx'});
}
