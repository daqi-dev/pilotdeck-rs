import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Real fixed-baseline oracle for the model module adapter.
// The dispatcher's private SidecarTurnProtocol is exported in memory only;
// baseline sources are never copied into this repository.
import assert from 'node:assert/strict';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = assertBaseline(baseline);
const base = 'src/agent/modules/transport/';
const clientFile = base + 'agentLoopSidecarClient.ts';
const dispatcherFile = base + 'sidecarDefaultModuleDispatcher.ts';
const portsFile = base + 'sidecarHostModulePorts.ts';
const files = [clientFile, dispatcherFile, portsFile];

const requireBaseline = createRequire(join(baseline, 'package.json'));
const requireTsx = createRequire(requireBaseline.resolve('tsx'));
const { transformSync } = requireTsx('esbuild');
const sources = await baselineSourceHashes(baseline, files);
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
const { createSidecarModuleComposition } = await import(pathToFileURL(join(baseline, portsFile)).href);

const clone = (value: any): any => value === undefined ? undefined : JSON.parse(JSON.stringify(value));
function specError(spec: any) {
  const error = new Error(spec.message) as Error & Record<string, unknown>;
  if (spec.fields !== undefined) Object.assign(error, clone(spec.fields));
  return error;
}
// Script values are literal JSON. `{undefined:true}` and `{nonfinite:"NaN"}`
// preserve JS values JSON cannot spell without changing the oracle shape.
function sentinel(value: any): any {
  if (Array.isArray(value)) return value.map(sentinel);
  if (value && typeof value === 'object') {
    if (value.throw !== undefined) throw new Error(value.throw);
    if (value.undefined === true) return undefined;
    if (value.nonfinite !== undefined) return Number(value.nonfinite);
    const object: Record<string, unknown> = {};
    for (const [key, inner] of Object.entries(value)) object[key] = sentinel(inner);
    return object;
  }
  return value;
}
function logValue(log: any[], value: any) { log.push(clone(value)); }

function createFake(script: any, log: any[]) {
  const prepares = [...(script.prepares ?? [])];
  const streams = [...(script.streams ?? [])];
  const execution = {
    async prepare({ request, context }: any) {
      logValue(log, { op: 'prepare', request, context });
      assert.ok(prepares.length > 0, 'prepare script exhausted');
      const step = prepares.shift();
      if (step.error) throw specError(step.error);
      return clone(step.ok);
    },
    stream({ prepared, context }: any) {
      logValue(log, { op: 'stream', prepared, context });
      assert.ok(streams.length > 0, 'stream script exhausted');
      const step = streams.shift();
      if (step.createError) throw specError(step.createError);
      const steps = [...(step.steps ?? [])];
      const iterator = {
        async next() {
          logValue(log, { op: 'stream.next' });
          if (steps.length === 0) return { done: true, value: undefined };
          const current = steps.shift();
          if (current.error) throw specError(current.error);
          return { done: false, value: clone(current.event) };
        },
        async return() {
          logValue(log, { op: 'stream.return' });
          if (step.returnError) throw specError(step.returnError);
          return { done: true, value: undefined };
        },
        [Symbol.asyncIterator]() { return this; },
      };
      return iterator;
    },
  };
  let metadata: any;
  if (script.metadata !== null) {
    metadata = {};
    const methods: [string, string][] = [
      ['maxContextTokens', 'getModelMaxContextTokens'],
      ['maxOutputTokens', 'getModelMaxOutputTokens'],
      ['tokenLimits', 'getModelTokenLimits'],
      ['protocol', 'getModelProtocol'],
      ['supportsPromptCache', 'getModelSupportsPromptCache'],
    ];
    for (const [key, method] of methods) {
      if (!(key in script.metadata)) continue;
      metadata[method] = (...args: any[]) => {
        logValue(log, { op: `metadata.${key}`, args });
        return sentinel(script.metadata[key]);
      };
    }
  }
  return {
    execution,
    metadata,
    remaining: () => ({ prepares: prepares.length, streams: streams.length }),
  };
}

