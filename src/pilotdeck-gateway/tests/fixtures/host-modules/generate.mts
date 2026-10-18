import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Real fixed-baseline oracle; private exports are exposed in memory, never copied.
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
const contracts = JSON.parse(await readFile(new URL('../../../src/config/contracts.json', import.meta.url), 'utf8'));
const operations = [...new Set([
  'unknown', '', null, 3, ' ', '\ud800',
  ...Object.values(contracts).flatMap((contract: any) => contract.methods),
])];
const commit = assertBaseline(baseline);
const base = 'src/agent/modules/transport/';
const files = [base + 'agentLoopSidecarClient.ts', base + 'sidecarDefaultModuleDispatcher.ts',
  'src/agent/modules/capability/hostPlanTodoModuleHandler.ts'];
const requireBaseline = createRequire(join(baseline, 'package.json'));
const requireTsx = createRequire(requireBaseline.resolve('tsx'));
const { transformSync } = requireTsx('esbuild');
const sources = await baselineSourceHashes(baseline, files);
// Appending exports changes visibility only. Compile the entire original file
// at its original filename so imports still resolve to real baseline modules.
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
const { SidecarTurnProtocol } = await expose(files[0], ['SidecarTurnProtocol']);
const helpers = await expose(files[1], ['stringField', 'optionalStringField', 'booleanField', 'positiveFinite', 'optionalNonNegativeFinite']);
const { createSidecarDefaultModuleDispatcher } = await import(pathToFileURL(join(baseline, files[1])).href);
const { createHostPlanTodoModuleHandler } = await import(pathToFileURL(join(baseline, files[2])).href);
const input = { sessionId: 'session', turnId: 'turn', messages: [], execution: { runId: 'run', operationId: 'operation' } };
const context = { permissionContext: () => ({}), toolRuntimeContext: () => ({}), executionContext: () => ({}) };
const dispatcher = createSidecarDefaultModuleDispatcher({
  config: { cwd: '/fixture', provider: 'fixture', model: 'fixture', permissionMode: 'default' }, input,
  checkpoint: {}, capabilityResultObserver: {},
  planTodoHandler: createHostPlanTodoModuleHandler({ sessionId: input.sessionId, turnId: input.turnId, resolve: () => undefined }),
  modules: {
    model: { execution: {} }, budget: {},
    capability: { runtimeContext: { bindTurn: () => context }, execution: { list: () => [] } },
    permission: { requestContext: { bindTurn: () => context } },
    // No context execution port is installed in X0. An empty object would
    // instead claim a port exists and produce a fixture-only JS TypeError.
    context: { requestIdentity: { bindTurn: () => ({ contextIdentity: () => ({}) }) } },
    lifecycle: {}, event: {},
  },
} as any);
let nextId = 0;
function call(module: string, payload: any) {
  return { kind: 'request', method: 'module_call', messageId: `call-${++nextId}`, requestId: 'callback-request', runId: 'run', operationId: 'operation', module, payload };
}
async function response(call: any, handlers = dispatcher.handlers) {
  const protocol = new SidecarTurnProtocol({ config: {}, input, checkpoint: {}, manifest: {}, moduleHandlers: handlers, uuid: () => call.messageId });
  const first = await protocol.moduleResponseFor(call);
  const replay = await protocol.moduleResponseFor(call);
  assert.equal(first.replayed, false);
  assert.equal(replay.replayed, true);
  assert.deepEqual(first.response, replay.response);
  return JSON.parse(JSON.stringify(first.response));
}
const routing = [];
for (const module of ['model', 'budget', 'capability', 'permission', 'context', 'turn', 'lifecycle', 'event', 'checkpoint', 'skills', 'knowledge', 'unknown', '\ud800']) {
  for (const operation of operations) {
    const request = call(module, { operation, preparationId: 'prepared', input: {} });
    const expected = await response(request);
    if (!expected.ok) assert.equal(expected.code, 'HOST_MODULE_FAILED');
    routing.push({ call: request, response: expected });
  }
}
for (const payload of [{}, { operation: 'unknown' }, { operation: 'unknown', preparationId: null }]) {
  const request = call('model', payload);
  const expected = await response(request);
  assert.equal(expected.ok, false);
  routing.push({ call: request, response: expected });
}
for (const module of ['budget', 'capability', 'permission', 'context', 'turn', 'lifecycle', 'event']) {
  for (const payload of [{ operation: 'unknown' }, { preparationId: 'prepared' }]) {
    const request = call(module, payload);
    const expected = await response(request);
    assert.equal(expected.ok, false);
    routing.push({ call: request, response: expected });
    console.log('MODULE_FIELD_BOUNDARY ' + JSON.stringify({ module, payload, response: expected }));
  }
}
const metadataWithoutPreparation = call('model', { operation: 'get_metadata' });
routing.push({ call: metadataWithoutPreparation, response: await response(metadataWithoutPreparation) });
const errors = [];
const errorCases = [
  {}, { code: 'MODEL_RATE_LIMIT', error: { code: 'rate_limit', message: 'retry', vendor: { status: 429 } }, retryable: true, retryAfterMs: 12.5 },
  { code: '', error: { code: '', message: '', extra: 1 }, retryable: false, retryAfterMs: -0.25 },
  { code: 7, error: [], retryable: 'yes', retryAfterMs: '3' },
  { code: null, error: null, retryable: null, retryAfterMs: null },
  { error: { code: 'bad' }, retryAfterMs: 0 },
  { error: { code: 1, message: 'invalid' }, retryAfterMs: 2 },
  { code: '\ud800', error: { code: 'ok', message: '\ud800' }, retryable: false },
];
for (const fields of errorCases) {
  const request = call('model', { operation: 'test_error' });
  const message = 'failure \ud800';
  const expected = await response(request, { model: async () => { throw Object.assign(new Error(message), fields); } });
  errors.push({ call: request, message, fields, response: expected });
}
for (const nonfinite of ['NaN', 'Infinity', '-Infinity']) {
  const request = call('model', { operation: 'test_error' });
  const expected = await response(request, { model: async () => { throw Object.assign(new Error('nonfinite'), { retryAfterMs: Number(nonfinite) }); } });
  errors.push({ call: request, message: 'nonfinite', fields: {}, nonfinite, response: expected });
}
const fields = [];
const values = [null, '', ' ', '\ud800', '3', false, true, 0, -0.25, 0.25, [], {}];
for (const helper of ['stringField', 'optionalStringField', 'booleanField', 'positiveFinite', 'optionalNonNegativeFinite']) {
  for (const input of [{ missing: true }, { absent: true }, { undefined: true }, ...values.map(value => ({ value })), ...['NaN', 'Infinity', '-Infinity'].map(nonfinite => ({ nonfinite }))]) {
    const value = input.nonfinite ? Number(input.nonfinite) : input.value;
    let expected;
    try {
      const record = input.missing ? undefined : input.absent ? {} : { value };
      const result = helper === 'positiveFinite' || helper === 'optionalNonNegativeFinite'
        ? helpers[helper](record?.value, 'value') : helpers[helper](record, 'value');
      expected = result === undefined ? { missing: true } : { value: result };
    } catch (error) { expected = { error: (error as Error).message }; }
    fields.push({ helper, input, expected });
    if ((helper === 'optionalStringField' || helper === 'optionalNonNegativeFinite')
      && (input.absent || input.undefined || input.value === null)) {
      console.log('OPTIONAL_FIELD_BOUNDARY ' + JSON.stringify({ helper, input, expected }));
    }
  }
}
await dispatcher.dispose();
assert.equal(routing.length, 13 * operations.length + 18);
assert.equal(errors.length, 11);
assert.equal(fields.length, 90);
const rejectedModules = new Set(['unknown', '\ud800']);
const stdioCounts = Object.fromEntries(Object.entries({ routing, errors }).map(([key, cases]) => [
  key, cases.filter(({ call }) => !rejectedModules.has(call.module)).length,
]));
assert.equal(stdioCounts.routing, 11 * operations.length + 18);
assert.equal(stdioCounts.errors, errors.length);
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), JSON.stringify({ commit, sources, stdioCounts, routing, errors, fields }, null, 2) + '\n');
console.log(`HOST_MODULE_ORACLE_PASS routing=${routing.length} errors=${errors.length} fields=${fields.length}`);
