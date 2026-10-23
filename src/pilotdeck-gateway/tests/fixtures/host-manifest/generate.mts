// Real TS constructors only. No Rust output is used to manufacture expectations.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, writeFile, mkdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const sources = [
  'src/agent/modules/checkpoint/hostToolCheckpoint.ts',
  'src/agent/modules/transport/sidecarToolContext.ts',
  'src/agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
];
function clean() {
  assert.equal(execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(), commit);
  assert.equal(execFileSync('git', ['-C', baseline, 'status', '--porcelain', '--untracked-files=no'], { encoding: 'utf8' }), '', 'baseline must be clean');
  assert.equal(execFileSync('git', ['-C', baseline, 'status', '--porcelain', '--untracked-files=all', '--', 'src'], { encoding: 'utf8' }), '', 'baseline source must have no untracked overrides');
}
async function hashes() {
  return Object.fromEntries(await Promise.all(sources.map(async source => [source,
    createHash('sha256').update(await readFile(join(baseline, source))).digest('hex')])));
}
clean();
const sourceSha256 = await hashes();
const load = (source: string) => import(pathToFileURL(join(baseline, source)).href);
const { HostToolCheckpoint } = await load(sources[0]);
const { createSidecarToolRuntimeServices: runtimeService,
  createSidecarPermissionRequestContextServices: permissionService,
  createSidecarContextRequestIdentityServices: identityService } = await load(sources[1]);
const { createSidecarDefaultModuleDispatcher: dispatcher } = await load(sources[2]);
const json = (value: any) => JSON.parse(JSON.stringify(value));
const capture = (f: () => any) => {
  try { return json({ ok: true, value: f() }); }
  catch (error) { return { ok: false, error: (error as Error).message }; }
};
function decode(value: any): any {
  if (Array.isArray(value)) return value.map(decode);
  if (value && typeof value === 'object') {
    if (value.$undefined) return undefined;
    return Object.fromEntries(Object.entries(value).map(([key, v]) => [key, decode(v)]));
  }
  return value;
}

// A tool spec records the exact own fields and predicate setups both the TS
// construction here and the Rust replay read; 'absent'/'throw' both serialize
// as false through TS safely(), 'undefined'/'null' pass the raw value through.
type ToolSpec = {
  name?: string; description?: string; kind?: string; inputSchema?: any;
  readOnly?: string; concurrencySafe?: string; requiresUserInteraction?: string;
  requiredRuntimeCapabilities?: string[];
};
const buildTool = (spec: ToolSpec) => {
  const tool: any = { execute: async () => ({}) };
  for (const key of ['name', 'description', 'kind', 'inputSchema'] as const) {
    if (key in spec) tool[key] = decode((spec as any)[key]);
  }
  const predicate = (setup?: string) => {
    if (setup === undefined || setup === 'absent') return undefined;
    if (setup === 'throw') return () => { throw new Error(`predicate ${setup} failed`); };
    if (setup === 'undefined') return () => undefined;
    if (setup === 'null') return () => null;
    return () => setup === 'true';
  };
  const readOnly = predicate(spec.readOnly);
  if (readOnly !== undefined) tool.isReadOnly = readOnly;
  const concurrencySafe = predicate(spec.concurrencySafe);
  if (concurrencySafe !== undefined) tool.isConcurrencySafe = concurrencySafe;
  const interaction = predicate(spec.requiresUserInteraction);
  if (interaction !== undefined) tool.requiresUserInteraction = interaction;
  if (spec.requiredRuntimeCapabilities !== undefined) tool.requiredRuntimeCapabilities = spec.requiredRuntimeCapabilities;
  return tool;
};
const buildModules = (spec: any, tools: any[]) => {
  const modules: any = {
    model: {
      execution: { prepare: async () => ({}) },
      ...(spec.model.materializeRequest ? { materializeRequest: async () => ({}) } : {}),
      ...(spec.model.metadata ? { metadata: {} } : {}),
    },
    ...(spec.budget ? { budget: {
      ...(spec.budget.estimateRequestInput ? { estimateRequestInput: async () => 0 } : {}),
      ...(spec.budget.evaluateRequestBudget ? { evaluateRequestBudget: async () => ({}) } : {}),
      ...(spec.budget.estimateUsageCost ? { estimateUsageCost: async () => 1 } : {}),
    } } : {}),
    capability: { execution: { list: () => tools }, runtimeContext: runtimeService({}) },
    ...(spec.permission ? { permission: { decision: {}, catalog: { list: () => tools }, requestContext: permissionService({}) } } : {}),
    ...(spec.planTodo ? { planTodo: { forSession: () => undefined } } : {}),
    ...(spec.context ? { context: {
      execution: {
        ...(spec.context.applyToolResults ? { applyToolResults: async () => ({}) } : {}),
        ...(spec.context.recoverFromModelError ? { recoverFromModelError: async () => ({}) } : {}),
        ...(spec.context.captureTurn ? { captureTurn: async () => {} } : {}),
        ...(spec.context.tryAutoCompact ? { tryAutoCompact: async () => ({}) } : {}),
      },
      requestIdentity: identityService({}),
    } } : {}),
    ...(spec.lifecycle ? { lifecycle: {} } : {}),
    ...(spec.event ? { event: { emit: () => {} } } : {}),
    ...(spec.interaction === null || spec.interaction === undefined
      ? {}
      : { interaction: { elicitationAvailable: spec.interaction === true } }),
  };
  return modules;
};
const buildInput = (spec: any) => {
  const input: any = { sessionId: 'session', turnId: 'turn' };
  if ('canPrompt' in spec) input.canPrompt = spec.canPrompt;
  if (spec.drainSteer) input.drainSteerMessages = async () => [];
  if (spec.drainOrClose) input.drainOrCloseSteerMailbox = async () => ({ messages: [] });
  if (spec.persistCompaction) input.onCompactPersisted = async () => {};
  return input;
};

