import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.versions.node.split('.')[0], '22');
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/router/config/parseRouterConfig.ts', 'src/router/config/schema.ts',
 'src/router/tokenSaver/classifyAndRoute.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { parseRouterConfig } = await import(pathToFileURL(join(baseline, sources[0])).href);
const { classifyAndRoute } = await import(pathToFileURL(join(baseline, sources[2])).href);
const model = { providers: { p: { models: { j: {}, m: {} } } } };
const nativeSetTimeout = globalThis.setTimeout;
// Observe the actual integer timer list delay, not fractional _idleTimeout.
function scheduled(timer: any): number {
 let link = timer._idleNext;
 for (let i=0; i<100; i++,link=link?._idleNext) {
  if (typeof link?.msecs === 'number') return link.msecs;
 }
 throw Error('Node timer list layout changed');
}
const cases=[];
for (const input of [undefined, null, false, '500', -Infinity, -5, 0, .5, 1, 1.9,
 499.5, 500, 500.9, 5000, 2147483647, 2147483647.5, 2147483648,
 Number.MAX_SAFE_INTEGER, 1e100, Infinity, NaN]) {
 const raw = {tokenSaver:{judge:'p/j',defaultTier:'simple',tiers:{simple:{model:'p/m'}},
  ...(input === undefined ? {} : {judgeTimeoutMs:input})}};
 const parsed = parseRouterConfig(raw, model);
 const config = parsed.config.tokenSaver;
 assert.ok(config); assert.equal(typeof config.judgeTimeoutMs, 'number');
 let timerMs: number | undefined;
 globalThis.setTimeout = ((callback:any, delay:any, ...args:any[])=>{
  const timer=nativeSetTimeout(callback,delay,...args); timerMs=scheduled(timer); return timer;
 }) as typeof setTimeout;
 try {
  const result=await classifyAndRoute({config,
   messages:[{role:'user',content:[{type:'text',text:'hi'}]}],
   judgeRuntime:{complete:async()=>({role:'assistant',content:[{type:'text',text:'<tier>simple</tier>'}]})}});
  assert.equal(result?.resolvedFrom,'judge'); assert.ok(timerMs !== undefined);
 } finally { globalThis.setTimeout=nativeSetTimeout; }
 cases.push({name:String(input), input: typeof input === 'number' && !Number.isFinite(input) ? String(input) : input,
  nonFinite:typeof input === 'number' && !Number.isFinite(input),
  timeoutMs:config.judgeTimeoutMs, diagnostics:parsed.diagnostics, timerMs});
}
assertBaseline(baseline); assert.deepEqual(await baselineSourceHashes(baseline,sources),sourceSha256);
await mkdir(resolve(process.argv[2]),{recursive:true});
await writeFile(join(resolve(process.argv[2]),'oracle.json'),JSON.stringify({sourceCommit,sourceSha256,node:process.version,cases},null,2)+'\n');
console.log(`recorded ${cases.length} real TS parser and classifier timer cases`);