let nextId = 0;
function call(payload: any, runId = 'run', operationId = 'operation', extra: any = {}) {
  return {
    kind: 'request', method: 'module_call', messageId: `call-${++nextId}`,
    requestId: 'callback-request', runId, operationId, module: 'model', payload, ...extra,
  };
}
const config = {
  cwd: '/fixture', provider: 'fixture', model: 'fixture', permissionMode: 'default',
  permissionContext: { canPrompt: false, rules: [] },
};
const prompt = 'fixture';
function inputFor(turn: any) {
  return {
    sessionId: turn.sessionId,
    turnId: turn.turnId,
    messages: [],
    ...(turn.workspaceId ? { workspaceId: turn.workspaceId } : {}),
    ...(turn.storageConfigVersion ? { storageConfigVersion: turn.storageConfigVersion } : {}),
    execution: {
      runId: 'run',
      operationId: 'operation',
      ...(turn.operationDeadline ? { operationDeadline: turn.operationDeadline } : {}),
    },
  };
}
function makeProtocol(input: any, handlers: any, current: any) {
  return new SidecarTurnProtocol({
    config: {}, input, checkpoint: {}, manifest: {}, moduleHandlers: handlers,
    uuid: () => current.messageId,
  });
}
async function response(protocol: any, request: any) {
  const first = await protocol.moduleResponseFor(request);
  const replay = await protocol.moduleResponseFor(request);
  assert.equal(first.replayed, false);
  assert.equal(replay.replayed, true);
  assert.deepEqual(first.response, replay.response);
  return JSON.parse(JSON.stringify(first.response));
}

const req = (provider = 'provider-a', model = 'model-a', text = 'hello') => ({
  provider, model,
  messages: [{ role: 'user', content: [{ type: 'text', text }] }],
  systemPrompt: 'fixture-system',
});
const prepared = (request: any, provider = request.provider, model = request.model, maxContextTokens?: any, maxOutputTokens?: any) => ({
  request, provider, model,
  ...(maxContextTokens !== undefined ? { maxContextTokens } : {}),
  ...(maxOutputTokens !== undefined ? { maxOutputTokens } : {}),
});
const event = (text: string) => ({ type: 'text_delta', text, raw: { vendor: 'fixture' } });
const turn = { sessionId: 'session', turnId: 'turn', workspaceId: 'workspace', storageConfigVersion: 'storage-v1', operationDeadline: 'deadline' };

const scenarios: any[] = [];
function scenario(name: string, script: any, calls: any[], options: any = {}) {
  scenarios.push({ name, turn: options.turn ?? turn, script, calls, ...(options.bindTurn ? { bindTurn: true } : {}), ...(options.dispatcherOnly ? { dispatcherOnly: true } : {}) });
}

scenario('metadata_full', {
  metadata: {
    maxContextTokens: 200000, maxOutputTokens: 8192,
    tokenLimits: { maxContextTokens: 200000, maxOutputTokens: 8192 },
    protocol: 'anthropic', supportsPromptCache: true,
  },
  prepares: [], streams: [],
}, [
  { payload: { operation: 'get_metadata', provider: 'provider-a', model: 'model-a' } },
  { payload: { operation: 'get_metadata', provider: 'provider-b', model: 'model-b' } },
  { payload: { operation: 'get_metadata', model: 'model-a' } },
  { payload: { operation: 'get_metadata', provider: 'provider-a' } },
  { payload: { operation: 'get_metadata', provider: 3, model: 'model-a' } },
]);

scenario('metadata_partial', {
  metadata: { maxContextTokens: { undefined: true }, supportsPromptCache: true },
  prepares: [], streams: [],
}, [
  { payload: { operation: 'get_metadata', provider: 'provider-a', model: 'model-a' } },
]);

scenario('metadata_empty_unavailable', { metadata: {}, prepares: [], streams: [] }, [
  { payload: { operation: 'get_metadata', provider: 'provider-a', model: 'model-a' } },
]);
scenario('metadata_absent_unavailable', { metadata: null, prepares: [], streams: [] }, [
  { payload: { operation: 'get_metadata', provider: 'provider-a', model: 'model-a' } },
]);

for (const [name, metadata] of [
  ['metadata_invalid_fraction', { maxContextTokens: 1.5 }],
  ['metadata_invalid_zero', { maxOutputTokens: 0 }],
  ['metadata_invalid_negative', { maxContextTokens: -1 }],
  ['metadata_invalid_safe_integer', { maxContextTokens: 9007199254740992 }],
  ['metadata_invalid_nonfinite_nan', { maxContextTokens: { nonfinite: 'NaN' } }],
  ['metadata_invalid_nonfinite_infinity', { maxContextTokens: { nonfinite: 'Infinity' } }],
  ['metadata_invalid_string', { maxContextTokens: '200000' }],
  ['metadata_invalid_null', { maxOutputTokens: null }],
  ['metadata_invalid_limits_context', { tokenLimits: { maxContextTokens: 0 } }],
  ['metadata_invalid_limits_output', { tokenLimits: { maxContextTokens: 10, maxOutputTokens: 2.5 } }],
  ['metadata_invalid_protocol', { protocol: 'fixture' }],
  ['metadata_null_protocol', { protocol: null }],
  ['metadata_throw', { maxContextTokens: { throw: 'metadata max context lookup failed' } }],
] as const) {
  scenario(name, { metadata, prepares: [], streams: [] }, [
    { payload: { operation: 'get_metadata', provider: 'provider-a', model: 'model-a' } },
  ]);
}
scenario('metadata_limits_extra_and_safe_max', {
  metadata: {
    maxContextTokens: 9007199254740991,
    tokenLimits: { maxContextTokens: 100, extra: 'keep-me' },
    protocol: 'openai-responses', supportsPromptCache: false,
  },
  prepares: [], streams: [],
}, [{ payload: { operation: 'get_metadata', provider: 'provider-a', model: 'model-a' } }]);

