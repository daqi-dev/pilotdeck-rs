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
const result={candidate,profile,channel:'chrome',checks:[],nativeOSIME:false};
let mock,context;
try {
 mock=await startMock();
 // launch() forbids --user-data-dir; the persistent API is required for an explicit isolated profile.
 context=await chromium.launchPersistentContext(profile,{channel:'chrome',headless:true});
 const page=await context.newPage();await page.goto(`${origin}/probe/${candidate}/`);
 await page.locator('#password').fill('probe-secret');await page.locator('#login').click();
 await page.waitForFunction(()=>document.getElementById('sessions')?.textContent.includes('F1 session'));
 assert.equal(await page.evaluate(()=>localStorage.getItem('auth-token')),'f1-probe-token');
 result.checks.push({name:'login/storage/lists',status:'PASS'});
 await page.locator('#composer').fill('你好');
 await page.locator('#composer').dispatchEvent('compositionstart',{data:'ni'});
 await page.locator('#composer').press('Enter');assert.equal(await page.locator('#sent').textContent(),'0');
 await page.locator('#composer').dispatchEvent('compositionend',{data:'你好'});
 await page.locator('#composer').press('Enter');await page.waitForFunction(()=>document.getElementById('sent').textContent==='1');
 result.checks.push({name:'composition event enter guard',status:'PASS',nativeOSIME:false});
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
   globalThis.__streamObserver=new MutationObserver(()=>{const seq=Number(stream.dataset.received);if(seq!==previous){__streamFrames.push({seq,t:performance.now()});previous=seq;}});
   __streamObserver.observe(stream,{attributes:true,childList:true,characterData:true,subtree:true});
   function frame(now){__paintGaps.push(now-last);last=now;if(globalThis.__reading)__reader.push(stream.scrollTop);globalThis.__raf=requestAnimationFrame(frame);}
   __raf=requestAnimationFrame(frame);
 });
 await fetch(origin+'/__probe/stream?hz=25&count=1500');
 await page.waitForFunction(()=>Number(document.getElementById('stream').dataset.received)>=100);
 await page.locator('#stream').evaluate(node=>{node.scrollTop=80;globalThis.__reading=true;});
 await page.waitForFunction(()=>Number(document.getElementById('stream').dataset.received)===1500,{},{timeout:70000});
 const stats=await page.evaluate(()=>{cancelAnimationFrame(__raf);__streamObserver.disconnect();return {frames:__streamFrames,paintGaps:__paintGaps,reader:__reader,missing:Number(document.getElementById('stream').dataset.missing)};});
 assert.equal(stats.frames.length,1500);stats.frames.forEach((f,i)=>assert.equal(f.seq,i+1));assert.equal(stats.missing,0);
 assert(stats.reader.length>0);assert(Math.max(...stats.reader)-Math.min(...stats.reader)<=1);
 const server=mock.stats.streams[0];assert(server.elapsedMs>=60000);assert(1500/(server.elapsedMs/1000)>=20);
 result.checks.push({name:'>=20Hz 60s count/scroll',status:'PASS',received:1500,DOMFrames:stats.frames.length,actualHz:1500/(server.elapsedMs/1000),scrollRange:[Math.min(...stats.reader),Math.max(...stats.reader)],maxAnimationFrameGapMs:Math.max(...stats.paintGaps),jankVerdict:'Frame timings recorded; perceptible smoothness requires human review'});
 await writeFile(resolve(evidence,`${candidate}-browser-trace.json`),JSON.stringify(stats)+'\n');
 await page.screenshot({path:resolve(evidence,`${candidate}.png`),fullPage:true});
 result.status='PASS_BROWSER_MACHINE_CHECKS';
} catch(e) { result.status=context?'FAIL':'BLOCKED_CHROME_LAUNCH';result.error=e.stack;process.exitCode=context?1:2; }
finally {if(context)await context.close();if(mock)await mock.close();await writeFile(resolve(evidence,`${candidate}-browser.json`),JSON.stringify(result,null,2)+'\n');console.log(JSON.stringify(result,null,2));}
