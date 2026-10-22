// Real TS recordings for the A6.5-pr12 background tasks: task_* tools over the
// real BackgroundTaskRuntime (native detached shell), BackgroundSubagentRuntime
// and the ProjectRuntimeRegistry stop/list SDK projections.
// Run with the fixed baseline checkout and Node 22:
//   PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
//     node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/background-tasks/generate.mts /abs/output-parent
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { sequences, validations } from './cases.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide an output parent directory; each run writes into its own mkdtemp child');
const baseline = resolve(process.env.PDRS_BASELINE!);
const outputParent = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const bound = bindSourceTree(baseline, commit, git);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');
assert.equal(process.platform, 'darwin', 'the real-process corpus is recorded on macOS (/bin/bash 3.2)');

const entries = [
  'src/tool/builtin/taskTools.ts',
  'src/task/runtime/BackgroundTaskRuntime.ts',
  'src/agent/sub/BackgroundSubagentRuntime.ts',
  'src/cli/ProjectRuntimeRegistry.ts',
];
const load = (name: string) => import(pathToFileURL(join(baseline, name)).href);
const { createTaskTools, createTaskCreateTool, createTaskListTool, createTaskOutputTool, createTaskWaitTool, createTaskStopTool } = await load(entries[0]);
const { BackgroundTaskRuntime } = await load(entries[1]);
const { BackgroundSubagentRuntime } = await load(entries[2]);
const { ProjectRuntimeRegistry } = await load(entries[3]);
const sourceSha256 = sourceClosureSha256(baseline, commit, git, entries);

const fixed = Date.parse('2026-01-01T00:00:00.000Z');
const clean = (value: unknown) => value === undefined ? null : JSON.parse(JSON.stringify(value));
const sleep = (ms: number) => new Promise((done) => setTimeout(done, ms));
const errorOf = (error: any) => ({
  error: {
    name: error?.name ?? 'Error',
    ...(typeof error?.code === 'string' ? { code: error.code } : {}),
    message: error?.message ?? String(error),
  },
});

// ---- definitions, unsupported, validation ---------------------------------
const toolNames = ['task_create', 'task_list', 'task_output', 'task_wait', 'task_stop'];
const probeRuntime = new BackgroundTaskRuntime();
const allTools = Object.values(createTaskTools({ runtime: probeRuntime })) as any[];
assert.deepEqual(allTools.map((tool) => tool.name), toolNames);
const definitions = allTools.map((tool) => clean({
  name: tool.name,
  aliases: tool.aliases,
  description: tool.description,
  kind: tool.kind,
  inputSchema: tool.inputSchema,
  maxResultBytes: tool.maxResultBytes,
  isReadOnly: tool.isReadOnly({}),
  isConcurrencySafe: tool.isConcurrencySafe({}),
}));
await probeRuntime.dispose();
const unsupported: any[] = [];
for (const create of [createTaskCreateTool, createTaskListTool, createTaskOutputTool, createTaskWaitTool, createTaskStopTool]) {
  const tool = create();
  try {
    await tool.execute({ command: 'x', taskId: 'x' }, { cwd: '/', sessionId: 's1' });
    unsupported.push({ tool: tool.name, ok: true });
  } catch (error: any) {
    unsupported.push({ tool: tool.name, code: error.code, message: error.message });
  }
}
const waitTool = allTools[3];
const validated: any[] = [];
for (const input of validations) validated.push({ input, expected: clean(await waitTool.validateInput(input)) });

// ---- sequences against real processes ---------------------------------------
const scratchParent = realpathSync(tmpdir());
const scratch = await mkdtemp(join(scratchParent, 'pdrs-background-tasks-'));
const ownerToken = createHash('sha256').update(`${scratch}:${process.pid}:${Date.now()}`).digest('hex');
await writeFile(join(scratch, '.pdrs-scratch-owner'), ownerToken, { flag: 'wx' });
async function removeScratch(): Promise<void> {
  assert.ok(scratch.startsWith(join(scratchParent, 'pdrs-background-tasks-')), `remove refused: ${scratch}`);
  assert.equal(await readFile(join(scratch, '.pdrs-scratch-owner'), 'utf8'), ownerToken, `remove refused: ${scratch}`);
  await rm(scratch, { recursive: true, force: true });
}

function groupExists(pid: number): boolean {
  try { process.kill(-pid, 0); return true; } catch (error: any) { return error.code !== 'ESRCH'; }
}
async function until(what: string, check: () => boolean, ms = 8000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!check()) {
    assert.ok(Date.now() < deadline, `timed out waiting for ${what}`);
    await sleep(10);
  }
}

