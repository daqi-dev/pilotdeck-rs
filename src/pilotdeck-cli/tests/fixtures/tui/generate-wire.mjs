import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { resolve, join } from 'node:path';
import { createServer } from 'node:http';
const [base, output] = process.argv.slice(2);
if (!base || !output || process.argv.length !== 4) throw Error('usage: generate-wire.mjs BASELINE NEW_DIR');
if (process.version !== 'v22.22.0' || process.versions.icu !== '78.2') throw Error('Node/ICU mismatch');
if (execFileSync('git',['-C',base,'rev-parse','HEAD'],{encoding:'utf8'}).trim() !== 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85') throw Error('baseline mismatch');
for (const [name, expected] of [['GatewayWsClient','7df1c5f46704ea8d90d5cb897f5588d91eec2dda39de1cbd92cf3c4317d9fd54'],['RemoteGateway','9c2168d891747e67549a34d0c97df287cd06564902a6430ffb155f83fcc35a28']]) {
 const bytes=await readFile(join(base,`src/gateway/client/${name}.ts`));
 if(createHash('sha256').update(bytes).digest('hex')!==expected)throw Error(`${name} source mismatch`);
}
await mkdir(output);
const require=createRequire(join(resolve(base),'package.json')); const {WebSocketServer}=require('ws');
const {createRemoteGateway}=await import(pathToFileURL(join(resolve(base),'src/gateway/client/RemoteGateway.ts')).href);
const server=createServer(); const wss=new WebSocketServer({server}); const rows=[]; let clientSocket; let streamId;
wss.on('connection',ws=>{clientSocket=ws; ws.on('message',raw=>{
 const frame=JSON.parse(String(raw)); rows.push({direction:'client',frame});
 const reply = f=>{rows.push({direction:'server',frame:f});ws.send(JSON.stringify(f));};
 if(frame.type==='hello') reply({type:'hello_ok',protocolVersion:'1.2',serverVersion:'fixture',serverInfo:{capabilities:[]}});
 else if(frame.method==='submit_turn') {streamId=frame.id; reply({type:'event',id:streamId,seq:0,final:false,event:{type:'permission_request',requestId:'p1',toolName:'bash',payload:{command:'git status'}}});}
 else if(frame.method==='permission_decide') reply({type:'response',id:frame.id,ok:true,result:{delivered:true}});
 else if(frame.method==='abort_turn') {reply({type:'response',id:frame.id,ok:true,result:{ok:true}});reply({type:'event',id:streamId,seq:1,final:true,event:{type:'turn_completed'}});}
 });});
await new Promise(ok=>server.listen(0,'127.0.0.1',ok));
try {
 const gateway=await createRemoteGateway({url:`ws://127.0.0.1:${server.address().port}/ws`,token:'fixture-token',clientName:'cli'});
 const events=[]; const stream=gateway.submitTurn({sessionKey:'fixture-session',channelKey:'tui',projectKey:'/fixture-project',message:'hello',mode:'default'});
 for await(const event of stream){events.push(event); if(event.type==='permission_request'){
   await gateway.permissionDecide({sessionKey:'fixture-session',requestId:'p1',decision:'allow',remember:false});
   await gateway.abortTurn({sessionKey:'fixture-session'});
 }}
 gateway.client.close();
 await new Promise(ok=>clientSocket.once('close',ok));
 await writeFile(join(output,'wire.json'),JSON.stringify({rows,events},null,2)+'\n',{flag:'wx'});
} finally {for (const socket of wss.clients) socket.terminate(); await new Promise(ok=>wss.close(ok)); await new Promise(ok=>server.close(ok));}