scenario('prepare_and_materialize', {
  metadata: {
    maxContextTokens: 200000, maxOutputTokens: 8192,
    tokenLimits: { maxContextTokens: 200000, maxOutputTokens: 8192 },
    protocol: 'openai', supportsPromptCache: true,
  },
  prepares: [
    { ok: prepared(req('provider-a', 'model-a', 'first'), 'provider-a', 'model-a', 200000, 8192) },
    { ok: prepared(req('provider-b', 'model-b', 'second'), 'provider-b', 'model-b', 0, -5) },
    { error: { message: 'prepare exploded \ud800', fields: { code: 'MODEL_PREPARE', error: { code: 'prepare_failed', message: 'prepare exploded \ud800' }, retryable: true, retryAfterMs: 12.5 } } },
    { ok: prepared(req('provider-c', 'model-c', 'third'), 'provider-c', 'model-c') },
    { ok: prepared(req('provider-a', 'model-a', 'overwritten'), 'provider-a', 'model-a') },
  ],
  streams: [],
}, [
  { payload: { operation: 'prepare', preparationId: 'prep-1', request: req('provider-a', 'model-a', 'first'), context: { modelOverride: { provider: 'override-provider', model: 'override-model' }, metadata: { seed: 1 } } }, extra: { idempotencyKey: 'idem-1' } },
  { payload: { operation: 'materialize_prepared_request', preparationId: 'prep-1', request: req('provider-b', 'model-b', 'candidate') } },
  { payload: { operation: 'materialize_prepared_request', preparationId: 'missing', request: req() } },
  { payload: { operation: 'materialize_prepared_request', preparationId: 'prep-1', request: { provider: 'x', model: 'y' } } },
  { payload: { operation: 'prepare', preparationId: 'prep-2', request: req('provider-b', 'model-b', 'second') } },
  { payload: { operation: 'prepare', request: req() } },
  { payload: { operation: 'prepare', preparationId: 'prep-3' } },
  { payload: { operation: 'prepare', preparationId: 'prep-4', request: { provider: 'p', model: 'm' } } },
  { payload: { operation: 'prepare', preparationId: 'prep-5', request: [] } },
  { payload: { operation: 'prepare', preparationId: 'prep-6', request: { provider: 3, model: 'm', messages: [] } } },
  { payload: { operation: 'prepare', preparationId: 'prep-7', request: req() } },
  { payload: { operation: 'materialize_prepared_request', preparationId: 'prep-7', request: req() } },
  { payload: { operation: 'prepare', preparationId: 'prep-8', request: req('provider-c', 'model-c', 'third') } },
  { payload: { operation: 'prepare', preparationId: 'prep-1', request: req('provider-a', 'model-a', 'overwritten') } },
  { payload: { operation: 'materialize_prepared_request', preparationId: 'prep-1', request: req('provider-b', 'model-b', 'after-overwrite') } },
]);

scenario('prepare_metadata_error_keeps_preparation', {
  metadata: { maxContextTokens: 0 },
  prepares: [{ ok: prepared(req()) }], streams: [],
}, [
  { payload: { operation: 'prepare', preparationId: 'kept', request: req() } },
  { payload: { operation: 'materialize_prepared_request', preparationId: 'kept', request: req('provider-b', 'model-b') } },
]);

scenario('stream_operation', {
  metadata: null,
  prepares: [{ ok: prepared(req()) }],
  streams: [
    { steps: [{ event: event('a') }, { event: event('b') }] },
    { steps: [{ event: event('c') }, { error: { message: 'stream next exploded', fields: { code: 'MODEL_STREAM' } } }] },
  ],
}, [
  { payload: { operation: 'prepare', preparationId: 'stream-prepared', request: req() } },
  { payload: { operation: 'stream', preparationId: 'stream-prepared' } },
  { payload: { operation: 'stream', preparationId: 'stream-prepared', request: req('provider-b', 'model-b', 'override') } },
  { payload: { operation: 'materialize_prepared_request', preparationId: 'stream-prepared', request: req('provider-b', 'model-b', 'after-error') } },
  { payload: { operation: 'stream', preparationId: 'stream-prepared', request: { provider: 'p' } } },
  { payload: { operation: 'stream', preparationId: 'missing' } },
]);

