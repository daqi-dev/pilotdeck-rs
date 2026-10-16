import assert from 'node:assert/strict';
import test from 'node:test';
import {checkSuffix} from './check_suffix.mjs';

const row = side => ({side, exception: {message: 'Client WebSocket frames must be masked.'},
  frames: [], after101WireHex: '', socketBeforeCleanup: {readableEnded: false, destroyed: false,
    closed: false, errors: []}, baselineBeforeCleanup: {exitCode: null, signalCode: null}});
test('matching actual suffix outcome shape passes', () => checkSuffix([row('ts'), row('rust')]));
for (const field of ['readableEnded', 'destroyed', 'closed']) {
  test('one-side premature socket ' + field + ' fails', () => {
    const rows = [row('ts'), row('rust')]; rows[1].socketBeforeCleanup[field] = true;
    assert.throws(() => checkSuffix(rows));
  });
}
for (const [field, value] of [['exitCode', 1], ['signalCode', 'SIGTERM']]) {
  test('one-side baseline ' + field + ' fails', () => {
    const rows = [row('ts'), row('rust')]; rows[1].baselineBeforeCleanup[field] = value;
    assert.throws(() => checkSuffix(rows));
  });
}
test('one-side socket error fails', () => {
  const rows = [row('ts'), row('rust')]; rows[1].socketBeforeCleanup.errors.push({code: 'ECONNRESET', message: 'read ECONNRESET'});
  assert.throws(() => checkSuffix(rows));
});
test('missing shared fields do not pass by equal undefined', () => {
  const rows = [row('ts'), row('rust')];
  for (const r of rows) delete r.socketBeforeCleanup.closed;
  assert.throws(() => checkSuffix(rows));
});
