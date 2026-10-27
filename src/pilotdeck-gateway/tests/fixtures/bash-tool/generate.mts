// Real TS recordings for the A6.5-pr7 bash tool (bash.ts, bash/commandRunner.ts,
// bash/permissions.ts) and its PermissionRuntime integration.
// Run with the fixed baseline checkout and Node 22:
//   PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
//     node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/bash-tool/generate.mts /abs/output-parent
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { pathToFileURL } from 'node:url';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
import { cases } from './cases.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide an output parent directory; each run writes into its own mkdtemp child');
assert.equal(process.platform, 'darwin', 'the real-process corpus is recorded on macOS (/bin/bash 3.2)');
const baseline = resolve(process.env.PDRS_BASELINE!);
const outputParent = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const bound = bindBaseline({ baseline, commit, gitExecutable: git }).provenance;
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const load = (name: string) => import(pathToFileURL(join(baseline, 'src', name)).href);
const { createBashTool } = await load('tool/builtin/bash.ts');
const { classifyBashPermission, isReadOnlyShellCommand } = await load('tool/builtin/bash/permissions.ts');
const { PermissionRuntime } = await load('permission/decision/PermissionRuntime.ts');
const { applyResultSizeLimit } = await load('tool/protocol/result.ts');
const sourceSha256 = sourceClosureSha256(baseline, commit, git, [
  'src/tool/builtin/bash.ts',
  'src/tool/builtin/bash/commandRunner.ts',
  'src/tool/builtin/bash/permissions.ts',
  'src/permission/decision/PermissionRuntime.ts',
  'src/tool/protocol/result.ts',
]);

const fixed = Date.parse('2026-01-01T00:00:00.000Z');
const clean = (value: unknown) => value === undefined ? null : JSON.parse(JSON.stringify(value));
const sha = (text: string) => createHash('sha256').update(Buffer.from(text, 'utf8')).digest('hex');
// What the OS receives for a JS string argument (lone surrogates -> U+FFFD).
const osString = (value: string) => Buffer.from(value, 'utf8').toString('utf8');

function outcome(promise: Promise<any>, normalize: (value: any) => any = (value) => value) {
  return promise.then(
    (output) => ({ ok: normalize(clean({ content: output.content, data: output.data, metadata: output.metadata })) }),
    (error) => ({
      error: normalize(clean({
        code: error?.name === 'PilotDeckToolRuntimeError' ? error.code : 'tool_execution_failed',
        message: error?.message ?? String(error),
        ...(error?.details !== undefined ? { details: error.details } : {}),
      })),
    }),
  );
}

// ---- definition, classification, tool predicates --------------------------
const definitionTool = createBashTool();
const definition = clean({
  name: definitionTool.name,
  aliases: definitionTool.aliases,
  description: definitionTool.description,
  kind: definitionTool.kind,
  inputSchema: definitionTool.inputSchema,
  maxResultBytes: definitionTool.maxResultBytes,
});
const commands = cases.commands.map((command: string) => ({
  command,
  readOnly: isReadOnlyShellCommand(command),
  permission: clean(classifyBashPermission(command)),
}));
const predicates: any[] = [];
for (const input of cases.predicateInputs) {
  predicates.push({
    input,
    isReadOnly: definitionTool.isReadOnly(input),
    isConcurrencySafe: definitionTool.isConcurrencySafe(input),
    checkPermissions: clean(await definitionTool.checkPermissions(input, {})),
  });
}

// ---- PermissionRuntime.decide with the real bash tool ----------------------
const decisions: any[] = [];
for (const scenario of cases.decisions) {
  const decision = await new PermissionRuntime().decide(
    definitionTool,
    scenario.input,
    { permissionContext: scenario.context } as any,
    'call-1',
  );
  decisions.push({ ...scenario, expected: clean(decision) });
}

// ---- scripted runner: formatting, diagnostics, validation -----------------
const scripted: any[] = [];
for (const scenario of cases.scripted) {
  const calls: any[] = [];
  const events: any[] = [];
  const runner = {
    run: async (command: string, request: any) => {
      calls.push({
        command: osString(command),
        cwd: request.cwd,
        envKeys: request.env === undefined ? null : Object.keys(request.env),
        timeoutMs: request.timeoutMs,
        signal: request.signal !== undefined,
        onStdout: typeof request.onStdout === 'function',
        onStderr: typeof request.onStderr === 'function',
      });
      for (const [stream, chunk] of scenario.chunks ?? []) {
        (stream === 'stdout' ? request.onStdout : request.onStderr)?.(chunk);
      }
      if (scenario.throws !== undefined) throw new Error(scenario.throws);
      return { durationMs: 5, ...scenario.result };
    },
  };
  const tool = createBashTool({ runner, ...(scenario.options ?? {}) });
  const context: any = {
    cwd: '/ws/project',
    env: { PATH: '/usr/bin:/bin', FOO: '1' },
    sessionId: 's1',
    turnId: 't1',
    now: () => new Date(fixed),
    ...(scenario.signal ? { abortSignal: new AbortController().signal } : {}),
    ...(scenario.progress ? { progress: (event: any) => { events.push(clean(event)); } } : {}),
    ...(scenario.noEnv ? { env: undefined } : {}),
  };
  const result = await outcome(tool.execute(scenario.input, context));
  scripted.push({ ...scenario, calls, events, expected: result });
}

