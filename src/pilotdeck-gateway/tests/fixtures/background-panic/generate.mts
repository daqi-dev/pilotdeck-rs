// Real baseline lifecycle methods; injected storage failure and predecessor gates.
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/task/runtime/BackgroundTaskRuntime.ts', 'src/task/storage/TaskOutputStore.ts',
  'src/agent/sub/BackgroundSubagentRuntime.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { BackgroundTaskRuntime } = await import(pathToFileURL(join(baseline, sources[0])).href);
const { BackgroundSubagentRuntime } = await import(pathToFileURL(join(baseline, sources[2])).href);
const defer = () => { let resolve!: () => void, reject!: (e: Error) => void; const promise = new Promise<void>((r, j) => { resolve = r; reject = j; }); return { promise, resolve, reject }; };
const tick = () => new Promise<void>(r => setImmediate(r));
const deadline = setTimeout(() => { throw Error('background panic probe deadline'); }, 10000);
const cases: any[] = [];
for (const family of ['write-a', 'write-b', 'flush-empty', 'flush-pair']) for (const n of [0, 1, 3]) for (const timing of ['before', 'after']) {
  for (const mode of ['sync', 'poll', 'err', 'ok']) {
    const point = family.startsWith('write-') ? family.replace('-', ':') : 'flush';
    const id = `${family}-${n}-${timing}-${mode}`, gate = defer(), entered = defer();
    const calls: string[] = [], diagnostics: string[] = [];
    const fail = () => { if (mode !== 'ok') throw Error('snapshot failure'); };
    const operation = (name: string) => {
      calls.push(name); if (name !== point) return Promise.resolve();
      entered.resolve(); if (mode === 'sync') fail();
      return gate.promise.then(fail);
    };
    const runtime = new BackgroundTaskRuntime({ diskSpillDir: '/pdrs-fixture/unused',
      snapshotStore: { load: () => [], write: (snapshot: any) => operation(`write:${snapshot.task.taskId}`), flush: () => operation('flush') },
      onDiagnostic: (e: Error) => diagnostics.push(e.message) });
    // Deterministic pending-start predecessor; the real dispose body is unchanged.
    const starts = defer(); runtime.whenStartsDrained = () => starts.promise;
    let predecessor = starts.promise;
    for (const taskId of family === 'flush-empty' ? [] : ['a', 'b']) {
      // Real settleEntry/persistEntry; no shell is needed for an already completed task.
      const entry = { task: { taskId, status: 'completed', type: 'local_bash', kind: 'bash',
        command: '', cwd: '/', startedAt: new Date(0), outputBytes: 0 },
        output: { flush: async () => {}, close: async () => {}, totalBytes: () => 0,
          hasDurableOutput: () => false }, done: Promise.resolve() };
      entry.done = predecessor.then(() => runtime.settleEntry(entry));
      predecessor = entry.done;
      runtime.entries.set(taskId, entry);
    }
    const observed = () => runtime.dispose().then(() => 'fulfilled', (e: Error) => e.message);
    const work = observed(); let finished = false; void work.then(() => finished = true);
    const observers = timing === 'before' ? Array.from({ length: n }, observed) : [];
    await tick(); assert.equal(finished, false, `${id}: predecessor bypass`);
    const cancel = new AbortController();
    const detached = Promise.race([work, new Promise(r => cancel.signal.addEventListener('abort', () => r('cancelled'), { once: true }))]);
    cancel.abort(); assert.equal(await detached, 'cancelled'); assert.equal(finished, false);
    starts.resolve(); await entered.promise;
    if (mode !== 'sync') {
      await tick(); assert.equal(finished, false, `${id}: bypassed storage gate`);
    }
    if (mode === 'poll' || mode === 'err') gate.reject(Error('snapshot failure')); else gate.resolve();
    const outcome = await work;
    if (timing === 'after') observers.push(...Array.from({ length: n }, observed));
    const outcomes = await Promise.all(observers);
    const expected = point === 'flush' && mode !== 'ok' ? 'snapshot failure' : 'fulfilled';
    assert.equal(outcome, expected); assert.deepEqual(outcomes, Array(n).fill(expected));
    assert.equal(await observed(), expected, 'late observer must retain result');
    assert.deepEqual(calls, family === 'flush-empty' ? ['flush'] : ['write:a', 'write:b', 'flush']);
    cases.push({ id, outcome, phase: runtime.state, calls, diagnostics });
  }
}
for (const mode of ['sync', 'poll', 'cancel-before', 'cancel-running', 'ok', 'err']) {
  const runtime = new BackgroundSubagentRuntime(), gate = defer(), entered = defer();
  let calls = 0;
  const task = runtime.start({ sessionId: 's', parentTurnId: 'p', subagentId: 'c', subagentType: 'general',
    run() { calls++; entered.resolve();
      if (mode === 'sync') throw Error('subagent synchronous failure');
      return gate.promise;
    } });
  if (mode === 'cancel-before') runtime.stop(task.taskId);
  else {
    await entered.promise;
    if (mode === 'poll') {
      await tick(); assert.equal(runtime.get(task.taskId).status, 'running');
    }
    if (mode === 'cancel-running') {
      await tick(); assert.equal(runtime.get(task.taskId).status, 'running');
      runtime.stop(task.taskId);
    }
  }
  const endedAtCancel = runtime.get(task.taskId).endedAt;
  if (['poll', 'cancel-running', 'err'].includes(mode)) gate.reject(Error(mode === 'err' ? 'ordinary error' : 'subagent asynchronous failure')); else gate.resolve();
  await runtime.entries.get(task.taskId).done;
  const results = await Promise.all(Array.from({ length: 3 }, () => runtime.stopAndWait(task.taskId)));
  assert.deepEqual(results, [false, false, false]);
  const result = runtime.get(task.taskId);
  if (mode.startsWith('cancel-')) assert.equal(result.endedAt, endedAtCancel, 'cancel timestamp retained');
  cases.push({ id: `subagent-${mode}`, status: result.status, error: result.error ?? null,
    ended: result.endedAt instanceof Date, calls });
}
clearTimeout(deadline);
assertBaseline(baseline); assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }) + '\n', { flag: 'wx' });
console.log(JSON.stringify(cases, null, 2));
