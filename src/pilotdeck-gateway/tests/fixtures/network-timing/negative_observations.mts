// Mutate raw evidence, not expectations: each new collector guard must reject.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { validate, matrix } from './observations.mts';
const raw = readFileSync(process.argv[2], 'utf8').trim().split('\n').map(line => JSON.parse(line));
const hashes = Object.fromEntries(raw.filter(r => r.source).map(r => [r.source, r.sha256]));
const dependency = raw.find(r => r.dependency);
const row = (rows: any[]) => rows.find(r => r.id === 'fetch_redirect/headers/fulfilled/tie-parent-first');
const io = (rows:any[],event='fulfilled',order='tie-parent-last') => rows.find(r=>r.id===`hooks/io-headers/${event}/${order}`);
const life = (rows:any[]) => rows.find(r=>r.id==='background/lifecycle/pending-stop/ordered');
const tests: [string, (rows: any[]) => void, RegExp][] = [
  ['io-provenance',rows=>io(rows).io.transport='timer',/I\/O provenance/],
  ['io-peer',rows=>io(rows).trace=io(rows).trace.filter((e:string)=>e!=='peer-request'),/real peer reached/],
  ['io-peer-order',rows=>io(rows).trace.push(io(rows).trace.shift()),/peer before callbacks/],
  ['io-duplicate',rows=>io(rows).trace.push('parent'),/unique I\/O event/],
  ['io-wire',rows=>io(rows).trace=io(rows).trace.filter((e:string)=>!e.startsWith('wire:')),/wire before gate/],
  ['io-gate',rows=>{const r=io(rows);[r.trace[2],r.trace[3]]=[r.trace[3],r.trace[2]];},/gate before dispatch/],
  ['io-order',rows=>{const r=io(rows);[r.trace[3],r.trace[4]]=[r.trace[4],r.trace[3]];},/I\/O dispatch order/],
  ['io-drain',rows=>{const r=io(rows,'fulfilled','continuation-first');r.trace=r.trace.filter((e:string)=>e!=='continuations-drained');},/I\/O continuation drain/],
  ['io-drain-order',rows=>{const r=io(rows,'fulfilled','continuation-first');r.trace=r.trace.filter((e:string)=>e!=='continuations-drained');r.trace.push('continuations-drained');},/drain before parent/],
  ['io-parent-drain',rows=>{const r=io(rows,'fulfilled','parent-first');r.trace=r.trace.filter((e:string)=>e!=='parent-continuations-drained');},/I\/O parent drain/],
  ['io-parent-drain-order',rows=>{const r=io(rows,'fulfilled','parent-first');r.trace=r.trace.filter((e:string)=>e!=='parent-continuations-drained');r.trace.push('parent-continuations-drained');},/parent drained before release/],
  ['io-idle-response',rows=>io(rows,'idle').trace.splice(1,0,'wire:onResponseStart'),/idle cannot have a response/],
  ['hook-delivery',rows=>io(rows).observable.delivery='throw',/hook returns instead/],
  ['hook-result',rows=>delete io(rows).observable.hook,/hook result retained/],
  ['hook-name',rows=>io(rows).observable.name='Error',/hook has no thrown name/],
  ['hook-content',rows=>io(rows).observable.content='wrong',/hook stdout projection/],
  ['hook-message',rows=>io(rows).observable.message='wrong',/hook stderr projection/],
  ['io-continuation-witness',rows=>{const r=io(rows,'fulfilled','continuation-first');r.trace=r.trace.filter((e:string)=>e!=='child-settled');},/actual continuation witness/],
  ['lifecycle-calls',rows=>life(rows).observable.calls++,/lifecycle call trace/],
  ['lifecycle-pending',rows=>life(rows).observable.pendingAfterStop=true,/lifecycle pending trace/],
  ['lifecycle-start',rows=>life(rows).trace[0]='initial:running',/background deferred run/],
  ['lifecycle-trace',rows=>life(rows).trace.push('run'),/background lifecycle trace/],
  ['source-list', rows => rows.splice(0, 1), /source coverage/],
  ['source-order', rows => rows[0].source = 'src/wrong.ts', /source order/],
  ['source-hash', rows => rows[0].sha256 = '0'.repeat(64), /source hash/],
  ['node-version', rows => rows[0].node = 'v0.0.0', /Node version/],
  ['dependency', rows => rows.find(r => r.dependency).version = '0', /installed undici fingerprint/],
  ['matrix-size', rows => rows.pop(), /matrix size/],
  ['unexpected-record', rows => rows.push({extra: true}), /unexpected probe output/],
  ['matrix-identity', rows => row(rows).id = 'duplicate', /matrix id/],
  ['matrix-dimensions', rows => row(rows).phase = 'body', /matrix phase/],
  ['target-timer', rows => row(rows).timers.push({...row(rows).timers.find((t: any) => t.kind === 'completion')}), /exactly one target timer/],
  ['parent-timer', rows => row(rows).timers = row(rows).timers.filter((t: any) => t.kind !== 'parent'), /exactly one parent timer/],
  ['finite-timer', rows => row(rows).timers.find((t: any) => t.kind === 'parent').start = NaN, /finite raw timer/],
  ['actual-deadline', rows => row(rows).timers.find((t: any) => t.kind === 'parent').start++, /actual Node deadline gap/],
  ['registration-order', rows => row(rows).timers.reverse(), /actual registration order/],
  ['timing-projection', rows => row(rows).timing.parentRegisteredFirst = false, /timing projection/],
  ['requested-duration', rows => {const r = row(rows); r.timing.requested++; r.timers.find((t: any) => t.kind === 'completion').requested++;}, /expected requested/],
  ['effective-duration', rows => {const r = row(rows); r.timing.effective++; for (const t of r.timers) t.delay++;}, /expected effective/],
  ['watchdog-source', rows => {rows.find(r => r.id === 'fetch_redirect/body/idle/continuation-first').timing.source = 'baseline timeout';}, /expected timing source/],
  ['parent-requested', rows => row(rows).timers.find((t: any) => t.kind === 'parent').requested++, /parent requested/],
  ['winning-callback', rows => row(rows).timeline = [], /winning callback executed/],
  ['callback-order', rows => row(rows).timeline = ['completion','parent'], /actual callback order/],
  ['call-trace', rows => row(rows).observable.calls++, /calls trace/],
  ['retry-trace', rows => row(rows).observable.retries++, /retry trace/],
  ['transport-reached', rows => {const r = row(rows); r.requests = []; r.observable.calls = 0; r.observable.retries = -1;}, /reached transport/],
  ['redirect-reached', rows => {const r = rows.find(r => r.id === 'fetch_redirect/redirect-hop/fulfilled/continuation-first'); r.requests = ['/start','/wrong'];}, /reached redirect hop/],
];
for (const source of raw.filter(r=>r.source)) tests.push([`source-item:${source.source}`,rows=>{rows.find(r=>r.source===source.source).sha256='0'.repeat(64);},/source hash/]);
for (const file of Object.keys(dependency.sourceSha256)) tests.push([`dependency-item:${file}`,rows=>{rows.find(r=>r.dependency).sourceSha256[file]='0'.repeat(64);},/installed undici fingerprint/]);
// Every added matrix member is independently protected against silent omission.
for (const cell of raw.filter(r=>r.id && (['mcp_get','mcp_post','hooks','background'].includes(r.path)||r.phase==='io-headers'))) {
  tests.push([`matrix-item:${cell.id}`,rows=>{rows.splice(rows.findIndex(r=>r.id===cell.id),1);},/matrix size/]);
  if(cell.timing) tests.push([`timing-item:${cell.id}`,rows=>{rows.find(r=>r.id===cell.id).timing.requested++;},/expected requested/]);
  if(cell.io) tests.push([`io-item:${cell.id}`,rows=>{rows.find(r=>r.id===cell.id).io.gate='timer';},/I\/O provenance/]);
  if(cell.phase==='lifecycle') tests.push([`lifecycle-item:${cell.id}`,rows=>{rows.find(r=>r.id===cell.id).trace.push('unexpected');},/background lifecycle trace/]);
}
assert.equal(validate(raw, hashes, dependency).length, matrix().length);
for (const [name, mutate, message] of tests) {
  const changed = structuredClone(raw); mutate(changed);
  assert.throws(() => validate(changed, hashes, dependency), message, name);
  assert.equal(validate(raw, hashes, dependency).length, matrix().length, `${name}: restored positive`);
  console.log(`${name}: rejected mutant, restored complete capture PASS`);
}
console.log(`${tests.length} raw-evidence negative controls PASS`);