scenario('stream_next_lifecycle', {
  metadata: null,
  prepares: [
    { ok: prepared(req('provider-a', 'model-a')) },
    { ok: prepared(req('provider-b', 'model-b')) },
    { ok: prepared(req('provider-c', 'model-c')) },
    { ok: prepared(req('provider-d', 'model-d')) },
    { ok: prepared(req('provider-e', 'model-e')) },
    { ok: prepared(req('provider-f', 'model-f')) },
  ],
  streams: [
    { steps: [{ event: event('e1') }, { event: event('e2') }] },
    { steps: [{ event: event('e3') }, { error: { message: 'next failure', fields: { code: 'MODEL_NEXT' } } }] },
    { steps: [] },
    { createError: { message: 'stream create failure', fields: { code: 'MODEL_CREATE' } } },
    { steps: [{ event: event('e4') }] },
    { steps: [{ event: event('e5') }, { event: event('e6') }], returnError: { message: 'return failure', fields: { code: 'MODEL_RETURN' } } },
    { steps: [{ event: event('e7') }] },
  ],
}, [
  { payload: { operation: 'prepare', preparationId: 'a', request: req('provider-a', 'model-a') } },
  { payload: { operation: 'prepare', preparationId: 'b', request: req('provider-b', 'model-b') } },
  { payload: { operation: 'prepare', preparationId: 'c', request: req('provider-c', 'model-c') } },
  { payload: { operation: 'prepare', preparationId: 'd', request: req('provider-d', 'model-d') } },
  { payload: { operation: 'prepare', preparationId: 'e', request: req('provider-e', 'model-e') } },
  { payload: { operation: 'prepare', preparationId: 'f', request: req('provider-f', 'model-f') } },
  { payload: { operation: 'stream_next', preparationId: 'a', request: req('provider-a', 'model-a') } },
  { payload: { operation: 'stream_next', preparationId: 'a', request: req('provider-a', 'model-a') } },
  { payload: { operation: 'stream_next', preparationId: 'a', request: req('provider-a', 'model-a') } },
  { payload: { operation: 'stream_next', preparationId: 'a', request: req('provider-a', 'model-a') } },
  { payload: { operation: 'stream_next', preparationId: 'b', request: req('provider-b', 'model-b') } },
  { payload: { operation: 'stream_next', preparationId: 'b', request: req('provider-b', 'model-b') } },
  { payload: { operation: 'stream_next', preparationId: 'b', request: req('provider-b', 'model-b') } },
  { payload: { operation: 'stream_next', preparationId: 'c', request: req('provider-c', 'model-c') } },
  { payload: { operation: 'stream_next', preparationId: 'c', request: req('provider-c', 'model-c') } },
  { payload: { operation: 'stream_next', preparationId: 'd', request: req('provider-d', 'model-d') } },
  { payload: { operation: 'stream_next', preparationId: 'd' } },
  { payload: { operation: 'stream_next', preparationId: 'd', request: req('provider-d', 'model-d') } },
  { payload: { operation: 'stream_next', preparationId: 'd', request: req('provider-d', 'model-d') } },
  { payload: { operation: 'stream_next', preparationId: 'e', request: req('provider-e', 'model-e') } },
  { payload: { operation: 'close_stream', preparationId: 'e' } },
  { payload: { operation: 'stream_next', preparationId: 'e', request: req('provider-e', 'model-e') } },
  { payload: { operation: 'close_stream', preparationId: 'e' } },
  { payload: { operation: 'stream_next', preparationId: 'f', request: req('provider-f', 'model-f') } },
  { payload: { operation: 'close_stream', preparationId: 'f' } },
  { payload: { operation: 'stream_next', preparationId: 'f', request: req('provider-f', 'model-f') } },
  { payload: { operation: 'stream_next', preparationId: 'missing', request: req() } },
  { payload: { operation: 'close_stream', preparationId: 'missing' } },
]);

scenario('close_prepared_without_stream', {
  metadata: null, prepares: [{ ok: prepared(req()) }], streams: [],
}, [
  { payload: { operation: 'prepare', preparationId: 'prepared-only', request: req() } },
  { payload: { operation: 'close_stream', preparationId: 'prepared-only' } },
  { payload: { operation: 'materialize_prepared_request', preparationId: 'prepared-only', request: req() } },
]);

