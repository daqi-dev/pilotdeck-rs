// Real fixed-baseline runtime; inject only its Clock and shell ports.
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/task/runtime/BackgroundTaskRuntime.ts', 'src/task/storage/TaskOutputStore.ts',
  'src/task/runtime/BackgroundTaskCompletionEvents.ts', 'src/agent/sub/BackgroundSubagentRuntime.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { BackgroundTaskRuntime } = await import(pathToFileURL(join(baseline, sources[0])).href);
const deadline = setTimeout(() => { throw Error('clock probe exceeded 10 seconds'); }, 10000);
const cases = [];
for (const failAt of [1, 2]) {
  let calls = 0, shellCalls = 0;
  const runtime: any = new BackgroundTaskRuntime({ maxTasks: 1,
    now() { if (++calls === failAt) throw Error('injected clock panic'); return new Date(0); },
    shell: { async start() { shellCalls++; throw Error('injected spawn error'); } },
  });
  const spec = { command: 'unused', cwd: '/', sessionId: 's', agentId: 'a' };
  const error = await runtime.start(spec).then(() => 'unexpected', (e: Error) => e.message);
  const activeStarts = runtime.activeStarts;
  const entries = runtime.list().map((t: any) => ({ status: t.status, ended: !!t.endedAt }));
  const spawned = shellCalls;
  const retry = (await runtime.start(spec)).status;
  let disposed = false;
  const observers = Array.from({ length: 3 }, () => runtime.dispose());
  void Promise.all(observers).then(() => disposed = true);
  // All injected operations resolve immediately. Drain their reactions without
  // resolving the missing done promise; B2 must still be pending.
  for (let i = 0; i < 3; i++) await new Promise<void>(r => setImmediate(r));
  assert.equal(error, 'injected clock panic');
  assert.equal(activeStarts, 0);
  assert.equal(retry, 'failed');
  assert.deepEqual(entries, failAt === 1 ? [] : [{ status: 'failed', ended: false }]);
  assert.equal(disposed, failAt === 1);
  cases.push({ id: `B${failAt}`, error, activeStarts, entries, spawned, retry, disposed });
}
const tailCases = [];
const turn = () => new Promise<void>(r => setImmediate(r));
for (const exit of ['zero', 'nonzero', 'reject', 'never']) {
  let resolveExit!: (value: any) => void, rejectExit!: (error: Error) => void;
  const exited = new Promise((resolve, reject) => { resolveExit = resolve; rejectExit = reject; });
  let calls = 0, events = 0;
  const unhandled: string[] = [];
  const observe = (error: any) => unhandled.push(error.message);
  process.on('unhandledRejection', observe);
  const runtime: any = new BackgroundTaskRuntime({ maxTasks: 1,
    now() { if (++calls === 2) throw Error('terminal clock panic'); return new Date(0); },
    onCompletion() { events++; },
    shell: { async start() { return { pid: exit === 'never' ? undefined : 42,
      exit: exited, terminate() {} }; } },
  });
  const spec = { command: 'unused', cwd: '/', sessionId: 's', agentId: 'a' };
  const task = await runtime.start(spec);
  let done = false, disposed = false, stopped = false;
  void runtime.entries.get(task.taskId).done.then(() => done = true);
  if (exit === 'reject') rejectExit(Error('exit rejection'));
  else if (exit !== 'never') resolveExit({ exitCode: exit === 'zero' ? 0 : 7 });
  for (let i = 0; i < 3; i++) await turn();
  const retry = await runtime.start(spec).then(() => false, () => true);
  const abort = new AbortController();
  const waiting = runtime.wait(task.taskId, { abortSignal: abort.signal });
  abort.abort();
  const outcome = (await waiting).outcome;
  void runtime.stop(task.taskId, { graceMs: 0 }).then(() => stopped = true);
  void Promise.all([runtime.dispose(), runtime.dispose()]).then(() => disposed = true);
  for (let i = 0; i < 3; i++) await turn();
  const row = { id: `B-${exit}`, status: task.status, ended: !!task.endedAt,
    done, events, retryRejected: retry, outcome, stopped, disposed, unhandled };
  assert.deepEqual(row, { id: `B-${exit}`, status: 'running', ended: false,
    done: false, events: 0, retryRejected: true, outcome: 'aborted', stopped: false,
    disposed: false, unhandled: exit === 'never' ? [] : ['terminal clock panic'] });
  tailCases.push(row);
  process.off('unhandledRejection', observe);
}
const { BackgroundSubagentRuntime } = await import(pathToFileURL(join(baseline, sources[3])).href);
for (const mode of ['ok', 'err', 'pending', 'running']) {
  const runtime: any = new BackgroundSubagentRuntime();
  let enter!: () => void, release!: () => void, ran = false, cancelled = false;
  const entered = new Promise<void>(r => enter = r);
  const gate = new Promise<void>(r => release = r);
  const task = runtime.start({ sessionId: 's', parentTurnId: 'p', subagentId: 'a',
    subagentType: 'test', async run(signal: AbortSignal) {
      ran = true;
      const abort = new Promise<void>(r => signal.addEventListener('abort', () => {
        cancelled = true; r();
      }, { once: true }));
      enter();
      if (mode === 'running') await abort;
      else { await gate; if (mode === 'err') throw Error('body failure'); }
    } });
  if (mode === 'pending') runtime.stop(task.taskId);
  else { await entered; if (mode === 'running') runtime.stop(task.taskId); else release(); }
  await runtime.entries.get(task.taskId).done;
  const row = { id: `S-${mode}`, status: task.status, ended: !!task.endedAt,
    error: task.error ?? null, ran, cancelled };
  assert.deepEqual(row, { id: `S-${mode}`,
    status: mode === 'ok' ? 'completed' : mode === 'err' ? 'failed' : 'cancelled',
    ended: true, error: mode === 'err' ? 'body failure' : null,
    ran: mode !== 'pending', cancelled: mode === 'running' });
  tailCases.push(row);
}
clearTimeout(deadline);
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases, tailCases }) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ cases, tailCases }, null, 2));
