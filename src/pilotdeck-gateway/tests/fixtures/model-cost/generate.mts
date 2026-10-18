import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
// Run from the repo root with Node 22, the fixed baseline's tsx loader and
// PDRS_BASELINE pointing at the fixed PilotDeck checkout:
//
//   PDRS_BASELINE=/abs/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/model-cost/generate.mts
//
// Every number below is produced by the real baseline modules; no arithmetic is
// copied into the fixture beyond the assertions that re-derive components from
// the exported helpers.
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rename, rm, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
const baseline = resolve(process.env.PDRS_BASELINE);
const fixtureDir = dirname(fileURLToPath(import.meta.url));
const sourceCommit = assertBaseline(baseline);
const SOURCES = [
  'router/utils/modelPricing.ts',
  'router/stats/TokenStatsCollector.ts',
  'context/budget/TokenAccountingRuntime.ts',
  'context/budget/TokenBudgetManager.ts',
  'context/budget/tokenizer.ts',
  'model/protocol/toolResultContent.ts',
  'model/catalog/providers.ts',
  'agent/loop/AgentLoop.ts',
  'agent/loop/nativeAgentTurnCapabilitiesAdapter.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, SOURCES.map(source => `src/${source}`));

const importBaseline = (relative: string) => import(pathToFileURL(join(baseline, 'src', relative)).href);
const pricingModule = await importBaseline('router/utils/modelPricing.ts');
const { PROVIDER_CATALOG } = await importBaseline('model/catalog/providers.ts');
const { TokenAccountingRuntime } = await importBaseline('context/budget/TokenAccountingRuntime.ts');
const { TokenStatsCollector } = await importBaseline('router/stats/TokenStatsCollector.ts');
const { lookupModelPricing, lookupModelPricingWithSource, calculateInputCost, calculateCacheReadCost } = pricingModule;

// `modelPricing.ts` documents the unit as display-only; it never enters arithmetic.
type ConfiguredPricing = Record<string, { input?: number; output?: number; cacheRead?: number; unit?: string }>;
const CONFIGURED: Record<string, ConfiguredPricing> = {
  exact: { 'custom/exact-model': { input: 1.25, output: 7.5, cacheRead: 0.5 } },
  partial: { 'custom/partial-model': { input: 2 } },
  emptyEntry: { 'custom/empty-model': {} },
  unitOnly: { 'custom/unit-model': { unit: '¥/百万 Token' } },
  modelIncludesKey: { 'sonnet-4': { input: 4, output: 20, cacheRead: 1 } },
  keyIncludesModel: { 'openai/gpt-4o-2024-11-20': { input: 9, output: 27 } },
  insertionOrder: { model: { input: 1, output: 2 }, 'zzz-model': { input: 3, output: 4 } },
  catalogOverride: { gpt: { input: 11, output: 22, cacheRead: 3 } },
  empty: {},
  integerKeys: { '12': { input: 12, output: 24 }, '2': { input: 2, output: 4 } },
};

const catalogRoutes: Array<{ provider: string; model: string }> = [];
for (const [provider, entry] of Object.entries(PROVIDER_CATALOG as any)) {
  for (const model of Object.keys((entry as any).models)) catalogRoutes.push({ provider, model });
}
const configuredRoutes = [
  { provider: 'custom', model: 'exact-model' },
  { provider: 'custom', model: 'partial-model' },
  { provider: 'custom', model: 'empty-model' },
  { provider: 'custom', model: 'unit-model' },
  { provider: 'anthropic', model: 'claude-sonnet-4-5-20250929' },
  { provider: 'unknown', model: 'zzz-model' },
  { provider: 'openai', model: 'gpt-4o' },
  { provider: 'unknown', model: 'claude-sonnet-x' },
];
const integerKeyRoutes = [{ provider: 'custom', model: 'model12' }];
const syntheticRoutes = [
  { provider: 'unknown', model: 'unknown-model' },
  { provider: 'deepseek', model: 'deepseek-v3' },
  { provider: 'deepseek', model: 'deepseek-chat' },
  { provider: 'deepseek', model: 'deepseek-reasoner' },
  { provider: 'deepseek', model: 'deepseek-flash' },
  { provider: 'anthropic', model: 'claude-opus-4' },
  { provider: 'anthropic', model: 'claude-sonnet-4' },
  { provider: 'anthropic', model: 'claude-haiku' },
  { provider: 'openai', model: 'gpt-4o-mini' },
  { provider: 'openai', model: 'gpt-4.1' },
  { provider: 'openai', model: 'gpt-5' },
  { provider: 'openai', model: 'o1-mini' },
  { provider: 'openai', model: 'o3-pro' },
  { provider: 'openai', model: 'o4' },
  { provider: 'google', model: 'gemini-2.5-flash' },
  { provider: 'google', model: 'gemini-2.5-pro' },
  { provider: 'zhipu', model: 'glm-4-flash' },
  { provider: 'dashscope', model: 'qwen-turbo' },
  { provider: 'dashscope', model: 'qwen-plus' },
  { provider: 'dashscope', model: 'qwen-max' },
  { provider: 'dashscope', model: 'qwen3' },
  { provider: 'meta', model: 'llama-3-70b' },
  { provider: 'meta', model: 'llama-3-405b' },
  { provider: 'meta', model: 'llama-3-8b' },
  { provider: 'mistral', model: 'mistral-large' },
  { provider: 'mistral', model: 'mistral-small' },
  { provider: 'mistral', model: 'mistral-7b' },
  { provider: '01ai', model: 'yi-34b' },
  { provider: 'moonshot', model: 'kimi-k2' },
  { provider: 'volc_ark', model: 'doubao-pro' },
  { provider: 'CaseSensitive', model: 'CLAUDE-SONNET-X' },
];
// Kept separate so the new boundary cases append after the pre-existing
// corpus instead of shifting every configured block.
const boundaryRoutes = [
  // JS `/i` (no `u`) canonicalization and `.` line-terminator boundaries.
  // U+212A KELVIN SIGN and U+017F LATIN SMALL LETTER LONG S uppercase into
  // ASCII, but JS step 5 keeps a non-ASCII code unit unfolded, so these must
  // NOT match `kimi` / `mistral.*small`.
  { provider: 'unknown', model: '\u212Aimi' },
  { provider: 'unknown', model: 'mi\u017Fstral-large' },
  // Each JS line terminator must stop `.`; \v and \t are ordinary chars.
  { provider: 'unknown', model: 'claude\rsonnet' },
  { provider: 'unknown', model: 'claude\nsonnet' },
  { provider: 'unknown', model: 'deepseek\u2028flash' },
  { provider: 'unknown', model: 'deepseek\u2029flash' },
  { provider: 'unknown', model: 'deepseek\u000Bflash' },
  { provider: 'unknown', model: 'deepseek\tflash' },
];

const pricingJson = (pricing: any) => ({
  ...(pricing.input !== undefined ? { input: pricing.input } : {}),
  ...(pricing.output !== undefined ? { output: pricing.output } : {}),
  ...(pricing.cacheRead !== undefined ? { cacheRead: pricing.cacheRead } : {}),
});

const pricingLookups: any[] = [];
function pushLookup(provider: string, model: string, configuredName: string | null, configured?: ConfiguredPricing) {
  const result = lookupModelPricingWithSource(provider, model, configured as any);
  pricingLookups.push({
    provider,
    model,
    ...(configuredName === null ? {} : { configuredName }),
    ...(configured === undefined ? {} : { configured }),
    pricing: pricingJson(result.pricing),
    source: result.source,
  });
}

for (const route of catalogRoutes) {
  pushLookup(route.provider, route.model, 'none');
  pushLookup(route.provider, route.model, 'catalogOverride', CONFIGURED.catalogOverride);
}
for (const route of syntheticRoutes) {
  pushLookup(route.provider, route.model, 'none');
  pushLookup(route.provider, route.model, 'catalogOverride', CONFIGURED.catalogOverride);
}
for (const [name, map] of Object.entries(CONFIGURED)) {
  for (const route of configuredRoutes) pushLookup(route.provider, route.model, name, map);
}
for (const route of boundaryRoutes) {
  pushLookup(route.provider, route.model, 'none');
  pushLookup(route.provider, route.model, 'catalogOverride', CONFIGURED.catalogOverride);
}
for (const route of integerKeyRoutes) pushLookup(route.provider, route.model, 'integerKeys', CONFIGURED.integerKeys);

const TOKEN_COUNTS = [0, 1_000_000, 1_234_567, 999_999_999, 0.5];
const costs: any[] = [];
function pushCosts(route: { provider: string; model: string }, configuredName: string | null, configured?: ConfiguredPricing) {
  for (const tokens of TOKEN_COUNTS) {
    costs.push({
      provider: route.provider,
      model: route.model,
      ...(configuredName === null ? {} : { configuredName }),
      ...(configured === undefined ? {} : { configured }),
      tokens,
      input: calculateInputCost(tokens, route.provider, route.model, configured as any),
      cacheRead: calculateCacheReadCost(tokens, route.provider, route.model, configured as any),
    });
  }
}
for (const route of [...catalogRoutes, ...syntheticRoutes]) pushCosts(route, null);
for (const [name, map] of Object.entries(CONFIGURED)) {
  for (const route of configuredRoutes) pushCosts(route, name, map);
}
for (const route of boundaryRoutes) pushCosts(route, null);
for (const route of integerKeyRoutes) pushCosts(route, 'integerKeys', CONFIGURED.integerKeys);

const collectorFor = (modelPricing: ConfiguredPricing | undefined) =>
  new TokenStatsCollector({ enabled: false, modelPricing } as any);

// The TokenStatsCollector keeps `estimateCostBreakdown` private; re-derive its
// components with the exported lookup and assert the total against the real
// method so a component mismatch cannot hide inside a rounded total.
function replicaBreakdown(usage: any, provider: string, model: string, modelPricing: ConfiguredPricing | undefined) {
  if (typeof usage.nativeCost === 'number' && Number.isFinite(usage.nativeCost) && usage.nativeCost >= 0) {
    return { input: 0, output: 0, cacheRead: 0, total: usage.nativeCost };
  }
  const pricing = lookupModelPricing(provider, model, modelPricing as any);
  const input = ((usage.inputTokens ?? 0) / 1_000_000) * (pricing.input ?? 0);
  const output = ((usage.outputTokens ?? 0) / 1_000_000) * (pricing.output ?? 0);
  const cacheRead = ((usage.cacheReadTokens ?? 0) / 1_000_000) * (pricing.cacheRead ?? 0);
  const cacheWrite = ((usage.cacheWriteTokens ?? 0) / 1_000_000) * (pricing.input ?? 0);
  return { input, output, cacheRead, total: input + output + cacheRead + cacheWrite };
}

function compositionFinal(routerEstimate: number, usage: any): number | null {
  if (typeof routerEstimate === 'number' && Number.isFinite(routerEstimate) && routerEstimate >= 0) return routerEstimate;
  const nativeCost = usage?.nativeCost;
  return typeof nativeCost === 'number' && Number.isFinite(nativeCost) && nativeCost >= 0 ? nativeCost : null;
}

const USAGES: any[] = [
  {},
  { inputTokens: 0 },
  { inputTokens: 1000, outputTokens: 500 },
  { inputTokens: 123456, outputTokens: 7890, cacheReadTokens: 4567, cacheWriteTokens: 890 },
  { inputTokens: 999999999, outputTokens: 888888888, cacheReadTokens: 777777777, cacheWriteTokens: 666666666 },
  { cacheReadTokens: 12345 },
  { cacheWriteTokens: 54321 },
  { inputTokens: 100, nativeCost: 0.0042 },
  { inputTokens: 100, nativeCost: 0 },
  { inputTokens: 100, nativeCost: -1 },
  { inputTokens: -1_000_000 },
  { inputTokens: -1_000_000, nativeCost: 0.25 },
];

const usageCosts: any[] = [];
function pushUsageCosts(route: { provider: string; model: string }, configuredName: string | null, configured?: ConfiguredPricing, usages: any[] = USAGES) {
  const collector = collectorFor(configured);
  for (const usage of usages) {
    const routerEstimate = collector.estimateCost(usage, route.provider, route.model);
    const breakdown = replicaBreakdown(usage, route.provider, route.model, configured);
    assert.equal(breakdown.total, routerEstimate, `${route.provider}/${route.model} total`);
    if (typeof usage.nativeCost !== 'number' || !(Number.isFinite(usage.nativeCost) && usage.nativeCost >= 0)) {
      // `calculateCacheReadCost` falls back to the input rate while the
      // collector's cache-read bucket falls back to zero; only the input helper
      // is the same formula, and both exported helpers are pinned in `costs`.
      assert.equal(calculateInputCost(usage.inputTokens ?? 0, route.provider, route.model, configured as any), breakdown.input);
    }
    usageCosts.push({
      provider: route.provider,
      model: route.model,
      ...(configuredName === null ? {} : { configuredName }),
      ...(configured === undefined ? {} : { configured }),
      usage,
      breakdown,
      routerEstimate,
      costUsd: compositionFinal(routerEstimate, usage),
    });
  }
  // `estimateCost(undefined)` is the zero case; keep it once per route.
  const undefinedEstimate = collector.estimateCost(undefined, route.provider, route.model);
  assert.equal(undefinedEstimate, 0);
  usageCosts.push({
    provider: route.provider,
    model: route.model,
    ...(configuredName === null ? {} : { configuredName }),
    ...(configured === undefined ? {} : { configured }),
    usage: null,
    breakdown: null,
    routerEstimate: undefinedEstimate,
    costUsd: compositionFinal(undefinedEstimate, undefined),
  });
}

for (const route of catalogRoutes) pushUsageCosts(route, null);
for (const route of syntheticRoutes) pushUsageCosts(route, null);
for (const [name, map] of Object.entries(CONFIGURED)) {
  for (const route of configuredRoutes) pushUsageCosts(route, name, map, USAGES.slice(0, 4));
}
for (const route of boundaryRoutes) pushUsageCosts(route, null);
for (const route of integerKeyRoutes) pushUsageCosts(route, 'integerKeys', CONFIGURED.integerKeys, USAGES.slice(0, 4));

// Request shapes. Every object key order is deliberate: the oracle keeps the
// JSON.stringify order and the Rust replay re-serializes with JS key ordering.
const message = (role: 'user' | 'assistant', content: any[], metadata?: any) => ({
  role,
  content,
  ...(metadata === undefined ? {} : { metadata }),
});
const request = (messages: any[], extra: any = {}) => ({
  provider: 'fixture',
  model: 'fixture-model',
  messages,
  ...extra,
});
const text = (value: string) => ({ type: 'text', text: value });
const thinking = (value: string, reasoningContent?: string) => ({
  type: 'thinking',
  text: value,
  ...(reasoningContent === undefined ? {} : { reasoningContent }),
});
const image = (source: 'base64' | 'url', data: string, mimeType = 'image/png', bytes?: number) => ({
  type: 'image',
  source,
  data,
  mimeType,
  ...(bytes === undefined ? {} : { bytes }),
});
const pdf = (data: string, bytes: number, pages?: number) => ({
  type: 'pdf',
  data,
  mimeType: 'application/pdf',
  bytes,
  ...(pages === undefined ? {} : { pages }),
});
const audio = (data: string, mimeType = 'audio/mpeg', bytes?: number) => ({
  type: 'audio',
  source: 'base64',
  data,
  mimeType,
  ...(bytes === undefined ? {} : { bytes }),
});
const toolCall = (id: string, name: string, input?: any) => ({
  type: 'tool_call',
  id,
  name,
  ...(input === undefined ? {} : { input }),
});
const toolResult = (toolCallId: string, content: any[]) => ({ type: 'tool_result', toolCallId, content });
const toolReference = (preview: string) => ({
  type: 'tool_result_reference',
  toolCallId: 'call-ref',
  path: '/tmp/persisted.txt',
  originalBytes: 987654,
  preview,
  hasMore: true,
});
const mediaReference = (preview: string, mediaType: 'image' | 'pdf' | 'audio') => ({
  type: 'media_reference',
  path: '/tmp/media.bin',
  originalBytes: 55555,
  preview,
  hasMore: false,
  mimeType:
    mediaType === 'image' ? 'image/png' : mediaType === 'pdf' ? 'application/pdf' : 'audio/mpeg',
  mediaType,
  ...(mediaType === 'image' ? { pages: 3 } : {}),
});
const tool = (name: string, description: string | undefined, inputSchema: any) => ({
  name,
  ...(description === undefined ? {} : { description }),
  inputSchema,
});

const orderedInput = { z: 1, a: { two: 2, one: 1 }, list: [3, 1, 2], 0: 'zero', 中: '文' };
const orderedSchema = {
  type: 'object',
  properties: { z: { type: 'string' }, a: { type: 'number' }, 0: { type: 'boolean' } },
  required: ['z', 'a'],
  additionalProperties: false,
};

const shapes: Array<{ name: string; request: any }> = [];
const add = (name: string, value: any) => shapes.push({ name, request: value });

add('empty-messages', request([]));
add('text-ascii', request([message('user', [text('Hello, world! 123')])]));
add('text-empty', request([message('user', [text('')])]));
add('text-chinese', request([message('user', [text('你好，世界！这是一个中文测试。')])]));
add('text-emoji', request([message('user', [text('😀😃😄👨‍👩‍👧‍👦🧑🏽‍🚀')])]));
add('text-multiblock', request([message('user', [text('first'), thinking('pondering'), text('second')])]));
add('text-multimessage', request([
  message('user', [text('one')]),
  message('assistant', [text('two')]),
  message('user', [text('three')]),
]));
add('assistant-text', request([message('assistant', [text('answer')])]));
add('system-ascii', request([message('user', [text('hi')])], { systemPrompt: 'You are a helpful assistant.' }));
add('system-empty', request([message('user', [text('hi')])], { systemPrompt: '' }));
add('system-chinese', request([message('user', [text('hi')])], { systemPrompt: '你是助手。' }));
add('system-mcp', request([message('user', [text('hi')])], {
  systemPrompt: 'prefix <mcp-instructions>use the search tool</mcp-instructions> suffix',
}));
add('system-memory', request([message('user', [text('hi')])], {
  systemPrompt: 'A <memory-context>remember: 42</memory-context> B',
}));
add('system-both', request([message('user', [text('hi')])], {
  systemPrompt: '<mcp-instructions>mcp body</mcp-instructions> middle <memory-context>memory body</memory-context> tail',
}));
add('system-adjacent', request([message('user', [text('hi')])], {
  systemPrompt: '<mcp-instructions>one</mcp-instructions><mcp-instructions>two</mcp-instructions>',
}));
add('system-unclosed', request([message('user', [text('hi')])], {
  systemPrompt: '<mcp-instructions>never closed',
}));
add('system-mcp-memory-overlap', request([message('user', [text('hi')])], {
  systemPrompt: '<memory-context>outer</memory-context><mcp-instructions>inner</mcp-instructions>',
}));
add('tools-empty', request([message('user', [text('hi')])], { tools: [] }));
add('tools-simple', request([message('user', [text('hi')])], {
  tools: [tool('read_file', 'Read a file', { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] })],
}));
add('tools-many', request([message('user', [text('hi')])], {
  tools: [
    tool('search', '搜索网络内容', { type: 'object', properties: { q: { type: 'string' } } }),
    tool('write', undefined, { type: 'object', properties: {} }),
    tool('中文工具', '中文描述 😀', { type: 'object', properties: { 值: { type: 'string' } } }),
  ],
}));
add('tools-ordered', request([message('user', [text('hi')])], { tools: [tool('ordered', 'keys', orderedSchema)] }));
add('tools-empty-schema', request([message('user', [text('hi')])], { tools: [tool('noargs', undefined, {})] }));
add('tool-call-object', request([message('assistant', [toolCall('call-1', 'do_it', orderedInput)])]));
add('tool-call-null', request([message('assistant', [toolCall('call-2', 'do_it', null)])]));
add('tool-call-string', request([message('assistant', [toolCall('call-3', 'do_it', 'raw-argument')])]));
add('tool-call-array', request([message('assistant', [toolCall('call-4', 'do_it', [1, 'two', { three: 3 }])])]));
add('tool-call-undefined', request([message('assistant', [toolCall('call-5', 'do_it')])]));
add('tool-result-text', request([message('user', [toolResult('call-6', [text('line one'), text('line two')])])]));
add('tool-result-mixed', request([message('user', [toolResult('call-7', [
  text('output'),
  image('base64', 'a'.repeat(37)),
  pdf('b'.repeat(21), 21, 3),
])])]));
add('tool-result-image-url', request([message('user', [toolResult('call-8', [
  image('url', 'https://example.invalid/x.png', 'image/png'),
])])]));
add('tool-result-pdf-no-pages', request([message('user', [toolResult('call-9', [pdf('c'.repeat(9), 9)])])]));
add('tool-result-empty', request([message('user', [toolResult('call-10', [])])]));
add('tool-result-reference', request([message('user', [toolReference('preview of a persisted tool result')])]));
add('media-reference', request([message('user', [mediaReference('media preview', 'image')])]));
add('image-block', request([message('user', [text('see'), image('base64', 'd'.repeat(41), 'image/jpeg', 41)])]));
add('pdf-block', request([message('user', [pdf('e'.repeat(17), 17, 2)])]));
add('audio-block', request([message('user', [audio('f'.repeat(19), 'audio/mpeg', 19)])]));
add('thinking-reasoning', request([message('assistant', [thinking('legacy text', 'provider reasoning')])]));
add('thinking-text-only', request([message('assistant', [thinking('only legacy text')])]));
add('special-tokens', request([message('user', [text('<|endoftext|> <|endofprompt|> ordinary')])]));
add('newlines', request([message('user', [text('a\n\nb\r\nc\t d   e\n')])]));
add('long-ascii', request([message('user', [text('a'.repeat(10_000))])]));
add('long-mixed', request([message('user', [text('中文😀abc, '.repeat(1_000))])]));
add('all-blocks', request([
  message('user', [text('start'), image('url', 'https://example.invalid/y.png'), pdf('g'.repeat(5), 5, 0)]),
  message('assistant', [thinking('legacy', 'reasoning'), toolCall('call-11', 'go', orderedInput)]),
  message('user', [toolResult('call-11', [text('done'), image('base64', 'h'.repeat(8))]), toolReference('tail preview'), mediaReference('tail media', 'audio')]),
], {
  systemPrompt: 'S <mcp-instructions>m</mcp-instructions> <memory-context>c</memory-context>',
  tools: [tool('t', 'd', orderedSchema)],
}));
add('metadata-ignored', request([message('user', [text('with metadata')], { model: 'other', synthetic: true, toolCallId: 'x' })]));
add('fractional-pages', request([message('user', [pdf('i'.repeat(3), 3, 2.5)])]));
// `String(pages)` boundaries: JS switches to exponential notation outside
// [1e-6, 1e21), keeps the shortest round-trip digits, and drops a -0 sign.
add('pdf-pages-tiny', request([message('user', [toolResult('call-12', [pdf('j'.repeat(3), 3, 1e-7)])])]));
add('pdf-pages-large', request([message('user', [toolResult('call-13', [pdf('k'.repeat(3), 3, 1e21)])])]));
add('pdf-pages-2.5e21', request([message('user', [toolResult('call-14', [pdf('l'.repeat(3), 3, 2.5e21)])])]));
add('pdf-pages-float', request([message('user', [toolResult('call-15', [pdf('m'.repeat(3), 3, 0.1 + 0.2)])])]));
add('pdf-pages-negative-zero', request([message('user', [toolResult('call-16', [pdf('n'.repeat(3), 3, -0)])])]));

