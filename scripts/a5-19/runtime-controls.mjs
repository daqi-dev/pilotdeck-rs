import assert from 'node:assert/strict';
import { appendObserver } from './append-observer.mjs';
import { cleanupAll } from './cleanup-observer.mjs';
const pending = Promise.resolve('original');
const tap = appendObserver(() => pending, { commit() {}, failure() {} });
assert.equal(tap.append({}), pending);
await tap.drain();
const sync = new Error('sync'); let seen;
const throwing = appendObserver(() => { throw sync; }, { commit() {}, failure(input, e) { seen = e; } });
assert.throws(() => throwing.append({}), e => e === sync); assert.equal(seen, sync); await throwing.drain();
const rejected = Promise.reject(new Error('product'));
const rejectedTap = appendObserver(() => rejected, { commit() {}, failure() {} });
assert.equal(rejectedTap.append({}), rejected); await assert.rejects(rejected, /product/); await rejectedTap.drain();
for (const phase of ['commit', 'failure']) {
  const promise = phase === 'commit' ? Promise.resolve() : Promise.reject(new Error('product'));
  const broken = appendObserver(() => promise, { commit() { throw new Error('observer'); }, failure() { throw new Error('observer'); } });
  assert.equal(broken.append({}), promise);
  await assert.rejects(broken.drain(), e => e instanceof AggregateError && e.errors[0].message === 'observer');
}
for (let broken = 0; broken < 3; broken++) {
  const attempted = []; let restored = false;
  await assert.rejects(cleanupAll([0, 1, 2].map(i => async () => { attempted.push(i); if (i === broken) throw new Error('cleanup'); }), () => { restored = true; }), AggregateError);
  assert.deepEqual(attempted, [0, 1, 2]); assert.equal(restored, true);
}
console.log('PASS original promise identity / same sync throw / original rejection / both observer failures / all cleanup failure positions and restoration');
