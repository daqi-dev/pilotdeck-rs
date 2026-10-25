// Mutate raw evidence, not expectations: each new collector guard must reject.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { validate } from './observations.mts';
const raw = readFileSync(process.argv[2], 'utf8').trim().split('\n').map(line => JSON.parse(line));
const hashes = Object.fromEntries(raw.filter(r => r.source).map(r => [r.source, r.sha256]));
const dependency = raw.find(r => r.dependency);
const row = (rows: any[]) => rows.find(r => r.id === 'fetch_redirect/headers/fulfilled/tie-parent-first');
const tests: [string, (rows: any[]) => void, RegExp][] = [
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
  ['winning-callback', rows => row(rows).timeline = [], /winning callback executed/],
  ['callback-order', rows => row(rows).timeline = ['completion','parent'], /actual callback order/],
  ['call-trace', rows => row(rows).observable.calls++, /calls trace/],
  ['retry-trace', rows => row(rows).observable.retries++, /retry trace/],
  ['transport-reached', rows => {const r = row(rows); r.requests = []; r.observable.calls = 0; r.observable.retries = -1;}, /reached transport/],
  ['redirect-reached', rows => {const r = rows.find(r => r.id === 'fetch_redirect/redirect-hop/fulfilled/continuation-first'); r.requests = ['/start','/wrong'];}, /reached redirect hop/],
];
assert.equal(validate(raw, hashes, dependency).length, 100);
for (const [name, mutate, message] of tests) {
  const changed = structuredClone(raw); mutate(changed);
  assert.throws(() => validate(changed, hashes, dependency), message, name);
  assert.equal(validate(raw, hashes, dependency).length, 100, `${name}: restored positive`);
  console.log(`${name}: rejected mutant, restored 100-cell capture PASS`);
}
console.log(`${tests.length} raw-evidence negative controls PASS`);
