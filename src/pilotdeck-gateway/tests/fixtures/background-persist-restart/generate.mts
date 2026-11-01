// #1233: real runtime + real JsonFile snapshot store + real detached children;
// dispose flush failures, spill write failures, and crash-style restart recovery.
import assert from 'node:assert/strict';
import { promises as fs, readFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1233 pinned Node');
assert(process.env.PDRS_BASELINE && process.argv[2], '#1233 baseline/output required');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/task/runtime/BackgroundTaskRuntime.ts',
  'src/task/storage/TaskOutputStore.ts',
  'src/task/storage/BackgroundTaskSnapshotStore.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { BackgroundTaskRuntime } = await import(pathToFileURL(join(baseline, sources[0])).href);
const { TaskOutputStore } = await import(pathToFileURL(join(baseline, sources[1])).href);
const { JsonFileBackgroundTaskSnapshotStore } = await import(pathToFileURL(join(baseline, sources[2])).href);
const deadline = setTimeout(() => { throw Error('#1233 fixture exceeded 60 seconds'); }, 60000);
const terminal = (status: string) => status === 'completed' || status === 'failed' || status === 'cancelled';
const normText = (text: string, stateDir: string, taskMap: Record<string, string>) => {
  let out = text.split(stateDir).join('<state>');
  for (const [taskId, name] of Object.entries(taskMap)) out = out.split(taskId).join(name);
  return out;
};
const normSnapshot = (entry: any, taskMap: Record<string, string>) => ({
  taskId: taskMap[entry.task.taskId],
  status: entry.task.status,
  exitCode: entry.task.exitCode ?? null,
  hasPid: typeof entry.task.pid === 'number',
  interrupted: entry.task.interrupted,
  startedAt: entry.task.startedAt,
  endedAt: entry.task.endedAt ?? null,
  outputBytes: entry.task.outputBytes,
  output: { totalBytes: entry.output.totalBytes, persisted: entry.output.persisted },
});
const readSlices = (runtime: any, taskId: string) => {
  const value = runtime.getOutput(taskId, 0);
  return { content: value.content, nextOffset: value.nextOffset, totalBytes: value.totalBytes, truncated: value.truncated };
};
const normEvent = (event: any, taskMap: Record<string, string>) => ({
  taskId: taskMap[event.taskId] ?? event.taskId,
  status: event.status,
  exitCode: event.exitCode,
  outputPreview: event.outputPreview,
  totalBytes: event.totalBytes,
  startedAt: event.startedAt,
  endedAt: event.endedAt,
});
const makeSinks = () => ({ events: [] as any[], diagnostics: [] as string[] });
const makeRuntime = (stateDir: string, snapshotStore: any, sinks: { events: any[]; diagnostics: string[] }) =>
  new (BackgroundTaskRuntime as any)({
    diskSpillDir: join(stateDir, 'output'),
    snapshotStore,
    now: () => new Date(0),
    onCompletion: (event: any) => sinks.events.push(event),
    onDiagnostic: (error: unknown) => sinks.diagnostics.push(error instanceof Error ? error.message : String(error)),
  });
const reopen = (stateDir: string) =>
  new (JsonFileBackgroundTaskSnapshotStore as any)({ filePath: join(stateDir, 'state.json') });
const cases: any[] = [];

// A. Terminal tasks + graceful dispose, then restart into a second runtime.
{
  const root = await fs.mkdtemp(join(tmpdir(), 'pdrs-1233-a-'));
  const stateDir = join(root, 'state');
  const sinks = makeSinks();
  const runtime = makeRuntime(stateDir, reopen(stateDir), sinks);
  const printf = await runtime.start({ command: 'printf abcdef', cwd: root });
  const silent = await runtime.start({ command: 'true', cwd: root });
  await runtime.waitFor(printf.taskId);
  await runtime.waitFor(silent.taskId);
  await runtime.dispose();
  const taskMap = { [printf.taskId]: 'task-printf', [silent.taskId]: 'task-silent' };
  const snapshot = JSON.parse(readFileSync(join(stateDir, 'state.json'), 'utf8')).tasks
    .slice().sort((a: any, b: any) => (a.task.command < b.task.command ? -1 : 1))
    .map((entry: any) => normSnapshot(entry, taskMap));
  const sinks2 = makeSinks();
  const runtime2 = makeRuntime(stateDir, reopen(stateDir), sinks2);
  const restored = runtime2.list().slice().sort((a: any, b: any) => (a.command < b.command ? -1 : 1))
    .map((task: any) => ({
      taskId: taskMap[task.taskId],
      status: task.status,
      exitCode: task.exitCode ?? null,
      hasPid: typeof task.pid === 'number',
      endedAt: task.endedAt instanceof Date ? task.endedAt.toISOString() : null,
      outputBytes: task.outputBytes,
    }));
  const readPrintf = readSlices(runtime2, printf.taskId);
  const readSilent = readSlices(runtime2, silent.taskId);
  await runtime2.dispose();
  cases.push({
    id: 'terminal-recovery',
    snapshot,
    restored,
    readPrintf,
    readSilent,
    firstRuntimeEvents: sinks.events.map((event: any) => normEvent(event, taskMap)).sort((a: any, b: any) => (a.taskId < b.taskId ? -1 : 1)),
    secondRuntimeEvents: sinks2.events.length,
    secondRuntimeDiagnostics: sinks2.diagnostics,
  });
  await fs.rm(root, { recursive: true, force: true });
}

// B. Crash-style recovery: a second runtime reads state the first still owns.
{
  const root = await fs.mkdtemp(join(tmpdir(), 'pdrs-1233-b-'));
  const stateDir = join(root, 'state');
  const sinks = makeSinks();
  const runtime = makeRuntime(stateDir, reopen(stateDir), sinks);
  const sleep = await runtime.start({ command: 'printf abcdef; sleep 30', cwd: root });
  const taskMap = { [sleep.taskId]: 'task-sleep' };
  const snapshotPath = join(stateDir, 'state.json');
  const spillPath = join(stateDir, 'output', `${sleep.taskId}.log`);
  let running: any = null;
  for (let attempt = 0; attempt < 200 && running === null; attempt++) {
    await new Promise<void>(resolveDone => setTimeout(resolveDone, 50));
    let spillBytes = -1;
    try { spillBytes = (await fs.readFile(spillPath)).length; } catch { /* not written yet */ }
    if (spillBytes !== 6) continue;
    try {
      const parsed = JSON.parse(readFileSync(snapshotPath, 'utf8'));
      const entry = parsed.tasks.find((x: any) => x.task.taskId === sleep.taskId);
      if (entry && entry.task.status === 'running' && entry.output.totalBytes === 6 && entry.output.persisted === true) running = entry;
    } catch { /* snapshot not yet written */ }
  }
  assert(running, '#1233 running snapshot with six durable bytes must appear');
  const sinks2 = makeSinks();
  const runtime2 = makeRuntime(stateDir, reopen(stateDir), sinks2);
  const restored = runtime2.list().map((task: any) => ({
    status: task.status,
    hasPid: typeof task.pid === 'number',
    exitCode: task.exitCode === undefined ? 'absent' : task.exitCode,
    endedAt: task.endedAt === undefined ? 'absent' : 'present',
    outputBytes: task.outputBytes,
  }));
  const read = readSlices(runtime2, sleep.taskId);
  const waited = await runtime2.wait(sleep.taskId);
  let stopError: any = 'none';
  try { await runtime2.stop(sleep.taskId); } catch (error: any) {
    stopError = { name: error.name, message: normText(error.message, stateDir, taskMap) };
  }
  // Cleanup through the owning runtime: SIGTERM the sleeping child, settle, dispose both.
  await runtime.stop(sleep.taskId);
  await runtime.waitFor(sleep.taskId);
  await runtime.dispose();
  await runtime2.dispose();
  cases.push({
    id: 'crashed-running-recovery',
    runningSnapshot: normSnapshot(running, taskMap),
    restored,
    read,
    waitOutcome: waited.outcome,
    stopError,
    firstRuntimeEvents: sinks.events.map((event: any) => normEvent(event, taskMap)),
    secondRuntimeEvents: sinks2.events.length,
    secondRuntimeDiagnostics: sinks2.diagnostics,
  });
  await fs.rm(root, { recursive: true, force: true });
}

// C. Spill writes all fail: the snapshot still claims persisted output, and a
//    restart of those durable claims fails closed in the constructor.
{
  const root = await fs.mkdtemp(join(tmpdir(), 'pdrs-1233-c-'));
  const stateDir = join(root, 'state');
  await fs.mkdir(stateDir, { recursive: true });
  await fs.writeFile(join(stateDir, 'output'), 'blocked');
  const sinks = makeSinks();
  const runtime = makeRuntime(stateDir, reopen(stateDir), sinks);
  const printf = await runtime.start({ command: 'printf abcdef', cwd: root });
  await runtime.waitFor(printf.taskId);
  const taskMap = { [printf.taskId]: 'task-printf' };
  const inMemory = readSlices(runtime, printf.taskId);
  await runtime.dispose();
  const snapshot = JSON.parse(readFileSync(join(stateDir, 'state.json'), 'utf8')).tasks
    .map((entry: any) => normSnapshot(entry, taskMap));
  let restart: any;
  try {
    makeRuntime(stateDir, reopen(stateDir), makeSinks());
    restart = 'constructed';
  } catch (error: any) { restart = { name: error.name, message: normText(error.message, stateDir, taskMap) }; }
  cases.push({
    id: 'spill-write-failure',
    inMemory,
    completionEvents: sinks.events.map((event: any) => event.status),
    snapshot,
    restart,
  });
  await fs.rm(root, { recursive: true, force: true });
}

// D. dispose() with a flush that rejects / throws synchronously: the runtime
//    stays draining, later callers reuse the same rejection, reads and start
//    refusal behave as observed from a live host.
for (const mode of ['reject', 'sync-throw'] as const) {
  const root = await fs.mkdtemp(join(tmpdir(), 'pdrs-1233-d-'));
  const stateDir = join(root, 'state');
  const sinks = makeSinks();
  const store = reopen(stateDir);
  const wrapped = {
    load: () => store.load(),
    write: (snapshot: any) => store.write(snapshot),
    flush: () => (mode === 'sync-throw'
      ? (() => { throw Error('#1233 flush refused'); })()
      : Promise.reject(Error('#1233 flush refused'))),
  };
  const runtime = makeRuntime(stateDir, wrapped, sinks);
  const printf = await runtime.start({ command: 'printf abcdef', cwd: root });
  await runtime.waitFor(printf.taskId);
  const taskMap = { [printf.taskId]: 'task-printf' };
  const outcome = await runtime.dispose().then(() => 'fulfilled', (error: Error) => `rejected: ${error.message}`);
  const again = await runtime.dispose().then(() => 'fulfilled', (error: Error) => `rejected: ${error.message}`);
  assert.equal(again, outcome, '#1233 dispose memoizes the flush failure');
  const listed = runtime.list().map((task: any) => ({ status: task.status, exitCode: task.exitCode ?? null }));
  const readAfterDispose = readSlices(runtime, printf.taskId);
  let startError = 'none';
  try { await runtime.start({ command: 'true', cwd: root }); } catch (error: any) { startError = error.message; }
  cases.push({
    id: `dispose-flush-${mode}`,
    outcome,
    state: runtime.state,
    listed,
    readAfterDispose,
    startError,
    completionEvents: sinks.events.map((event: any) => event.status),
    diagnostics: sinks.diagnostics,
  });
  await fs.rm(root, { recursive: true, force: true });
}

// E. Restart against output that no longer matches the durable claims.
for (const variant of ['mismatch', 'missing'] as const) {
  const root = await fs.mkdtemp(join(tmpdir(), 'pdrs-1233-e-'));
  const stateDir = join(root, 'state');
  const sinks = makeSinks();
  const runtime = makeRuntime(stateDir, reopen(stateDir), sinks);
  const printf = await runtime.start({ command: 'printf abcdef', cwd: root });
  await runtime.waitFor(printf.taskId);
  await runtime.dispose();
  const taskMap = { [printf.taskId]: 'task-printf' };
  const spillPath = join(stateDir, 'output', `${printf.taskId}.log`);
  assert.equal((await fs.readFile(spillPath)).length, 6, '#1233 spill written before restart');
  if (variant === 'mismatch') await fs.appendFile(spillPath, 'X');
  else await fs.unlink(spillPath);
  let restart: any;
  try {
    makeRuntime(stateDir, reopen(stateDir), makeSinks());
    restart = 'constructed';
  } catch (error: any) { restart = { name: error.name, message: normText(error.message, stateDir, taskMap) }; }
  cases.push({ id: `restart-${variant}`, restart });
  await fs.rm(root, { recursive: true, force: true });
}

// F. Terminal settle whose snapshot write fails: the completion notification is
//    suppressed and the durable claim stays at the pre-settle state (control: ok).
for (const mode of ['ok', 'write-failure'] as const) {
  const root = await fs.mkdtemp(join(tmpdir(), 'pdrs-1233-f-'));
  const stateDir = join(root, 'state');
  const sinks = makeSinks();
  const store = reopen(stateDir);
  const snapshotStore: any = mode === 'ok' ? store : {
    load: () => store.load(),
    write: (snapshot: any) => (terminal(snapshot.task.status)
      ? Promise.reject(Error('#1233 settle write refused'))
      : store.write(snapshot)),
    flush: () => store.flush(),
  };
  const runtime = makeRuntime(stateDir, snapshotStore, sinks);
  const task = {
    taskId: 'task-settle', type: 'local_bash', kind: 'bash', command: 'printf abcdef', cwd: '/',
    status: 'completed', exitCode: 0, completionStatusSentInAttachment: false, lastReportedTotalLines: 0,
    isBackgrounded: true, interrupted: false, startedAt: new Date(0), endedAt: new Date(0), outputBytes: 6,
  };
  const output = new (TaskOutputStore as any)({ taskId: 'task-settle', diskSpillDir: join(stateDir, 'output') });
  output.append('abcdef');
  await output.flush();
  const entry = { task, output, done: Promise.resolve() };
  runtime.entries.set('task-settle', entry);
  await runtime.settleEntry(entry);
  await runtime.dispose();
  let onDiskTasks: any;
  try { onDiskTasks = JSON.parse(readFileSync(join(stateDir, 'state.json'), 'utf8')).tasks.length; }
  catch { onDiskTasks = 'absent'; }
  const sinks2 = makeSinks();
  const runtime2 = makeRuntime(stateDir, reopen(stateDir), sinks2);
  const restored = runtime2.list().map((taskRestored: any) => ({ status: taskRestored.status, outputBytes: taskRestored.outputBytes }));
  let read: any;
  try { read = readSlices(runtime2, 'task-settle'); } catch (error: any) { read = { error: error.message }; }
  await runtime2.dispose();
  cases.push({
    id: `settle-${mode}`,
    completionEvents: sinks.events.map((event: any) => normEvent(event, {})),
    diagnostics: sinks.diagnostics,
    onDiskTasks,
    restored,
    read,
    secondRuntimeDiagnostics: sinks2.diagnostics,
  });
  await fs.rm(root, { recursive: true, force: true });
}

// G. Corrupted snapshot files: every defect that a crash-style partial write
//    can leave behind must fail the store constructor closed (bad JSON, an
//    unsupported version, a repeated taskId, an invalid status). Each variant
//    keeps every other field fully valid, so only the named defect rejects.
for (const variant of ['bad-json', 'version-2', 'duplicate-id', 'invalid-status'] as const) {
  const root = await fs.mkdtemp(join(tmpdir(), 'pdrs-1233-g-'));
  const stateDir = join(root, 'state');
  await fs.mkdir(stateDir, { recursive: true });
  const task = (taskId: string, status: string) => ({
    taskId, type: 'local_bash', kind: 'bash', command: 'printf abcdef', cwd: '/',
    status, completionStatusSentInAttachment: false, lastReportedTotalLines: 0,
    isBackgrounded: true, interrupted: false,
    startedAt: '2026-10-24T00:00:00.000Z', endedAt: '2026-10-24T00:00:01.000Z',
    exitCode: 0, outputBytes: 0,
  });
  const output = { totalBytes: 0, persisted: false };
  const payloads: Record<string, () => string> = {
    'bad-json': () => '{ not json',
    'version-2': () => JSON.stringify({ version: 2, tasks: [{ task: task('task-a', 'completed'), output }] }),
    'duplicate-id': () => JSON.stringify({ version: 1, tasks: [{ task: task('task-a', 'completed'), output }, { task: task('task-a', 'completed'), output }] }),
    'invalid-status': () => JSON.stringify({ version: 1, tasks: [{ task: task('task-a', 'banana'), output }] }),
  };
  await fs.writeFile(join(stateDir, 'state.json'), payloads[variant]());
  let restart: any;
  try {
    reopen(stateDir);
    restart = 'constructed';
  } catch (error: any) { restart = { name: error.name, message: normText(error.message, stateDir, {}) }; }
  cases.push({ id: `corrupt-${variant}`, restart });
  await fs.rm(root, { recursive: true, force: true });
}

clearTimeout(deadline);
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await fs.mkdir(resolve(process.argv[2]), { recursive: true });
await fs.writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ node: process.version, cases }, null, 2));