// A replay that fails before a sequence's `touch go` / `dispose` step must not
// leave its detached shells behind: the provider unrefs them, so dropping the
// runtime does not stop them and the gate file that ends `until [ -f go ]`
// never appears. Track every runtime and process group and clean up in the
// `finally` below, before the scratch tree (whose `go` files they still see) is
// deleted.
const trackedGroups = new Set<number>();
let active: { runtime: any; subagents: any; disposable: boolean; disposed: boolean } | undefined;
async function killTrackedGroups(): Promise<void> {
  for (const pid of trackedGroups) {
    if (!groupExists(pid)) continue;
    try { process.kill(-pid, 'SIGKILL'); } catch (error: any) {
      if (error.code !== 'ESRCH') throw error;
    }
  }
  const deadline = Date.now() + 5000;
  for (const pid of trackedGroups) {
    while (groupExists(pid) && Date.now() < deadline) await sleep(10);
    assert.ok(!groupExists(pid), `leftover process group ${pid} (orphaned detached shell)`);
  }
}
async function stopActiveSequence(): Promise<void> {
  if (active) {
    if (active.disposable && !active.disposed) {
      try {
        await active.runtime.dispose();
      } catch (error) {
        // The group KILL below is the fallback; report so the failure stays visible.
        console.error(`dispose failed during cleanup: ${String(error)}`);
      }
    }
    active.subagents.shutdown();
  }
  await killTrackedGroups();
}

