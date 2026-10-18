import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Execute the real fixed-baseline dispatcher with deterministic, in-memory ports.
import assert from 'node:assert/strict';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = assertBaseline(baseline);
const source = 'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts';
const sources = await baselineSourceHashes(baseline, [source]);
const { createSidecarDefaultModuleDispatcher } = await import(pathToFileURL(join(baseline, source)).href);
const json = (value: any) => JSON.parse(JSON.stringify(value));
const basic = { fields: { name: 'read', description: 'Read fixture', kind: 'builtin', inputSchema: { type: 'object' }, private: 'catalog-only' } };
const toolCall = { toolCallId: 'tool-1', name: 'read', arguments: { path: 'fixture' } };
const specs: any[] = [];
function add(name: string, module: string, payload: any, config: any = {}) {
  specs.push({ name, module, payload, config });
}
add('list-refresh', 'capability', { operation: 'list_tools' }, { refreshedTools: [{ fields: { ...basic.fields, name: 'fresh' } }] });
add('list-empty', 'capability', { operation: 'list_tools' }, { tools: [] });
add('list-descriptor-shapes', 'capability', { operation: 'list_tools' }, { tools: [
  basic,
  { fields: { name: 'full', description: null, kind: 'mcp', inputSchema: { anyOf: [{ type: 'string' }, { type: 'array' }] }, title: 'omit', aliases: ['omit'] }, flags: { readOnly: { value: true }, concurrencySafe: { value: true }, requiresUserInteraction: { value: true } }, capabilities: ['files', 'network'] },
  { fields: { name: 'throwing', inputSchema: false }, flags: { readOnly: { throws: true }, concurrencySafe: { throws: true }, requiresUserInteraction: { throws: true } }, capabilities: [] },
  { fields: { name: '\ud800', description: '', kind: null }, flags: { readOnly: { value: null }, concurrencySafe: {}, requiresUserInteraction: { value: null } } },
  { fields: { name: 'undefined-interaction', inputSchema: [] }, flags: { readOnly: { value: 0 }, concurrencySafe: { value: 'truthy' }, requiresUserInteraction: {} } },
] });
for (const failure of ['refresh', 'list']) add(`list-fail-${failure}`, 'capability', { operation: 'list_tools' }, { failure });
add('batch-empty', 'capability', { operation: 'execute_batch', calls: [] });
add('batch-pair', 'capability', { operation: 'execute_batch', calls: [toolCall, { toolCallId: 'tool-2', name: 'unknown' }], context: { cwd: '/fixture', custom: 4 } });
for (const results of [[], [{ toolCallId: 'first' }, { toolCallId: 'extra' }]]) {
  add(`batch-length-${results.length}`, 'capability', { operation: 'execute_batch', calls: [toolCall] }, { results });
}
add('batch-empty-extra', 'capability', { operation: 'execute_batch', calls: [] }, { results: [{}] });
add('execute-ok', 'capability', { operation: 'execute', ...toolCall, context: { custom: 4 } });
add('execute-first-only', 'capability', { operation: 'execute', ...toolCall }, { results: [{ first: 1 }, { second: 2 }] });
add('execute-no-result', 'capability', { operation: 'execute', ...toolCall }, { results: [] });
add('execute-empty-object-result', 'capability', { operation: 'execute', ...toolCall }, { results: [{}] });
add('execute-unknown-tool', 'capability', { operation: 'execute', toolCallId: 'unknown', name: 'unknown' });
for (const [label, args] of [['missing', undefined], ['null', null], ['false', false], ['zero', 0], ['empty-string', ''], ['array', []], ['nested', { nested: [1, null, '中'] }]]) {
  add(`execute-arguments-${label}`, 'capability', { operation: 'execute', toolCallId: 'id', name: 'read', arguments: args });
  add(`batch-arguments-${label}`, 'capability', { operation: 'execute_batch', calls: [{ toolCallId: 'id', name: 'read', arguments: args }] });
}
for (const operation of ['execute', 'execute_batch']) {
  for (const field of ['toolCallId', 'name']) {
    for (const [label, value] of [['missing', undefined], ['null', null], ['empty', ''], ['number', 7], ['array', []], ['space', ' '], ['surrogate', '\ud800']]) {
      const call = { ...toolCall, [field]: value };
      add(`${operation}-${field}-${label}`, 'capability', operation === 'execute' ? { operation, ...call } : { operation, calls: [call] });
    }
  }
  for (const failure of ['context', 'execute', 'observer']) add(`${operation}-fail-${failure}`, 'capability', operation === 'execute' ? { operation, ...toolCall } : { operation, calls: [toolCall] }, { failure });
}
for (const [label, calls] of [['missing', undefined], ['null', null], ['object', {}], ['string', 'bad']]) add(`batch-calls-${label}`, 'capability', { operation: 'execute_batch', calls });
for (const entry of [null, [], 'bad', 2]) add(`batch-entry-${JSON.stringify(entry)}`, 'capability', { operation: 'execute_batch', calls: [entry] });
for (const operation of [undefined, null, '', 7, 'unknown', ' ', '\ud800']) {
  add(`capability-operation-${String(operation)}`, 'capability', { operation });
  add(`permission-operation-${String(operation)}`, 'permission', { operation });
}
add('unknown-operation-context-first', 'capability', { operation: 'unknown' }, { failure: 'context' });
add('plan-forward', 'capability', { operation: 'plan_todo', action: 'custom', data: { arbitrary: true }, context: null });
add('plan-failure', 'capability', { operation: 'plan_todo', action: 'list' }, { failure: 'plan' });
const permission = { operation: 'decide', tool: { name: 'read', private: 'untrusted', description: 'spoof' }, toolCallId: 'permission-1', input: { path: 'fixture' }, context: { custom: true } };
add('permission-ok', 'permission', permission);
add('permission-first-match', 'permission', permission, { tools: [basic, { fields: { ...basic.fields, private: 'second' } }] });
add('permission-unknown', 'permission', { ...permission, tool: { name: 'unknown' }, toolCallId: undefined });
add('permission-unknown-surrogate', 'permission', { ...permission, tool: { name: '\ud800' } });
for (const [label, value] of [['missing', undefined], ['null', null], ['empty', ''], ['number', 8], ['array', []], ['space', ' '], ['surrogate', '\ud800']]) {
  add(`permission-toolCallId-${label}`, 'permission', { ...permission, toolCallId: value });
  add(`permission-name-${label}`, 'permission', { ...permission, tool: { name: value } });
}
for (const [label, value] of [['missing', undefined], ['null', null], ['false', false], ['zero', 0], ['string', ''], ['array', []]]) {
  add(`permission-input-${label}`, 'permission', { ...permission, input: value, context: value });
}
for (const tool of [null, [], 'read', undefined]) add(`permission-tool-${JSON.stringify(tool)}`, 'permission', { ...permission, tool });
for (const failure of ['catalog', 'permission-context', 'decide']) add(`permission-fail-${failure}`, 'permission', permission, { failure });
add('permission-context-before-id', 'permission', { ...permission, toolCallId: undefined }, { failure: 'permission-context' });
const nonObjectPayloads = [['array', []], ['string', 'x'], ['number', 7], ['null', null]] as const;
for (const module of ['capability', 'permission']) {
  for (const [label, payload] of nonObjectPayloads) add(`${module}-payload-${label}`, module, payload);
}

