import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.versions.node.split('.')[0], '22');
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline=resolve(process.env.PDRS_BASELINE);
const sourceCommit=assertBaseline(baseline);
const sources=['src/network/fetch.ts'];
const sourceSha256=await baselineSourceHashes(baseline,sources);
const {networkFetch, NetworkFetchError}=await import(pathToFileURL(join(baseline,sources[0])).href);
Math.random=()=>0;
const cases=[];
for (const mode of ['raw-no-abort','ready-rejection-sync-abort','timer-error-before-parent',
 'parent-before-error','normalized-sync-abort','last-get-rejection-sync-abort']) {
 const controller=new AbortController(); let calls=0; let parentTimer:any;
 const raw=()=>Object.assign(new TypeError('socket hang up'),{cause:{code:'ECONNRESET'}});
 const fetchImpl=()=>{
  ++calls;
  if(mode==='last-get-rejection-sync-abort'&&calls===1) return Promise.reject(raw());
  const error=mode==='normalized-sync-abort'?new NetworkFetchError('network_tls_error','already normalized'):raw();
  if(mode==='timer-error-before-parent'||mode==='parent-before-error') {
   const promise=new Promise((_r,reject)=>setTimeout(()=>reject(error),mode==='timer-error-before-parent'?5:10));
   parentTimer=setTimeout(()=>controller.abort(),mode==='timer-error-before-parent'?10:5);
   return promise;
  }
  const promise=Promise.reject(error);
  if(mode!=='raw-no-abort')controller.abort();
  return promise;
 };
 const pending=networkFetch('https://example.com/',{method:mode.startsWith('last-get')?'GET':'POST'},
  {signal:controller.signal,retry:{maxRetries:1,baseDelayMs:1,maxDelayMs:1},fetchImpl:fetchImpl as any});
 if(mode.includes('before-')) { const until=performance.now()+40;while(performance.now()<until){} }
 let expected;
 try {await pending;assert.fail('must reject');} catch(error:any) {
  expected={name:error.name,code:error.code,message:error.message};
 }
 clearTimeout(parentTimer);
 cases.push({mode,calls,expected});
}
assertBaseline(baseline);assert.deepEqual(await baselineSourceHashes(baseline,sources),sourceSha256);
await mkdir(resolve(process.argv[2]),{recursive:true});
await writeFile(join(resolve(process.argv[2]),'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,cases},null,2)+'\n');
console.log(`recorded ${cases.length} real TS networkFetch error order cases`);