scenario('dispose_clears_streams', {
  metadata: null,
  prepares: [
    { ok: prepared(req('provider-a', 'model-a')) },
    { ok: prepared(req('provider-b', 'model-b')) },
    { ok: prepared(req('provider-c', 'model-c')) },
  ],
  streams: [
    { steps: [{ event: event('d1') }, { event: event('d2') }] },
    { steps: [{ event: event('d3') }] },
    { steps: [] },
  ],
}, [
  { payload: { operation: 'prepare', preparationId: 'da', request: req('provider-a', 'model-a') } },
  { payload: { operation: 'prepare', preparationId: 'db', request: req('provider-b', 'model-b') } },
  { payload: { operation: 'prepare', preparationId: 'dc', request: req('provider-c', 'model-c') } },
  { payload: { operation: 'stream_next', preparationId: 'da', request: req('provider-a', 'model-a') } },
  { payload: { operation: 'stream_next', preparationId: 'db', request: req('provider-b', 'model-b') } },
  { payload: { operation: 'stream_next', preparationId: 'dc', request: req('provider-c', 'model-c') } },
]);

scenario('sticky_turn', {
  metadata: null, prepares: [{ ok: prepared(req()) }, { ok: prepared(req()) }], streams: [{ steps: [] }, { steps: [] }],
  routing: { sticky: { previousTier: 'tier-1', previousProvider: 'provider-a', previousModel: 'model-a', orchestrating: false } },
}, [
  { payload: { operation: 'prepare', preparationId: 's1', request: req() } },
  { payload: { operation: 'stream_next', preparationId: 's1', request: req() } },
  { payload: { operation: 'prepare', preparationId: 's2', request: req() } },
  { payload: { operation: 'stream_next', preparationId: 's2', request: req() } },
], { bindTurn: true });

scenario('sticky_orchestrating', {
  metadata: null, prepares: [{ ok: prepared(req()) }, { ok: prepared(req()) }], streams: [{ steps: [] }, { steps: [] }],
  routing: { sticky: { previousTier: 'tier-1', previousProvider: 'provider-a', previousModel: 'model-a', orchestrating: true } },
}, [
  { payload: { operation: 'prepare', preparationId: 'o1', request: req() } },
  { payload: { operation: 'stream_next', preparationId: 'o1', request: req() } },
  { payload: { operation: 'prepare', preparationId: 'o2', request: req() } },
  { payload: { operation: 'stream_next', preparationId: 'o2', request: req() } },
], { bindTurn: true });

scenario('sticky_empty', {
  metadata: null, prepares: [{ ok: prepared(req()) }], streams: [],
  routing: { sticky: {} },
}, [
  { payload: { operation: 'prepare', preparationId: 'e1', request: req(), context: { metadata: { seed: 2 } } } },
], { bindTurn: true });

scenario('sticky_absent', {
  metadata: null, prepares: [{ ok: prepared(req()) }], streams: [],
}, [
  { payload: { operation: 'prepare', preparationId: 'a1', request: req(), context: { metadata: { seed: 4 } } } },
]);

scenario('sticky_null_return', {
  metadata: null, prepares: [{ ok: prepared(req()) }], streams: [],
  routing: { sticky: null },
}, [
  { payload: { operation: 'prepare', preparationId: 'n1', request: req(), context: { metadata: { seed: 3 } } } },
], { bindTurn: true });

scenario('sticky_error_clears_tier', {
  metadata: null, prepares: [{ ok: prepared(req()) }, { ok: prepared(req()) }],
  streams: [{ steps: [{ error: { message: 'sticky stream failed' } }] }, { steps: [] }],
  routing: { sticky: { previousTier: 'tier-1', previousProvider: 'provider-a', previousModel: 'model-a', orchestrating: false } },
}, [
  { payload: { operation: 'prepare', preparationId: 'x1', request: req() } },
  { payload: { operation: 'stream_next', preparationId: 'x1', request: req() } },
  { payload: { operation: 'prepare', preparationId: 'x2', request: req() } },
  { payload: { operation: 'stream_next', preparationId: 'x2', request: req() } },
], { bindTurn: true });

scenario('operation_errors', { metadata: null, prepares: [], streams: [] }, [
  { payload: { operation: 'unknown' } },
  { payload: { operation: 'unknown', preparationId: 'prepared' } },
  { payload: { operation: '', preparationId: 'prepared' } },
  { payload: { operation: 'get_metadata', provider: '', model: 'm' } },
  { payload: {} },
  { payload: { operation: 'stream' } },
]);

for (const [name, tokenLimits] of [
  ['metadata_limits_undefined_output', { maxContextTokens: 100, maxOutputTokens: { undefined: true } }],
  ['metadata_limits_undefined_context', { maxContextTokens: { undefined: true }, maxOutputTokens: 100 }],
] as const) {
  scenario(name, { metadata: { tokenLimits }, prepares: [], streams: [] }, [
    { payload: { operation: 'get_metadata', provider: 'provider-a', model: 'model-a' } },
  ]);
}

