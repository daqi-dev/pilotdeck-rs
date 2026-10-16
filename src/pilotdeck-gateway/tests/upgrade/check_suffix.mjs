import assert from 'node:assert/strict';

// Compare observations before our cleanup; PID and stack contain side identity.
export function checkSuffix(rows) {
  assert.equal(rows.length, 2, 'two suffix observations required');
  assert.deepEqual(rows.map(row => row.side).sort(), ['rust', 'ts']);
  for (const row of rows) {
    assert.equal(row.exception.message, 'Client WebSocket frames must be masked.');
    assert.deepEqual(row.frames, []);
    assert.equal(row.after101WireHex, '');
    for (const key of ['readableEnded', 'destroyed', 'closed']) {
      assert.equal(typeof row.socketBeforeCleanup[key], 'boolean', 'missing socket ' + key);
    }
    assert.ok(Array.isArray(row.socketBeforeCleanup.errors));
    for (const error of row.socketBeforeCleanup.errors) {
      assert.equal(typeof error.code, 'string');
      assert.equal(typeof error.message, 'string');
    }
    assert.ok(row.baselineBeforeCleanup.exitCode === null || Number.isInteger(row.baselineBeforeCleanup.exitCode));
    assert.ok(row.baselineBeforeCleanup.signalCode === null || typeof row.baselineBeforeCleanup.signalCode === 'string');
  }
  assert.deepEqual(rows[0].socketBeforeCleanup, rows[1].socketBeforeCleanup, 'suffix socket outcome differs');
  assert.deepEqual(rows[0].baselineBeforeCleanup, rows[1].baselineBeforeCleanup, 'suffix baseline exit differs');
}
