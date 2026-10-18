// Real fixed-baseline oracle for the host `event.emit` adapter.
// The dispatcher's private SidecarTurnProtocol is exported in memory only;
// baseline sources are never copied into this repository.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
assert.equal(commit, 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85');
// The checkout is mutable in place; a dirty tracked file would change the
// numbers below while `rev-parse HEAD` still reports the pinned commit.
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

const base = 'src/agent/modules/transport/';
const clientFile = base + 'agentLoopSidecarClient.ts';
const dispatcherFile = base + 'sidecarDefaultModuleDispatcher.ts';
const eventsFile = 'src/agent/protocol/events.ts';
const files = [clientFile, dispatcherFile, eventsFile];
const sourceSha256: Record<string, string> = {};
for (const source of files) {
  const workingTreeHash = createHash('sha256').update(await readFile(join(baseline, source))).digest('hex');
  const committed = execFileSync('git', ['-C', baseline, 'show', `${commit}:${source}`]);
  const committedHash = createHash('sha256').update(committed).digest('hex');
  assert.equal(
    workingTreeHash,
    committedHash,
    `${source} differs from ${commit}; the baseline checkout is not clean`,
  );
  sourceSha256[source] = committedHash;
}

const requireBaseline = createRequire(join(baseline, 'package.json'));
const requireTsx = createRequire(requireBaseline.resolve('tsx'));
const { transformSync } = requireTsx('esbuild');
// Appending an export changes visibility only. Compile the entire original
// file at its original filename so imports still resolve to real baseline
// modules.
async function expose(source: string, names: string[]) {
  const path = join(baseline, source);
  const text = await readFile(path, 'utf8');
  for (const name of names) assert.ok(text.includes(name), `missing private export ${name}`);
  const transformed = transformSync(text + '\nexport { ' + names.join(', ') + ' };\n', {
    loader: 'ts', format: 'esm', target: 'es2022', sourcefile: path,
  });
  const code = transformed.code.replace(/from (["'])(\.[^"']+)\1/g, (_all: string, quote: string, specifier: string) => {
    let dependency = resolve(dirname(path), specifier);
    if (dependency.endsWith('.js') && existsSync(dependency.slice(0, -3) + '.ts')) dependency = dependency.slice(0, -3) + '.ts';
    return 'from ' + quote + pathToFileURL(dependency).href + quote;
  });
  return import('data:text/javascript;base64,' + Buffer.from(code).toString('base64'));
}
const { SidecarTurnProtocol } = await expose(clientFile, ['SidecarTurnProtocol']);
const { createSidecarDefaultModuleDispatcher } = await import(pathToFileURL(join(baseline, dispatcherFile)).href);
// `createAgentEventBuffer` is the real optional drain contract
// (`events.ts:155-162`): drain returns every buffered event and clears.
const { createAgentEventBuffer } = await import(pathToFileURL(join(baseline, eventsFile)).href);

const UNDEFINED = { __pdrsUndefined: true };
// JSON cannot spell JS `undefined`; the marker is kept in the recorded call so
// Rust can rebuild the same property-present value. Nested markers are
// supported, but the fixture only needs a top-level `turnId`/`type`/`event`.
function materialize(value: any): any {
  if (Array.isArray(value)) return value.map(materialize);
  if (value && typeof value === 'object') {
    if (value.__pdrsUndefined === true && Object.keys(value).length === 1) return undefined;
    const object: Record<string, unknown> = {};
    for (const [key, inner] of Object.entries(value)) object[key] = materialize(inner);
    return object;
  }
  return value;
}
const clone = (value: any): any => value === undefined ? undefined : JSON.parse(JSON.stringify(value));

const turn = { sessionId: 'session', turnId: 'turn' };
const input = {
  sessionId: turn.sessionId,
  turnId: turn.turnId,
  messages: [],
  execution: { runId: 'run', operationId: 'operation' },
};
const context = { permissionContext: () => ({}), toolRuntimeContext: () => ({}), executionContext: () => ({}) };
const config = { cwd: '/fixture', provider: 'fixture', model: 'fixture', permissionMode: 'default' };
let nextId = 0;
function request(payload: any) {
  return {
    kind: 'request', method: 'module_call', messageId: `call-${++nextId}`,
    requestId: 'callback-request', runId: 'run', operationId: 'operation',
    module: 'event', payload,
  };
}
function response(protocol: any, call: any) {
  return protocol.moduleResponseFor(call).then(async (first: any) => {
    const replay = await protocol.moduleResponseFor(call);
    assert.equal(first.replayed, false);
    assert.equal(replay.replayed, true);
    assert.deepEqual(first.response, replay.response);
    return clone(first.response);
  });
}

function dispatcherWith(eventPort: any) {
  return createSidecarDefaultModuleDispatcher({
    config,
    input,
    checkpoint: {},
    capabilityResultObserver: {},
    planTodoHandler: async () => ({}),
    modules: {
      model: { execution: {} },
      capability: {
        runtimeContext: { bindTurn: () => context },
        execution: { list: () => [] },
      },
      permission: { requestContext: { bindTurn: () => context } },
      context: { requestIdentity: { bindTurn: () => ({ contextIdentity: () => ({}) }) } },
      event: eventPort,
    },
  } as any);
}
const protocolFor = (handlers: any, call: any) => new SidecarTurnProtocol({
  config: {}, input, checkpoint: {}, manifest: {}, moduleHandlers: handlers,
  uuid: () => call.messageId,
});

const cases: any[] = [];
// Each step may drain after the call. The final drain is always recorded, so a
// sequence can prove emission order plus the clear-on-drain semantics.
// A case may also declare a throwing event port. The descriptor is fixture
// input, not TS output: the generator uses it to throw and Rust uses it to
// rebuild the same `ModuleError::from_js_fields` projection.
type ThrowingPort =
  | { kind: 'error'; message: string; fields?: Record<string, unknown> }
  | { kind: 'string'; value: string };
function eventCase(name: string, steps: any[], sink?: ThrowingPort) {
  cases.push({ name, steps, ...(sink ? { sink } : {}) });
}
function throwingPort(sink: ThrowingPort) {
  return {
    emit() {
      if (sink.kind === 'string') throw sink.value;
      throw Object.assign(new Error(sink.message), sink.fields ?? {});
    },
  };
}
const event = (extra: Record<string, unknown> = {}) => ({
  type: 'tool_progress',
  sessionId: turn.sessionId,
  turnId: turn.turnId,
  toolCallId: 'tool-1',
  toolName: 'read',
  message: 'half',
  ...extra,
});
function step(payload: any, options: any = {}) {
  return { payload, ...options };
}

eventCase('accept-session-only', [step({ operation: 'emit', event: { type: 'session_started', sessionId: turn.sessionId } }, { drain: true })]);
eventCase('accept-turn', [step({ operation: 'emit', event: event() }, { drain: true })]);
eventCase('accept-empty-type', [step({ operation: 'emit', event: event({ type: '' }) }, { drain: true })]);
eventCase('accept-lone-surrogate-type', [step({ operation: 'emit', event: event({ type: '\ud800' }) }, { drain: true })]);
eventCase('accept-extra-fields', [step({
  operation: 'emit',
  event: event({
    timeline: { sequence: 7, nested: { keep: [1, null, '中文😀'] } },
    streamBoundary: { reason: 'boundary' },
    moduleId: 'module-1',
    lone: '\ud800',
    extra: { arbitrary: true, list: [false, 0.25, null] },
  }),
}, { drain: true })]);
eventCase('ordering', [
  step({ operation: 'emit', event: event({ sequence: 1 }) }),
  step({ operation: 'emit', event: event({ sequence: 2 }) }),
  step({ operation: 'emit', event: event({ sequence: 3 }) }, { drain: true }),
]);
eventCase('failed-emit-does-not-buffer', [
  step({ operation: 'emit', event: event({ sessionId: 'other' }) }),
  step({ operation: 'emit', event: event({ sequence: 'after-error' }) }, { drain: true }),
]);
eventCase('reject-turn-mismatch', [step({ operation: 'emit', event: event({ turnId: 'other' }) }, { drain: true })]);
eventCase('reject-turn-undefined', [step({ operation: 'emit', event: event({ turnId: UNDEFINED }) }, { drain: true })]);
eventCase('reject-turn-null', [step({ operation: 'emit', event: event({ turnId: null }) }, { drain: true })]);
eventCase('reject-turn-number', [step({ operation: 'emit', event: event({ turnId: 3 }) }, { drain: true })]);
eventCase('reject-turn-array', [step({ operation: 'emit', event: event({ turnId: ['turn'] }) }, { drain: true })]);
eventCase('reject-turn-lone-surrogate', [step({ operation: 'emit', event: event({ turnId: '\ud800' }) }, { drain: true })]);
eventCase('reject-session-missing', [step({ operation: 'emit', event: { type: 'turn_started', turnId: turn.turnId } }, { drain: true })]);
eventCase('reject-session-mismatch', [step({ operation: 'emit', event: event({ sessionId: 'other' }) }, { drain: true })]);
eventCase('reject-session-empty', [step({ operation: 'emit', event: event({ sessionId: '' }) }, { drain: true })]);
eventCase('reject-session-null', [step({ operation: 'emit', event: event({ sessionId: null }) }, { drain: true })]);
eventCase('reject-session-number', [step({ operation: 'emit', event: event({ sessionId: 3 }) }, { drain: true })]);
eventCase('reject-session-lone-surrogate', [step({ operation: 'emit', event: event({ sessionId: '\ud800' }) }, { drain: true })]);
eventCase('reject-type-missing', [step({ operation: 'emit', event: { sessionId: turn.sessionId, turnId: turn.turnId } }, { drain: true })]);
eventCase('reject-type-undefined', [step({ operation: 'emit', event: event({ type: UNDEFINED }) }, { drain: true })]);
eventCase('reject-type-null', [step({ operation: 'emit', event: event({ type: null }) }, { drain: true })]);
eventCase('reject-type-number', [step({ operation: 'emit', event: event({ type: 3 }) }, { drain: true })]);
eventCase('reject-event-null', [step({ operation: 'emit', event: null }, { drain: true })]);
eventCase('reject-event-array', [step({ operation: 'emit', event: [event()] }, { drain: true })]);
eventCase('reject-event-string', [step({ operation: 'emit', event: 'event' }, { drain: true })]);
eventCase('reject-event-number', [step({ operation: 'emit', event: 7 }, { drain: true })]);
eventCase('reject-event-boolean', [step({ operation: 'emit', event: false }, { drain: true })]);
eventCase('reject-event-missing', [step({ operation: 'emit' }, { drain: true })]);
eventCase('reject-event-undefined', [step({ operation: 'emit', event: UNDEFINED }, { drain: true })]);
eventCase('operation-unknown', [step({ operation: 'unknown' }, { drain: true })]);
eventCase('operation-uppercase', [step({ operation: 'Emit' }, { drain: true })]);
eventCase('operation-space', [step({ operation: ' ' }, { drain: true })]);
eventCase('operation-empty', [step({ operation: '' }, { drain: true })]);
eventCase('operation-null', [step({ operation: null }, { drain: true })]);
eventCase('operation-number', [step({ operation: 7 }, { drain: true })]);
eventCase('operation-lone-surrogate', [step({ operation: '\ud800' }, { drain: true })]);
eventCase('operation-undefined', [step({ operation: UNDEFINED }, { drain: true })]);
eventCase('payload-missing-operation', [step({}, { drain: true })]);
eventCase('payload-event-without-operation', [step({ event: event() }, { drain: true })]);
eventCase('payload-array', [step([], { drain: true })]);
eventCase('payload-string', [step('emit', { drain: true })]);
eventCase('payload-number', [step(7, { drain: true })]);
// A throwing emitter is an ordinary module failure in TS (`moduleResponse`):
// an Error keeps its message, a non-Error is String()-coerced, and a string
// `code` is projected onto the response.
eventCase('sink-throws-error', [step({ operation: 'emit', event: event() }, { drain: true })], { kind: 'error', message: 'sink failed' });
eventCase('sink-throws-error-code', [step({ operation: 'emit', event: event() }, { drain: true })], { kind: 'error', message: 'sink failed', fields: { code: 'SINK_CODE' } });
eventCase('sink-throws-non-error', [step({ operation: 'emit', event: event() }, { drain: true })], { kind: 'string', value: 'sink failed' });

const recorded = [];
for (const item of cases) {
  const buffer = item.sink ? undefined : createAgentEventBuffer();
  const dispatcher = dispatcherWith(item.sink
    ? throwingPort(item.sink)
    : { emit: buffer.emitter, drain: buffer.drain });
  const steps = [];
  for (const current of item.steps) {
    const call = request(current.payload);
    const actualCall = { ...call, payload: materialize(current.payload) };
    const protocol = protocolFor(dispatcher.handlers, call);
    const expected = await response(protocol, actualCall);
    const stepRecord: any = { call, response: expected };
    if (current.drain) stepRecord.drained = buffer ? clone(buffer.drain()) : [];
    steps.push(stepRecord);
  }
  const drainAfter = buffer ? clone(buffer.drain()) : [];
  recorded.push({ name: item.name, steps, drainAfter, ...(item.sink ? { sink: item.sink } : {}) });
  await dispatcher.dispose();
}

const oracle = JSON.stringify({ sourceCommit: commit, sourceSha256, turn, cases: recorded }, null, 2) + '\n';
for (const path of [baseline, output]) assert.ok(!oracle.includes(path), `oracle.json contains ${path}`);
await mkdir(output, { recursive: true });
const target = join(output, 'oracle.json');
const temporary = `${target}.tmp`;
// Write-then-rename: a killed or concurrent generator must never leave a
// truncated oracle.json behind.
await writeFile(temporary, oracle);
await rename(temporary, target);
const count = recorded.reduce((sum, item) => sum + item.steps.length, 0);
console.log(`HOST_EVENT_ORACLE_PASS cases=${recorded.length} calls=${count} drains=${recorded.filter((item) => item.steps.some((step: any) => step.drained !== undefined)).length}`);
