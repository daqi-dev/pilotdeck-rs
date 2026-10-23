// Real TS constructors only. No Rust output is used to manufacture expectations.
// Per configuration combination this records (a) the manifest advertisement
// (dispatcher.manifest.hostModules), (b) the actually callable module handlers
// (Object.keys of the dispatcher registry bound for the turn), and (c) probe
// module_call responses proving per module whether a handler is registered and
// dispatchable. The default turn composition binds the per-turn plan/todo
// handler exactly like the client does (agentLoopSidecarClient.ts:341-354).
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import assert from 'node:assert/strict';
import { existsSync } from 'node:fs';
import { mkdtemp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = assertBaseline(baseline);
const transport = 'src/agent/modules/transport/';
const files = [
  transport + 'agentLoopSidecarClient.ts',
  transport + 'sidecarDefaultModuleDispatcher.ts',
  transport + 'sidecarTurnComposition.ts',
  transport + 'sidecarToolContext.ts',
  'src/agent/modules/capability/hostPlanTodoModuleHandler.ts',
  'src/agent/modules/checkpoint/hostToolCheckpoint.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, files);
// Appending exports changes visibility only. Compile the entire original file
// at its original filename so imports still resolve to real baseline modules.
const requireBaseline = createRequire(join(baseline, 'package.json'));
const requireTsx = createRequire(requireBaseline.resolve('tsx'));
const { transformSync } = requireTsx('esbuild');
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
const { createSidecarDefaultModuleDispatcher: dispatcher } = await import(pathToFileURL(join(baseline, files[1])).href);
const { createDefaultSidecarTurnComposition: defaultTurnComposition } = await import(pathToFileURL(join(baseline, files[2])).href);
const {
  createSidecarToolRuntimeServices: runtimeService,
  createSidecarPermissionRequestContextServices: permissionService,
  createSidecarContextRequestIdentityServices: identityService,
} = await import(pathToFileURL(join(baseline, files[3])).href);
const { HostToolCheckpoint } = await import(pathToFileURL(join(baseline, files[5])).href);
const json = (value: any) => JSON.parse(JSON.stringify(value));

// The fixed plan/todo state handle a resolving composition returns.
const planTodoHandle = {
  getSnapshot: () => ({ items: [] }),
  markPlanApproved: async () => {},
  recordTodoWrite: async () => {},
  writeTodos: async () => {},
  markToolProgress: async () => {},
};
function buildModules(spec: any) {
  const modules: any = {
    model: {
      execution: { prepare: async () => ({}) },
      ...(spec.model.materializeRequest ? { materializeRequest: async () => ({}) } : {}),
      ...(spec.model.metadata ? { metadata: {} } : {}),
    },
    ...(spec.budget ? {
      budget: {
        ...(spec.budget.estimateRequestInput ? { estimateRequestInput: async () => 0 } : {}),
        ...(spec.budget.evaluateRequestBudget ? { evaluateRequestBudget: async () => ({}) } : {}),
        ...(spec.budget.estimateUsageCost ? { estimateUsageCost: async () => 1 } : {}),
      },
    } : {}),
    capability: { execution: { list: () => [] }, runtimeContext: runtimeService({}) },
    ...(spec.permission ? {
      permission: { decision: {}, catalog: { list: () => [] }, requestContext: permissionService({}) },
    } : {}),
    // plan/todo advertisement switch (modules.planTodo) is independent of the
    // always-installed planTodoHandler: 'unresolved' advertises plan_todo while
    // the handler throws on every call, exactly like #570's scope note.
    ...(spec.planTodo === 'resolving' ? { planTodo: { forSession: () => planTodoHandle } } : {}),
    ...(spec.planTodo === 'unresolved' ? { planTodo: { forSession: () => undefined } } : {}),
    ...(spec.context === 'no-runtime' ? { context: { requestIdentity: identityService({}) } } : {}),
    ...(spec.context && spec.context !== 'no-runtime' ? {
      context: {
        execution: {
          ...(spec.context.applyToolResults ? { applyToolResults: async () => ({}) } : {}),
          ...(spec.context.recoverFromModelError ? { recoverFromModelError: async () => ({}) } : {}),
          ...(spec.context.captureTurn ? { captureTurn: async () => {} } : {}),
          ...(spec.context.tryAutoCompact ? { tryAutoCompact: async () => ({}) } : {}),
        },
        requestIdentity: identityService({}),
      },
    } : {}),
    ...(spec.lifecycle ? { lifecycle: {} } : {}),
    ...(spec.event ? { event: { emit: () => {} } } : {}),
  };
  return modules;
}
function buildInput(spec: any) {
  const input: any = { sessionId: 'session', turnId: 'turn', execution: { runId: 'run', operationId: 'operation' } };
  if (spec.turn.drainSteer) input.drainSteerMessages = async () => [];
  if (spec.turn.drainOrClose) input.drainOrCloseSteerMailbox = async () => ({ messages: [] });
  if (spec.turn.persistCompaction) input.onCompactPersisted = async () => {};
  return input;
}
const config = {
  cwd: '/fixture/project',
  permissionMode: 'default',
  permissionContext: { canPrompt: false, rules: { allow: [], deny: [], ask: [] } },
};

const cases: any[] = [];
const add = (name: string, patch: any = {}) => cases.push({
  name,
  modules: {
    model: { materializeRequest: false, metadata: false }, budget: null, planTodo: null,
    context: null, permission: false, lifecycle: false, event: false,
    ...patch.modules,
  },
  turn: { drainSteer: false, drainOrClose: false, persistCompaction: false, ...patch.turn },
});

add('minimal');
add('model-materialize-request', { modules: { model: { materializeRequest: true, metadata: false } } });
add('model-metadata', { modules: { model: { materializeRequest: false, metadata: true } } });
add('budget-no-methods', { modules: { budget: {} } });
add('budget-estimate-input', { modules: { budget: { estimateRequestInput: true } } });
add('budget-evaluate', { modules: { budget: { evaluateRequestBudget: true } } });
add('budget-usage-cost', { modules: { budget: { estimateUsageCost: true } } });
add('budget-two-methods', { modules: { budget: { estimateRequestInput: true, estimateUsageCost: true } } });
add('turn-drain', { turn: { drainSteer: true } });
add('turn-or-close', { turn: { drainOrClose: true } });
add('turn-persist', { turn: { persistCompaction: true } });
add('turn-all', { turn: { drainSteer: true, drainOrClose: true, persistCompaction: true } });
add('plan-todo-unresolved', { modules: { planTodo: 'unresolved' } });
add('plan-todo-resolving', { modules: { planTodo: 'resolving' } });
// An installed context module without its execution port: the handler is
// registered (probe answers "does not support") but nothing is advertised.
add('context-no-runtime', { modules: { context: 'no-runtime' } });
add('context-bare', { modules: { context: {} } });
add('context-apply-tool-results', { modules: { context: { applyToolResults: true } } });
add('context-recover', { modules: { context: { recoverFromModelError: true } } });
add('context-capture-turn', { modules: { context: { captureTurn: true } } });
add('context-auto-compact', { modules: { context: { tryAutoCompact: true } } });
add('context-all-optional', { modules: { context: { applyToolResults: true, recoverFromModelError: true, captureTurn: true, tryAutoCompact: true } } });
add('permission-module', { modules: { permission: true } });
add('lifecycle-module', { modules: { lifecycle: true } });
add('event-module', { modules: { event: true } });
add('mixed-budget-event-turn', {
  modules: { budget: { estimateRequestInput: true }, event: true },
  turn: { drainSteer: true },
});
add('mixed-permission-lifecycle-context', {
  modules: { permission: true, lifecycle: true, context: {} },
});
add('full-composition', {
  modules: {
    model: { materializeRequest: true, metadata: true },
    budget: { estimateRequestInput: true, evaluateRequestBudget: true, estimateUsageCost: true },
    planTodo: 'resolving',
    context: { applyToolResults: true, recoverFromModelError: true, captureTurn: true, tryAutoCompact: true },
    permission: true, lifecycle: true, event: true,
  },
  turn: { drainSteer: true, drainOrClose: true, persistCompaction: true },
});
assert.equal(new Set(cases.map(c => c.name)).size, cases.length);
assert.equal(cases.length, 27);

const ROUTED = ['model', 'budget', 'turn', 'capability', 'permission', 'context', 'lifecycle', 'event'];
const recorded: any[] = [];
for (const spec of cases) {
  const modules = buildModules(spec.modules);
  const input = buildInput(spec);
  const checkpoint = new HostToolCheckpoint(undefined, []);
  // The client binds the default turn composition per turn, then hands its
  // observer and plan/todo handler to the dispatcher (client:341-354).
  const composition = defaultTurnComposition({ config, modules });
  const turnComposition = composition.forTurn(input);
  const composed = dispatcher({
    config, modules, input, checkpoint,
    capabilityResultObserver: turnComposition.capabilityResultObserver,
    planTodoHandler: turnComposition.planTodoHandler,
  });
  const hostModules = json(composed.manifest.hostModules);
  const handlers = Object.keys(composed.handlers);
  // TS-side invariant pr2b relies on: every advertised module has a
  // registered handler, and registration never exceeds the routed set.
  for (const advertised of Object.keys(hostModules)) {
    assert.ok(handlers.includes(advertised), `${spec.name}: advertised ${advertised} without a handler`);
  }
  assert.ok(handlers.every(name => ROUTED.includes(name)), `${spec.name}: unknown handler registered`);
  let nextId = 0;
  const call = (module: string, payload: any) => ({
    kind: 'request', method: 'module_call', messageId: `call-${++nextId}`,
    requestId: 'callback-request', runId: 'run', operationId: 'operation', module, payload,
  });
  const response = async (request: any) => {
    const protocol = new SidecarTurnProtocol({
      config: {}, input, checkpoint: {}, manifest: {},
      moduleHandlers: composed.handlers, uuid: () => request.messageId,
    });
    const first = await protocol.moduleResponseFor(request);
    const replay = await protocol.moduleResponseFor(request);
    assert.equal(first.replayed, false);
    assert.equal(replay.replayed, true);
    assert.deepEqual(first.response, replay.response);
    return json(first.response);
  };
  const probes: any[] = [];
  // An unknown operation reaches every registered handler and reports the
  // unregistered-module error for every module without one.
  for (const module of ROUTED) {
    const request = call(module, { operation: 'pdrs_probe', preparationId: 'prepared', input: {} });
    probes.push({ probe: 'unknown-operation', call: request, response: await response(request) });
  }
  // plan/todo probes pin the two independent switches: the advertisement comes
  // from modules.planTodo, the handler is always installed per turn.
  const planProbes = [
    { probe: 'plan-todo-read', payload: { operation: 'plan_todo', sessionId: 'session', turnId: 'turn', method: 'read' } },
    { probe: 'plan-todo-missing-session', payload: { operation: 'plan_todo', method: 'read' } },
    { probe: 'plan-todo-identity', payload: { operation: 'plan_todo', sessionId: 'other', turnId: 'turn', method: 'read' } },
    { probe: 'plan-todo-unknown-method', payload: { operation: 'plan_todo', sessionId: 'session', turnId: 'turn', method: 'unknown' } },
  ];
  for (const { probe, payload } of planProbes) {
    const request = call('capability', payload);
    probes.push({ probe, call: request, response: await response(request) });
  }
  await composed.dispose();
  recorded.push({
    name: spec.name,
    modules: spec.modules,
    turn: spec.turn,
    hostModules,
    handlers,
    probes,
  });
}
assert.equal(recorded.length, 27);
for (const case_ of recorded) assert.equal(case_.probes.length, ROUTED.length + 4);
const owned = await mkdtemp(join(tmpdir(), 'pdrs-host-module-binding-'));
await writeFile(join(owned, 'oracle.json'), JSON.stringify({ commit, sourceSha256, cases: recorded }, null, 2) + '\n');
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), await readFile(join(owned, 'oracle.json')));
await rm(owned, { recursive: true, force: true });
console.log(`HOST_MODULE_BINDING_ORACLE_PASS cases=${recorded.length} probes=${recorded.reduce((sum, c) => sum + c.probes.length, 0)}`);
