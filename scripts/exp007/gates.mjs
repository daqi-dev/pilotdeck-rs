import assert from 'node:assert/strict';
import {readFile,writeFile,mkdir} from 'node:fs/promises';
import {resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {root,evidence,origin} from './env.mjs';
import {createDOM,pause,waitFor} from './dom-env.mjs';
import {startMock} from './mock.mjs';
const candidate=process.argv[2];
if(!['leptos','dioxus'].includes(candidate))throw new Error('Usage: node scripts/exp007/gates.mjs leptos|dioxus [--quick]');
const count=process.argv.includes('--quick')?50:1500;
const hz=25;
await mkdir(evidence,{recursive:true});
const result={candidate,environment:'jsdom + actual wasm CSR + real xterm + localhost HTTP/WS',nativeIME:false,paint:false,geometry:false,checks:[],workload:{hz,count},startedAt:new Date().toISOString()};
const trace=[];
let mock,dom;
async function check(name,run){const start=performance.now();try{const data=await run();result.checks.push({name,status:'PASS',elapsedMs:performance.now()-start,...data});}catch(e){result.checks.push({name,status:'FAIL',error:e.stack});throw e;}}
try {
  mock=await startMock();
  const env=await createDOM(`/probe/${candidate}/`);dom=env.dom;
  result.domErrors=env.errors;
  const w=env.w,d=w.document;
  const {createHash}=await import('node:crypto');
  result.wasmSHA256=createHash('sha256').update(await readFile(resolve(root,`_build/dist-${candidate}/probe_bg.wasm`))).digest('hex');
  const app=await import(pathToFileURL(resolve(root,`_build/dist-${candidate}/probe.js`)));
  assert.equal(typeof app.TerminalBridge,'function', 'typed Rust facade exported by wasm-bindgen');
  const initStart=performance.now();
  await app.default({module_or_path:await readFile(resolve(root,`_build/dist-${candidate}/probe_bg.wasm`))});
  await waitFor(()=>d.querySelector('#login') && d.querySelector('[data-island]'));
  result.initMs=performance.now()-initStart;
  const click=async id=>{d.getElementById(id).click();await pause(30);};
  const text=id=>d.getElementById(id)?.textContent;
  await check('S2 login, token, authenticated projects and sessions',async()=>{
    d.getElementById('password').value='wrong';await click('login');await waitFor(()=>text('status').includes('401'));
    assert.equal(w.localStorage.getItem('auth-token'),null);
    d.getElementById('password').value='probe-secret';await click('login');
    await waitFor(()=>d.querySelector('#composer') && text('sessions').includes('F1 session'));
    await waitFor(()=>mock.stats.outbound.some(m=>m.type==='watch-session'));
    assert.equal(w.localStorage.getItem('auth-token'),'f1-probe-token');
    for(const path of ['/api/projects','/api/projects/probe/sessions'])assert(mock.stats.requests.some(r=>r.path===path && r.authorization==='Bearer f1-probe-token'));
    assert(mock.stats.connections.some(c=>c.path==='/ws'&&c.token==='f1-probe-token'));
    return {paths:mock.stats.requests.map(r=>r.path),watchSession:true};
  });
  await check('S3 compositionstart/end enter guard + shift + isComposing + keyCode229',async()=>{
    const composer=d.getElementById('composer');
    const event=(name,opts)=>composer.dispatchEvent(new w.KeyboardEvent(name,{key:'Enter',code:'Enter',bubbles:true,cancelable:true,...opts}));
    const fill=value=>{composer.value=value;composer.dispatchEvent(new w.Event('input',{bubbles:true}));};
    fill('你好');composer.dispatchEvent(new w.CompositionEvent('compositionstart',{data:'ni',bubbles:true}));await pause(30);
    event('keydown',{});await pause(30);assert.equal(text('sent'),'0');assert.equal(composer.value,'你好');
    composer.dispatchEvent(new w.CompositionEvent('compositionend',{data:'你好',bubbles:true}));await pause(30);
    event('keydown',{isComposing:true});event('keydown',{keyCode:229});event('keydown',{shiftKey:true});await pause(30);
    assert.equal(text('sent'),'0');
    event('keydown',{});await waitFor(()=>text('sent')==='1');assert.equal(composer.value,'');
    assert.equal(d.getElementById('composer'),composer, 'Composer DOM node retained after send');
    fill('未发送草稿');await waitFor(()=>w.localStorage.getItem('draft_input_probe:s')==='未发送草稿');
    return {compositionEnterSent:0,committedEnterSent:1,draftSaved:true,nodeRetained:true};
  });
  await check('S2 projects_updated refresh',async()=>{
    await fetch(origin+'/__probe/projects');await waitFor(()=>text('sessions').includes('Probe 2'));
    return {projectsRevision:2};
  });
  await check('S3 xterm mount/unmount/remount buffer, DOM identity, focus, input',async()=>{
    await waitFor(()=>mock.stats.connections.some(c=>c.path==='/shell'));
    await click('write-island');await pause(80);
    const rootNode=d.querySelector('[data-island]'),termInput=rootNode.querySelector('textarea');
    await click('focus-island');assert.equal(d.activeElement,termInput);
    const snapshots=[];
    for(let i=0;i<10;i++) {
      await click('toggle-island');assert.equal(rootNode.isConnected,false);
      await click('toggle-island');await waitFor(()=>rootNode.isConnected);
      assert.equal(d.querySelector('[data-island]'),rootNode);assert.equal(d.activeElement,termInput);
      await click('inspect-island');const snapshot=JSON.parse(text('island-state'));
      assert(snapshot.lines.some(line=>line.includes('BUFFER_SENTINEL')));assert(snapshot.focused);snapshots.push(snapshot);
    }
    termInput.dispatchEvent(new w.KeyboardEvent('keydown',{key:'Enter',code:'Enter',keyCode:13,bubbles:true,cancelable:true}));
    await pause(50);await click('inspect-island');const after=JSON.parse(text('island-state'));
    assert(after.inputs>0);assert(mock.stats.outbound.some(m=>m.path==='/shell'&&m.type==='input'));
    return {cycles:10,rootRetained:true,inputRetained:true,focusRetained:true,bufferRetained:true,inputs:after.inputs,snapshot:after};
  });
  await check('S3 stream_delta >=20Hz 60s receive AND observed DOM frame accounting',async()=>{
    const stream=d.getElementById('stream'),composer=d.getElementById('composer');
    let previous=0;
    const observer=new w.MutationObserver(()=>{
      const seq=Number(stream.dataset.received);
      if(seq!==previous){trace.push({seq,t:performance.now(),textLength:stream.textContent.length});previous=seq;}
    });
    observer.observe(stream,{attributes:true,childList:true,characterData:true,subtree:true});
    await fetch(origin+`/__probe/stream?hz=${hz}&count=${count}`);
    await waitFor(()=>Number(stream.dataset.received)===count,Math.ceil(count/hz*1000)+10000);
    observer.disconnect();
    assert(mock.stats.streams[0].count/(mock.stats.streams[0].elapsedMs/1000)>=20);
    if(count===1500)assert(mock.stats.streams[0].elapsedMs>=60_000);
    assert.equal(Number(stream.dataset.missing),0);assert.equal(trace.length,count);
    trace.forEach((frame,i)=>assert.equal(frame.seq,i+1));
    assert.equal(d.getElementById('composer'),composer);assert.equal(composer.value,'未发送草稿');
    const sentMap=new Map(mock.stats.sent.map(f=>[f.seq,f.t]));
    const latency=trace.map(f=>f.t-sentMap.get(f.seq)).sort((a,b)=>a-b);
    const pct=p=>latency[Math.min(latency.length-1,Math.floor(latency.length*p))];
    const gaps=trace.slice(1).map((f,i)=>f.t-trace[i].t);
    return {received:count,observedDOMFrames:trace.length,missing:0,domLatencyMs:{p50:pct(.5),p95:pct(.95),p99:pct(.99),max:Math.max(...latency)},maxDOMGapMs:Math.max(...gaps),server:mock.stats.streams[0],scroll:'UNVERIFIED: jsdom has no layout/scroll geometry',jank:'UNVERIFIED: jsdom has no browser paint'};
  });
  await check('D1 main WS reconnect + 1s..30s backoff formula + 30s ping',async()=>{
    assert.deepEqual([1,2,3,4,5,6,7,8].map(app.backoff),[1000,2000,4000,8000,16000,30000,30000,30000]);
    if(count===1500)assert(mock.stats.outbound.some(m=>m.type==='ping'));
    const before=mock.stats.connections.filter(c=>c.path==='/ws').length;
    const start=performance.now();await fetch(origin+'/__probe/close?channel=/ws');
    await waitFor(()=>mock.stats.connections.filter(c=>c.path==='/ws').length>before,4000);
    await waitFor(()=>mock.stats.outbound.filter(m=>m.type==='watch-session').length>1);
    const delay=performance.now()-start;assert(delay>=950 && delay<2500);
    return {reconnectMs:delay,watchReattached:true,pingObserved:mock.stats.outbound.filter(m=>m.type==='ping').length,backoffFormula:true,failedHandshakeLadder:'not exercised'};
  });
  await check('D2 shell close does not automatically reconnect',async()=>{
    const before=mock.stats.connections.filter(c=>c.path==='/shell').length;
    await fetch(origin+'/__probe/close?channel=/shell');await pause(2200);
    assert.equal(mock.stats.connections.filter(c=>c.path==='/shell').length,before);
    await click('reenter-shell');await waitFor(()=>mock.stats.connections.filter(c=>c.path==='/shell').length===before+1);
    return {observedMs:2200,connections:before,manualReentryConnected:true};
  });
  result.domErrors=env.errors;
  result.status='PASS_DOM_ONLY';
} catch(error) { result.status='FAIL';result.error=error.stack;process.exitCode=1; }
finally {
  if(dom)dom.window.close();
  if(mock)await mock.close();
  result.finishedAt=new Date().toISOString();
  await writeFile(resolve(evidence,`${candidate}${count===50?'-quick':''}.json`),JSON.stringify(result,null,2)+'\n');
  await writeFile(resolve(evidence,`${candidate}${count===50?'-quick':''}-trace.json`),JSON.stringify(trace)+'\n');
  console.log(JSON.stringify(result,null,2));
  // Dioxus owns a CSR event loop until document exit; emulate page teardown here.
  process.exit(process.exitCode||0);
}
