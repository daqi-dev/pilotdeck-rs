import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.versions.node.split('.')[0], '22');
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/tool/execution-world/SubprocessPort.ts', 'src/router/tokenSaver/classifyAndRoute.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { classifyAndRoute } = await import(pathToFileURL(join(baseline, sources[1])).href);
const nativeSetTimeout = globalThis.setTimeout;
// Observe the real Node timer list's scheduled integer milliseconds. Node keeps
// fractions in _idleTimeout, so reading only that field would be a false oracle.
function scheduled(timer: any): number {
 let link = timer._idleNext;
 for (let i=0; i<100; i++,link=link?._idleNext) {
  if (typeof link?.msecs === 'number') return link.msecs;
 }
 throw Error('Node timer list layout changed');
}
const cases=[];
for (const input of [-Infinity, -5, 0, .5, 1, 1.9, 499.5, 500.9,
 2147483647, 2147483647.5, 2147483648, Infinity, NaN]) {
 const timer=nativeSetTimeout(()=>{},input);
 const hookMs=scheduled(timer); clearTimeout(timer);
 let routerMs: number | undefined;
 globalThis.setTimeout = ((callback:any, delay:any, ...args:any[])=>{
  const timer=nativeSetTimeout(callback,delay,...args); routerMs=scheduled(timer); return timer;
 }) as typeof setTimeout;
 try {
  const result=await classifyAndRoute({config:{enabled:true,judge:{provider:'p',model:'j'},
   defaultTier:'simple',tiers:{simple:{model:{provider:'p',model:'m'}}},judgeTimeoutMs:input},
   messages:[{role:'user',content:[{type:'text',text:'hi'}]}],
   judgeRuntime:{complete:async()=>({role:'assistant',content:[{type:'text',text:'<tier>simple</tier>'}]})}});
  assert.equal(result?.resolvedFrom,'judge'); assert.ok(routerMs !== undefined);
 } finally { globalThis.setTimeout=nativeSetTimeout; }
 cases.push({input:Number.isFinite(input)?input:String(input),hookMs,routerMs});
}
assertBaseline(baseline); assert.deepEqual(await baselineSourceHashes(baseline,sources),sourceSha256);
await mkdir(resolve(process.argv[2]),{recursive:true});
await writeFile(join(resolve(process.argv[2]),'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,cases},null,2)+'\n');
console.log(`recorded ${cases.length} real Node timers and real TS classifier timers`);
