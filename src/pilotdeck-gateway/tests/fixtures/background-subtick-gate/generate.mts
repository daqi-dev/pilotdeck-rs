// #957: real TS subagent/SDK lifecycle with explicit entered/release barriers.
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#957 requires Node v22.22.0');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/agent/sub/BackgroundSubagentRuntime.ts', 'src/cli/ProjectRuntimeRegistry.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { BackgroundSubagentRuntime } = await import(pathToFileURL(join(baseline, sources[0])).href);
const { ProjectRuntimeRegistry } = await import(pathToFileURL(join(baseline, sources[1])).href);
const deferred = () => {
  let resolve!: () => void;
  const promise = new Promise<void>(done => { resolve = done; });
  return { promise, resolve };
};
async function bounded<T>(work: Promise<T>, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(Error(`#957 ${label} deadline`)), 8000);
  });
  try { return await Promise.race([work, deadline]); }
  finally { clearTimeout(timer!); }
}
const cases: any[] = [];
for (const behavior of ['release', 'fail', 'block', 'ignore-abort']) {
  const runtime = new BackgroundSubagentRuntime();
  const entered = deferred();
  const release = deferred();
  const registry = Object.create(ProjectRuntimeRegistry.prototype);
  // Supply only the real registry's project lookup seam; stop implementation is untouched.
  (registry as any).resolve = () => ({ backgroundSubagents: runtime });
  const task = runtime.start({ sessionId: 's1', parentTurnId: 'turn', subagentId: 'sa', subagentType: 'general',
    run: async (signal: AbortSignal) => {
      entered.resolve();
      if (behavior === 'fail') throw Error('boom');
      if (behavior === 'release' || behavior === 'ignore-abort') { await release.promise; return; }
      await new Promise<void>((_, reject) => {
        signal.addEventListener('abort', () => reject(Error('stopped')), { once: true });
      });
    },
  });
  const start = task.status;
  const done = (runtime as any).entries.get(task.taskId).done;
  const snapshot = () => {
    const state = runtime.get(task.taskId)!;
    return { status: state.status, ended: state.endedAt !== undefined, error: state.error ?? null };
  };
  try {
    await bounded(entered.promise, 'entered');
    if (behavior === 'fail') await bounded(done, 'failed completion');
    const held = snapshot();
    assert.equal(held.status, behavior === 'fail' ? 'failed' : 'running', '#957 held status');
    if (behavior === 'release') {
      release.resolve();
      await bounded(done, 'released completion');
    }
    let stopSettled = false;
    const stop = registry.stopBackgroundTaskForSdk({ sessionKey: 's1', taskId: task.taskId })
      .then((value: any) => { stopSettled = true; return value; });
    let cancelled = null;
    let stopPending = null;
    if (behavior === 'ignore-abort') {
      // The real stop has synchronously committed cancellation before its await.
      cancelled = snapshot();
      await Promise.resolve();
      stopPending = !stopSettled;
      assert.equal(cancelled.status, 'cancelled', '#957 cancellation committed before release');
      assert.equal(stopPending, true, '#957 stop waits for held body');
      release.resolve();
    }
    const stopped = await bounded(stop, 'SDK stop');
    const terminal = snapshot();
    cases.push({ behavior, start, held, cancelled, stopPending, stopped, terminal });
  } finally {
    release.resolve();
    runtime.shutdown();
    await bounded(done, 'cleanup');
  }
}
const oracle = { sourceCommit, sourceSha256, node: process.version, cases };
const out = resolve(process.argv[2]);
await mkdir(out, { recursive: true });
await writeFile(join(out, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify(oracle, null, 2));
