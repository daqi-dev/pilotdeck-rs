// Browser gate for an environment where sandboxed system Chrome can start.
// No playwright browser download; profile is always a worktree-owned directory.
import assert from 'node:assert/strict';
import {mkdir,writeFile} from 'node:fs/promises';
import {resolve} from 'node:path';
import {root,evidence,origin,requireUI} from './env.mjs';
import {startMock} from './mock.mjs';
const candidate=process.argv[2];
if(!['leptos','dioxus'].includes(candidate))throw new Error('Usage: node scripts/exp007/browser.mjs leptos|dioxus');
const {chromium}=requireUI('@playwright/test');
const profile=resolve(root,`_tmp/chrome-profile-${candidate}`);
await mkdir(evidence,{recursive:true});
const result={candidate,profile,channel:'chrome',checks:[],nativeOSIME:false,compositionScope:'synthetic composition only, 未测 OS IME'};
let mock,context;
try {
 mock=await startMock();
 // launch() forbids --user-data-dir; the persistent API is required for an explicit isolated profile.
 context=await chromium.launchPersistentContext(profile,{channel:'chrome',headless:true});
 await context.addInitScript(()=>{localStorage.clear();sessionStorage.clear();});
 const page=await context.newPage();await page.goto(`${origin}/probe/${candidate}/`);
 await page.locator('#password').fill('probe-secret');await page.locator('#login').click();
 await page.waitForFunction(()=>document.getElementById('sessions')?.textContent.includes('F1 session'));
 assert.equal(await page.evaluate(()=>localStorage.getItem('auth-token')),'f1-probe-token');
 result.checks.push({name:'login/storage/lists',status:'PASS'});
 const settle=()=>page.evaluate(()=>new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve))));
 const compose=async(type,data,expected)=>{
  const constructor=await page.evaluate(({type,data})=>{
   const event=new CompositionEvent(type,{data,bubbles:true});
   document.getElementById('composer').dispatchEvent(event);return event.constructor.name;
  },{type,data});
  assert.equal(constructor,'CompositionEvent');
  await page.waitForFunction(expected=>document.getElementById('composer').dataset.composing===expected,expected);
  await settle();
 };
 const keydown=async options=>{
  const before=Number(await page.locator('#composer').getAttribute('data-keydowns'));
  if(Object.keys(options).length===0)await page.locator('#composer').press('Enter');
  else {
   const observed=await page.evaluate(options=>{
    const event=new KeyboardEvent('keydown',{key:'Enter',code:'Enter',bubbles:true,cancelable:true,...options});
    document.getElementById('composer').dispatchEvent(event);
    return {isComposing:event.isComposing,keyCode:event.keyCode};
   },options);
   if(options.isComposing)assert.equal(observed.isComposing,true);
   if(options.keyCode)assert.equal(observed.keyCode,options.keyCode);
  }
  await page.waitForFunction(expected=>Number(document.getElementById('composer').dataset.keydowns)===expected,before+1);
  await settle();
 };
 assert.equal(await page.locator('#composer').getAttribute('data-composing'),'false');
 assert.equal(await page.locator('#composer').getAttribute('data-keydowns'),'0');
 await page.locator('#composer').fill('你好');
 await compose('compositionstart','ni','true');
 await keydown({});assert.equal(await page.locator('#sent').textContent(),'0');
 assert.equal(await page.locator('#composer').inputValue(),'你好');
 await compose('compositionend','你好','false');
 for(const options of [{isComposing:true},{keyCode:229},{shiftKey:true}]) {
  await keydown(options);assert.equal(await page.locator('#sent').textContent(),'0');
  assert.equal(await page.locator('#composer').inputValue(),'你好');
 }
 await keydown({});await page.waitForFunction(()=>document.getElementById('sent').textContent==='1');
 assert.equal(await page.locator('#composer').inputValue(),'');
 const keydowns=Number(await page.locator('#composer').getAttribute('data-keydowns'));assert.equal(keydowns,5);
 result.checks.push({name:'compositionstart/end + Enter + isComposing + keyCode229 + Shift guard',status:'PASS',handlerMarkers:{composing:'false -> true -> false',keydowns},nativeOSIME:false,scope:result.compositionScope});
 await page.locator('#write-island').click();await page.locator('#focus-island').click();
 await page.evaluate(()=>{globalThis.__originalIsland=document.querySelector('[data-island]');globalThis.__originalInput=__originalIsland.querySelector('textarea');});
 for(let cycle=0;cycle<10;cycle++) {
  await page.locator('#toggle-island').click();await page.waitForFunction(()=>!__originalIsland.isConnected);
  await page.locator('#toggle-island').click();await page.waitForFunction(()=>__originalIsland.isConnected&&document.activeElement===__originalInput);
  assert(await page.evaluate(()=>document.querySelector('[data-island]')===__originalIsland));
  await page.locator('#inspect-island').click();
  await page.waitForFunction(()=>document.getElementById('island-state').textContent.includes('BUFFER_SENTINEL'));
 }
 await page.locator('[data-island] textarea').press('Enter');await page.locator('#inspect-island').click();
 await page.waitForFunction(()=>JSON.parse(document.getElementById('island-state').textContent).inputs>0);
 result.checks.push({name:'xterm lifetime DOM/buffer/focus/input',status:'PASS',cycles:10});
 await page.evaluate(()=>{
   globalThis.__streamFrames=[];globalThis.__paintGaps=[];globalThis.__reader=[];let previous=0,last=performance.now();
   const stream=document.getElementById('stream');
   globalThis.__streamObserver=new MutationObserver(()=>{const received=Number(stream.dataset.received);if(received!==previous){__streamFrames.push({seq:Number(stream.dataset.seq),received,t:performance.now()});previous=received;}});
   __streamObserver.observe(stream,{attributes:true,childList:true,characterData:true,subtree:true});
   function frame(now){__paintGaps.push(now-last);last=now;if(globalThis.__reading)__reader.push(stream.scrollTop);globalThis.__raf=requestAnimationFrame(frame);}
   __raf=requestAnimationFrame(frame);
 });
 await fetch(origin+'/__probe/stream?hz=25&count=1500');
 await page.waitForFunction(()=>Number(document.getElementById('stream').dataset.received)>=100);
 const geometry=await page.locator('#stream').evaluate(node=>({scrollHeight:node.scrollHeight,clientHeight:node.clientHeight}));
 assert(geometry.clientHeight>0&&geometry.scrollHeight>geometry.clientHeight,'Stream must have real scrollable overflow');
 const readingStart=await page.locator('#stream').evaluate(node=>{node.scrollTop=80;globalThis.__reading=true;return node.scrollTop;});
 assert(readingStart>0,'Reader must actually scroll away from the top');
 await page.waitForFunction(()=>Number(document.getElementById('stream').dataset.received)===1500,{},{timeout:70000});
 const stats=await page.evaluate(()=>{cancelAnimationFrame(__raf);__streamObserver.disconnect();return {frames:__streamFrames,paintGaps:__paintGaps,reader:__reader,missing:Number(document.getElementById('stream').dataset.missing)};});
 assert.equal(stats.frames.length,1500);stats.frames.forEach((f,i)=>{assert.equal(f.seq,i+1);assert.equal(f.received,i+1);});assert.equal(stats.missing,0);
 assert.deepEqual(stats.frames.map(f=>f.seq),mock.stats.sent.map(f=>f.seq));
 assert(stats.reader.length>0);assert(Math.max(...stats.reader)-Math.min(...stats.reader)<=1);
 const server=mock.stats.streams[0];assert(server.elapsedMs>=60000);assert(1500/(server.elapsedMs/1000)>=20);
 result.checks.push({name:'>=20Hz 60s count/real frame seq/scroll',status:'PASS',received:1500,DOMFrames:stats.frames.length,sequenceSource:'data-seq = inbound Frame.seq',actualHz:1500/(server.elapsedMs/1000),geometry,readingStart,scrollRange:[Math.min(...stats.reader),Math.max(...stats.reader)],maxAnimationFrameGapMs:Math.max(...stats.paintGaps),jankVerdict:'Frame timings recorded; perceptible smoothness requires human review'});
 await writeFile(resolve(evidence,`${candidate}-browser-trace.json`),JSON.stringify(stats)+'\n');
 await page.screenshot({path:resolve(evidence,`${candidate}.png`),fullPage:true});
 result.status='PASS_BROWSER_MACHINE_CHECKS';
} catch(e) { result.status=context?'FAIL':mock?'BLOCKED_CHROME_LAUNCH':'BLOCKED_MOCK_START';result.error=e.stack;process.exitCode=context?1:2; }
finally {if(context)await context.close();if(mock)await mock.close();await writeFile(resolve(evidence,`${candidate}-browser.json`),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result,null,2));}
