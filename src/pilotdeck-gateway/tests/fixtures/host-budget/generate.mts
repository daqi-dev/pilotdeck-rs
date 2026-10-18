// Real dispatcher oracle. All runtime files belong to a unique directory.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = (...args: string[]) => execFileSync('git', ['-C', baseline, ...args], { encoding: 'utf8' });
assert.equal(git('rev-parse', 'HEAD').trim(), sourceCommit);
function assertClean() { assert.equal(git('status', '--porcelain', '--untracked-files=no'), '', 'tracked baseline files must be clean'); }
assertClean();
const sources = [
  'agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
  'agent/loop/AgentTurnCapabilities.ts',
  'agent/modules/transport/agentLoopSidecarClient.ts',
  'context/budget/TokenAccountingRuntime.ts', 'context/budget/TokenBudgetManager.ts',
  'context/budget/tokenizer.ts', 'model/protocol/toolResultContent.ts',
  'router/stats/TokenStatsCollector.ts', 'router/utils/modelPricing.ts',
  'agent/loop/AgentLoop.ts',
];
const sourceSha256: Record<string, string> = {};
for (const source of sources) {
  const path = `src/${source}`;
  const committed = execFileSync('git', ['-C', baseline, 'show', `${sourceCommit}:${path}`]);
  const sha = (bytes: Buffer) => createHash('sha256').update(bytes).digest('hex');
  sourceSha256[path] = sha(committed);
  assert.equal(sha(await readFile(join(baseline, path))), sourceSha256[path], path);
}
const load = (path: string) => import(pathToFileURL(join(baseline, 'src', path)).href);
const { createSidecarDefaultModuleDispatcher } = await load(sources[0]);
// Same private-export technique as host-modules: compile in memory, preserving
// imports against their original paths; no baseline file is written.
const clientPath = join(baseline, 'src/agent/modules/transport/agentLoopSidecarClient.ts');
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
const client = transformSync((await readFile(clientPath, 'utf8')) + '\nexport { SidecarTurnProtocol };\n', { loader: 'ts', format: 'esm', target: 'es2022', sourcefile: clientPath }).code;
const executable = client.replace(/from (["'])(\.[^"']+)\1/g, (_all: string, quote: string, specifier: string) => {
  let dependency = resolve(dirname(clientPath), specifier);
  if (dependency.endsWith('.js') && existsSync(dependency.slice(0, -3) + '.ts')) dependency = dependency.slice(0, -3) + '.ts';
  return 'from ' + quote + pathToFileURL(dependency).href + quote;
});
const { SidecarTurnProtocol } = await import('data:text/javascript;base64,' + Buffer.from(executable).toString('base64'));
const { TokenAccountingRuntime } = await load('context/budget/TokenAccountingRuntime.ts');
const { TokenStatsCollector } = await load('router/stats/TokenStatsCollector.ts');
const output = await mkdtemp(join(process.argv[2] ? resolve(process.argv[2]) : tmpdir(), 'host-budget-'));
const accounting = new TokenAccountingRuntime({ modelConfig: { providers: {} }, fetch: async () => { throw new Error('oracle must never use network'); } });
const json = (value: any) => JSON.parse(JSON.stringify(value));
const special = (value: any) => value === 'undefined' ? undefined : ['NaN', 'Infinity', '-Infinity'].includes(value) ? Number(value) : value;
const request = { provider: 'openai', model: 'gpt-4o', messages: [] };
const names = ['estimate_request_input', 'evaluate_request_budget', 'estimate_usage_cost'];
const specs: any[] = [];
function add(name: string, payload: any, config: any = {}) {
  specs.push({ name, payload, capabilities: [true, true, true], ...config });
}
for (const operation of [undefined, null, '', 7, false, [], {}, ' ', '\ud800', 'unknown', ...names]) {
  for (const available of [false, true]) add(`operation-${specs.length}`, { operation }, { available, capabilities: [false, false, false] });
}
// All eight independent capability combinations, including disabled operations
// with invalid payloads: support is checked before operation-specific fields.
for (let mask = 0; mask < 8; mask++) {
  for (const operation of names) add(`capability-${mask}-${operation}`, { operation, request, options: { maxContextTokens: 100 }, provider: 'openai', model: 'gpt-4o' }, { capabilities: [0, 1, 2].map(bit => !!(mask & (1 << bit))) });
}
for (const operation of names.slice(0, 2)) {
  for (const candidate of [undefined, null, [], {}, { ...request, provider: 1 }, { ...request, model: null }, { ...request, messages: {} }, { ...request, provider: '', model: '' }, { ...request, messages: [null], extra: { preserved: true } }]) {
    add(`request-${specs.length}`, { operation, request: candidate, options: { maxContextTokens: 100 } });
  }
}
for (const options of [undefined, null, [], {}, { maxContextTokens: 0 }, { maxContextTokens: -1 }, { maxContextTokens: '1' }, { maxContextTokens: true }, { maxContextTokens: 1.5 }]) {
  add(`options-${specs.length}`, { operation: names[1], request, options });
}
for (const field of ['maxContextTokens', 'reservedOutputTokens', 'useProviderCount', 'calibration']) {
  for (const value of [null, -1, 0, 0.5, '', '1', true, false, [], {}, { provider: 'mismatch', extra: [1] }]) {
    add(`option-${field}-${specs.length}`, { operation: names[1], request, options: { maxContextTokens: 100, [field]: value, signal: 'forged', ignored: true } });
  }
}
for (const field of ['maxContextTokens', 'reservedOutputTokens']) {
  for (const value of ['NaN', 'Infinity', '-Infinity']) {
    add(`nonfinite-${field}-${value}`, { operation: names[1], request, options: { maxContextTokens: 100 } }, { nonfinite: { field, value } });
  }
}
for (const signal of ['absent', 'active', 'aborted']) {
  add(`trusted-signal-${signal}`, { operation: names[1], request, options: { maxContextTokens: 100, reservedOutputTokens: 0, useProviderCount: false, calibration: { x: 1 }, signal: 'forged' } }, { signal });
}
for (const field of ['provider', 'model']) {
  for (const value of [undefined, null, '', 3, false, [], {}, ' ', '\ud800']) {
    add(`cost-route-${field}-${specs.length}`, { operation: names[2], provider: 'openai', model: 'gpt-4o', [field]: value });
  }
}
for (const usage of [undefined, null, [], 1, false, {}, { nativeCost: 0 }, { extra: { untouched: true } }]) {
  add(`usage-${specs.length}`, { operation: names[2], provider: 'openai', model: 'gpt-4o', usage });
}
for (const cost of ['undefined', 'NaN', 'Infinity', '-Infinity', -1, 0, 0.25]) {
  add(`cost-return-${cost}`, { operation: names[2], provider: 'openai', model: 'gpt-4o' }, { cost });
}
for (const tokens of [-1, 0.5, 'NaN']) add(`tokens-return-${tokens}`, { operation: names[0], request }, { tokens });
for (const snapshot of [null, {}, { state: 'custom', extra: true }]) add(`snapshot-return-${specs.length}`, { operation: names[1], request, options: { maxContextTokens: 100 } }, { snapshot });
for (const operation of names) add(`port-error-${operation}`, { operation, request, options: { maxContextTokens: 100 }, provider: 'openai', model: 'gpt-4o' }, { error: 'budget port rejected' });
// Reuse the existing corpus's inputs, recomputing every output via the real
// dispatcher + real local port (never use model-cost's recorded numbers).
const modelCost = JSON.parse(await readFile(new URL('../model-cost/oracle.json', import.meta.url), 'utf8'));
for (const row of modelCost.requests) add(`local-request-${specs.length}`, { operation: names[0], request: row.request }, { local: true });
for (const usage of [undefined, null, [], {}, { inputTokens: 1e6, outputTokens: 2e6, cacheReadTokens: 3e6, cacheWriteTokens: 4e6 }, { nativeCost: 0.125 }, { inputTokens: -1 }, { inputTokens: '2', outputTokens: true }, { inputTokens: {} }]) {
  for (const pricing of [undefined, { 'openai/gpt-4o': { input: 2, output: 5, cacheRead: 1 } }]) {
    add(`local-cost-${specs.length}`, { operation: names[2], provider: 'openai', model: 'gpt-4o', usage }, { local: true, pricing });
  }
}
for (const [provider, model] of [['\ud800', 'x'], ['p', '\ud800x'], ['p', 'deepseek\ud800flash']]) {
  add(`local-surrogate-${specs.length}`, { operation: names[2], provider, model, usage: { inputTokens: 1e6 } }, { local: true, pricing: { 'x': { input: 2 }, '�/x': { input: 99 }, '�': { input: 88 } } });
}
for (const type of ['text', 'thinking', 'tool_result_reference']) {
  for (const value of [undefined, null]) {
    const block = { type, [type === 'tool_result_reference' ? 'preview' : 'text']: value };
    add(`local-missing-text-${specs.length}`, { operation: names[0], request: { ...request, messages: [{ role: 'user', content: [block] }] } }, { local: true });
  }
}
for (const schema of [undefined, null, [], true, 12, 'schema']) {
  add(`local-lax-schema-${specs.length}`, { operation: names[0], request: { ...request, tools: [{ name: 'tool', description: 'lax', inputSchema: schema }] } }, { local: true });
}
for (const mimeType of [undefined, 'custom/pdf']) {
  add(`local-pdf-mime-${specs.length}`, { operation: names[0], request: { ...request, messages: [{ role: 'user', content: [{ type: 'tool_result', toolCallId: 't', content: [{ type: 'pdf', mimeType, data: 'abc', pages: 1e-7 }] }] }] } }, { local: true });
}
const cases = [];
for (const spec of specs) {
  const calls: any[] = [];
  const controller = new AbortController();
  if (spec.signal === 'aborted') controller.abort();
  const stats = new TokenStatsCollector({ enabled: false, filePath: join(output, 'unused-stats.json'), modelPricing: spec.pricing });
  const fail = () => { if (spec.error) throw new Error(spec.error); };
  const budget: any = {};
  if (spec.capabilities[0]) budget.estimateRequestInput = async (value: any) => {
    calls.push({ operation: names[0], request: value }); fail();
    return spec.local ? accounting.estimateRequestInput(value) : special(spec.tokens ?? 17.5);
  };
  if (spec.capabilities[1]) budget.evaluateRequestBudget = async (value: any, options: any) => {
    const { signal, ...rest } = options;
    if (signal) assert.equal(signal, controller.signal, 'must pass the trusted signal by identity');
    calls.push({ operation: names[1], request: value, options: rest, signal: signal ? (signal.aborted ? 'aborted' : 'active') : 'absent' }); fail();
    return Object.hasOwn(spec, 'snapshot') ? spec.snapshot : { tokens: 17.5, state: 'warning', extra: { preserved: true } };
  };
  if (spec.capabilities[2]) budget.estimateUsageCost = async (usage: any, provider: string, model: string) => {
    calls.push({ operation: names[2], ...(usage === undefined ? {} : { usage }), provider, model }); fail();
    if (!spec.local) return special(spec.cost ?? 0.25);
    const estimate = stats.estimateCost(usage, provider, model);
    if (typeof estimate === 'number' && Number.isFinite(estimate) && estimate >= 0) return estimate;
    return typeof usage?.nativeCost === 'number' && Number.isFinite(usage.nativeCost) && usage.nativeCost >= 0 ? usage.nativeCost : undefined;
  };
  const dispatcher = createSidecarDefaultModuleDispatcher({
    config: { cwd: output, provider: 'fixture', model: 'fixture', permissionMode: 'default' },
    input: { sessionId: 'session', turnId: 'turn', messages: [], ...(spec.signal && spec.signal !== 'absent' ? { abortSignal: controller.signal } : {}) },
    checkpoint: {}, capabilityResultObserver: {}, planTodoHandler: async () => ({}),
    modules: { model: { execution: {} }, ...(spec.available === false ? {} : { budget }), capability: { execution: { list: () => [] }, runtimeContext: { bindTurn: () => ({ permissionContext: () => ({}) }) } } },
  } as any);
  const payload = json(spec.payload);
  if (spec.nonfinite) payload.options[spec.nonfinite.field] = special(spec.nonfinite.value);
  const call = { kind: 'request', method: 'module_call', module: 'budget', messageId: 'call', requestId: 'request', runId: 'run', operationId: 'operation', payload };
  const protocol = new SidecarTurnProtocol({ config: {}, input: { sessionId: 'session', turnId: 'turn', execution: { runId: 'run', operationId: 'operation' } }, checkpoint: {}, manifest: {}, moduleHandlers: dispatcher.handlers, uuid: () => 'response' });
  const wireResponse = (await protocol.moduleResponseFor(call)).response;
  const response = wireResponse.ok ? { ok: true, result: wireResponse.payload } : { ok: false, message: wireResponse.error.message };
  cases.push(json({ ...spec, call: { ...call, payload: spec.payload }, manifest: dispatcher.manifest.hostModules.budget ?? null, calls, response, wireResponse }));
  await dispatcher.dispose();
}
assertClean();
await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n');
console.log(JSON.stringify({ output, cases: cases.length }));
