// #956 real native-shell creation observations, pinned to Node v22.22.0.
import assert from 'node:assert/strict';
import { constants, openSync, closeSync, writeSync, readFileSync, chmodSync } from 'node:fs';
import { mkdir, mkdtemp, writeFile, rm } from 'node:fs/promises';
import { execFileSync, spawn } from 'node:child_process';
import { join, resolve, dirname } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { tmpdir } from 'node:os';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.equal(process.version, 'v22.22.0', '#956 requires Node v22.22.0');
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/tool/builtin/taskTools.ts', 'src/task/runtime/BackgroundTaskRuntime.ts',
  'src/runtime/commandShell.ts', 'src/tool/execution-world/DetachedShellPort.ts'];
const { createTaskCreateTool } = await import(pathToFileURL(join(baseline, sources[0])).href);
const { BackgroundTaskRuntime } = await import(pathToFileURL(join(baseline, sources[1])).href);
const command = "printf 'done-output\\n'";
const scratch = await mkdtemp(join(tmpdir(), 'pdrs-956-ts-'));
const tick = () => new Promise<void>(resolve => setImmediate(resolve));
async function bounded<T>(work: Promise<T>, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  const deadline = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(Error(`#956 ${label} deadline`)), 8000);
  });
  try { return await Promise.race([work, deadline]); }
  finally { clearTimeout(timer!); }
}
const groups = new Set<number>();
function groupAlive(pid: number): boolean {
  try { process.kill(-pid, 0); return true; }
  catch (error: any) { if (error.code === 'ESRCH') return false; throw error; }
}
async function drained(): Promise<void> {
  const deadline = Date.now() + 8000;
  while ([...groups].some(groupAlive)) {
    assert.ok(Date.now() < deadline, '#956 process groups drained deadline');
    await tick();
  }
  groups.clear();
}
// Track at native spawn, even if creation later rejects or times out.
const runtime = () => new BackgroundTaskRuntime({ spawn: (...args: Parameters<typeof spawn>) => {
  const child = spawn(...args);
  if (child.pid !== undefined) groups.add(child.pid);
  return child;
} });
const cases: any[] = [];
let active: any;
let writer: number | undefined;
try {
  const shell = join(scratch, 'gate.sh');
  await writeFile(shell, readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'shell.sh')), { flag: 'wx' });
  chmodSync(shell, 0o700);
  for (const mode of ['release', 'stop', 'dispose']) {
    const fifo = join(scratch, `${mode}.fifo`);
    execFileSync('/usr/bin/mkfifo', [fifo]);
    active = runtime();
    const output = await bounded(createTaskCreateTool(active).execute({ command }, {
      cwd: scratch, sessionId: 's1', env: { PATH: '/usr/bin:/bin', HOME: scratch,
        PILOTDECK_SHELL_PATH: shell, PDRS_CREATE_FIFO: fifo },
    }), 'gated create');
    const connectDeadline = Date.now() + 8000;
    while (writer === undefined) {
      assert.ok(Date.now() < connectDeadline, "#956 FIFO reader rendezvous deadline");
      try { writer = openSync(fifo, constants.O_WRONLY | constants.O_NONBLOCK); }
      catch (error: any) { if (error.code !== 'ENXIO') throw error; await tick(); }
    }
    const id = output.data.taskId;
    assert.equal(output.data.status, 'running', '#956 create snapshot');
    await tick();
    const held = { ...active.get(id) };
    assert.equal(held.status, 'running', '#956 predecessor stays pending');
    assert.equal(held.command, command, '#956 command unchanged');
    assert.equal(active.getOutput(id, 0).totalBytes, 0, '#956 no early command output');
    if (mode === 'release') {
      writeSync(writer, 'release\n');
      await bounded(active.entries.get(id).done, 'released completion');
    } else if (mode === 'stop') await bounded(active.stop(id), 'stop');
    else await bounded(active.dispose(), 'dispose');
    const terminal = active.get(id);
    const result = { mode, create: output.data.status, held: held.status,
      command: held.command, terminal: terminal.status,
      outputBytes: terminal.outputBytes };
    assert.equal(result.terminal, mode === 'release' ? 'completed' : 'cancelled', '#956 terminal');
    assert.equal(result.outputBytes, mode === 'release' ? 12 : 0, '#956 output');
    cases.push(result);
    closeSync(writer); writer = undefined;
    await bounded(active.dispose(), 'dispose'); await drained(); active = undefined;
  }
  // Original dispose-drains step 2: it is finite, not held until dispose.
  const native: string[] = [];
  for (let n = 0; n < 16; n++) {
    active = runtime();
    const output = await bounded(createTaskCreateTool(active).execute({ command }, {
      cwd: scratch, sessionId: 's1', env: { PATH: '/usr/bin:/bin', HOME: scratch },
    }), 'native create');
    native.push(output.data.status);
    await bounded(active.entries.get(output.data.taskId).done, 'native completion');
    assert.equal(active.get(output.data.taskId).status, 'completed', '#956 original command completes');
    await bounded(active.dispose(), 'dispose'); await drained(); active = undefined;
  }
  assert.deepEqual(native, Array(16).fill('running'), '#956 original TS creation snapshots');
  const sourceSha256 = await baselineSourceHashes(baseline, sources);
  const oracle = { sourceCommit, sourceSha256, node: process.version, native, cases };
  const out = resolve(process.argv[2]);
  await mkdir(out, { recursive: true });
  await writeFile(join(out, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n', { flag: 'wx' });
  console.log(JSON.stringify(oracle, null, 2));
} finally {
  try {
    if (active) await bounded(active.dispose(), 'cleanup dispose');
  } finally {
    // Only these freshly recorded process groups belong to this probe.
    try {
      for (const pid of groups) {
        if (!groupAlive(pid)) continue;
        try { process.kill(-pid, 'SIGKILL'); }
        catch (error: any) { if (error.code !== 'ESRCH') throw error; }
      }
      await drained();
    } finally {
      if (writer !== undefined) closeSync(writer);
      await rm(scratch, { recursive: true, force: true });
    }
  }
}