const emptyRules = { allow: [], deny: [], ask: [] };
const readTool: ToolSpec = {
  name: 'Read', description: 'Reads files', kind: 'read',
  inputSchema: { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] },
  readOnly: 'true', concurrencySafe: 'true', requiresUserInteraction: 'false',
  requiredRuntimeCapabilities: [],
};
const grepTool: ToolSpec = {
  name: 'Grep', description: 'Searches files', kind: 'search', inputSchema: { type: 'object' },
  readOnly: 'true', concurrencySafe: 'false', requiresUserInteraction: 'false',
  requiredRuntimeCapabilities: ['fs.read', 'net.fetch', '\ud800'],
};
const fullConfig = {
  cwd: '/fixture/project', permissionMode: 'bypassPermissions',
  permissionContext: {
    mode: 'spoof-mode',
    rules: { allow: [{ source: 'user', behavior: 'allow', toolName: 'Read', pattern: '**' }], deny: [], ask: [] },
    cwd: '/spoof', canPrompt: true, additionalWorkingDirectories: ['/extra/one', '/extra/two'],
    bypassAvailable: true, policyCanPrompt: false, acceptEdits: false, planDirectoryPath: '/plans', extraKey: 'kept',
  },
};
const defaultConfig = { cwd: '/fixture/project', permissionMode: 'default', permissionContext: { canPrompt: false, rules: emptyRules } };
const cases: any[] = [];
const add = (name: string, patch: any = {}) => cases.push({
  name,
  tools: patch.tools ?? [],
  config: patch.config ?? defaultConfig,
  input: patch.input ?? {},
  modules: {
    model: { materializeRequest: false, metadata: false }, budget: null, planTodo: false, context: null,
    permission: false, lifecycle: false, event: false, interaction: null,
    ...(patch.modules ?? {}),
  },
});