scenario('sticky_create_error_cleans_preparation', {
  metadata: null, prepares: [{ ok: prepared(req()) }, { ok: prepared(req()) }],
  streams: [{ createError: { message: 'sticky create failed' } }],
  routing: { sticky: { previousTier: 'tier-1', previousProvider: 'provider-a', previousModel: 'model-a', orchestrating: false } },
}, [
  { payload: { operation: 'prepare', preparationId: 'failed', request: req() } },
  { payload: { operation: 'stream_next', preparationId: 'failed', request: req() } },
  { payload: { operation: 'materialize_prepared_request', preparationId: 'failed', request: req() } },
  { payload: { operation: 'prepare', preparationId: 'retry', request: req() } },
], { bindTurn: true });

scenario('sticky_empty_strings_preserve_metadata', {
  metadata: null, prepares: [{ ok: prepared(req()) }, { ok: prepared(req()) }], streams: [],
  routing: { sticky: { previousTier: '', previousProvider: '', previousModel: '', orchestrating: false } },
}, [
  { payload: { operation: 'prepare', preparationId: 'remote', request: req(), context: {
    metadata: { previousTier: 'remote-tier', previousProvider: 'remote-provider', previousModel: 'remote-model' },
  } } },
  { payload: { operation: 'prepare', preparationId: 'empty', request: req() } },
], { bindTurn: true });

// Exercise odd port echoes through the actual dispatcher. Object spread only
// copies enumerable keys: array/string length is not part of the result.
for (const [name, request] of [
  ['string', 'ab'],
  ['string_utf16', 'A\ud83d\ude00\ud800'],
  ['array', ['first', { nested: true }]],
  ['null', null],
  ['number', 42],
] as [string, any][]) {
  scenario(`materialize_fallback_${name}`, {
    metadata: null,
    prepares: [{ ok: prepared(request, 'prepared-provider', 'prepared-model') }],
    streams: [],
  }, [
    { payload: { operation: 'prepare', preparationId: 'odd-echo', request: req() } },
    { payload: { operation: 'materialize_prepared_request', preparationId: 'odd-echo', request: req('candidate-provider', 'candidate-model', 'candidate') } },
  ]);
}

// Malformed transport IDs cannot pass SidecarTurnProtocol's identity check;
// probe these adapter inputs directly at the real dispatcher boundary.
const contextValues = [
  { runId: 42, operationId: null, idempotencyKey: 42 },
  { runId: null, operationId: 42, idempotencyKey: true },
  { runId: false, operationId: {}, idempotencyKey: [] },
  { runId: [], operationId: true, idempotencyKey: {} },
  { runId: 0, operationId: false, idempotencyKey: 0 },
  { runId: {}, operationId: [], idempotencyKey: false },
  { runId: '', operationId: '', idempotencyKey: '' },
  { runId: null, operationId: null, idempotencyKey: null },
];
scenario('execution_context_values', {
  metadata: null,
  prepares: contextValues.map(() => ({ ok: prepared(req()) })),
  streams: [{ steps: [] }, { steps: [] }],
}, [
  ...contextValues.map((extra, index) => ({
    payload: { operation: 'prepare', preparationId: `context-${index}`, request: req() }, extra,
  })),
  { payload: { operation: 'stream', preparationId: 'context-0' }, extra: contextValues[0] },
  { payload: { operation: 'stream_next', preparationId: 'context-1', request: req() }, extra: contextValues[1] },
], { dispatcherOnly: true });

const results = [];
for (const item of scenarios) {
  const log: any[] = [];
  const routingLog: any[] = [];
  const fake = createFake(item.script, log);
  const ports: any = {
    model: fake.execution,
    ...(fake.metadata ? { metadata: fake.metadata } : {}),
    ...(item.script.routing ? {
      routing: {
        invalidateSticky(sessionId: string) {
          logValue(routingLog, { op: 'invalidateSticky', sessionId });
          return sentinel(item.script.routing.sticky);
        },
      },
    } : {}),
    toolExecution: { list: () => [], executeAll: async () => [] },
  };
  const composition = createSidecarModuleComposition(ports);
  const modules = item.bindTurn
    ? { ...composition, model: composition.model.bindTurn!({ sessionId: item.turn.sessionId, turnId: item.turn.turnId }) }
    : composition;
  const input = inputFor(item.turn);
  const dispatcher = createSidecarDefaultModuleDispatcher({
    config, input, checkpoint: {}, capabilityResultObserver: {},
    planTodoHandler: async () => ({}), modules,
  });
  const calls = [];
  for (const step of item.calls) {
    const request = call(step.payload, step.runId ?? 'run', step.operationId ?? 'operation', step.extra ?? {});
    const start = log.length;
    const protocol = makeProtocol(input, dispatcher.handlers, request);
    const expected = item.dispatcherOnly
      ? clone(await dispatcher.handlers.model(request))
      : await response(protocol, request);
    calls.push({ call: request, response: expected, portCalls: JSON.parse(JSON.stringify(log.slice(start))) });
  }
  const disposeStart = log.length;
  await dispatcher.dispose();
  const remaining = fake.remaining();
  assert.deepEqual(remaining, { prepares: 0, streams: 0 }, `${item.name} script exhausted`);
  results.push({
    name: item.name, turn: item.turn, script: item.script,
    ...(item.dispatcherOnly ? { dispatcherOnly: true } : {}),
    ...(item.script.routing ? { routing: item.script.routing, routingCalls: routingLog } : {}),
    calls,
    dispose: { portCalls: JSON.parse(JSON.stringify(log.slice(disposeStart))) },
  });
}

