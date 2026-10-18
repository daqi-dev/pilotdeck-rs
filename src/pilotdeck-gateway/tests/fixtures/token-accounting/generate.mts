// Run only the fixed checkout; all writes/listeners belong to this invocation.
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'context/budget/TokenAccountingRuntime.ts', 'context/budget/TokenBudgetManager.ts',
  'context/budget/effectiveContext.ts', 'context/budget/tokenizer.ts',
  'model/providers/anthropic/request.ts', 'model/providers/openai-responses/request.ts',
  'model/providers/openai/schema.ts', 'model/providers/toolResultReferenceText.ts',
  'model/protocol/clone.ts', 'model/protocol/toolResultContent.ts', 'model/protocol/errors.ts',
  'model/thinking/registry.ts', 'model/thinking/settings.ts', 'model/streaming/streamModel.ts',
  'model/normalizeProviderBaseUrl.ts', 'agent/modules/transport/sidecarDefaultModuleDispatcher.ts',
];
// Source path is independently checked before executing any baseline imports.
const sourceSha256 = await baselineSourceHashes(baseline, sources.map(p => `src/${p}`));
const load = (p: string) => import(pathToFileURL(join(baseline, 'src', p)).href);
const { TokenAccountingRuntime } = await load(sources[0]);
const { TokenBudgetManager } = await load(sources[1]);
const { createSidecarDefaultModuleDispatcher } = await load(sources.at(-1)!);
const captureTransport = process.argv.includes('--capture-transport');
const output = await mkdtemp(join(process.argv[2] ? resolve(process.argv[2]) : tmpdir(), 'token-accounting-'));
const json = (v: any) => JSON.parse(JSON.stringify(v));
const baseRequest = { provider: 'p', model: 'm', messages: [{ role: 'user', content: [{ type: 'text', text: 'Hello 世界' }] }] };
const model = { id: 'claude-sonnet-4-6', capabilities: { maxOutputTokens: 4096, contextWindow: 200000 } };
const provider = { id: 'p', protocol: 'anthropic', url: '$MOCK', apiKey: ' fixture-key ', headers: {}, models: { m: model } };
let response: any = { input_tokens: 73 };
let status = 200;
let requests: any[] = [];
const server = createServer(async (req, res) => {
  let body = ''; for await (const chunk of req) body += chunk;
  requests.push({ method: req.method, path: req.url, headers: Object.fromEntries(Object.entries(req.headers).filter(([key]) => ['content-type', 'x-api-key', 'anthropic-version', 'authorization', 'x-fixture'].includes(key)).sort(([a], [b]) => a.localeCompare(b))), body: JSON.parse(body) });
  res.writeHead(status, { 'content-type': 'application/json' }); res.end(JSON.stringify(response));
});
if (!captureTransport) await new Promise<void>((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
const address = server.address();
assert.ok(captureTransport || (address && typeof address !== 'string'));
const mock = captureTransport ? 'http://127.0.0.1:1' : `http://127.0.0.1:${(address as any).port}`;
const materialize = (v: any): any => typeof v === 'string' ? v.replaceAll('$MOCK', mock) : Array.isArray(v) ? v.map(materialize) : v && typeof v === 'object' ? Object.fromEntries(Object.entries(v).map(([k, v]) => [k, materialize(v)])) : v;
const sequences: any[] = [];
function add(name: string, steps: any[], config: any = { providers: {} }, settings: any = {}) { sequences.push({ name, config, settings, steps }); }
const step = (request: any = baseRequest, options: any = {}, extra: any = {}) => ({ request, options: { maxContextTokens: 100, ...options }, ...extra });
add('local-reserve-thresholds', [0, 5, 90, 100, 1000, 0.75].map(reservedOutputTokens => step(baseRequest, { reservedOutputTokens, useProviderCount: false })));
const localInputs = JSON.parse(await readFile(new URL('../model-cost/oracle.json', import.meta.url), 'utf8')).requests;
add('local-composition', localInputs.map((r: any) => step(r.request, { useProviderCount: false })));
const hostInputs = JSON.parse(await readFile(new URL('../host-budget/oracle.json', import.meta.url), 'utf8')).cases;
add('local-shallow-boundary', hostInputs.filter((r: any) => r.local && r.payload.operation === 'estimate_request_input').map((r: any) => step(r.payload.request, { useProviderCount: false })));
const calibration = { provider: 'p', model: 'm', actualInputTokens: 20, estimatedInputTokens: 10 };
add('calibration', [undefined, {}, calibration, { ...calibration, provider: 'x' }, { ...calibration, model: 'x' }, ...[-1, 0, 0.25, 10, 10.5, 10000].flatMap(n => [{ ...calibration, actualInputTokens: n }, { ...calibration, estimatedInputTokens: n }]), { ...calibration, actualInputTokens: '20' }].map(calibration => step(baseRequest, { calibration })));
add('calibration-empty', [step({ ...baseRequest, messages: [] }, { calibration }), step({ ...baseRequest, messages: [] }, { calibration: { ...calibration, actualInputTokens: 1, estimatedInputTokens: 100 } })]);
const providerConfig = { providers: { p: provider } };
const allBlocks = [
  { type: 'text', text: 'text' }, { type: 'thinking', text: 'thought', reasoningContent: 'native', signature: 'opaque' },
  ...['image', 'audio'].flatMap(type => ['url', 'base64'].map(source => ({ type, source, data: source === 'url' ? 'https://example.test/media' : 'YQ==', mimeType: `${type}/fixture` }))),
  { type: 'pdf', data: 'YQ==', mimeType: 'application/pdf' },
  { type: 'tool_call', id: 'call', name: 'lookup', input: { n: 1 } },
  { type: 'tool_result', toolCallId: 'call', isError: true, content: [{ type: 'text', text: 'result' }, { type: 'image', source: 'url', data: 'https://example.test/img', mimeType: 'image/png' }, { type: 'pdf', data: 'abcd', mimeType: 'custom/pdf', pages: 2 }] },
  ...[false, true].map(hasMore => ({ type: 'tool_result_reference', toolCallId: 'call', preview: 'preview', hasMore, originalBytes: 900, path: '/fixture/result', readFilePath: '/fixture/read' })),
  { type: 'media_reference', preview: 'media' },
];
const tools = [{ name: 'lookup', description: 'find', inputSchema: { type: 'object', properties: { a: { type: ['array', 'string'] }, b: { enum: [1, 1.5] }, c: { const: false }, d: { type: null, items: null } } } }];
const rich = { ...baseRequest, messages: [{ role: 'assistant', content: allBlocks }], systemPrompt: 'System <mcp-instructions>mcp</mcp-instructions> <memory-context>memory</memory-context>', tools, toolChoice: { name: 'lookup' }, maxOutputTokens: 99, stream: true, metadata: { user_id: 'fixture' }, speed: 'fast', temperature: 0.5 };
add('anthropic-content', [step(baseRequest), step(rich), ...['auto', 'none', 'required'].map(toolChoice => step({ ...baseRequest, toolChoice })), ...[true, false].map(strict => step({ ...rich, outputSchema: { name: 'answer', schema: { type: 'object' }, strict } }))], providerConfig);
add('anthropic-cache-markers', [step({ ...rich, messages: [0, 1, 2, 3].map(n => ({ role: 'user', content: n === 3 ? [{ type: 'thinking', text: 'only thinking' }] : [{ type: 'text', text: `msg${n}` }, { type: 'thinking', text: 'last' }] })), cachePlan: { tools: true, system: true, messages: [0, 1, 2, 3] } }), step({ ...rich, cacheBreakpoints: [0] }), step({ ...rich, cachePlan: {}, cacheBreakpoints: [0] })], providerConfig);
add('headers-and-path', [step()], { providers: { p: { ...provider, url: '$MOCK/v1/', headers: { 'x-api-key': 'overridden', 'anthropic-version': 'fixture-version', 'x-fixture': 'yes', 'content-type': 'application/custom' } } } });
for (const protocol of ['openai', 'google', 'openai-responses']) add(`unsupported-${protocol}`, [step(), step({ ...baseRequest, model: 'toString' })], { providers: { p: { ...provider, protocol } } });
add('unknown-provider-model', [step({ ...baseRequest, provider: 'unknown' }), step({ ...baseRequest, model: 'unknown' })], providerConfig);
// The baseline indexes the plain provider/model records, so inherited
// `Object.prototype` members are truthy finds rather than misses. Keep the
// provider and model names separate: each one reaches a different read in
// `countWithProvider`.
add('prototype-lookup', [
  ...['__proto__', 'constructor', 'toString'].map(key => step({ ...baseRequest, provider: key })),
  ...['__proto__', 'constructor', 'toString'].map(key => step({ ...baseRequest, model: key })),
  // `speed` is read as `model.capabilities.supportsSpeed` after max_tokens.
  ...['__proto__', 'constructor', 'toString'].map(key => step({ ...baseRequest, model: key, maxOutputTokens: 10, speed: 'fast' })),
  // `request.tools?.map` is evaluated before max_tokens/speed.
  step({ ...baseRequest, model: 'toString', tools: 'x' }),
  step({ ...baseRequest, model: 'toString', maxOutputTokens: 10, speed: 'fast', tools: 'x' }),
], providerConfig);
for (const protocol of ['anthropic', 'openai-responses']) {
  const p = { ...provider, protocol, url: protocol === 'anthropic' ? '$MOCK' : 'https://api.openai.com/v1/' };
  add(`${protocol}-counts`, [{ input_tokens: 0, inputTokens: 99 }, { input_tokens: -2.5 }, { input_tokens: '3', inputTokens: 4.25 }, { inputTokens: null, tokens: 5 }, {}, null, []].map(reply => step(baseRequest, {}, { reply })), { providers: { p } }, { cacheSize: 0 });
  add(`${protocol}-http-error`, [step(baseRequest, { calibration }, { status: 429 }), step(baseRequest, {}, { status: 503 })], { providers: { p } });
  add(`${protocol}-cache`, [step(baseRequest), step(baseRequest, { calibration }, { reply: { input_tokens: 999 } }), step({ ...baseRequest, maxOutputTokens: 1, temperature: 0.1, speed: 'fast', stream: true, metadata: { x: 1 } }), step({ ...baseRequest, systemPrompt: 'new' }), step(baseRequest), step(baseRequest, { useProviderCount: false }), step({ ...baseRequest, tools }), step({ ...baseRequest, systemPrompt: 'new' })], { providers: { p } }, { cacheSize: 2 });
  for (const state of ['default', 'enabled', 'disabled']) for (const format of ['provider', 'server-default']) {
    add(`${protocol}-thinking-${state}-${format}`, [step(baseRequest), step({ ...baseRequest, thinking: { mode: 'high' } }), step({ ...baseRequest, thinking: { mode: 'max' } })], { providers: { p: { ...p, models: { m: { ...model, thinking: { state, format, efforts: ['high'] } } } } } });
  }
}
add('anthropic-old-thinking', [step(baseRequest), step({ ...baseRequest, thinking: { mode: 'high' } })], { providers: { p: { ...provider, models: { m: { ...model, id: 'claude-sonnet-4-5', thinking: { state: 'enabled', format: 'provider', efforts: ['high'] } } } } } });
for (const url of ['https://api.openai.com', ' https://user:password@api.openai.com/v1?query=x#hash ', 'https://api.openai.com/v1/extra', 'http://api.openai.com', 'https://example.test/v1']) {
  add(`responses-url-${url}`, [step(rich), step({ ...rich, outputSchema: { name: 'shape', schema: { const: [] }, strict: false } })], { providers: { p: { ...provider, protocol: 'openai-responses', url } } });
}
add('provider-shallow-tools', ['abc', { length: 0 }].map(tools => step({ ...baseRequest, tools })), providerConfig);
for (const protocol of ['anthropic', 'openai-responses']) {
  const unicode = { ...rich, outputSchema: { name: '\ud800', schema: {} }, messages: [{ role: 'assistant', content: [
    { type: 'image', source: 'base64', data: '\ud800', mimeType: 'image/\ud800' },
    { type: 'audio', source: 'url', data: '\ud800', mimeType: 'audio/fixture' },
    { type: 'tool_result_reference', toolCallId: 't', hasMore: true, preview: '\ud800', path: '/fixture/\ud800', originalBytes: 100,
      content: [{ type: 'image', source: 'url', data: 'https://example.test', mimeType: 'image/png' }] },
    { type: 'tool_result', toolCallId: 't', content: [{ type: 'tool_result', content: [] }] },
  ] }] };
  add(`${protocol}-unicode`, [step(unicode)], { providers: { p: { ...provider, protocol, url: protocol === 'anthropic' ? '$MOCK' : 'https://api.openai.com/v1' } } });
}
add('responses-schema-prototype', [step({ ...baseRequest, tools: [{ name: 'x', inputSchema: JSON.parse('{"__proto__":{"type":"array"},"enum":[1]}') }] })], { providers: { p: { ...provider, protocol: 'openai-responses', url: 'https://api.openai.com/v1' } } });
add('custom-manager', [step(rich, { useProviderCount: false }), step({ ...baseRequest, messages: [] })], {}, { managerOptions: { multimediaTokens: 3.5, perMessageOverhead: 0.25, warningRatio: 0.1, blockingRatio: 0.2 } });
for (const protocol of ['anthropic', 'openai-responses']) {
  for (const [name, efforts] of [['string', 'high'], ['substring', 'xhigh'], ['null', null], ['missing', undefined], ['number', 5]] as const) {
    add(`${protocol}-efforts-${name}`, ['high', 'max'].map(mode => step({ ...baseRequest, thinking: { mode } })),
      { providers: { p: { ...provider, protocol, url: protocol === 'anthropic' ? '$MOCK' : 'https://api.openai.com/v1',
        models: { m: { ...model, thinking: { state: 'enabled', format: 'provider', efforts } } } } } });
  }
}
add('prototype-responses', [
  step({ ...baseRequest, model: 'toString' }),
  step({ ...baseRequest, model: 'toString', maxOutputTokens: 10, speed: 'fast' }),
  step({ ...baseRequest, model: 'toString', maxOutputTokens: 10, speed: 'fast', tools: 'x' }),
  step({ ...baseRequest, tools: 'x' }),
], { providers: { p: { ...provider, protocol: 'openai-responses', url: 'https://api.openai.com/v1' } } });
const cases: any[] = [];
try {
  for (const sequence of sequences) {
    const logicalUrls: string[] = [];
    const runtime = new TokenAccountingRuntime({ modelConfig: materialize(sequence.config), ...sequence.settings, tokenBudget: new TokenBudgetManager(sequence.settings.managerOptions), fetch: async (url: any, options: any) => {
      const logical = String(url); logicalUrls.push(logical.replaceAll(mock, '$MOCK'));
      const target = logical.startsWith('https://api.openai.com/') ? mock + new URL(logical).pathname : logical;
      assert.ok(target.startsWith(mock + '/'), 'fixture must never contact external providers');
      if (captureTransport) {
        // Limited fallback: records the real TS transport boundary, not HTTP.
        // The default generator still requires a loopback HTTP listener.
        const headers = Object.fromEntries(new Headers(options.headers));
        requests.push({ method: options.method, path: new URL(target).pathname, headers, body: JSON.parse(options.body) });
        return new Response(JSON.stringify(response), { status, headers: { 'content-type': 'application/json' } });
      }
      return fetch(target, options);
    } });
    const steps = [];
    for (const spec of sequence.steps) {
      response = spec.reply === undefined ? { input_tokens: 73 } : spec.reply; status = spec.status ?? 200; requests = []; logicalUrls.length = 0;
      let result;
      try { result = { ok: true, snapshot: await runtime.evaluateRequestBudget(spec.request, spec.options) }; }
      catch (error: any) { result = { ok: false, message: error.message }; }
      steps.push(json({ ...spec, result, requests, urls: logicalUrls }));
    }
    cases.push({ ...sequence, steps });
  }
  // The public evaluator estimates locally first and rejects these inputs
  // before provider projection. Call the real private provider-count entry
  // directly so this corpus detects regressions in the Responses path too.
  const providerErrors = [];
  for (const [name, content] of [['null', null], ['missing', undefined], ['number', 5]] as const) {
    const config = { providers: { p: { ...provider, protocol: 'openai-responses', url: 'https://api.openai.com/v1' } } };
    const request = { ...baseRequest, messages: [{ role: 'user', content: [{ type: 'tool_result', toolCallId: 'call', content }] }] };
    let calls = 0;
    const runtime = new TokenAccountingRuntime({ modelConfig: config, fetch: async () => { calls++; throw new Error('unexpected transport'); } });
    let result;
    try { result = { ok: true, value: await (runtime as any).countWithProvider(request) }; }
    catch (error: any) { result = { ok: false, message: error.message }; }
    assert.equal(result.ok, false, `tool-result-${name}: provider projection must reject`);
    assert.equal(calls, 0, `tool-result-${name}: reject before transport`);
    providerErrors.push(json({ name: `openai-responses-tool-result-${name}`, config, request, result, calls }));
  }
  const snapshots = [];
  for (const managerOptions of [{}, { warningRatio: 0.5, blockingRatio: 0.75 }]) {
    const manager = new TokenBudgetManager(managerOptions);
    for (const tokens of [-1, 0, 79.99, 80, 89.99, 90, 100]) for (const reservedOutputTokens of [0, 10.75, 200]) {
      const metadata = { reservedOutputTokens, localEstimateTokens: 50, displayTokens: tokens === 0 ? 0 : 5, usageTokens: tokens, source: 'provider', exact: true };
      snapshots.push({ managerOptions, tokens, context: 100.9, metadata, result: json(manager.snapshotFromTokens(tokens, 100.9, metadata)) });
    }
  }
  const operations = [];
  const runtime = new TokenAccountingRuntime({ modelConfig: { providers: {} } });
  const budget = { estimateRequestInput: (r: any) => runtime.estimateRequestInput(r), evaluateRequestBudget: (r: any, o: any) => runtime.evaluateRequestBudget(r, o), estimateUsageCost: () => undefined };
  const dispatcher = createSidecarDefaultModuleDispatcher({ config: { cwd: output }, input: {}, checkpoint: {}, modules: { budget, model: { execution: {} }, capability: { runtimeContext: { bindTurn: () => ({ permissionContext: () => ({}) }) }, execution: { list: () => [] } } } } as any);
  const payloads: any[] = [
    ...['estimate_request_input', 'evaluate_request_budget'].flatMap(operation => [undefined, {}, { ...baseRequest, provider: undefined }, { ...baseRequest, model: undefined }, { ...baseRequest, messages: undefined }].map(request => ({ operation, request, options: { maxContextTokens: 100 } }))),
    ...[undefined, {}, { maxContextTokens: 0 }, { maxContextTokens: 100, reservedOutputTokens: -1 }, { maxContextTokens: 100, useProviderCount: 1 }, { maxContextTokens: 100, useProviderCount: false }].map(options => ({ operation: 'evaluate_request_budget', request: baseRequest, options })),
    ...[{}, { provider: 'p' }, { model: 'm' }, { provider: 'p', model: 'm' }].map(route => ({ operation: 'estimate_usage_cost', ...route })),
  ];
  for (const payload of payloads) {
    let result; try { result = { ok: true, value: await dispatcher.handlers.budget({ payload } as any) }; } catch (error: any) { result = { ok: false, message: error.message }; }
    operations.push(json({ payload, result }));
  }
  await dispatcher.dispose();
  assertBaseline(baseline);
  assert.deepEqual(await baselineSourceHashes(baseline, sources.map(p => `src/${p}`)), sourceSha256);
  await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases, snapshots, operations, providerErrors }, null, 2) + '\n');
  console.log(JSON.stringify({ output, transport: captureTransport ? 'injected (HTTP not verified)' : 'loopback HTTP', sequences: cases.length, steps: cases.reduce((n, c) => n + c.steps.length, 0), snapshots: snapshots.length, operations: operations.length, providerErrors: providerErrors.length }));
} finally { await new Promise<void>(resolve => { server.closeAllConnections(); server.close(() => resolve()); }); }
