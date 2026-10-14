// Deterministic two-session input schedule; all received frames remain evidence.
import assert from 'node:assert/strict';

export async function driveChatTurns(clients, sessionIds, command, until) {
  assert.equal(clients.length, sessionIds.length);
  for (const [index, sessionId] of sessionIds.entries()) {
    const offsets = clients.map(client => client.frames.length);
    const fresh = i => clients[i].frames.slice(offsets[i]);
    clients[index].send(command(sessionId));
    await until(() => fresh(index).some(f => f.kind === 'complete' || f.kind === 'error'),
      `mock turn ${index + 1} complete`);
    assert(!fresh(index).some(f => f.kind === 'error'), 'mock turn failed');
    const complete = fresh(index).find(f => f.kind === 'complete');
    assert.equal(complete.sessionId, sessionId);
    assert.equal(complete.success, true);
    assert.equal(complete.finishReason, 'completed');
    assert(typeof complete.runId === 'string' && complete.runId.length > 0);
    assert(fresh(index).some(f => JSON.stringify(f).includes('EXP008_MOCK_OK')));
    // A complete on the sender can race the other client's activity broadcast.
    // Wait for both clients and the project watcher before starting the next turn.
    const isIdle = f =>
      f.type === 'session-activity' && f.activity?.sessionId === sessionId &&
      f.activity.processing === false && f.activity.completedRunId === complete.runId;
    await until(() => clients.every((_, i) => fresh(i).some(isIdle)),
    `mock turn ${index + 1} idle on both connections`);
    await until(() => clients.every((_, i) => fresh(i).slice(fresh(i).findIndex(isIdle) + 1).some(f =>
      f.type === 'projects_updated' &&
      f.projects?.some(project => project.sessions?.some(session => session.id === sessionId)))),
    `mock turn ${index + 1} project watcher on both connections`);
  }
}
