import assert from 'node:assert/strict';
import {createRequire} from 'node:module';
import {spawn} from 'node:child_process';
import {readFile,writeFile,open} from 'node:fs/promises';
import {resolve} from 'node:path';
import {baseline,requireUI,root,evidence} from './env.mjs';
import {startMock} from './mock.mjs';
import {pause} from './dom-env.mjs';
const {createServer}=await import(requireUI.resolve('vite'));
const {default:react}=await import(requireUI.resolve('@vitejs/plugin-react'));
const {WebSocket}=requireUI('ws');
const checks=[];
let mock,vite,caddy,log,runResult;
async function ready(url,process){for(let i=0;i<100;i++){if(process?.exitCode!==null&&process?.exitCode!==undefined)throw new Error(`process exited ${process.exitCode}`);try{const r=await fetch(url);if(r.ok)return r;}catch{}await pause(50);}throw new Error(`not ready: ${url}`);}
try {
 mock=await startMock();
 vite=await createServer({configFile:false,root:baseline,cacheDir:resolve(root,'_build/vite-cache'),plugins:[react()],server:{host:'127.0.0.1',port:28374,strictPort:true},optimizeDeps:{noDiscovery:true,include:[]}});
 await vite.listen();
 log=await open(resolve(evidence,'caddy.log'),'w');
 caddy=spawn(resolve(root,'_build/tools/bin/caddy'),['run','--config',resolve(root,'scripts/exp007/Caddyfile'),'--adapter','caddyfile'],{cwd:root,detached:true,stdio:['ignore',log.fd,log.fd],env:{...process.env,XDG_DATA_HOME:resolve(root,'_build/caddy-data'),XDG_CONFIG_HOME:resolve(root,'_build/caddy-config')}});
 await ready('http://127.0.0.1:28375/',caddy);
 for(const path of ['/','/settings','/probex']) {
   const r=await fetch(`http://127.0.0.1:28375${path}`);const body=await r.text();assert.equal(r.status,200);assert(body.includes('/src/main.jsx'));checks.push({path,status:'PASS',bundle:'actual baseline Vite index',statusCode:r.status});
 }
 for(const candidate of ['leptos','dioxus']) {
   const r=await fetch(`http://127.0.0.1:28375/probe/${candidate}/`);const body=await r.text();assert.equal(r.status,200);assert(body.includes(`F1 ${candidate} probe`));
   const wasm=await fetch(`http://127.0.0.1:28375/probe/${candidate}/probe_bg.wasm`);assert.equal(wasm.status,200);assert.equal(wasm.headers.get('content-type'),'application/wasm');
   checks.push({path:`/probe/${candidate}/`,status:'PASS',bundle:'actual compiled wasm static entry',wasmBytes:(await wasm.arrayBuffer()).byteLength});
 }
 const response=await fetch('http://127.0.0.1:28375/api/auth/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({username:'probe',password:'probe-secret'})});assert.equal((await response.json()).token,'f1-probe-token');
 checks.push({path:'/api/auth/login',status:'PASS',backend:'mock same-origin'});
 for(const path of ['/ws','/shell']) {
   const s=new WebSocket(`ws://127.0.0.1:28375${path}?token=f1-probe-token`);
   await new Promise((r,j)=>{s.once('open',r);s.once('error',j);});
   s.send(JSON.stringify(path==='/ws'?{type:'watch-session',sessionId:'s'}:{type:'input',input:'CADDY_ECHO'}));
   if(path==='/shell'){const message=await new Promise(r=>s.once('message',data=>r(JSON.parse(data.toString()))));assert.equal(message.data,'CADDY_ECHO');}
   s.close();await new Promise(r=>s.once('close',r));
   checks.push({path,status:'PASS',webSocketUpgrade:true,tokenRetained:true});
 }
 const result={status:'PASS_HTTP_WS_ONLY',variant:'B Caddy front proxy',checks,processes:{runner:process.pid,caddy:caddy.pid,vite:'in-process',mock:'in-process'},bind:'127.0.0.1',ports:[28370,28374,28375],admin:false,baselineFilesChanged:0,limitation:'HTTP entries and WS upgrade measured; native browser reload/storage/BFCache behavior not measured'};
 runResult=result;
}catch(error){process.exitCode=1;runResult={status:'FAIL',error:error.stack};}
finally{
 const cleanup={caddyStopped:!caddy,closeCallsCompleted:false,deadlineMs:5000};
 let deadline;
 const closing=(async()=>{
  if(caddy&&caddy.exitCode===null){const exited=new Promise(r=>caddy.once('exit',r));process.kill(-caddy.pid,'SIGTERM');await exited;}
  cleanup.caddyStopped=true;
  if(log)await log.close();
  // Begin both closes even if Vite's dependency worker cleanup stalls.
  await Promise.all([vite?.close(),mock?.close()]);cleanup.closeCallsCompleted=true;
 })();
 try{await Promise.race([closing,new Promise(resolve=>{deadline=setTimeout(resolve,5000);})]);}
 catch(error){process.exitCode=1;cleanup.error=error.stack;}
 clearTimeout(deadline);
 runResult.cleanup=cleanup;
 await writeFile(resolve(evidence,'coexist.json'),JSON.stringify(runResult,null,2)+'\n');console.log(JSON.stringify(runResult,null,2));
 // Vite/esbuild can retain idle worker handles even after close and freed ports.
 // This terminates only this runner; Caddy has its own bounded process group above.
 process.exit(process.exitCode||0);
}