// Concurrency goldens call the actual dispatcher with controlled promises.
// The Rust port borrows &mut self, so next-body polling is serialized there;
// compare accepted responses and creation/return ownership, not JS scheduling.
function deferred() {
  let release!: () => void;
  const promise = new Promise<void>(resolve => { release = resolve; });
  return { promise, release };
}
function concurrentDispatcher(model: any) {
  const modules = createSidecarModuleComposition({
    model, toolExecution: { list: () => [], executeAll: async () => [] },
  } as any);
  const dispatcher = createSidecarDefaultModuleDispatcher({
    config, input: inputFor(turn), checkpoint: {}, capabilityResultObserver: {},
    planTodoHandler: async () => ({}), modules,
  } as any);
  const invoke = (operation: string, id = 'a') => dispatcher.handlers.model!(call({
    operation, preparationId: id, request: { ...req(), id },
  }) as any);
  return { dispatcher, invoke };
}
const concurrency: Record<string, any> = {};
{
  const gate = deferred();
  const closes: string[] = [];
  const { dispatcher, invoke } = concurrentDispatcher({
    prepare: async ({ request }: any) => prepared(request),
    stream({ prepared }: any) {
      const id = prepared.request.id;
      return {
        async next() { return { done: false, value: event(id) }; },
        async return() { closes.push(id); await gate.promise; return { done: true }; },
        [Symbol.asyncIterator]() { return this; },
      };
    },
  });
  for (const id of ['a', 'b']) { await invoke('prepare', id); await invoke('stream_next', id); }
  const pending = dispatcher.dispose();
  const started = [...closes];
  assert.deepEqual(started, ['a', 'b']);
  gate.release(); await pending;
  concurrency.dispose = { started, completed: closes };
}
{
  const gate = deferred();
  let creates = 0;
  const { dispatcher, invoke } = concurrentDispatcher({
    prepare: async ({ request }: any) => prepared(request),
    stream() {
      creates++;
      return (async function* () { await gate.promise; })();
    },
  });
  await invoke('prepare');
  const pending = [invoke('stream_next'), invoke('stream_next')];
  assert.equal(creates, 1);
  const createsBeforeRelease = creates;
  gate.release();
  const responses = await Promise.all(pending);
  assert.deepEqual(responses, [{ events: [], done: true }, { events: [], done: true }]);
  const after = await invoke('stream_next').then(value => ({ value }), error => ({ error: error.message }));
  concurrency.eof = { createsBeforeRelease, responses, after };
  await dispatcher.dispose();
}
{
  const nextGate = deferred(), closeGate = deferred();
  let returns = 0;
  const { dispatcher, invoke } = concurrentDispatcher({
    prepare: async ({ request }: any) => prepared(request),
    stream() {
      return {
        async next() { await nextGate.promise; return { done: true }; },
        async return() { returns++; await closeGate.promise; return { done: true }; },
        [Symbol.asyncIterator]() { return this; },
      };
    },
  });
  await invoke('prepare');
  const next = invoke('stream_next');
  const close = invoke('close_stream');
  assert.equal(returns, 1);
  const returnsBeforeRelease = returns;
  nextGate.release(); closeGate.release();
  await next;
  const response = await close;
  await dispatcher.dispose();
  concurrency.close = { returnsBeforeRelease, response, returnsAfterDispose: returns };
}

