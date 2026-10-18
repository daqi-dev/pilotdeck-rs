// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see
// README.md.
//
// Every transcript byte, return value and error below comes from the real
// baseline `AgentSessionEventRecorder` / `JsonlTranscriptWriter`; no recorder
// implementation is copied into the fixture.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory or a cross-test scratch directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], {
  encoding: 'utf8',
}).trim();
assert.equal(sourceCommit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85', 'baseline must be the fixed PilotDeck commit');
const trackedChanges = execFileSync(
  'git',
  ['-C', baseline, 'status', '--porcelain', '--untracked-files=no'],
  { encoding: 'utf8' },
);
assert.equal(
  trackedChanges,
  '',
  `baseline has uncommitted changes to tracked files:\n${trackedChanges}`,
);

const SOURCES = [
  'agent/session/AgentSessionEventRecorder.ts',
  'agent/modules/llm/durableModelInvokerPort.ts',
  'agent/modules/capability/durableToolPort.ts',
  'session/transcript/JsonlTranscriptWriter.ts',
  'session/transcript/TranscriptEntry.ts',
  'session/transcript/TranscriptReader.ts',
  'session/events/SessionRuntime.ts',
  'session/events/SessionEventLogValidation.ts',
  'session/events/SessionDomainValidation.ts',
  'session/persistence/JsonlSessionPersistence.ts',
];
const sourceSha256: Record<string, string> = {};
for (const source of SOURCES) {
  const path = `src/${source}`;
  const workingTreeHash = createHash('sha256').update(await readFile(join(baseline, path))).digest('hex');
  const committed = execFileSync('git', ['-C', baseline, 'show', `${sourceCommit}:${path}`]);
  const committedHash = createHash('sha256').update(committed).digest('hex');
  assert.equal(
    workingTreeHash,
    committedHash,
    `${path} differs from ${sourceCommit}; the baseline checkout is not clean`,
  );
  sourceSha256[path] = committedHash;
}

const load = (path: string) => import(pathToFileURL(join(baseline, 'src', path)).href);
const { AgentSessionEventRecorder } = await load('agent/session/AgentSessionEventRecorder.ts');
const { JsonlTranscriptWriter } = await load('session/transcript/JsonlTranscriptWriter.ts');
const { readTranscript } = await load('session/transcript/TranscriptReader.ts');

const NOW = '2026-10-18T00:00:00.000Z';
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

function open(dir: string) {
  const path = join(dir, `${SESSION}.jsonl`);
  const writer = new JsonlTranscriptWriter({ path, now: () => new Date(NOW) });
  const recorder = new AgentSessionEventRecorder(writer);
  return { path, writer, recorder };
}
type Ctx = ReturnType<typeof open>;

async function runStep(ctx: Ctx, step: any): Promise<any> {
  try {
    const turnId = step.turnId ?? 'turn-1';
    switch (step.op) {
      case 'startTurn':
        await ctx.recorder.startTurn(SESSION, turnId);
        return ok;
      case 'modelRequest':
        return { step: await ctx.recorder.recordModelRequest(SESSION, turnId, { request: step.request } as any) };
      case 'modelEvent':
        await ctx.recorder.recordModelEvent(SESSION, turnId, step.event);
        return ok;
      case 'toolCalls':
        await ctx.recorder.recordToolCalls(SESSION, turnId, step.calls);
        return ok;
      case 'toolResults':
        await ctx.recorder.recordToolResults(SESSION, turnId, step.results);
        return ok;
      case 'reopen': {
        const read = await readTranscript(ctx.path);
        const last = read.entries[read.entries.length - 1];
        const fresh = open(ctx.path.slice(0, ctx.path.lastIndexOf('/')));
        fresh.writer.restoreState(last?.sequence ?? 0, last?.entryId ?? null);
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

/** Machine-independent text: scenario dir → "<dir>", each v4 UUID → "<id-N>" by first appearance. */
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
  : o.step !== undefined ? `step:${o.step}`
  : o.entries !== undefined ? `read:${o.entries}/${o.diagnostics ?? 0}@${o.sequence}`
  : o.lastEntryId !== undefined ? `state:${o.sequence}`
  : 'ok';

async function runScenario(scratch: string, name: string, steps: any[]) {
  const dir = join(scratch, name);
  await mkdir(dir, { recursive: true });
  const ctx = open(dir);
  const replayed = [];
  for (const step of steps) replayed.push({ ...step, outcome: await runStep(ctx, step) });
  return normalize({ name, steps: replayed, files: await snapshotFiles(dir) }, dir);
}

const request = (text: string, extra: any = {}) => ({
  model: 'fixture-model',
  messages: [{ role: 'user', content: [{ type: 'text', text }] }],
  ...extra,
});
const call = (id: string, extra: any = {}) => ({ id, name: 'read_file', input: { path: `${id}.txt` }, ...extra });
const result = (toolCallId: string, extra: any = {}) => ({
  type: 'success',
  toolCallId,
  toolName: 'read_file',
  content: [{ type: 'text', text: `result ${toolCallId}` }],
  startedAt: NOW,
  completedAt: NOW,
  ...extra,
});

// --- Cross test modes (driven by the ignored Rust test) ---
const crossPath = (dir: string) => join(dir, `${SESSION}.jsonl`);
if (process.argv[3] === '--cross-ts-write') {
  await mkdir(output, { recursive: true });
  const ctx = open(output);
  for (const step of [
    { op: 'startTurn', turnId: 'turn-ts-1' },
    { op: 'modelRequest', turnId: 'turn-ts-1', request: request('from TS') },
    { op: 'modelEvent', turnId: 'turn-ts-1', event: { type: 'text_delta', delta: 'ts' } },
    { op: 'toolCalls', turnId: 'turn-ts-1', calls: [call('ts-a')] },
    { op: 'toolResults', turnId: 'turn-ts-1', results: [result('ts-a')] },
    { op: 'startTurn', turnId: 'turn-ts-2' },
    { op: 'modelRequest', turnId: 'turn-ts-2', request: request('second') },
    { op: 'modelEvent', turnId: 'turn-ts-2', event: { type: 'done' } },
    { op: 'toolCalls', turnId: 'turn-ts-2', calls: [call('ts-b')] },
    { op: 'toolResults', turnId: 'turn-ts-2', results: [result('ts-b')] },
  ]) {
    const outcome = await runStep(ctx, step);
    assert.ok(!outcome.error, `${JSON.stringify(step)}: ${JSON.stringify(outcome)}`);
  }
  const read = await readTranscript(crossPath(output));
  console.log(JSON.stringify({ path: crossPath(output), entries: read.entries.length, last: read.entries.at(-1)?.sequence }));
  process.exit(0);
}
if (process.argv[3] === '--cross-ts-verify') {
  const path = crossPath(output);
  const read = await readTranscript(path);
  assert.ok(read.diagnostics.length === 0, JSON.stringify(read.diagnostics));
  assert.ok(read.entries.length > 0, 'cross transcript is empty');
  for (let i = 0; i < read.entries.length; i++) {
    assert.equal(read.entries[i].sequence, i + 1, 'sequence must be contiguous from 1');
    assert.equal(
      read.entries[i].parentEntryId,
      i === 0 ? null : read.entries[i - 1].entryId,
      'parent chain must be unbroken',
    );
  }
  assert.equal(read.entries.at(-1).sessionId, SESSION);
  const ctx = open(output);
  ctx.writer.restoreState(read.entries.at(-1).sequence, read.entries.at(-1).entryId);
  for (const step of [
    { op: 'startTurn', turnId: 'turn-ts-after' },
    { op: 'modelRequest', turnId: 'turn-ts-after', request: request('TS after the other writer') },
    { op: 'modelEvent', turnId: 'turn-ts-after', event: { type: 'done', raw: { dropped: true } } },
    { op: 'toolCalls', turnId: 'turn-ts-after', calls: [call('ts-after')] },
    { op: 'toolResults', turnId: 'turn-ts-after', results: [result('ts-after')] },
  ]) {
    const outcome = await runStep(ctx, step);
    assert.ok(!outcome.error, `${JSON.stringify(step)}: ${JSON.stringify(outcome)}`);
  }
  const after = await readTranscript(path);
  assert.equal(after.entries.length, read.entries.length + 7, 'turn/step/request/event/call/result/completed');
  assert.equal(after.entries.at(-1).sequence, read.entries.length + 7);
  assert.equal(after.entries.at(-1).type, 'step_completed');
  console.log(JSON.stringify({ entries: after.entries.length, last: after.entries.at(-1).sequence }));
  process.exit(0);
}

// --- Oracle scenarios ---
await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
const scenarios: any[] = [];
async function scenario(name: string, steps: any[], expected: string[]) {
  const result = await runScenario(scratch, name, steps);
  scenarios.push(result);
  const kinds = result.steps.map((s: any) => kind(s.outcome));
  console.log(`${name}: ${kinds.join(' ')}`);
  assert.deepEqual(kinds, expected, name);
  return result;
}

try {
  await scenario('model', [
    { op: 'startTurn', turnId: 'turn-1' },
    { op: 'modelRequest', turnId: 'turn-1', request: request(' first ', { raw: { preserved: true }, temperature: 0.25 }) },
    { op: 'modelEvent', turnId: 'turn-1', event: { type: 'text_delta', delta: 'a', raw: { secret: 1 } } },
    { op: 'modelEvent', turnId: 'turn-1', event: { type: 'usage', usage: { inputTokens: 1, outputTokens: 2 } } },
    { op: 'modelEvent', turnId: 'turn-1', event: { type: 'done' } },
    { op: 'modelRequest', turnId: 'turn-1', request: request('second') },
    { op: 'modelEvent', turnId: 'turn-1', event: { type: 'text_delta', delta: 'b' } },
    { op: 'read' },
  ], ['ok', 'step:1', 'ok', 'ok', 'ok', 'step:2', 'ok', 'read:10/0@10']);

  await scenario('tools', [
    { op: 'startTurn', turnId: 'turn-1' },
    { op: 'modelRequest', turnId: 'turn-1', request: request('tools') },
    { op: 'toolCalls', turnId: 'turn-1', calls: [
      call('call-a', { raw: { dropped: { nested: true } } }),
      call('call-b', { input: { path: 'b', raw: 'kept inside input' }, raw: [1, 2, 3] }),
    ] },
    { op: 'toolResults', turnId: 'turn-1', results: [
      result('call-a', { raw: { dropped: true }, metadata: { raw: 'kept', n: 1 } }),
      result('call-b', { type: 'error', error: { code: 'E', message: 'no', raw: { dropped: true } }, content: [] }),
    ] },
    { op: 'read' },
    { op: 'modelRequest', turnId: 'turn-1', request: request('empty results') },
    { op: 'toolResults', turnId: 'turn-1', results: [] },
    { op: 'read' },
  ], ['ok', 'step:1', 'ok', 'ok', 'read:8/0@8', 'step:2', 'ok', 'read:11/0@11']);

  await scenario('raw', [
    { op: 'startTurn', turnId: 'turn-1' },
    { op: 'modelRequest', turnId: 'turn-1', request: { raw: { request: 'kept' }, nested: { raw: 'inside request kept' }, list: [{ raw: 1, keep: 2 }] } },
    { op: 'modelEvent', turnId: 'turn-1', event: {
      type: 'tool_call',
      raw: { top: true },
      nested: { raw: [1, 2], keep: 'yes' },
      list: [{ raw: 'gone', keep: { raw: 'gone too', keep: 1 } }],
    } },
    { op: 'toolCalls', turnId: 'turn-1', calls: [{
      id: 'raw-call',
      raw: { top: true },
      input: { raw: 'kept inside input', nested: { raw: 'gone', keep: 1 } },
      list: [{ raw: 'gone', keep: 2 }],
    }] },
    { op: 'toolResults', turnId: 'turn-1', results: [{
      type: 'success',
      toolCallId: 'raw-call',
      toolName: 'read_file',
      content: [{ type: 'text', text: 'x', raw: 'gone' }],
      raw: { top: true },
      metadata: { raw: 'gone', keep: 3 },
    }] },
    { op: 'read' },
  ], ['ok', 'step:1', 'ok', 'ok', 'ok', 'read:7/0@7']);

  {
    const r = await scenario('errors', [
      { op: 'modelEvent', turnId: 'turn-1', event: { type: 'done' } },
      { op: 'toolCalls', turnId: 'turn-1', calls: [call('x')] },
      { op: 'toolResults', turnId: 'turn-1', results: [result('x')] },
      { op: 'startTurn', turnId: 'turn-1' },
      { op: 'startTurn', turnId: 'turn-1' },
      { op: 'modelEvent', turnId: 'turn-1', event: { type: 'done' } },
      { op: 'modelRequest', turnId: 'turn-1', request: request('errors') },
      { op: 'toolResults', turnId: 'turn-1', results: [result('missing')] },
      { op: 'toolCalls', turnId: 'turn-1', calls: [call('a')] },
      { op: 'toolCalls', turnId: 'turn-1', calls: [call('a')] },
      { op: 'toolResults', turnId: 'turn-1', results: [result('a')] },
      { op: 'modelRequest', turnId: 'turn-1', request: request('second step') },
      { op: 'toolCalls', turnId: 'turn-1', calls: [call('b'), call('c')] },
      { op: 'toolResults', turnId: 'turn-1', results: [result('b')] },
      { op: 'toolResults', turnId: 'turn-1', results: [result('c')] },
      { op: 'modelEvent', turnId: 'turn-1', event: { type: 'done' } },
      { op: 'startTurn', turnId: 'turn-2' },
      { op: 'modelRequest', turnId: 'turn-2', request: request('second turn') },
      { op: 'toolCalls', turnId: 'turn-2', calls: [null] },
      { op: 'toolResults', turnId: 'turn-2', results: [null] },
      { op: 'read' },
    ], [
      'error:Error', 'error:Error', 'error:Error', 'ok', 'error:duplicate_turn_start', 'error:Error',
      'step:1', 'error:tool_result_without_call', 'ok', 'error:duplicate_tool_call', 'ok',
      'step:2', 'ok', 'error:step_has_open_tool_calls', 'ok', 'error:Error', 'ok', 'step:1', 'error:TypeError',
      'error:TypeError', 'read:16/0@16',
    ]);
    const last = r.steps.at(-1).outcome;
    assert.equal(last.entries, 16, '16 durable entries: domain rejects and TypeErrors wrote nothing');
  }

  await scenario('restart', [
    { op: 'startTurn', turnId: 'turn-1' },
    { op: 'modelRequest', turnId: 'turn-1', request: request('before restart') },
    { op: 'modelEvent', turnId: 'turn-1', event: { type: 'done' } },
    { op: 'toolCalls', turnId: 'turn-1', calls: [call('r')] },
    { op: 'toolResults', turnId: 'turn-1', results: [result('r')] },
    { op: 'reopen' },
    { op: 'startTurn', turnId: 'turn-2' },
    { op: 'modelRequest', turnId: 'turn-2', request: request('after restart') },
    { op: 'modelEvent', turnId: 'turn-2', event: { type: 'done' } },
    { op: 'toolCalls', turnId: 'turn-2', calls: [call('r2')] },
    { op: 'toolResults', turnId: 'turn-2', results: [result('r2')] },
    { op: 'read' },
    { op: 'state' },
  ], ['ok', 'step:1', 'ok', 'ok', 'ok', 'read:7/0@7', 'ok', 'step:1', 'ok', 'ok', 'ok', 'read:14/0@14', 'state:14']);

  const oracle = JSON.stringify({ sourceCommit, sourceSha256, node: process.version, now: NOW, sessionId: SESSION, scenarios }, null, 2) + '\n';
  for (const dir of [scratch, output, baseline]) assert.ok(!oracle.includes(dir), `oracle.json contains ${dir}`);
  assert.ok(!new RegExp(UUID.source).test(oracle), 'oracle.json contains an unnormalized UUID');
  await writeFile(join(output, 'oracle.json'), oracle);
  console.log(`PASS: generated ${scenarios.length} real TS durable recorder scenarios`);
} finally {
  await rm(scratch, { recursive: true, force: true });
}
