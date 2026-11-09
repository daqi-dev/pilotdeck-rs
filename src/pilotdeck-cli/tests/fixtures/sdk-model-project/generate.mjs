// Read-only fixed TS baseline; loopback mock, fake token and exclusive evidence.
import { spawn, execFileSync } from 'node:child_process';
import { mkdir, realpath, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { createRequire } from 'node:module';
import http from 'node:http';
import { once } from 'node:events';
const [checkoutArg, outputArg] = process.argv.slice(2);
if (!checkoutArg || !outputArg || process.argv.length !== 4) throw Error('usage: node capture-ts.mjs <ts-checkout> <new-evidence-directory>');
if (process.version !== 'v22.22.0') throw Error('Node v22.22.0 required');
const checkout = await realpath(checkoutArg);
const sha = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
if (execFileSync('git',['-C',checkout,'rev-parse','HEAD'],{encoding:'utf8'}).trim() !== sha) throw Error('wrong TS checkout');
if (execFileSync('git',['-C',checkout,'status','--porcelain','--untracked-files=no'],{encoding:'utf8'}).trim()) throw Error('TS tracked files must be clean');
const out = resolve(outputArg); await mkdir(out);
const require = createRequire(join(checkout, 'package.json'));
const { WebSocketServer } = require('ws');
const token = 'fixture-token-no-project';
const cases = [
 {id:'run', argv:['run','fixture prompt','--model','fixture/model']},
 {id:'resume', argv:['resume','fixture-session','fixture prompt','--model','fixture/model']},
 {id:'run-empty-prompt', argv:['run','--model','fixture/model']},
 {id:'resume-missing-id', argv:['resume','--model','fixture/model']},
];
for (const c of cases) {
 const root=join(out,c.id); await mkdir(root); const home=join(root,'home'), pilot=join(root,'pilot'),project=join(root,'project');
 for(const p of [home,pilot,project]) await mkdir(p);
 const rows=[]; let sockets=new Set(), seq=0, nextConnection=0, stdout='', stderr='', exit=null, closing=null;
 const record=(kind,fields={})=>rows.push({seq:seq++,kind,...fields});
 const httpServer=http.createServer((_req,res)=>res.writeHead(404).end());
 const wss=new WebSocketServer({server:httpServer});
 wss.on('connection', ws=>{
   const connection=++nextConnection; sockets.add(ws); record('connection',{connection});
   ws.on('close',(code,reason)=>{record('close',{connection,code,reason:reason.toString()});sockets.delete(ws)});
   ws.on('message',raw=>{
     const frame=JSON.parse(String(raw)); record('receive',{connection,frame});
     if(frame.type==='hello') ws.send(JSON.stringify({type:'hello_ok',protocolVersion:'1.2',serverVersion:'fixture',serverInfo:{capabilities:[]}}));
     else if(frame.type==='request') {
       if(frame.method==='new_session') ws.send(JSON.stringify({type:'response',id:frame.id,ok:true,result:{sessionKey:'fixture-session'}}));
       else if(frame.method==='resume_session') ws.send(JSON.stringify({type:'response',id:frame.id,ok:true,result:{sessionKey:'fixture-session'}}));
       else {record('unexpected-method',{connection,method:frame.method});ws.send(JSON.stringify({type:'response',id:frame.id,ok:false,error:{message:'unexpected fixture method'}}));}
     }
   });
 });
 await new Promise((ok,fail)=>{httpServer.once('error',fail);httpServer.listen(0,'127.0.0.1',ok)});
 const url=`ws://127.0.0.1:${httpServer.address().port}/ws`;
 const args=['--import',join(checkout,'node_modules/tsx/dist/loader.mjs'),join(checkout,'src/cli/pilotdeck.ts'),...c.argv,'--gateway-url',url,'--auth-token',token];
 const child=spawn(process.execPath,args,{cwd:project,env:{HOME:home,PILOT_HOME:pilot,PATH:'/usr/bin:/bin',NODE_NO_WARNINGS:'1'},stdio:['ignore','pipe','pipe']});
 child.stdout.on('data',b=>stdout+=b);child.stderr.on('data',b=>stderr+=b); child.on('exit',(code,signal)=>{exit={code,signal};record('exit',exit)});
 const closed=once(child,'close').then(([code,signal])=>{closing={code,signal};});
 await new Promise(ok=>setTimeout(ok,4500));
 const snapshot={id:c.id,argv:c.argv,exitBeforeCleanup:exit,openConnections:sockets.size,stdout,stderr,wire:[...rows]};
 if(exit===null) child.kill('SIGTERM');
 let timer; const timeout=new Promise((_,fail)=>{timer=setTimeout(()=>fail(Error(`cleanup timeout ${c.id}`)),5000)});
 try { await Promise.race([closed,timeout]); } catch (error) { if(exit===null) child.kill('SIGKILL'); await closed; throw error; } finally { clearTimeout(timer); }
 for(const ws of sockets) ws.terminate();
 await new Promise(ok=>wss.close(ok));await new Promise(ok=>httpServer.close(ok));
 snapshot.cleanup={childPid:child.pid,close:closing,exit,remainingSockets:sockets.size,listenerClosed:!httpServer.listening};
 await writeFile(join(root,'raw.json'),JSON.stringify(snapshot,null,2)+'\n',{flag:'wx'});
}
