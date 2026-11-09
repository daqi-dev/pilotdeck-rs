import {execFileSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import {readFile,mkdir,writeFile} from 'node:fs/promises';
import {createRequire} from 'node:module';
import {resolve,join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {createServer} from 'node:http';
const [base,out]=process.argv.slice(2);if(!base||!out||process.argv.length!==4)throw Error('usage: BASELINE NEW_DIR');
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw Error('Node/ICU mismatch');
if(execFileSync('git',['-C',base,'rev-parse','HEAD'],{encoding:'utf8'}).trim()!=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85')throw Error('commit mismatch');
for(const [name,sha] of [['GatewayWsClient','7df1c5f46704ea8d90d5cb897f5588d91eec2dda39de1cbd92cf3c4317d9fd54'],['RemoteGateway','9c2168d891747e67549a34d0c97df287cd06564902a6430ffb155f83fcc35a28']])if(createHash('sha256').update(await readFile(join(base,`src/gateway/client/${name}.ts`))).digest('hex')!==sha)throw Error('source mismatch');
await mkdir(out);const require=createRequire(join(resolve(base),'package.json'));const {WebSocketServer}=require('ws');
const {createRemoteGateway}=await import(pathToFileURL(join(resolve(base),'src/gateway/client/RemoteGateway.ts')).href);
const server=createServer(), wss=new WebSocketServer({server}), rows=[];let socket;
wss.on('connection',ws=>{socket=ws;ws.on('message',raw=>{const frame=JSON.parse(String(raw));rows.push({direction:'client',frame});let response;
if(frame.type==='hello')response={type:'hello_ok',serverInfo:{capabilities:[]}};
else if(frame.method==='submit_turn')response={type:'response',id:frame.id,ok:false,error:{code:'invalid_request',message:'fixture admission denied'}};
if(response){rows.push({direction:'server',frame:response});ws.send(JSON.stringify(response));}});});
await new Promise(ok=>server.listen(0,'127.0.0.1',ok));
try {
 const gateway=await createRemoteGateway({url:`ws://127.0.0.1:${server.address().port}/ws`,token:'fixture-token',clientName:'cli'});
 const iterator=gateway.submitTurn({sessionKey:'s',channelKey:'tui',projectKey:'/fixture',message:'hello',mode:'default'})[Symbol.asyncIterator]();
 const wait=iterator.next().then(value=>({settled:true,value}),error=>({settled:true,error:error.message}));
 const observation=await Promise.race([wait,new Promise(ok=>setTimeout(()=>ok({settled:false}),500))]);
 const openAtObservation=socket.readyState===socket.OPEN;
 const closed=new Promise(ok=>socket.once('close',ok));gateway.client.close();await closed;
 const afterCleanup=await wait;
 await writeFile(join(out,'rejection.json'),JSON.stringify({rows,observation,openAtObservation,afterCleanup},null,2)+'\n',{flag:'wx'});
} finally {for(const ws of wss.clients)ws.terminate();await new Promise(ok=>wss.close(ok));await new Promise(ok=>server.close(ok));}