// ---- real processes -------------------------------------------------------
const scratchParent = realpathSync(tmpdir());
const scratch = await mkdtemp(join(scratchParent, 'pdrs-bash-tool-'));
const ownerToken = createHash('sha256').update(`${scratch}:${process.pid}:${Date.now()}`).digest('hex');
await writeFile(join(scratch, '.pdrs-scratch-owner'), ownerToken, { flag: 'wx' });
// Delete only the per-run tree this process created and still owns.
async function removeScratch(): Promise<void> {
  assert.ok(scratch.startsWith(join(scratchParent, 'pdrs-bash-tool-')), `remove refused: ${scratch}`);
  const token = await readFile(join(scratch, '.pdrs-scratch-owner'), 'utf8');
  assert.equal(token, ownerToken, `remove refused: ${scratch} is not owned by this run`);
  await rm(scratch, { recursive: true, force: true });
}
const placeholders = (root: string) => (value: any): any => {
  if (typeof value === 'string') return value.split(root).join('<root>').split(scratch).join('<scratch>');
  if (Array.isArray(value)) return value.map(placeholders(root));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, key === 'durationMs' ? '<duration>' : placeholders(root)(item)]));
  }
  return value;
};
const substitute = (root: string) => (value: any): any => {
  if (typeof value === 'string') return value.split('<root>').join(root);
  if (Array.isArray(value)) return value.map(substitute(root));
  if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, substitute(root)(item)]));
  return value;
};

async function killEscaped(root: string): Promise<number | undefined> {
  let text: string;
  try { text = await readFile(join(root, 'escaped.pid'), 'utf8'); } catch { return undefined; }
  const pid = Number(text.trim());
  assert.ok(Number.isInteger(pid) && pid > 1, `bad escaped pid ${text}`);
  try { process.kill(-pid, 'SIGKILL'); } catch { /* already gone */ }
  return pid;
}

const real: any[] = [];
try {
  for (const scenario of cases.real) {
    const root = join(scratch, scenario.name);
    await mkdir(root);
    await writeFile(join(root, 'file'), 'not a directory\n');
    await chmod(join(root, 'file'), 0o644);
    try {
      const tool = createBashTool();
      const controller = new AbortController();
      if (scenario.preAborted) controller.abort();
      const streams = { stdout: '', stderr: '' };
      let progressEvents = 0;
      const context: any = {
        cwd: substitute(root)(scenario.cwd ?? '<root>'),
        env: substitute(root)(scenario.env),
        sessionId: 's1',
        turnId: 't1',
        now: () => new Date(fixed),
        ...(scenario.abortAfterMs !== undefined || scenario.preAborted ? { abortSignal: controller.signal } : {}),
        ...(scenario.progress ? {
          progress: (event: any) => {
            progressEvents += 1;
            const { stream, chunk, byteCount } = event.metadata;
            assert.equal(event.message, `${stream}: ${chunk.length} bytes`);
            assert.equal(byteCount, Buffer.byteLength(chunk, 'utf8'));
            streams[stream as 'stdout' | 'stderr'] += chunk;
          },
        } : {}),
      };
      const started = Date.now();
      const timer = scenario.abortAfterMs !== undefined ? setTimeout(() => controller.abort(), scenario.abortAfterMs) : undefined;
      const execution = tool.execute(substitute(root)(scenario.input), context);
      let raw: any;
      const result = await outcome(execution.then((output: any) => { raw = output; return output; }), placeholders(root));
      const wallMs = Date.now() - started;
      if (timer) clearTimeout(timer);
      if (scenario.minWallMs !== undefined) assert.ok(wallMs >= scenario.minWallMs, `${scenario.name}: ${wallMs}ms < ${scenario.minWallMs}`);
      if (scenario.maxWallMs !== undefined) assert.ok(wallMs <= scenario.maxWallMs, `${scenario.name}: ${wallMs}ms > ${scenario.maxWallMs}`);
      const entry: any = { ...scenario };
      if (scenario.hashOnly) {
        assert.ok(raw, `${scenario.name} must succeed`);
        entry.expected = {
          sha256: sha(JSON.stringify(result)),
          stdoutLength: raw.data.stdout.length,
          previewLimit: clean(applyResultSizeLimit(raw.content, definitionTool.maxResultBytes).metadata),
        };
      } else {
        entry.expected = result;
      }
      if (scenario.progress) entry.progressStreams = streams, assert.ok(progressEvents > 0);
      if (scenario.postCheckAtMs !== undefined) {
        const wait = started + scenario.postCheckAtMs - Date.now();
        assert.ok(wait > 0, `${scenario.name}: post check already late`);
        await new Promise((done) => setTimeout(done, wait));
        entry.postFilesExist = await Promise.all(scenario.postFiles.map((name: string) =>
          readFile(join(root, name)).then(() => true, () => false)));
      }
      real.push(entry);
    } finally {
      // The escaped job is a detached process group: kill it even when an
      // assertion above fails before the success-path cleanup.
      await killEscaped(root);
    }
  }
} finally {
  await removeScratch();
}

const oracle = {
  sourceCommit: commit,
  sourceSha256,
  baselineCount: bound.count,
  counts: {
    commands: commands.length,
    predicates: predicates.length,
    decisions: decisions.length,
    scripted: scripted.length,
    real: real.length,
  },
  definition,
  commands,
  predicates,
  decisions,
  scripted,
  real,
};

const output = join(await mkdtemp(join(outputParent, 'bash-tool-')), 'oracle.json');
await writeFile(output, `${JSON.stringify(oracle, null, 2)}\n`, { flag: 'wx' });
console.log(output);