add('minimal');
add('full-composition', {
  tools: [readTool, grepTool],
  config: fullConfig,
  input: { canPrompt: true, drainSteer: true, drainOrClose: true, persistCompaction: true },
  modules: {
    model: { materializeRequest: true, metadata: true },
    budget: { estimateRequestInput: true, evaluateRequestBudget: true, estimateUsageCost: true },
    planTodo: true,
    context: { applyToolResults: true, recoverFromModelError: true, captureTurn: true, tryAutoCompact: true },
    permission: true, lifecycle: true, event: true, interaction: true,
  },
});
// model capability methods
add('model-materialize-request', { modules: { model: { materializeRequest: true, metadata: false } } });
add('model-metadata', { modules: { model: { materializeRequest: false, metadata: true } } });
// budget presence and methods
add('budget-no-methods', { modules: { budget: {} } });
add('budget-estimate-input', { modules: { budget: { estimateRequestInput: true } } });
add('budget-evaluate', { modules: { budget: { evaluateRequestBudget: true } } });
add('budget-usage-cost', { modules: { budget: { estimateUsageCost: true } } });
add('budget-two-methods', { modules: { budget: { estimateRequestInput: true, estimateUsageCost: true } } });
// turn callbacks gate the whole module entry
add('turn-drain', { input: { drainSteer: true } });
add('turn-or-close', { input: { drainOrClose: true } });
add('turn-persist', { input: { persistCompaction: true } });
add('turn-drain-and-persist', { input: { drainSteer: true, persistCompaction: true } });
add('turn-all', { input: { drainSteer: true, drainOrClose: true, persistCompaction: true } });
// plan/todo advertising on the capability module
add('plan-todo', { modules: { planTodo: true } });
// context methods
add('context-bare', { modules: { context: {} } });
add('context-apply-tool-results', { modules: { context: { applyToolResults: true } } });
add('context-recover', { modules: { context: { recoverFromModelError: true } } });
add('context-capture-turn', { modules: { context: { captureTurn: true } } });
add('context-auto-compact', { modules: { context: { tryAutoCompact: true } } });
add('context-all-optional', { modules: { context: { applyToolResults: true, recoverFromModelError: true, captureTurn: true, tryAutoCompact: true } } });
// single fixed-method modules
add('permission-module', { modules: { permission: true } });
add('lifecycle-module', { modules: { lifecycle: true } });
add('event-module', { modules: { event: true } });
// interaction: false must not advertise elicitation
add('interaction-true', { modules: { interaction: true } });
add('interaction-false', { modules: { interaction: false } });
// permission context shape and overrides
add('permission-context-empty', { config: { cwd: '/w', permissionMode: 'default', permissionContext: {} } });
add('permission-context-spoof-overrides', { config: { cwd: '/real/cwd', permissionMode: 'plan', permissionContext: { mode: 'spoof', cwd: '/spoof', canPrompt: true, rules: 'spoof-rules' } } });
add('permission-context-order', { config: { cwd: '/w', permissionMode: 'default', permissionContext: { rules: emptyRules, canPrompt: false, cwd: '/spoof', mode: 'spoof' } } });
add('permission-context-numeric-keys', { config: { cwd: '/w', permissionMode: 'default', permissionContext: { b: 1, 10: 'numeric', a: 2, '01': 'leading-zero', canPrompt: true } } });
add('permission-context-missing-canprompt', { config: { cwd: '/w', permissionMode: 'default', permissionContext: { rules: emptyRules } } });
add('permission-context-missing-rules', { config: { cwd: '/w', permissionMode: 'default', permissionContext: { canPrompt: true } } });
add('input-canprompt-true', { config: { cwd: '/w', permissionMode: 'default', permissionContext: { canPrompt: false, rules: emptyRules } }, input: { canPrompt: true } });
add('input-canprompt-false', { config: { cwd: '/w', permissionMode: 'default', permissionContext: { canPrompt: true, rules: emptyRules } }, input: { canPrompt: false } });
add('input-canprompt-zero', { config: { cwd: '/w', permissionMode: 'default', permissionContext: { canPrompt: true, rules: emptyRules } }, input: { canPrompt: 0 } });
add('input-canprompt-null', { config: { cwd: '/w', permissionMode: 'default', permissionContext: { canPrompt: true, rules: emptyRules } }, input: { canPrompt: null } });
add('permission-mode-plan', { config: { cwd: '/w', permissionMode: 'plan', permissionContext: { canPrompt: false, rules: emptyRules } } });
add('permission-context-policy-fields', { config: { cwd: '/w', permissionMode: 'default', permissionContext: { canPrompt: false, rules: emptyRules, additionalWorkingDirectories: ['/a', '/b'], bypassAvailable: true, policyCanPrompt: false, acceptEdits: true, planDirectoryPath: '/plans' } } });
// tool descriptor boundaries
add('tools-one-full', { tools: [readTool] });
add('tools-required-capabilities', { tools: [grepTool] });
add('tools-missing-fields', { tools: [{ name: 'Bare' }] });
add('tools-predicates-throw', { tools: [{ name: 'Throws', description: 'd', kind: 'k', inputSchema: {}, readOnly: 'throw', concurrencySafe: 'throw', requiresUserInteraction: 'throw' }] });
add('tools-predicates-undefined', { tools: [{ name: 'Undef', description: 'd', kind: 'k', inputSchema: {}, readOnly: 'undefined', concurrencySafe: 'undefined', requiresUserInteraction: 'undefined' }] });
add('tools-predicate-null', { tools: [{ name: 'Null', description: 'd', kind: 'k', inputSchema: {}, readOnly: 'null', concurrencySafe: 'true' }] });
add('tools-requires-interaction', { tools: [{ name: 'Ask', description: 'd', kind: 'k', inputSchema: {}, requiresUserInteraction: 'true' }] });
add('tools-extra-fields', { tools: [{ name: 'Extra', description: 'd', kind: 'k', inputSchema: {}, title: 'Extra tool', aliases: ['X'], shouldDefer: true }] });
add('tools-unicode', { tools: [{ name: '\ud800-tool', description: '中文描述 😀', kind: 'read', inputSchema: { type: 'object', 'key\ud800': 'value\ud800', hidden: { $undefined: true } } }] });
add('tools-many', { tools: [readTool, grepTool, { name: 'Bare' }] });

assert.equal(new Set(cases.map(c => c.name)).size, cases.length);
assert.equal(cases.length, 48);
const recorded: any[] = [];
for (const spec of cases) {
  const tools = spec.tools.map(buildTool);
  const modules = buildModules(spec.modules, tools);
  const input = buildInput(spec.input);
  const checkpoint = new HostToolCheckpoint(undefined, []);
  const composed = dispatcher({
    config: spec.config,
    modules,
    input,
    checkpoint,
    capabilityResultObserver: { onCapabilityResults: async () => {} },
    planTodoHandler: async () => ({}),
  });
  const expected = capture(() => json(composed.manifest));
  await composed.dispose();
  assert.equal(expected.ok, true, `${spec.name}: manifest construction failed`);
  recorded.push({ ...spec, expected: expected.value });
}
clean();
assert.deepEqual(await hashes(), sourceSha256, 'source changed while recording');
const owned = await mkdtemp(join(tmpdir(), 'pdrs-host-manifest-'));
await writeFile(join(owned, 'oracle.json'), JSON.stringify({ commit, sourceSha256, cases: recorded }, null, 2) + '\n');
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), await readFile(join(owned, 'oracle.json')));
await rm(owned, { recursive: true, force: true });
console.log(`HOST_MANIFEST_ORACLE_PASS cases=${cases.length}`);
