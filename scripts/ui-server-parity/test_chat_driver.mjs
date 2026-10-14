// No server/PTY: prove the second send cannot precede both explicit barriers.
import assert from 'node:assert/strict';
import { driveChatTurns } from './chat_driver.mjs';

let stage = 0;
const sent = [];
const clients = [0, 1].map(index => ({ frames: [], send(value) {
  assert.equal(stage, index * 3, 'no next send before previous barriers');
  assert.equal(value.sessionId, `session-${index}`);
  sent.push(index);
} }));
await driveChatTurns(clients, ['session-0', 'session-1'], sessionId => ({ sessionId }),
  async (test, label) => {
    const index = Math.floor(stage / 3), sessionId = `session-${index}`, runId = `run-${index}`;
    assert.equal(test(), false, label + ': cannot reuse prior turn evidence');
    if (stage % 3 === 0) {
      // A scan published during the turn is not the post-idle watcher barrier.
      for (const client of clients) client.frames.push({ type: 'projects_updated', projects: [{ sessions: [{ id: sessionId }] }] });
      clients[index].frames.push({ kind: 'text', content: 'EXP008_MOCK_OK' },
        { kind: 'complete', sessionId, runId, success: true, finishReason: 'completed' });
    } else if (stage % 3 === 1) {
      clients[0].frames.push({ type: 'session-activity', activity: { sessionId, processing: false, completedRunId: runId } });
      assert.equal(test(), false, 'one connection idle is insufficient');
      clients[1].frames.push(clients[0].frames.at(-1));
    } else {
      clients[0].frames.push({ type: 'projects_updated', projects: [{ sessions: [{ id: sessionId }] }] });
      assert.equal(test(), false, 'one connection watcher is insufficient');
      clients[1].frames.push(clients[0].frames.at(-1));
    }
    assert.equal(test(), true, label);
    stage++;
  });
assert.deepEqual(sent, [0, 1]);
assert.equal(stage, 6);

// A failed first turn must never send the second command.
const failed = [0, 1].map(index => ({ frames: [], send() {
  assert.equal(index, 0);
  this.frames.push({ kind: 'error' });
} }));
await assert.rejects(driveChatTurns(failed, ['a', 'b'], sessionId => ({ sessionId }),
  async test => assert(test())), /mock turn failed/);
console.log('PASS: serial sends, dual idle/watcher barriers, failure stops next turn');
