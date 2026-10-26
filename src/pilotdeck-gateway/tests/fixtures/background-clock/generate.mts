// Real fixed-baseline runtime; inject only its Clock and shell ports.
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/task/runtime/BackgroundTaskRuntime.ts', 'src/task/storage/TaskOutputStore.ts',
  'src/task/runtime/BackgroundTaskCompletionEvents.ts'];
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
clearTimeout(deadline);
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }) + '\n', { flag: 'wx' });
console.log(JSON.stringify(cases, null, 2));