const cases = [];
for (const spec of specs) {
  const config = spec.config;
  const trace: any[] = [];
  let recording = false;
  function record(event: string, fields: any = {}) { if (recording) trace.push(json({ event, ...fields })); }
  function fail(point: string) { if (recording && config.failure === point) throw new Error(`fixture ${point} failed`); }
  function defineTool(def: any) {
    function predicate(key: string) {
      return (input: any) => {
        record('predicate', { name: def.fields.name, predicate: key, input });
        assert.deepEqual(input, {});
        if (def.flags?.[key]?.throws) throw new Error('predicate failed');
        return def.flags?.[key] ? def.flags[key].value : false;
      };
    }
    return { ...def.fields, isReadOnly: predicate('readOnly'), isConcurrencySafe: predicate('concurrencySafe'),
      ...(def.flags?.requiresUserInteraction ? { requiresUserInteraction: predicate('requiresUserInteraction') } : {}),
      ...(def.capabilities ? { requiredRuntimeCapabilities: def.capabilities } : {}),
    };
  }
  let tools = (config.tools ?? [basic]).map(defineTool);
  const execution = {
    list: () => { record('list'); fail('list'); return tools; },
    refresh: async () => { record('refresh'); await Promise.resolve(); fail('refresh'); if (config.refreshedTools) tools = config.refreshedTools.map(defineTool); },
    executeAll: async (calls: any[], context: any, execution: any) => {
      record('execute', { calls, context, execution }); await Promise.resolve(); fail('execute');
      return config.results ?? calls.map(call => ({ toolCallId: call.id, name: call.name, result: call.input, isError: call.name === 'unknown' }));
    },
  };
  const runtime = (value: any, planTodo: any, includeOneShotSubagent: boolean) => {
    record('context', { value, planTodo, includeOneShotSubagent }); fail('context');
    return { value, planTodo, includeOneShotSubagent };
  };
  const dispatcher = createSidecarDefaultModuleDispatcher({
    config: { cwd: '/fixture', provider: 'fixture', model: 'fixture', permissionMode: 'default' },
    input: { sessionId: 'session', turnId: 'turn', messages: [] }, checkpoint: {},
    modules: {
      model: { execution: {} },
      capability: { execution, runtimeContext: { bindTurn: () => ({
        permissionContext: () => ({}), toolRuntimeContext: runtime,
        executionContext: (call: any) => { record('execution-context', { call }); return { requestId: call.requestId, runId: call.runId, operationId: call.operationId }; },
      }) } },
      planTodo: { forSession: (sessionId: string) => { record('plan-session', { sessionId }); return { sessionId }; } },
      permission: {
        catalog: { list: () => { record('catalog'); fail('catalog'); return tools; } },
        requestContext: { bindTurn: () => ({ toolRuntimeContext: (value: any) => { record('permission-context', { value }); fail('permission-context'); return { value }; } }) },
        decision: { decide: async (tool: any, input: any, context: any, toolCallId: string) => {
          record('decide', { tool: json(tool), input, context, toolCallId }); await Promise.resolve(); fail('decide');
          return { behavior: 'allow', fixture: tool.private, input };
        } },
      },
    },
    capabilityResultObserver: { onCapabilityResults: async (results: any) => { record('observer', { results }); await Promise.resolve(); fail('observer'); } },
    planTodoHandler: async (call: any) => { record('plan', { call }); await Promise.resolve(); fail('plan'); return { forwarded: call.payload }; },
  } as any);
  // Exclude constructor manifest enumeration; this oracle exercises handler calls.
  recording = true;
  const call = json({ kind: 'request', method: 'module_call', messageId: `cap-${cases.length + 1}`, requestId: 'callback', runId: 'run', operationId: 'operation', module: spec.module, payload: spec.payload });
  let response;
  try { response = json({ ok: true, payload: await dispatcher.handlers[spec.module](call) }); }
  catch (error) { response = { ok: false, error: { message: (error as Error).message } }; }
  await dispatcher.dispose();
  cases.push({ name: spec.name, config, call, response, trace });
}
assert.equal(new Set(cases.map(c => c.name)).size, cases.length);
const get = (name: string) => cases.find(c => c.name === name)!;
assert.deepEqual(get('batch-empty').trace.map(e => e.event), ['plan-session', 'context', 'execution-context', 'execute', 'observer']);
assert.deepEqual(get('list-refresh').trace.slice(0, 2).map(e => e.event), ['refresh', 'list']);
assert.equal(get('permission-unknown').response.error.message, 'Permission request references unavailable tool: unknown');
for (const name of ['batch-length-0', 'batch-length-2', 'batch-empty-extra', 'execute-no-result']) {
  assert.equal(get(name).response.ok, false);
  assert.ok(!get(name).trace.some(e => e.event === 'observer'));
}
assert.equal(get('permission-ok').trace.find(e => e.event === 'decide').tool.private, 'catalog-only');
assert.deepEqual(get('execute-first-only').trace.find(e => e.event === 'observer').results, [{ first: 1 }]);
for (const module of ['capability', 'permission']) {
  for (const [label] of nonObjectPayloads) {
    const test = get(`${module}-payload-${label}`);
    assert.deepEqual(test.response, { ok: false, error: { message: 'Sidecar payload field operation must be a non-empty string.' } });
    assert.deepEqual(test.trace, [], 'operation validation must precede all domain port calls');
  }
}
assert.equal(cases.length, 128);
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), JSON.stringify({ commit, sources, cases }, null, 2) + '\n');
console.log(`HOST_CAPABILITY_ORACLE_PASS cases=${cases.length}`);