const runtime = new TokenAccountingRuntime({ modelConfig: { providers: {} } as any });
const requests = shapes.map(({ name, request: value }) => {
  const breakdown = runtime.estimateRequestBreakdown(value);
  assert.equal(runtime.estimateRequestInput(value), breakdown.total, `${name} total`);
  assert.equal(
    breakdown.system + breakdown.tools + breakdown.messages + breakdown.mcp + breakdown.memory,
    breakdown.total,
    `${name} additive`,
  );
  for (const key of ['total', 'system', 'tools', 'messages', 'mcp', 'memory'] as const) {
    assert.ok(Number.isInteger(breakdown[key]) && breakdown[key] >= 0, `${name} ${key}`);
  }
  return { name, request: value, breakdown };
});
assert.ok(requests.length >= 20);

// `Object.entries` puts canonical array-index keys first in numeric order, then
// the remaining keys in insertion order. Both orders are recorded so the Rust
// replay can insert in source order and still observe the JS scan order.
const integerKeyPricing: ConfiguredPricing = { '12': { input: 12, output: 24 }, '2': { input: 2, output: 4 } };
const mixedKeyPricing: ConfiguredPricing = { gpt: { input: 1 }, '100': { input: 100 }, zz: { input: 3 }, '10': { input: 10 } };
const propertyOrder = {
  integerKeys: {
    insertionOrder: ['12', '2'],
    entries: ['12', '2'].map((key) => ({ key, pricing: integerKeyPricing[key] })),
    objectEntries: Object.entries(integerKeyPricing).map(([key]) => key),
    lookup: (() => {
      const result = lookupModelPricingWithSource('custom', 'model12', integerKeyPricing);
      return { provider: 'custom', model: 'model12', pricing: pricingJson(result.pricing), source: result.source };
    })(),
  },
  mixedKeys: {
    insertionOrder: ['gpt', '100', 'zz', '10'],
    entries: ['gpt', '100', 'zz', '10'].map((key) => ({ key, pricing: mixedKeyPricing[key] })),
    objectEntries: Object.entries(mixedKeyPricing).map(([key]) => key),
  },
};

const oracle = JSON.stringify({
  sourceCommit,
  sourceSha256,
  pricingLookups,
  costs,
  usageCosts,
  propertyOrder,
  requests,
}, null, 2) + '\n';
for (const path of [baseline, fixtureDir]) assert.ok(!oracle.includes(path), `oracle.json contains ${path}`);
// Optional output directory (the ignored Rust regeneration test uses it); the
// default keeps the historical "run in place" invocation working.
const outputDir = process.argv[2] ? resolve(process.argv[2]) : fixtureDir;
const target = join(outputDir, 'oracle.json');
await mkdir(outputDir, { recursive: true });
const temporary = await mkdtemp(join(outputDir, '.generate-'));
try {
  const file = join(temporary, 'oracle.json');
  await writeFile(file, oracle);
  await rename(file, target);
} finally { await rm(temporary, { recursive: true, force: true }); }
console.log(
  `PASS: ${pricingLookups.length} pricing lookups, ${costs.length} cost cases, ` +
  `${usageCosts.length} usage-cost cases, ${requests.length} request shapes`,
);