// The detached-shell provider unrefs its children (T11); a live gateway keeps
// the event loop alive, so does this generator. Without it `stop` on a task
// that ignored SIGTERM ends with an empty loop before the exit event lands.
const keepAlive = setInterval(() => undefined, 60_000);
const recorded: any[] = [];
let failure: unknown;
try {
  for (const sequence of sequences) {
    const root = join(scratch, sequence.name);
    await mkdir(root);
    await writeFile(join(root, 'file'), 'not a directory\n');
    await chmod(join(root, 'file'), 0o644);
    const completions: any[] = [];
    const runtime = new BackgroundTaskRuntime({
      now: () => new Date(fixed),
      onCompletion: (event: any) => completions.push(clean(event)),
      ...(sequence.options ?? {}),
    });
    const subagents = new BackgroundSubagentRuntime();
    // `async-spawn-error` deliberately never disposes (the baseline hangs); it
    // also has no child, so skip only its dispose and still clean any groups.
    active = { runtime, subagents, disposable: sequence.noDispose !== true, disposed: false };
    const registry = Object.create(ProjectRuntimeRegistry.prototype);
    registry.resolve = () => ({ backgroundSubagents: subagents, executionWorld: { backgroundTasks: runtime } });
    const tools = Object.fromEntries((Object.values(createTaskTools({ runtime })) as any[]).map((tool) => [tool.name, tool]));
    const taskIds: string[] = [];
    const subIds: string[] = [];
    const pids = new Set<number>();
    const releases: Array<() => void> = [];
    const placeholders = (value: any): any => {
      if (typeof value === 'string') {
        let out = value.split(root).join('<root>');
        taskIds.forEach((id, index) => { out = out.split(id).join(`<task:${index}>`); });
        subIds.forEach((id, index) => { out = out.split(id).join(`<sub:${index}>`); });
        return out.replace(/waitedMs=\d+/g, 'waitedMs=<ms>').replace(/pid=\d+/g, 'pid=<pid>');
      }
      if (Array.isArray(value)) return value.map(placeholders);
      if (value && typeof value === 'object') {
        return Object.fromEntries(Object.entries(value).map(([key, item]) => [key,
          key === 'waitedMs' ? '<ms>' : key === 'pid' && typeof item === 'number' ? '<pid>' : placeholders(item)]));
      }
      return value;
    };
    const substitute = (value: any): any => {
      if (typeof value === 'string') {
        const task = /^<task:(\d+)>$/.exec(value);
        if (task) return taskIds[Number(task[1])];
        const sub = /^<sub:(\d+)>$/.exec(value);
        if (sub) return subIds[Number(sub[1])];
        return value.split('<root>').join(root);
      }
      if (Array.isArray(value)) return value.map(substitute);
      if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, substitute(item)]));
      return value;
    };
    const steps: any[] = [];
    for (const step of sequence.steps) {
      const entry: any = { ...step };
      if (step.op === 'tool') {
        const controller = new AbortController();
        if (step.abort === 'pre') controller.abort();
        const timer = typeof step.abort === 'number' ? setTimeout(() => controller.abort(), step.abort) : undefined;
        const context = {
          cwd: substitute(step.cwd ?? '<root>'),
          env: substitute({ PATH: '/usr/bin:/bin', LANG: 'en_US.UTF-8', HOME: '<root>', ...(step.env ?? {}) }),
          sessionId: step.session ?? 's1',
          ...(step.abort !== undefined ? { abortSignal: controller.signal } : {}),
        };
        try {
          const output = await tools[step.tool].execute(substitute(step.input), context);
          if (step.tool === 'task_create') {
            taskIds.push(output.data.taskId);
            if (typeof output.data.pid === 'number') { pids.add(output.data.pid); trackedGroups.add(output.data.pid); }
          }
          entry.result = placeholders(clean({ content: output.content, data: output.data }));
        } catch (error: any) {
          entry.result = { error: { code: error?.name === 'PilotDeckToolRuntimeError' ? error.code : 'tool_execution_failed', message: placeholders(error.message) } };
        }
        if (timer) clearTimeout(timer);
      } else if (step.op === 'validate') {
        entry.result = clean(await tools[step.tool].validateInput(step.input));
      } else if (step.op === 'awaitBytes') {
        await until(`${sequence.name} bytes`, () => runtime.getOutput(taskIds[step.task], 0).totalBytes >= step.bytes);
        assert.equal(runtime.getOutput(taskIds[step.task], 0).totalBytes, step.bytes, `${sequence.name}: exact byte count`);
      } else if (step.op === 'awaitStatus') {
        await until(`${sequence.name} status`, () => runtime.get(taskIds[step.task])?.status === step.status);
      } else if (step.op === 'touch') {
        await writeFile(join(root, step.path), '');
      } else if (step.op === 'groupGone') {
        const pid = runtime.get(taskIds[step.task])?.pid;
        assert.ok(typeof pid === 'number', `${sequence.name}: task ${step.task} has no pid`);
        await until(`${sequence.name} group ${pid}`, () => !groupExists(pid));
        entry.result = true;
      } else if (step.op === 'dispose') {
        await runtime.dispose();
        subagents.shutdown();
        if (active) active.disposed = true;
        entry.result = true;
      } else if (step.op === 'sdkStop' || step.op === 'sdkList') {
        const input: any = { sessionKey: step.sessionKey, ...('task' in step ? { taskId: substitute(step.task) } : {}) };
        try {
          const result = step.op === 'sdkStop'
            ? await registry.stopBackgroundTaskForSdk(input)
            : await registry.backgroundTasksForSdk(input);
          entry.result = placeholders(clean(result));
        } catch (error: any) {
          entry.result = placeholders(errorOf(error));
        }
      } else if (step.op === 'subStart') {
        const index = subIds.length;
        const task = subagents.start({
          kind: step.kind,
          sessionId: step.sessionId,
          parentTurnId: `turn-${index}`,
          subagentId: `sa-${index}`,
          subagentType: 'general',
          ...(step.observedSubagentId ? { observedSubagentId: step.observedSubagentId } : {}),
          run: (signal: AbortSignal) => new Promise<void>((done, fail) => {
            if (step.behavior === 'fail') return fail(new Error('boom'));
            if (step.behavior === 'release') { releases[index] = done; return; }
            if (step.behavior === 'ignore-abort') { setTimeout(done, 50); return; }
            signal.addEventListener('abort', () => fail(new Error('stopped')), { once: true });
          }),
        });
        subIds.push(task.taskId);
        entry.result = placeholders(clean({ ...task, startedAt: undefined }));
      } else if (step.op === 'subRelease') {
        releases[step.sub]!();
      } else if (step.op === 'subTick') {
        await sleep(20);
        entry.result = placeholders(clean(subagents.list().map((task: any) => ({
          taskId: task.taskId, kind: task.kind, status: task.status, error: task.error, ended: task.endedAt !== undefined,
        }))));
      } else {
        throw new Error(`unknown step ${JSON.stringify(step)}`);
      }
      steps.push(entry);
    }
    if (!sequence.noDispose) {
      for (const pid of pids) await until(`${sequence.name} leftover group ${pid}`, () => !groupExists(pid));
    }
    recorded.push({
      name: sequence.name,
      ...(sequence.options ? { options: sequence.options } : {}),
      ...(sequence.noDispose ? { noDispose: true } : {}),
      ...(sequence.darwinOnly ? { darwinOnly: sequence.darwinOnly } : {}),
      tasks: taskIds.length,
      steps,
      // Tasks released together settle in a racy order (also in TS); compare
      // completion events as a set ordered by task placeholder.
      completions: placeholders(completions).sort((a: any, b: any) =>
        Number(/\d+/.exec(a.taskId)![0]) - Number(/\d+/.exec(b.taskId)![0])),
    });
    active = undefined;
    // Groups are gone (or this sequence deliberately has no child); forget the
    // PIDs so a later cleanup can never signal a reused one.
    trackedGroups.clear();
  }
} catch (error) {
  failure = error;
} finally {
  clearInterval(keepAlive);
  try {
    await stopActiveSequence();
  } catch (cleanupError) {
    if (failure === undefined) throw cleanupError;
    throw new AggregateError([failure, cleanupError], 'replay failed and cleanup found leftovers');
  } finally {
    await removeScratch();
  }
}
if (failure !== undefined) throw failure;

const oracle = {
  sourceCommit: commit,
  sourceSha256,
  baselineCount: bound.count,
  counts: {
    definitions: definitions.length,
    unsupported: unsupported.length,
    validations: validated.length,
    sequences: recorded.length,
    steps: recorded.reduce((sum, sequence) => sum + sequence.steps.length, 0),
  },
  definitions,
  unsupported,
  validations: validated,
  sequences: recorded,
};
const output = join(await mkdtemp(join(outputParent, 'background-tasks-')), 'oracle.json');
await writeFile(output, `${JSON.stringify(oracle, null, 2)}\n`, { flag: 'wx' });
console.log(output);
// The async-spawn-error task never exits (no child), so leave explicitly.
process.exit(0);
