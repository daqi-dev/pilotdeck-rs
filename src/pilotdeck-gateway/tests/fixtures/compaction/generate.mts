// A6.6-pr2 compaction persistence fixture. Every transcript byte, return value
// and error below is produced by the fixed baseline's real
// AgentSessionEventRecorder / JsonlTranscriptWriter / durableContextRuntime /
// sidecarDefaultModuleDispatcher; no implementation is copied into the fixture.
//
// Run from the repo root with Node 22 and the baseline's tsx loader; see README.md.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { lstat, mkdir, mkdtemp, readdir, readFile, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory or a cross-test scratch directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const git = (...args: string[]) =>
  execFileSync(gitExecutable, ['--no-replace-objects', '-C', baseline, ...args], {
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
const tree = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable);
const trackedChanges = git('status', '--porcelain', '--untracked-files=no').toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);

// Every relative import reachable from a listed entry is part of the
// calibrating implementation, so sourceSha256 covers the closure rather than
// only the entry files. Bare/node: imports are external.
async function resolveSource(absolute: string): Promise<string | undefined> {
  const stem = absolute.endsWith('.js') ? absolute.slice(0, -3) : undefined;
  const candidates = [
    absolute,
    `${absolute}.ts`,
    `${absolute}.tsx`,
    `${absolute}.mts`,
    join(absolute, 'index.ts'),
    ...(stem ? [`${stem}.ts`, `${stem}.tsx`] : []),
  ];
  for (const candidate of candidates) {
    try {
      if ((await lstat(candidate)).isFile()) return candidate;
    } catch {
      // try the next extension
    }
  }
  return undefined;
}
async function sourceClosure(entry: string): Promise<string[]> {
  const seen = new Set<string>([entry]);
  const queue = [entry];
  while (queue.length > 0) {
    const relativePath = queue.shift()!;
    const text = await readFile(join(baseline, relativePath), 'utf8');
    const specifiers = [
      ...text.matchAll(/(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s+)(['"])([^'"]+)\1/g),
    ].map((match) => match[2]);
    for (const specifier of specifiers) {
      if (!specifier.startsWith('./') && !specifier.startsWith('../')) continue;
      const resolved = await resolveSource(join(baseline, dirname(relativePath), specifier));
      assert.ok(resolved, `unresolved baseline import ${specifier} in ${relativePath}`);
      const path = relative(baseline, resolved).split(sep).join('/');
      if (!seen.has(path)) {
        seen.add(path);
        queue.push(path);
      }
    }
  }
  return [...seen].sort();
}
const SOURCE_ENTRIES = [
  'src/agent/session/AgentSessionEventRecorder.ts',
  'src/agent/modules/context/durableContextRuntime.ts',
  'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
  'src/agent/turn/TurnRunner.ts',
  'src/session/transcript/JsonlTranscriptWriter.ts',
  'src/session/transcript/TranscriptReader.ts',
  'src/session/events/SessionRuntime.ts',
  'src/session/persistence/JsonlSessionPersistence.ts',
];
const closure = [...new Set((await Promise.all(SOURCE_ENTRIES.map(sourceClosure))).flat())].sort();
const sourceSha256: Record<string, string> = {};
for (const path of closure) {
  assert.ok(path.startsWith('src/'), `import closure escaped src/: ${path}`);
  const working = await readFile(join(baseline, path));
  const committed = git('show', `${BASELINE_COMMIT}:${path}`);
  assert.deepEqual(working, committed, `${path} differs from ${BASELINE_COMMIT}; the baseline checkout is not clean`);
  sourceSha256[path] = createHash('sha256').update(committed).digest('hex');
}

const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { AgentSessionEventRecorder } = await load('src/agent/session/AgentSessionEventRecorder.ts');
const { JsonlTranscriptWriter } = await load('src/session/transcript/JsonlTranscriptWriter.ts');
const { readTranscript } = await load('src/session/transcript/TranscriptReader.ts');
const { SessionRuntime } = await load('src/session/events/SessionRuntime.ts');
const { JsonlSessionPersistence } = await load('src/session/persistence/JsonlSessionPersistence.ts');
const { createSidecarDefaultModuleDispatcher } = await load(
  'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
);
const { TurnRunner } = await load('src/agent/turn/TurnRunner.ts');

const NOW = '2026-10-19T00:00:00.000Z';
const SESSION = 's1';
const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}/g;

const ok = { ok: true };
const errorOf = (error: any) => ({
  error: {
    name: error.name,
    ...(error.code === undefined ? {} : { code: error.code }),
    message: error.message,
  },
});

type Options = { tickingClock?: boolean; deterministicIds?: boolean; uuids?: string[] };
function open(dir: string, options: Options = {}) {
  const path = join(dir, `${SESSION}.jsonl`);
  let tick = 0;
  let ids = 0;
  let operations = 0;
  const runtime = new SessionRuntime({
    now: () => new Date(Date.parse(NOW) + (options.tickingClock ? tick++ * 1000 : 0)),
    uuid: () => options.uuids ? options.uuids[ids++]
      : options.deterministicIds ? `id-${++ids}` : randomUUID(),
  });
  const persistence = new JsonlSessionPersistence({ path });
  runtime.subscribe(entry => persistence.append(entry), { failureMode: 'propagate', flush: () => persistence.flush() });
  const writer = new JsonlTranscriptWriter({ path, eventStore: runtime });
  const recorder = new AgentSessionEventRecorder(writer, { uuid: () => `op-${++operations}` });
  return { path, writer, recorder, runtime };
}
type Ctx = ReturnType<typeof open>;

const request = (text: string) => ({
  model: 'fixture-model',
  messages: [{ role: 'user', content: [{ type: 'text', text }] }],
});
const message = (text: string, extra: any = {}) => ({
  role: 'user',
  content: [{ type: 'text', text }],
  ...extra,
});
const compactBoundary = (extra: any = {}) => ({
  kind: 'compact',
  subtype: 'compact_boundary',
  compactMetadata: {
    compactionId: 'compaction-1',
    trigger: 'auto',
    preTokens: 900,
    postTokens: 120,
    messagesSummarized: 3,
    targetTokens: 200,
    summaryGenerated: true,
    checkpointMerged: false,
    finalRatio: 0.6,
    extra: { tier: 'full', summarySucceeded: true },
  },
  ...extra,
});

async function runStep(ctx: Ctx, step: any): Promise<any> {
  try {
    const turnId = step.turnId ?? 'turn-1';
    switch (step.op) {
      case 'startTurn':
        await ctx.recorder.startTurn(SESSION, turnId);
        return ok;
      case 'modelRequest':
        return { step: await ctx.recorder.recordModelRequest(SESSION, turnId, request(step.text ?? 'context') as any) };
      case 'nextOperationId':
        return { id: ctx.recorder.nextCompactionOperationId() };
      case 'recordStarted':
        await ctx.recorder.recordCompactionStarted(SESSION, turnId, step.draft);
        return ok;
      case 'recordCompleted':
        await ctx.recorder.recordCompactionCompleted(SESSION, turnId, step.draft);
        return ok;
      case 'recordFailed':
        await ctx.recorder.recordCompactionFailed(SESSION, turnId, step.draft);
        return ok;
      case 'deferCompletion':
        ctx.recorder.deferCompactionCompletion(SESSION, turnId, step.draft);
        return ok;
      case 'commitDeferred':
        await ctx.recorder.commitDeferredCompaction(SESSION, turnId);
        return ok;
      case 'failDeferred':
        await ctx.recorder.failDeferredCompaction(SESSION, turnId, new Error(step.message));
        return ok;
      case 'writeReplacement':
        await ctx.writer.recordCompactionReplacement(SESSION, turnId, step.boundary, step.messages);
        return ok;
      case 'writeControlBoundary':
        await ctx.writer.recordControlBoundary(SESSION, turnId, step.boundary);
        return ok;
      case 'persistReplacement':
        // TurnRunner.ts:344-365: replacement first, then the deferred terminal.
        await ctx.writer.recordCompactionReplacement(SESSION, turnId, step.boundary, step.messages);
        await ctx.recorder.commitDeferredCompaction(SESSION, turnId);
        return ok;
      case 'reopen': {
        const read = await readTranscript(ctx.path);
        const fresh = open(dirname(ctx.path), ctx.options);
        fresh.runtime.restore(read.entries);
        Object.assign(ctx, fresh);
        return { entries: read.entries.length, sequence: fresh.writer.snapshotState().sequence };
      }
      case 'read': {
        const read = await readTranscript(ctx.path);
        return {
          entries: read.entries.length,
          diagnostics: read.diagnostics.length,
          sequence: read.entries.at(-1)?.sequence ?? 0,
        };
      }
      case 'state': {
        const state = ctx.writer.snapshotState();
        return { sequence: state.sequence, lastEntryId: state.lastEntryId };
      }
      default:
        throw new Error(`unknown step ${step.op}`);
    }
  } catch (error: any) {
    return errorOf(error);
  }
}

// Every file under the scenario dir (UTF-8 byte order), so stray temporaries fail too.
async function snapshotFiles(root: string): Promise<Record<string, string>> {
  const out: Record<string, string> = {};
  const walk = async (rel: string) => {
    const entries = await readdir(join(root, rel), { withFileTypes: true });
    entries.sort((a, b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name)));
    for (const entry of entries) {
      const child = rel ? `${rel}/${entry.name}` : entry.name;
      if (entry.isDirectory()) await walk(child);
      else out[child] = await readFile(join(root, child), 'utf8');
    }
  };
  await walk('');
  return out;
}

/** Machine-independent text: scenario dir → "<dir>", each v4 UUID → "<id-N>". */
function normalize(value: any, dir: string): any {
  const ids = new Map<string, string>();
  const text = JSON.stringify(value).split(dir).join('<dir>').replace(UUID, (id) => {
    if (!ids.has(id)) ids.set(id, `<id-${ids.size + 1}>`);
    return ids.get(id)!;
  });
  return JSON.parse(text);
}

const kind = (o: any): string =>
  o.error ? `error:${o.error.code ?? o.error.name}`
  : o.id !== undefined ? `id:${o.id}`
  : o.result !== undefined ? `result:${o.result.type}`
  : o.step !== undefined ? `step:${o.step}`
  : o.entries !== undefined ? `read:${o.entries}/${o.diagnostics ?? 0}@${o.sequence}`
  : o.lastEntryId !== undefined ? `state:${o.sequence}`
  : 'ok';

async function runScenario(scratch: string, name: string, steps: any[], options: Options = {}) {
  const dir = join(scratch, name);
  await mkdir(dir, { recursive: true });
  const ctx = open(dir, options);
  (ctx as any).options = options;
  const replayed = [];
  for (const step of steps) replayed.push({ ...step, outcome: await runStep(ctx, step) });
  return normalize({ name, ...(Object.keys(options).length ? { options } : {}), steps: replayed, files: await snapshotFiles(dir) }, dir);
}

// --- Turn module: real dispatcher dispatchTurn with a real onCompactPersisted ---
const context = { permissionContext: () => ({}), toolRuntimeContext: () => ({}), executionContext: () => ({}) };
const dispatcherBase = {
  config: { cwd: '/fixture', provider: 'fixture', model: 'fixture', permissionMode: 'default' },
  checkpoint: {},
  capabilityResultObserver: {},
  modules: {
    model: { execution: {} }, budget: {},
    capability: { runtimeContext: { bindTurn: () => context }, execution: { list: () => [] } },
    permission: { requestContext: { bindTurn: () => ({}) } },
    context: { requestIdentity: { bindTurn: () => ({ contextIdentity: () => ({}) }) } },
    lifecycle: {}, event: {},
  },
};
let turnCalls = 0;
async function runTurn(payload: any, mode: 'none' | 'ok' | 'throw') {
  const calls: any[] = [];
  const input: any = {
    sessionId: 'session', turnId: 'turn', messages: [],
    execution: { runId: 'run', operationId: 'operation' },
  };
  if (mode !== 'none') {
    input.onCompactPersisted = async ({ boundary, messages }: any) => {
      calls.push({ boundary, messages });
      if (mode === 'throw') throw new Error('disk full');
    };
  }
  const dispatcher = createSidecarDefaultModuleDispatcher({
    ...dispatcherBase, input,
    planTodoHandler: async () => ({}),
  } as any);
  const call = {
    kind: 'request', method: 'module_call', messageId: `call-${++turnCalls}`,
    requestId: 'callback-request', runId: 'run', operationId: 'operation',
    module: 'turn', payload,
  };
  let response;
  try {
    response = JSON.parse(JSON.stringify(await dispatcher.handlers.turn(call)));
  } catch (error: any) {
    response = { error: { name: error.name, message: error.message } };
  }
  return { mode, payload, response, calls };
}

// P2-1: TurnRunner.ts:338-374's catch is best-effort. A failed replacement is
// closed as compaction_failed, the loop aborts with
// "compaction_persistence_failed", and the callback itself resolves. The fake
// loop calls the real callback, then observes the real AbortSignal.
async function runRunnerFailure(scratch: string) {
  const dir = join(scratch, 'runner-persistence-failure');
  await mkdir(dir, { recursive: true });
  const ctx = open(dir);
  await ctx.recorder.startTurn(SESSION, 'turn-1');
  await ctx.recorder.recordCompactionStarted(SESSION, 'turn-1', {
    operationId: 'op-r', trigger: 'auto', messageCount: 1,
  });
  ctx.recorder.deferCompactionCompletion(SESSION, 'turn-1', {
    operationId: 'op-r', status: 'compacted', tier: 'full', compactionId: 'compaction-runner', messageCount: 1,
  });
  let callbackCalls = 0;
  let callbackReturned = false;
  let callbackError: string | null = null;
  let signalAborted = false;
  let signalReason: unknown;
  const loop = {
    snapshotFileState: () => ({}),
    run: async function* (input: any) {
      const signal = input.abortSignal;
      callbackCalls++;
      try {
        await input.onCompactPersisted({ boundary: { kind: 'resume' }, messages: [] });
        callbackReturned = true;
      } catch (error: any) {
        callbackError = error?.message ?? String(error);
      }
      signalAborted = signal.aborted;
      signalReason = signal.reason;
      yield { type: 'fixture_event', sessionId: SESSION, turnId: 'turn-1' };
    },
  };
  const runner = new TurnRunner(
    loop as any,
    ctx.writer,
    undefined,
    () => new Date(NOW),
    undefined,
    { cwd: '/fixture', transcriptPath: ctx.path, collectFileArtifacts: false },
    { eventRecorder: ctx.recorder },
  );
  const events: string[] = [];
  for await (const event of runner.run({
    sessionId: SESSION, turnId: 'turn-1', messages: [],
    input: { type: 'text', text: 'hi' }, turnAlreadyStarted: true,
  } as any)) {
    events.push(event.type);
    if (callbackReturned || callbackError !== null) break;
  }
  const read = await readTranscript(ctx.path);
  const compactionEntries = read.entries
    .filter((entry: any) => entry.type.startsWith('compaction_'))
    .map((entry: any) => ({ type: entry.type, operationId: entry.operationId, error: entry.error }));
  return normalize({
    name: 'runner-persistence-failure',
    callbackCalls, callbackReturned, callbackError, signalAborted, signalReason, events, compactionEntries,
  }, dir);
}

/** Deletes are confined to the run's own mkdtemp tree. */
function assertOwnedScratch(outputDir: string, path: string): string {
  const rel = relative(outputDir, path);
  if (!rel.startsWith('.generate-') || rel.includes(sep) || isAbsolute(rel)) {
    throw new Error(`remove refused: ${path} is not this run's mkdtemp tree under ${outputDir}`);
  }
  return path;
}

// --- Oracle scenarios ---
await mkdir(output, { recursive: true });
const scratch = assertOwnedScratch(output, await mkdtemp(join(output, '.generate-')));
const scenarios: any[] = [];
async function scenario(name: string, steps: any[], expected: string[], options: Options = {}) {
  const result = await runScenario(scratch, name, steps, options);
  scenarios.push(result);
  const kinds = result.steps.map((s: any) => kind(s.outcome));
  console.log(`${name}: ${kinds.join(' ')}`);
  assert.deepEqual(kinds, expected, name);
  return result;
}

try {
  await scenario('lifecycle', [
    { op: 'startTurn' },
    { op: 'nextOperationId' },
    { op: 'recordStarted', draft: { operationId: 'op-a', trigger: 'auto', messageCount: 3, maxContextTokens: 4096 } },
    { op: 'recordCompleted', draft: { operationId: 'op-a', status: 'skipped', messageCount: 3 } },
    { op: 'recordStarted', draft: { operationId: 'op-b', trigger: 'reactive', messageCount: 4 } },
    { op: 'recordFailed', draft: { operationId: 'op-b', error: 'provider down' } },
    { op: 'recordStarted', draft: { operationId: 'op-c', trigger: 'manual', messageCount: 1 } },
    { op: 'recordCompleted', draft: { operationId: 'op-c', status: 'compacted', tier: 'micro', compactionId: 'cmp-9', messageCount: 2, error: 'context_overflow_after_emergency_compaction' } },
    { op: 'read' },
    { op: 'state' },
  ], ['ok', 'id:op-1', 'ok', 'ok', 'ok', 'ok', 'ok', 'ok', 'read:7/0@7', 'state:7']);

  // durableContextRuntime.ts's compacted branch: started -> deferred
  // completion; TurnRunner.ts then writes the replacement and commits.
  await scenario('compacted-replacement', [
    { op: 'startTurn' },
    { op: 'recordStarted', draft: { operationId: 'op-c1', trigger: 'auto', messageCount: 3, maxContextTokens: 8000 } },
    { op: 'deferCompletion', draft: { operationId: 'op-c1', status: 'compacted', tier: 'full', compactionId: 'compaction-1', messageCount: 1 } },
    { op: 'persistReplacement', boundary: compactBoundary(), messages: [message('summary', { metadata: { compactReplacement: 'compaction-1' } })] },
    { op: 'read' },
  ], ['ok', 'ok', 'ok', 'ok', 'read:4/0@4']);

  // durableContextRuntime.ts's failure branch and TurnRunner.ts's catch:
  // failDeferredCompaction closes the deferred bracket as compaction_failed.
  await scenario('deferred-failure', [
    { op: 'startTurn' },
    { op: 'recordStarted', draft: { operationId: 'op-c2', trigger: 'auto', messageCount: 2 } },
    { op: 'deferCompletion', draft: { operationId: 'op-c2', status: 'compacted', tier: 'full', compactionId: 'compaction-2', messageCount: 1 } },
    { op: 'failDeferred', message: 'disk full' },
    { op: 'commitDeferred' },
    { op: 'read' },
  ], ['ok', 'ok', 'ok', 'ok', 'ok', 'read:3/0@3']);

  await scenario('control-boundaries', [
    { op: 'startTurn' },
    { op: 'writeControlBoundary', boundary: { kind: 'compact', subtype: 'microcompact_boundary', microCompactMetadata: { trigger: 'time_based', toolCallIds: ['t1'], rewrittenBytes: 12 } } },
    { op: 'writeControlBoundary', boundary: { kind: 'resume', metadata: { from: 'checkpoint' } } },
    { op: 'writeControlBoundary', boundary: { kind: 'manual' } },
    { op: 'writeControlBoundary', boundary: compactBoundary() },
    { op: 'read' },
  ], ['ok', 'ok', 'ok', 'ok', 'ok', 'read:5/0@5']);

  await scenario('errors', [
    { op: 'startTurn' },
    { op: 'deferCompletion', draft: { operationId: 'missing', status: 'compacted', messageCount: 1 } },
    { op: 'recordCompleted', draft: { operationId: 'missing', status: 'skipped', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'missing', error: 'x' } },
    { op: 'recordStarted', draft: { operationId: 'dup', trigger: 'auto', messageCount: 1 } },
    { op: 'recordStarted', draft: { operationId: 'dup', trigger: 'auto', messageCount: 1 } },
    { op: 'recordStarted', draft: { operationId: 'bad-count', trigger: 'auto', messageCount: -1 } },
    { op: 'recordStarted', draft: { operationId: 'bad-fraction', trigger: 'auto', messageCount: 1.5 } },
    { op: 'recordStarted', draft: { operationId: 'bad-missing', trigger: 'auto' } },
    { op: 'recordStarted', draft: { operationId: 'bad-max', trigger: 'auto', messageCount: 1, maxContextTokens: 0 } },
    { op: 'recordStarted', draft: { operationId: 'bad-max-fraction', trigger: 'auto', messageCount: 1, maxContextTokens: 1.5 } },
    { op: 'recordCompleted', draft: { operationId: 'bad-completion', status: 'skipped' } },
    { op: 'recordFailed', draft: { operationId: 'bad-failure', error: '' } },
    { op: 'recordFailed', draft: { operationId: 'null-error', error: null } },
    { op: 'recordStarted', draft: { operationId: '', trigger: 'auto', messageCount: 1 } },
    { op: 'commitDeferred' },
    { op: 'failDeferred', message: 'nothing deferred' },
    { op: 'read' },
  ], ['ok', 'error:Error', 'error:invalid_compaction', 'error:invalid_compaction', 'ok',
    'error:invalid_compaction', 'error:invalid_compaction', 'error:invalid_compaction',
    'error:invalid_compaction', 'error:invalid_compaction', 'error:invalid_compaction',
    'error:invalid_compaction', 'error:invalid_compaction', 'error:TypeError',
    'error:invalid_compaction', 'ok', 'ok', 'read:2/0@2']);

  await scenario('restore', [
    { op: 'startTurn' },
    { op: 'recordStarted', draft: { operationId: 'op-r', trigger: 'auto', messageCount: 2 } },
    { op: 'deferCompletion', draft: { operationId: 'op-r', status: 'compacted', tier: 'full', compactionId: 'compaction-3', messageCount: 1 } },
    { op: 'reopen' },
    { op: 'commitDeferred' },
    { op: 'recordCompleted', draft: { operationId: 'op-r', status: 'skipped', messageCount: 1 } },
    { op: 'recordCompleted', draft: { operationId: 'op-r', status: 'skipped', messageCount: 1 } },
    { op: 'read' },
    { op: 'state' },
  ], ['ok', 'ok', 'ok', 'read:2/0@2', 'ok', 'ok', 'error:invalid_compaction', 'read:3/0@3', 'state:3']);

  // Compaction records outside a started turn go through the same domain
  // validator as every other turn-scoped event.
  await scenario('outside-turn', [
    { op: 'recordStarted', draft: { operationId: 'outside', trigger: 'auto', messageCount: 1 } },
    { op: 'recordCompleted', draft: { operationId: 'outside', status: 'skipped', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'outside', error: 'x' } },
    { op: 'state' },
  ], ['error:event_outside_turn', 'error:event_outside_turn', 'error:event_outside_turn', 'state:0']);

  // The envelope does not constrain operationId, so domain validation sees the
  // raw value. `${operationId}` uses Array#join / Object#toString, and an own
  // non-callable toString throws the V8 TypeError.
  await scenario('non-string-operation-ids', [
    { op: 'startTurn' },
    { op: 'recordStarted', draft: { operationId: 7, trigger: 'auto', messageCount: 1 } },
    { op: 'recordStarted', draft: { operationId: 7, trigger: 'auto', messageCount: 1 } },
    { op: 'recordStarted', draft: { operationId: [1, 2], trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: [1, 2], error: 'x' } },
    { op: 'recordStarted', draft: { operationId: [[3], [4, [5]]], trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: [[3], [4, [5]]], error: 'x' } },
    { op: 'recordFailed', draft: { operationId: [null, undefined, 6], error: 'x' } },
    { op: 'recordStarted', draft: { operationId: [], trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: [], error: 'x' } },
    { op: 'recordStarted', draft: { operationId: { a: 1 }, trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: { a: 1 }, error: 'x' } },
    { op: 'recordFailed', draft: { operationId: { toString: 'not-callable' }, error: 'x' } },
    { op: 'recordFailed', draft: { operationId: [{ toString: 'not-callable' }], error: 'x' } },
    { op: 'recordStarted', draft: { operationId: true, trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: true, error: 'x' } },
    { op: 'read' },
  ], ['ok', 'ok', 'error:invalid_compaction', 'ok', 'error:invalid_compaction', 'ok',
    'error:invalid_compaction', 'error:invalid_compaction', 'ok', 'error:invalid_compaction',
    'ok', 'error:invalid_compaction', 'error:TypeError', 'error:TypeError', 'ok', 'ok',
    'read:8/0@8']);

  // validateCompactionFailed reads `entry.error.length === 0` on any value:
  // numbers, non-empty arrays and {length: 1} pass; {length: 0}, an empty
  // array/string, and a literal -0 fail.
  await scenario('failure-length', [
    { op: 'startTurn' },
    { op: 'recordStarted', draft: { operationId: 'length-0', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-0', error: { length: 0 } } },
    { op: 'recordStarted', draft: { operationId: 'length-neg0', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-neg0', error: { length: -0 } } },
    { op: 'recordStarted', draft: { operationId: 'length-empty-array', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-empty-array', error: [] } },
    { op: 'recordStarted', draft: { operationId: 'length-empty-string', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-empty-string', error: '' } },
    { op: 'recordStarted', draft: { operationId: 'length-number', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-number', error: 5 } },
    { op: 'recordStarted', draft: { operationId: 'length-array', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-array', error: [1] } },
    { op: 'recordStarted', draft: { operationId: 'length-1', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-1', error: { length: 1 } } },
    { op: 'recordStarted', draft: { operationId: 'length-string', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-string', error: { length: '0' } } },
    { op: 'recordStarted', draft: { operationId: 'length-null', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-null', error: { length: null } } },
    { op: 'recordStarted', draft: { operationId: 'length-array-zero', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-array-zero', error: { length: [0] } } },
    { op: 'recordStarted', draft: { operationId: 'length-bool', trigger: 'auto', messageCount: 1 } },
    { op: 'recordFailed', draft: { operationId: 'length-bool', error: true } },
    { op: 'read' },
  ], ['ok', 'ok', 'error:invalid_compaction', 'ok', 'error:invalid_compaction', 'ok',
    'error:invalid_compaction', 'ok', 'error:invalid_compaction', 'ok', 'ok', 'ok', 'ok',
    'ok', 'ok', 'ok', 'ok', 'ok', 'ok', 'ok', 'ok', 'ok', 'ok', 'read:19/0@19']);

  // --- TurnRunner's real onCompactPersisted catch (P2-1) ---
  const runner = await runRunnerFailure(scratch);
  console.log(
    `runner-persistence-failure: callbackReturned=${runner.callbackReturned} ` +
    `aborted=${runner.signalAborted} reason=${runner.signalReason}`,
  );

  // --- Turn module dispatcher scenarios ---
  const turn: any[] = [];
  const boundary = compactBoundary();
  const messages = [message('summary')];
  const addTurn = async (name: string, payload: any, mode: 'none' | 'ok' | 'throw') => {
    turn.push({ name, ...(await runTurn(payload, mode)) });
  };
  await addTurn('missing-port', { operation: 'persist_compaction', boundary, messages }, 'none');
  await addTurn('valid', { operation: 'persist_compaction', boundary, messages }, 'ok');
  await addTurn('callback-error', { operation: 'persist_compaction', boundary, messages }, 'throw');
  for (const [name, value] of [
    ['missing', undefined], ['null', null], ['array', [boundary]], ['string', 'boundary'], ['number', 7],
  ]) {
    const payload: any = { operation: 'persist_compaction', messages };
    if (value !== undefined) payload.boundary = value;
    await addTurn(`boundary-${name}`, payload, 'ok');
  }
  for (const [name, value] of [
    ['missing', undefined], ['object', {}], ['string', 'messages'], ['null', null], ['number', 3],
  ]) {
    const payload: any = { operation: 'persist_compaction', boundary };
    if (value !== undefined) payload.messages = value;
    await addTurn(`messages-${name}`, payload, 'ok');
  }
  await addTurn('both-missing', { operation: 'persist_compaction' }, 'ok');
  for (const [name, payload] of [
    ['operation-missing', {}],
    ['operation-empty', { operation: '' }],
    ['operation-number', { operation: 3, boundary, messages }],
  ] as [string, any][]) {
    await addTurn(name, payload, 'ok');
  }
  await addTurn('extra-fields', {
    operation: 'persist_compaction', boundary, messages,
    ignored: { nested: [1, null, true] }, turnId: 'other', sessionId: 'other',
  }, 'ok');

  const oracle = JSON.stringify({ sourceCommit: BASELINE_COMMIT, tree, sourceSha256, node: process.version, now: NOW, sessionId: SESSION, scenarios, turn, runner }, null, 2) + '\n';
  for (const dir of [scratch, output, baseline]) assert.ok(!oracle.includes(dir), `oracle.json contains ${dir}`);
  assert.ok(!new RegExp(UUID.source).test(oracle), 'oracle.json contains an unnormalized UUID');
  await writeFile(join(output, 'oracle.json'), oracle);
  console.log(`PASS: generated ${scenarios.length} recorder scenarios, ${turn.length} turn dispatcher scenarios and ${runner.callbackCalls} runner callback calls from real TS`);
} finally {
  await rm(assertOwnedScratch(output, scratch), { recursive: true, force: true });
}
