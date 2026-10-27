import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see
// README.md.
//
// Every transcript byte, return value and error below comes from the real
// baseline `AgentSessionEventRecorder` / `JsonlTranscriptWriter`; no recorder
// implementation is copied into the fixture.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import { mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory or a cross-test scratch directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);

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
const sourceSha256 = await baselineSourceHashes(baseline, SOURCES.map(source => `src/${source}`));

const load = (path: string) => import(pathToFileURL(join(baseline, 'src', path)).href);
const { AgentSessionEventRecorder } = await load('agent/session/AgentSessionEventRecorder.ts');
const { JsonlTranscriptWriter } = await load('session/transcript/JsonlTranscriptWriter.ts');
const { readTranscript } = await load('session/transcript/TranscriptReader.ts');
const { SessionRuntime } = await load('session/events/SessionRuntime.ts');
const { JsonlSessionPersistence } = await load('session/persistence/JsonlSessionPersistence.ts');

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

type Options = { tickingClock?: boolean; deterministicIds?: boolean; uuids?: string[] };
function open(dir: string, options: Options = {}) {
  const path = join(dir, `${SESSION}.jsonl`);
  let tick = 0;
  let ids = 0;
  const runtime = new SessionRuntime({
    now: () => new Date(Date.parse(NOW) + (options.tickingClock ? tick++ * 1000 : 0)),
    uuid: () => options.uuids ? options.uuids[ids++]
      : options.deterministicIds ? `id-${++ids}` : randomUUID(),
  });
  const persistence = new JsonlSessionPersistence({ path });
  runtime.subscribe(entry => persistence.append(entry), { failureMode: 'propagate', flush: () => persistence.flush() });
  const writer = new JsonlTranscriptWriter({ path, eventStore: runtime });
  const recorder = new AgentSessionEventRecorder(writer);
  return { path, writer, recorder, runtime };
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
      case 'writerAcceptedInput':
        await ctx.writer.recordAcceptedInput(SESSION, turnId, step.messages, step.metadata);
        return ok;
      case 'writerMetadata':
        await ctx.writer.recordSessionMetadata(SESSION, turnId, step.metadata);
        return ok;
      case 'restore':
        ctx.runtime.restore(step.entries);
        ctx.recorder = new AgentSessionEventRecorder(ctx.writer);
        return ok;
      case 'reopen': {
        const read = await readTranscript(ctx.path);
        const fresh = open(ctx.path.slice(0, ctx.path.lastIndexOf('/')));
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

async function runScenario(scratch: string, name: string, steps: any[], options: Options) {
  const dir = join(scratch, name);
  await mkdir(dir, { recursive: true });
  const ctx = open(dir, options);
  const replayed = [];
  for (const step of steps) replayed.push({ ...step, outcome: await runStep(ctx, step) });
  return normalize({ name, ...(Object.keys(options).length ? { options } : {}), steps: replayed, files: await snapshotFiles(dir) }, dir);
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
  ctx.runtime.restore(read.entries);
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
  const appendedEvent = after.entries[read.entries.length + 3];
  assert.equal(appendedEvent.type, 'model_stream_event');
  assert.deepEqual(appendedEvent.event, { type: 'done' });
  for (const entry of read.entries.filter(e => e.type === 'model_stream_event')) {
    assert.ok(entry.event && !Object.hasOwn(entry.event, 'raw'));
  }
  console.log(JSON.stringify({ entries: after.entries.length, last: after.entries.at(-1).sequence }));
  process.exit(0);
}

// --- Oracle scenarios ---
await mkdir(output, { recursive: true });
const scratch = await mkdtemp(join(output, '.generate-'));
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

  await scenario('interleaved-writer', [
    { op: 'writerAcceptedInput', messages: [{ role: 'user', content: [{ type: 'text', text: 'mixed' }] }] },
    { op: 'startTurn' },
    { op: 'writerMetadata', metadata: { title: 'between turn and step' } },
    { op: 'modelRequest', request: request('mixed') },
    { op: 'writerAcceptedInput', messages: [] },
    { op: 'modelEvent', event: { type: 'done' } },
    { op: 'toolCalls', calls: [call('mixed')] },
    { op: 'writerMetadata', metadata: { title: 'between call and result' } },
    { op: 'toolResults', results: [result('mixed')] },
    { op: 'reopen' },
    { op: 'writerMetadata', metadata: { title: 'after restore' } },
    { op: 'startTurn', turnId: 'after' },
    { op: 'read' },
  ], ['ok', 'ok', 'ok', 'step:1', 'ok', 'ok', 'ok', 'ok', 'ok', 'read:11/0@11', 'ok', 'ok', 'read:13/0@13']);

  const proto = JSON.parse('{"__proto__":{"keep":1},"ok":2,"nested":{"__proto__":null,"ok":3}}');
  await scenario('proto-payloads', [
    { op: 'startTurn' },
    { op: 'modelRequest', request: proto },
    { op: 'modelEvent', event: { type: 'done', nested: proto, list: [proto] } },
    { op: 'toolCalls', calls: [call('proto', { input: proto })] },
    { op: 'toolResults', results: [result('proto', { metadata: proto })] },
  ], ['ok', 'step:1', 'ok', 'ok', 'ok']);

  await scenario('typed-tool-ids', [
    { op: 'startTurn' }, { op: 'modelRequest', request: {} },
    { op: 'toolCalls', calls: [{ id: 1 }, { id: 2 }, { id: true }, { id: false }, { id: null }, {}, { id: '1' }] },
    { op: 'toolCalls', calls: [{ id: 1 }] },
    { op: 'toolResults', results: [{ toolCallId: 3 }] },
    { op: 'toolResults', results: [{ toolCallId: 1 }, { toolCallId: 2 }, { toolCallId: true }, { toolCallId: false }, { toolCallId: null }, {}, { toolCallId: '1' }] },
  ], ['ok', 'step:1', 'ok', 'error:duplicate_tool_call', 'error:tool_result_without_call', 'ok']);
  await scenario('reference-tool-ids', [
    { op: 'startTurn' }, { op: 'modelRequest', request: {} },
    { op: 'toolCalls', calls: [{ id: { x: 1 } }, { id: { x: 1 } }, { id: [1] }, { id: [1] }] },
    { op: 'toolResults', results: [{ toolCallId: { x: 1 } }] },
    { op: 'toolResults', results: [{ toolCallId: [1] }] },
  ], ['ok', 'step:1', 'ok', 'error:tool_result_without_call', 'error:tool_result_without_call']);

  await scenario('numeric-id-mismatch', [
    { op: 'startTurn' }, { op: 'modelRequest', request: {} },
    { op: 'toolCalls', calls: [{ id: 1 }] },
    { op: 'toolResults', results: [{ toolCallId: 2 }] },
    { op: 'toolResults', results: [{ toolCallId: 1 }] },
  ], ['ok', 'step:1', 'ok', 'error:tool_result_without_call', 'ok']);
  await scenario('id-coercion-error', [
    { op: 'startTurn' }, { op: 'modelRequest', request: {} },
    { op: 'toolResults', results: [{ toolCallId: { toString: null } }] },
    { op: 'toolResults', results: [{ toolCallId: [{ toString: 1 }] }] },
    { op: 'modelEvent', event: { type: 'done' } },
  ], ['ok', 'step:1', 'error:TypeError', 'error:TypeError', 'ok'], { tickingClock: true, deterministicIds: true });
  await scenario('request-failure-source-order', [
    { op: 'startTurn' }, { op: 'modelRequest', request: {} },
    { op: 'modelEvent', event: { type: 'done' } },
    { op: 'toolCalls', calls: [{ id: 1 }] },
    { op: 'modelRequest', request: {} },
    { op: 'toolResults', results: [] },
  ], ['ok', 'error:invalid_entry_id', 'error:model_request_missing', 'error:model_request_missing', 'step:1', 'ok'],
  { tickingClock: true, uuids: ['id-1', 'id-2', '', 'id-4', 'id-5', 'id-6', 'id-7'] });

  const timing = await scenario('failure-source-order', [
    { op: 'modelEvent', event: { type: 'done' } },
    { op: 'startTurn' }, { op: 'startTurn' },
    { op: 'modelRequest', request: {} },
    { op: 'toolCalls', calls: [{ id: 'a' }] },
    { op: 'toolCalls', calls: [{ id: 'a' }] },
    { op: 'toolCalls', calls: [null] },
    { op: 'toolResults', results: [null] },
    { op: 'toolResults', results: [{ toolCallId: 'missing' }] },
    { op: 'toolResults', results: [] },
    { op: 'toolResults', results: [{ toolCallId: 'a' }] },
    { op: 'state' },
  ], ['error:Error', 'ok', 'error:duplicate_turn_start', 'step:1', 'ok', 'error:duplicate_tool_call',
    'error:TypeError', 'error:TypeError', 'error:tool_result_without_call', 'error:step_has_open_tool_calls', 'ok', 'state:6'],
  { tickingClock: true, deterministicIds: true });
  const timingEntries = timing.files['s1.jsonl'].trim().split('\n').map(JSON.parse);
  assert.equal(timingEntries[4].entryId, 'id-11');
  assert.equal(timingEntries[4].createdAt, '2026-10-18T00:00:10.000Z');

  const history = (drafts: any[]) => drafts.map((draft, i) => ({
    ...draft, sessionId: SESSION, turnId: 'history', sequence: i + 1, createdAt: NOW,
    entryId: `history-${i + 1}`, parentEntryId: i ? `history-${i}` : null,
  }));
  const boundary = [{ type: 'turn_started' }, { type: 'step_started', step: 1 }];
  const requested = [...boundary, { type: 'model_request', step: 1, request: {} }];
  const invalidLogs: [string, any[], string][] = [
    ['outside-turn', [{ type: 'model_request', step: 1, request: {} }], 'event_outside_turn'],
    ['duplicate-turn', [{ type: 'turn_started' }, { type: 'turn_started' }], 'duplicate_turn_start'],
    ['event-before-request', [...boundary, { type: 'model_stream_event', step: 1, event: {} }], 'model_request_missing'],
    ['open-tool', [...requested, { type: 'tool_call', step: 1, call: { id: 'a' } }, { type: 'step_completed', step: 1, outcome: 'completed' }], 'step_has_open_tool_calls'],
    ['invalid-step', [{ type: 'turn_started' }, { type: 'step_started', step: 1.5 }], 'invalid_step'],
    ['step-already-open', [...boundary, { type: 'step_started', step: 2 }], 'step_already_open'],
    ['duplicate-request', [...requested, { type: 'model_request', step: 1, request: {} }], 'duplicate_model_request'],
    ['wrong-step-type', [...requested, { type: 'model_stream_event', step: '1', event: {} }], 'step_not_open'],
    ['call-before-request', [...boundary, { type: 'tool_call', step: 1, call: { id: 1 } }], 'model_request_missing'],
    ['duplicate-call', [...requested, { type: 'tool_call', step: 1, call: { id: 1 } }, { type: 'tool_call', step: 1, call: { id: 1 } }], 'duplicate_tool_call'],
    ['wrong-result-id', [...requested, { type: 'tool_call', step: 1, call: { id: 1 } }, { type: 'tool_result', step: 1, result: { toolCallId: 2 } }], 'tool_result_without_call'],
  ];
  for (const [name, drafts, code] of invalidLogs) {
    await scenario(`restore-${name}`, [
      { op: 'startTurn' }, { op: 'modelRequest', request: {} }, { op: 'toolCalls', calls: [{ id: 'live' }] },
      { op: 'restore', entries: history(drafts) }, { op: 'state' },
      { op: 'modelEvent', event: { type: 'done' } },
      { op: 'toolResults', results: [{ toolCallId: 'live' }] }, { op: 'read' },
    ], ['ok', 'step:1', 'ok', `error:${code}`, 'state:4', 'ok', 'ok', 'read:7/0@7'],
    { tickingClock: true, deterministicIds: true });
  }
  await scenario('envelope-empty-id', [
    { op: 'startTurn' }, { op: 'state' }, { op: 'startTurn' }, { op: 'read' },
  ], ['error:invalid_entry_id', 'state:0', 'ok', 'read:1/0@1'], { uuids: ['', 'valid'] });
  await scenario('envelope-self-parent', [
    { op: 'startTurn' }, { op: 'startTurn', turnId: 'second' }, { op: 'state' },
    { op: 'startTurn', turnId: 'second' }, { op: 'read' },
  ], ['ok', 'error:self_parent', 'state:1', 'ok', 'read:2/0@2'], { uuids: ['same', 'same', 'next'] });
  await scenario('envelope-history-duplicate', [
    { op: 'restore', entries: history([{ type: 'session_metadata', metadata: {} }, { type: 'session_metadata', metadata: {} }]) },
    { op: 'startTurn' }, { op: 'state' }, { op: 'startTurn' }, { op: 'read' },
  ], ['ok', 'error:duplicate_entry_id', 'state:2', 'ok', 'read:1/0@3'], { uuids: ['history-1', 'new'] });
  await scenario('envelope-unsafe-sequence', [
    { op: 'restore', entries: [{ ...history([{ type: 'session_metadata', metadata: {} }])[0], sequence: Number.MAX_SAFE_INTEGER }] },
    { op: 'startTurn' }, { op: 'state' }, { op: 'read' },
  ], ['ok', 'error:invalid_sequence', `state:${Number.MAX_SAFE_INTEGER}`, 'read:0/1@0'], { deterministicIds: true });

  // Changing scenarios requires updating the exact Rust count and README too.
  assert.equal(scenarios.length, 28, 'recorder oracle scenario count');
  const oracle = JSON.stringify({ sourceCommit, sourceSha256, node: process.version, now: NOW, sessionId: SESSION, scenarios }, null, 2) + '\n';
  for (const dir of [scratch, output, baseline]) assert.ok(!oracle.includes(dir), `oracle.json contains ${dir}`);
  assert.ok(!new RegExp(UUID.source).test(oracle), 'oracle.json contains an unnormalized UUID');
  await writeFile(join(output, 'oracle.json'), oracle);
  console.log(`PASS: generated ${scenarios.length} real TS durable recorder scenarios`);
} finally {
  await rm(scratch, { recursive: true, force: true });
}
