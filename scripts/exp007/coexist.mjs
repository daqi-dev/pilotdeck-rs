import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {readFile,writeFile,open,access,mkdtemp} from 'node:fs/promises';
import {resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {ports,backendPaths,makeIsolation,assertPortsFree} from './coexist-support.mjs';

if(process.argv.slice(2).some(arg=>arg!=='--browser'))throw new Error('Usage: node scripts/exp007/coexist.mjs [--browser]');
const browserRequested=process.argv.includes('--browser');
const operatorEnv={...process.env}; // R12: Chrome keeps real HOME, services get isolated HOME.
const root=resolve(fileURLToPath(new URL('../..',import.meta.url)));
const isolation=await makeIsolation(root,operatorEnv);
for(const key of Object.keys(process.env))delete process.env[key];
Object.assign(process.env,isolation.env);
const {baseline,requireUI,evidence}=await import('./env.mjs');
const origin=`http://127.0.0.1:${ports.router}`;
const mockOrigin=`http://127.0.0.1:${ports.mock}`;
const checks=[],children=[],logs=[];
const controller=new AbortController();
const result={status:'RUNNING',variant:'B repository Rust prefix router + frozen exp008 proxy',checks,
  processes:{runner:process.pid,vite:'in-process',mock:'in-process'},bind:'127.0.0.1',
  ports:Object.values(ports),admin:false,baselineFilesChanged:0,isolation:isolation.directory,
  browser:{status:'NOT_RUN',channel:'chrome',requested:browserRequested},
  limitation:'Mock backend; real ui/server prefix patch, production PTY and plugin backend not measured. Browser reload/storage require --browser; BFCache hit/scroll/focus, OS IME and HMR are outside this experiment.'};
let mock,vite,context;
async function bounded(promise,ms=5000){
  let timer;try{return await Promise.race([promise,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error(`deadline ${ms}ms`)),ms);})]);}finally{clearTimeout(timer);}
}
async function ready(url,child){
  for(let i=0;i<150;i++){
    controller.signal.throwIfAborted();
    if(child&&(child.exitCode!==null||child.signalCode!==null))throw new Error(`child exited: ${child.exitCode??child.signalCode}`);
    try{const response=await fetch(url,{signal:AbortSignal.any([controller.signal,AbortSignal.timeout(1000)])});await response.arrayBuffer();if(response.ok)return;}catch{controller.signal.throwIfAborted();}
    await new Promise(r=>setTimeout(r,50));
  }
  throw new Error(`not ready: ${url}`);
}
async function start(name,binary,env){
  controller.signal.throwIfAborted();
  const log=await open(resolve(evidence,`${name}.log`),'w');logs.push(log);
  controller.signal.throwIfAborted();
  const child=spawn(binary,[],{cwd:root,stdio:['ignore',log.fd,log.fd],env:{...isolation.env,...env}});
  children.push({name,child});result.processes[name]=child.pid;
  child.once('error',error=>controller.abort(error));
  return child;
}
const fetchLocal=(path,options={})=>fetch(origin+path,{...options,signal:AbortSignal.any([controller.signal,AbortSignal.timeout(5000)])});
async function browserChecks(){
  const {chromium}=requireUI('@playwright/test');
  const profile=await mkdtemp(resolve(evidence,'chrome-coexist-'));
  context=await chromium.launchPersistentContext(profile,{channel:'chrome',headless:true,
    env:{...operatorEnv,TMPDIR:isolation.env.TMPDIR,TMP:isolation.env.TMPDIR,TEMP:isolation.env.TMPDIR,PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD:'1'}});
  const boundary=(await readFile(resolve(root,'ui-rs/shared/boundary.js'),'utf8')).replace('export function','function');
  await context.addInitScript({content:`${boundary}\ninstallBoundary(window);window.__coexistDocument=crypto.randomUUID();`});
  const page=await context.newPage();
  await page.goto(origin+'/probe/leptos/');
  await page.locator('#password').fill('probe-secret');await page.locator('#login').click();
  await page.waitForFunction(()=>document.getElementById('sessions')?.textContent.includes('F1 session'));
  await page.evaluate(()=>{localStorage.setItem('draft_input_probe:s','共存草稿');sessionStorage.setItem('coexist-sentinel','same tab');});
  const first=await page.evaluate(()=>window.__coexistDocument);
  await page.evaluate(()=>history.pushState({},'','/settings'));
  await page.waitForURL(origin+'/settings');await page.waitForLoadState('domcontentloaded');
  assert.notEqual(await page.evaluate(()=>window.__coexistDocument),first);
  assert(await page.locator('script[src="/src/main.jsx"]').count());
  const old=await page.evaluate(()=>window.__coexistDocument);
  await page.evaluate(()=>history.pushState({},'','/probe/leptos/'));
  await page.waitForURL(origin+'/probe/leptos/');
  await page.waitForFunction(()=>document.getElementById('sessions')?.textContent.includes('F1 session'));
  assert.notEqual(await page.evaluate(()=>window.__coexistDocument),old);
  assert.equal(await page.evaluate(()=>localStorage.getItem('auth-token')),'f1-probe-token');
  assert.equal(await page.evaluate(()=>localStorage.getItem('draft_input_probe:s')),'共存草稿');
  assert.equal(await page.evaluate(()=>sessionStorage.getItem('coexist-sentinel')),'same tab');
  for(const direction of ['back','forward']){
    await page[direction==='back'?'goBack':'goForward']();
    await page.waitForURL(origin+(direction==='back'?'/settings':'/probe/leptos/'));
    await page.waitForLoadState('domcontentloaded');
    assert.equal(await page.evaluate(()=>localStorage.getItem('draft_input_probe:s')),'共存草稿');
  }
  result.browser={status:'PASS_BROWSER_MACHINE_CHECKS',channel:'chrome',profile,
    checks:['both history boundaries reload documents','auth/draft/sessionStorage retained','back/forward reach correct entry'],
    limitation:'History primitives exercised; actual React navigate/Link/NavLink interception is covered separately by navigation.mjs. BFCache hit/scroll/focus and OS IME are not asserted.'};
}
async function run(){
  await assertPortsFree();
  for(const candidate of ['leptos','dioxus'])for(const asset of ['index.html','probe_bg.wasm'])
    await access(resolve(root,`_build/dist-${candidate}`,asset));
  const routerBinary=resolve(root,'_build/exp007-target/debug/pdrs-exp007-prefix-router');
  const proxyBinary=resolve(root,'_build/exp007-target/debug/pdrs-exp008-proxy');
  await access(routerBinary);await access(proxyBinary);
  result.components={};
  for(const [name,binary,source] of [['prefixRouter',routerBinary,'scripts/exp007/prefix-router/src/main.rs'],['exp008',proxyBinary,'scripts/exp008/proxy/src/main.rs']])
    result.components[name]={source,binarySHA256:createHash('sha256').update(await readFile(binary)).digest('hex'),sourceSHA256:createHash('sha256').update(await readFile(resolve(root,source))).digest('hex')};
  controller.signal.throwIfAborted();
  const {startMock}=await import('./mock.mjs');mock=await startMock();
  const {createServer}=await import(requireUI.resolve('vite'));
  const {default:react}=await import(requireUI.resolve('@vitejs/plugin-react'));
  const {WebSocket}=requireUI('ws');
  const backend=Object.fromEntries(backendPaths.map(path=>[path,{target:mockOrigin,ws:true}]));
  // Tailwind's baseline content/config paths are relative to the baseline ui cwd.
  // Cache/temp paths above stay absolute and isolated; no baseline files are written.
  process.chdir(baseline);
  vite=await createServer({configFile:false,root:baseline,cacheDir:resolve(isolation.directory,'vite-cache'),plugins:[react()],
    server:{host:'127.0.0.1',port:ports.vite,strictPort:true,open:false,proxy:backend},optimizeDeps:{noDiscovery:true,include:[]}});
  await vite.listen();
  controller.signal.throwIfAborted();
  const proxy=await start('exp008',proxyBinary,{EXP008_LISTEN:`127.0.0.1:${ports.proxy}`,EXP008_UPSTREAM:`http://127.0.0.1:${ports.vite}`,EXP008_HANDLER:'ts'});
  await ready(`http://127.0.0.1:${ports.proxy}/`,proxy);
  const router=await start('prefixRouter',routerBinary,{EXP007_LISTEN:`127.0.0.1:${ports.router}`,EXP007_PREFIX:'/probe',
    EXP007_OLD_UPSTREAM:`http://127.0.0.1:${ports.proxy}`,EXP007_NEW_UPSTREAM:mockOrigin});
  await ready(origin+'/',router);
  for(const path of ['/','/settings','/probex']){
    const response=await fetchLocal(path),body=await response.text();assert.equal(response.status,200);assert(body.includes('/src/main.jsx'));
    assert.equal(response.headers.get('x-pilotdeck-handler'),'ts');
    checks.push({path,status:'PASS',bundle:'actual baseline Vite index',statusCode:response.status,exp008Handler:'ts'});
  }
  for(const candidate of ['leptos','dioxus']){
    const response=await fetchLocal(`/probe/${candidate}/`),body=await response.text();assert.equal(response.status,200);assert(body.includes(`F1 ${candidate} probe`));
    const wasm=await fetchLocal(`/probe/${candidate}/probe_bg.wasm`);assert.equal(wasm.status,200);assert.equal(wasm.headers.get('content-type'),'application/wasm');
    const bytes=Buffer.from(await wasm.arrayBuffer());assert(bytes.subarray(0,4).equals(Buffer.from([0,97,115,109])));
    assert(bytes.equals(await readFile(resolve(root,`_build/dist-${candidate}/probe_bg.wasm`))));
    checks.push({path:`/probe/${candidate}/`,status:'PASS',bundle:'actual compiled wasm static entry',wasmBytes:bytes.length});
  }
  for(const path of ['/shared/island.js','/vendor/xterm.js']){
    const response=await fetchLocal(path);assert.equal(response.status,200);assert((await response.text()).length>0);checks.push({path,status:'PASS',bundle:'new entry dependency'});
  }
  const response=await fetchLocal('/api/auth/login',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({username:'probe',password:'probe-secret'})});
  assert.equal((await response.json()).token,'f1-probe-token');assert.equal(response.headers.get('x-pilotdeck-handler'),'ts');
  checks.push({path:'/api/auth/login',status:'PASS',backend:'mock same-origin',exp008Handler:'ts'});
  for(const path of ['/ws','/shell','/plugin-ws/echo']){
    result.wsPhase={path,phase:'opening'};
    const socket=new WebSocket(`ws://127.0.0.1:${ports.router}${path}?token=f1-probe-token`,['coexist'],{headers:{Authorization:'Bearer f1-probe-token'}});
    let upgradeHeaders;
    socket.once('upgrade',response=>{upgradeHeaders=response.headers;});
    try{
      await bounded(new Promise((r,j)=>{socket.once('open',r);socket.once('error',j);}));
      result.wsPhase.phase='opened';
      assert.equal(upgradeHeaders.date,undefined);assert.equal(socket.protocol,'coexist');
      const receipt=path==='/ws'?null:new Promise((r,j)=>{socket.once('message',data=>{try{r(JSON.parse(data.toString()));}catch(error){j(error);}});socket.once('error',j);});
      socket.send(JSON.stringify(path==='/ws'?{type:'watch-session',sessionId:'s'}:{type:'input',input:'RUST_PROXY_ECHO'}));
      if(receipt)assert.equal((await bounded(receipt)).data,'RUST_PROXY_ECHO');
      result.wsPhase.phase='ping';
      const pong=new Promise((r,j)=>{socket.once('pong',r);socket.once('error',j);});socket.ping('peer-pong');assert.equal((await bounded(pong)).toString(),'peer-pong');
      const closed=new Promise(r=>socket.once('close',(code,reason)=>r({code,reason:reason.toString()})));socket.close(1000,'coexist done');
      result.wsPhase.phase='close';
      assert.deepEqual(await bounded(closed),{code:1000,reason:'coexist done'});
      assert(mock.stats.connections.some(connection=>connection.path===path&&connection.token==='f1-probe-token'&&connection.authorization==='Bearer f1-probe-token'));
      checks.push({path,status:'PASS',webSocketUpgrade:true,tokenRetained:true,authorizationRetained:true,subprotocol:'coexist',syntheticDate:false,pongRetained:true,closeCode:1000,closeReason:'coexist done'});
    }finally{socket.terminate();}
  }
  delete result.wsPhase;
  const sse=await fetchLocal('/__probe/sse');assert.equal(sse.status,200);assert.equal(sse.headers.get('content-type'),'text/event-stream');
  const reader=sse.body.getReader();const started=performance.now();const first=await reader.read();
  assert.equal(Buffer.from(first.value).toString(),'data: first\n\n');
  const second=await reader.read();const gapMs=performance.now()-started;
  assert.equal(Buffer.from(second.value).toString(),'data: second\n\n');assert(gapMs>=100,'first chunk must arrive before upstream finishes');assert((await reader.read()).done);
  checks.push({path:'/__probe/sse',status:'PASS',streaming:true,chunks:2,gapMs});
  result.status='PASS_HTTP_WS_SSE_ONLY';
  if(browserRequested){result.browser.status='RUNNING';await browserChecks();result.status='PASS_HTTP_WS_SSE_BROWSER';}
}
const onSignal=()=>controller.abort(new Error('runner interrupted'));
process.once('SIGTERM',onSignal);process.once('SIGINT',onSignal);
let abortListener;
try{
  await Promise.race([run(),new Promise((_,reject)=>{
    abortListener=()=>reject(controller.signal.reason);controller.signal.addEventListener('abort',abortListener,{once:true});
  })]);
}catch(error){process.exitCode=1;result.status='FAIL';result.error=error.stack;if(mock)result.mockStats=mock.stats;}
finally{
  const cleanup={prefixRouterStopped:true,exp008Stopped:true,closeCallsCompleted:false,deadlineMs:5000};
  const errors=[];
  const stopping=children.map(async({name,child})=>{
    if(child.exitCode===null&&child.signalCode===null){
      const exited=new Promise(r=>child.once('exit',r));child.kill('SIGTERM');
      try{await bounded(exited,2000);}catch{child.kill('SIGKILL');await bounded(exited,2000);}
    }
    cleanup[name==='exp008'?'exp008Stopped':'prefixRouterStopped']=child.exitCode!==null||child.signalCode!==null;
  });
  for(const outcome of await Promise.allSettled([...stopping,context?.close(),vite?.close(),mock?.close()].map(p=>bounded(Promise.resolve(p)))))
    if(outcome.status==='rejected')errors.push(outcome.reason.stack);
  for(const log of logs)await log.close();
  cleanup.closeCallsCompleted=errors.length===0;
  if(cleanup.closeCallsCompleted){
    try{await assertPortsFree();cleanup.portsReleased=true;}catch(error){cleanup.portsReleased=false;errors.push(error.stack);}
  }
  if(errors.length){cleanup.errors=errors;result.status='FAIL';process.exitCode=1;}
  result.cleanup=cleanup;
  await writeFile(resolve(evidence,'coexist.json'),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result,null,2));
  process.removeListener('SIGTERM',onSignal);process.removeListener('SIGINT',onSignal);
  if(abortListener)controller.signal.removeEventListener('abort',abortListener);
  // Vite/esbuild idle workers can retain handles. Child exits above are checked first.
  process.exit(process.exitCode||0);
}
