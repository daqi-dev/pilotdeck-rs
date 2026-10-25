import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/gateway/SessionRouter.ts', 'src/cli/createLocalGateway.ts',
  ...['AgentHandle', 'AgentFactoryProvider', 'AgentRegistry', 'ScopedServiceRegistry'].map(n => `src/agent/scope/${n}.ts`)];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { SessionRouter } = await import(pathToFileURL(join(baseline, sources[0])).href);
const context = (sessionKey = 'user', projectKey: string | undefined = 'p') =>
  ({ sessionKey, projectKey, channelKey: 'web', allowedTools: ['read'], disallowedTools: [] });
function harness(mode = 'normal', enabled = true) {
  let now = 7, reads = 0, release!: () => void;
  const gate = new Promise<void>(done => { release = done; });
  const events: any[] = []; const messages: number[] = [];
  const router = new SessionRouter({ idleSessionTimeoutMs: 100, idleSweepIntervalMs: 0,
    now: () => new Date(now),
    createSession: () => ({ sessionId: 'fake', abort() {},
      snapshot() { reads++; if (mode === 'throw') throw Error('snapshot failed');
        return mode === 'inherited-messages' ? Object.create({ messages: [1, 2] }) : mode === 'missing' ? {} : { messages: mode === 'string' ? '😀' :
          mode === 'inherited-length' ? Object.create({ length: 'two' }) : mode === 'length' ? { length: 'two' } : mode === 'undefined' ? { length: undefined } :
          mode === 'null' ? { length: null } : messages };  },
      async dispose() { events.push('dispose'); await gate; events.push('disposed'); messages.push(1);
        if (mode === 'dispose-error') throw Error('dispose failed'); },
    }) as any,
    recreateSession: () => ({ sessionId: 'replacement', snapshot: () => ({ messages: [] }), abort() {}, dispose() {} }) as any,
    onSessionEvict: (_key, reason) => { events.push(`evict:${reason}`); messages.push(2); },
    onSessionIdleEvict: enabled ? (key, snapshot) => events.push({ key, snapshot,
      cached: router.cachedSessionCount(), busy: router.hasActiveUserTurn('p') }) : undefined,
  });
  return { router, events, release, reads: () => reads, time: (n: number) => { now = n; } };
}
const active: any[] = [];
for (const [key, project, query, cached, turn] of [
  ['user', 'p', 'p', true, true], ['always-on/x', 'p', 'p', true, true],
  ['cron:x', 'p', 'p', true, true], ['always-on', 'p', 'p', true, true],
  ['Cron:x', 'p', 'p', true, true], ['xcron:x', 'p', 'p', true, true],
  ['user', 'p', 'other', true, true], ['user', '', '', true, true],
  ['user', null, '', true, true], ['user', 'p', 'p', false, true],
  ['user', 'p', 'p', true, false],
] as const) {
  const h = harness();
  if (cached) await h.router.getOrCreate({ ...context(key), projectKey: project ?? undefined });
  if (turn) h.router.beginTurn(key, '');
  active.push({ key, project, query, cached, turn, busy: h.router.hasActiveUserTurn(query) });
  h.release(); await h.router.shutdown();
}
const idle: any[] = [];
for (const mode of ['normal', 'missing', 'throw', 'dispose-error', 'string', 'inherited-messages', 'inherited-length', 'length', 'undefined', 'null', 'disabled']) {
  const h = harness(mode, mode !== 'disabled');
  await h.router.getOrCreate(context());
  h.time(107); const boundary = h.router.sessionCount();
  h.router.beginTurn('user', 'run'); h.time(108);
  const during = { cached: h.router.sessionCount(), busy: h.router.hasActiveUserTurn('p') };
  h.router.endTurn('user', 'run'); h.time(209);
  const removed = h.router.sessionCount();
  // Drain is suspended: admission without a cached record is not a user turn.
  h.router.beginTurn('user', 'next');
  h.router.endTurn('user', 'next');
  const draining = { cached: removed, busy: h.router.hasActiveUserTurn('p'), events: [...h.events], reads: h.reads() };
  h.release(); await h.router.shutdown().catch(() => {});
  idle.push({ mode, boundary, during, draining, events: h.events, reads: h.reads() });
}
const nonIdle: any[] = [];
for (const action of ['close', 'recreate', 'shutdown']) {
  const h = harness(); await h.router.getOrCreate(context()); h.release();
  if (action === 'close') await h.router.close('user');
  if (action === 'recreate') { h.router.markAllDirty(); await h.router.getOrCreate(context()); }
  if (action === 'shutdown') await h.router.shutdown();
  nonIdle.push({ action, idleCalls: h.events.filter(e => typeof e === 'object').length, reads: h.reads() });
  await h.router.shutdown();
}
// TS shallow-copies context but retains tool-array aliases.
const ownership = harness(); const original = context();
await ownership.router.getOrCreate(original); ownership.time(108);
ownership.router.sessionCount(); ownership.release(); await ownership.router.shutdown();
const captured = ownership.events.find(e => typeof e === 'object').snapshot.context;
captured.allowedTools.push('changed');
const sharedTools = original.allowedTools.includes('changed');
assertBaseline(baseline); await baselineSourceHashes(baseline, sources);
const result = { sourceCommit, sourceSha256, active, idle, nonIdle, sharedTools };
await mkdir(process.argv[2], { recursive: true });
await writeFile(join(process.argv[2], 'oracle.json'), JSON.stringify(result, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify(result));