// An async generator queues return() behind every already accepted next().
// The create gate lives in its first next(), matching a Rust port that awaits
// provider setup before it can return its stream object.
for (const [key, count, closeFails] of [
  ['creationBeforeClose', 1, false],
  ['acceptedBeforeClose', 2, false],
  ['acceptedBeforeCloseError', 2, true],
] as const) {
  const createGate = deferred(), closeGate = deferred();
  const trace: string[] = [];
  const { dispatcher, invoke } = concurrentDispatcher({
    prepare: async ({ request }: any) => prepared(request),
    stream({ prepared }: any) {
      const id = prepared.request.id;
      trace.push(`create:${id}`);
      return (async function* () {
        await createGate.promise;
        trace.push(`created:${id}`);
        try {
          for (let step = 1; ; step++) {
            trace.push(`next:${id}`);
            yield event(`${id}-${step}`);
          }
        } finally {
          trace.push(`close:${id}`);
          await closeGate.promise;
          if (closeFails) throw new Error(`close failed: ${id}`);
          trace.push(`closed:${id}`);
        }
      })();
    },
  });
  await invoke('prepare');
  const pending = Array.from({ length: count }, () => invoke('stream_next'));
  let closeSettled = false;
  const close = invoke('close_stream').then(
    value => { closeSettled = true; return { value }; },
    error => { closeSettled = true; return { error: error.message }; },
  );
  assert.deepEqual(trace, ['create:a']);
  const after = await invoke('stream_next').then(value => ({ value }), error => ({ error: error.message }));
  createGate.release();
  const responses = await Promise.all(pending);
  assert.deepEqual(responses, Array.from({ length: count }, (_, i) => ({ events: [event(`a-${i + 1}`)], done: false })));
  assert.equal(closeSettled, false, 'next responses must not wait for return()');
  closeGate.release();
  const closeResponse = await close;
  await dispatcher.dispose();
  assert.equal(trace.filter(entry => entry === 'close:a').length, 1);
  concurrency[key] = { responses, close: closeResponse, after, trace };
}
{
  const started: string[] = [], completed: string[] = [];
  const { dispatcher, invoke } = concurrentDispatcher({
    prepare: async ({ request }: any) => prepared(request),
    stream({ prepared }: any) {
      const id = prepared.request.id;
      return {
        async next() { return { done: false, value: event(id) }; },
        async return() {
          started.push(id);
          if (id === 'a') throw new Error(`close failed: ${id}`);
          completed.push(id);
          return { done: true };
        },
        [Symbol.asyncIterator]() { return this; },
      };
    },
  });
  for (const id of ['a', 'b']) { await invoke('prepare', id); await invoke('stream_next', id); }
  await dispatcher.dispose();
  assert.deepEqual(started, ['a', 'b']);
  assert.deepEqual(completed, ['b']);
  concurrency.disposeFailure = { started, completed };
}
{
  const nextGate = deferred(), closeGate = deferred();
  const started: string[] = [], completed: string[] = [];
  const { dispatcher, invoke } = concurrentDispatcher({
    prepare: async ({ request }: any) => prepared(request),
    async *stream({ prepared }: any) {
      const id = prepared.request.id;
      if (id === 'a') await nextGate.promise;
      try { yield event(`${id}-1`); }
      finally {
        started.push(id);
        if (id === 'a') await closeGate.promise;
        completed.push(id);
      }
    },
  });
  await invoke('prepare');
  const next = invoke('stream_next');
  await invoke('prepare', 'b');
  const bNext = await invoke('stream_next', 'b');
  nextGate.release();
  const aNext = await next;
  const close = invoke('close_stream');
  const bClose = await invoke('close_stream', 'b');
  const startedBeforeRelease = [...started], completedBeforeRelease = [...completed];
  assert.deepEqual(startedBeforeRelease, ['a', 'b']);
  assert.deepEqual(completedBeforeRelease, ['b']);
  closeGate.release();
  const aClose = await close;
  await dispatcher.dispose();
  concurrency.crossStreams = { aNext, bNext, aClose, bClose, startedBeforeRelease, completedBeforeRelease, completed };
}

{
  let creates = 0;
  let metadata: any;
  const modules = createSidecarModuleComposition({
    model: {
      async prepare({ request, context }: any) { metadata = clone(context.metadata); return prepared(request); },
      stream() { creates++; return (async function* () {})(); },
    },
    routing: { invalidateSticky: () => ({ previousTier: 'sticky-tier', orchestrating: false }) },
    toolExecution: { list: () => [], executeAll: async () => [] },
  } as any);
  const execution = modules.model.bindTurn!({ sessionId: 's', turnId: 't' }).execution;
  const stream = execution.stream({ prepared: prepared(req()), context: {} } as any)[Symbol.asyncIterator]();
  await stream.return!();
  const afterClose = clone(await stream.next());
  await execution.prepare({ request: req(), context: {} } as any);
  assert.equal(creates, 0);
  assert.deepEqual(metadata, { previousTier: 'sticky-tier' });
  concurrency.stickyUnstarted = { creates, afterClose, metadata };
}

await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), JSON.stringify({ commit, sources, scenarios: results, concurrency }, null, 2) + '\n');
console.log(`HOST_MODEL_ORACLE_PASS scenarios=${results.length} calls=${results.reduce((sum, s) => sum + s.calls.length, 0)} disposeReturns=${results.reduce((sum, s) => sum + s.dispose.portCalls.length, 0)} concurrency=${Object.keys(concurrency).length}`);
