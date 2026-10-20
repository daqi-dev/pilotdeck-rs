// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
// Real TS `streamModel` (openai-responses) against an owned loopback HTTP mock.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createServer as createHttpServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { createServer as createNetServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the output directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8');
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const ENTRY = 'src/model/streaming/streamModel.ts';
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [ENTRY]);
// The native retry policy jitters with Math.random; pin it so delays replay.
Math.random = () => 0.5;
const { streamModel } = await import(pathToFileURL(join(baseline, ENTRY)).href);
const undici = await import(pathToFileURL(join(baseline, 'node_modules/undici/index.js')).href);

// Fetch/undici transport headers, not provider projection. Excluded on both sides.
const TRANSPORT_HEADERS = ['host', 'connection', 'content-length', 'user-agent', 'accept',
  'accept-language', 'sec-fetch-mode', 'accept-encoding', 'transfer-encoding', 'keep-alive'];
type Chunk = { text?: string; hex?: string };
type Reply = { status?: number; headers?: Record<string, string>; chunks?: Chunk[]; end?: 'end' | 'destroy' | 'hold' };
type Spec = {
  name: string; target?: 'mock' | 'closed'; url?: string;
  request?: Record<string, unknown>; provider?: Record<string, unknown>; replies: Reply[];
  abortAfterTextDeltas?: number; streamTimeoutMs?: number; bodyTimeoutMs?: number;
};

const sse = (event: Record<string, unknown>, name = String(event.type)) =>
  ({ text: `event: ${name}\ndata: ${JSON.stringify(event)}\n\n` });
const created = (id = 'resp_1') => sse({ type: 'response.created', response: { id, status: 'in_progress' } });
const inProgress = (id = 'resp_1') => sse({ type: 'response.in_progress', response: { id } });
const text = (delta: string) => sse({ type: 'response.output_text.delta', item_id: 'msg_1', output_index: 0, delta });
const fnAdded = (item_id: string, call_id: string | undefined, name: string, output_index: number) =>
  sse({ type: 'response.output_item.added', output_index, item: { type: 'function_call', id: item_id, ...(call_id ? { call_id } : {}), name, arguments: '' } });
const argsDelta = (item_id: string, delta: string, output_index: number) =>
  sse({ type: 'response.function_call_arguments.delta', item_id, output_index, delta });
const argsDone = (item_id: string, args: string, output_index: number) =>
  sse({ type: 'response.function_call_arguments.done', item_id, output_index, arguments: args });
const itemDone = (item_id: string, call_id: string | undefined, name: string, args: string, output_index: number) =>
  sse({ type: 'response.output_item.done', output_index, item: { type: 'function_call', id: item_id, ...(call_id ? { call_id } : {}), name, arguments: args } });
const usage = { input_tokens: 20, input_tokens_details: { cached_tokens: 5 }, output_tokens: 9, output_tokens_details: { reasoning_tokens: 3 }, total_tokens: 29 };
const completed = (extra: Record<string, unknown> = {}) => sse({ type: 'response.completed', response: { id: 'resp_1', status: 'completed', usage, ...extra } });
const short = (t = 'ok'): Reply => ({ chunks: [created(), text(t), completed()] });

const specs: Spec[] = [
  { name: 'text-stream', replies: [{ chunks: [
    { text: ': keep-alive\n\n' }, created(), inProgress(), text('Hello'), text(' 世界😀'), text(''),
    sse({ type: 'response.output_text.done', item_id: 'msg_1', text: 'Hello 世界😀' }),
    sse({ type: 'response.unknown_future_event', foo: 1 }), completed(), { text: 'data: [DONE]\n\n' },
  ] }] },
  { name: 'interleaved-function-calls', replies: [{ chunks: [
    created('resp:x y/1'), fnAdded('fc_1', 'call_a', 'lookup', 0), fnAdded('fc_2', 'call_b', 'fetch', 1),
    argsDelta('fc_1', '{"q":', 0), argsDelta('fc_2', '{"url":"htt', 1), argsDelta('fc_1', '"天气"}', 0),
    argsDelta('fc_2', 'ps://x"}', 1), argsDone('fc_1', '{"q":"天气"}', 0), itemDone('fc_1', 'call_a', 'lookup', '{"q":"天气"}', 0),
    itemDone('fc_2', 'call_b', 'fetch', '{"url":"https://x"}', 1),
    // A third call reuses call_a and a fourth has none: synthetic ids from the response id.
    fnAdded('fc_3', 'call_a', 'lookup', 2), argsDone('fc_3', '{}', 2),
    fnAdded('fc_4', undefined, 'lookup', 3), argsDelta('fc_4', '{"n":1}', 3),
    completed(),
  ] }] },
  { name: 'reasoning', replies: [{ chunks: [
    created(), sse({ type: 'response.reasoning_summary_text.delta', item_id: 'rs_1', delta: 'Plan: ' }),
    sse({ type: 'response.reasoning_text.delta', item_id: 'rs_1', delta: 'think' }),
    sse({ type: 'response.reasoning.delta', delta: '...' }), sse({ type: 'response.reasoning_summary_text.delta', delta: '' }),
    text('Answer'), completed({ usage: { prompt_tokens: 7, completion_tokens: 2, prompt_tokens_details: { cached_tokens: 1, cache_write_tokens: 2 }, cost: 0.25 } }),
  ] }] },
  { name: 'incomplete-max-output', replies: [{ chunks: [
    created(), text('Partial'),
    sse({ type: 'response.incomplete', response: { id: 'resp_1', status: 'incomplete', incomplete_details: { reason: 'max_output_tokens' } } }),
  ] }] },
  { name: 'response-failed', replies: [{ chunks: [
    created(), sse({ type: 'response.failed', response: { id: 'resp_1', status: 'failed', error: { code: 'server_error', message: 'The model failed.' } } }),
  ] }] },
  { name: 'error-event', replies: [{ chunks: [
    sse({ type: 'error', code: 'x', error: { code: 'rate_limit_exceeded', message: 'Slow down. Please try again in 20ms.' } }),
  ] }] },
  { name: 'error-event-empty', replies: [{ chunks: [created(), text('so far'), sse({ type: 'error', error: { code: '  ', message: '' } })] }] },
  { name: 'http-400-error-body', replies: [{ status: 400, headers: { 'content-type': 'application/json' },
    chunks: [{ text: JSON.stringify({ error: { message: "Invalid value for 'input'.", type: 'invalid_request_error', param: 'input', code: 'invalid_value' } }) }] }] },
  { name: 'http-429-retry-after', replies: [
    { status: 429, headers: { 'content-type': 'application/json', 'retry-after': '0.015' },
      chunks: [{ text: JSON.stringify({ error: { message: 'Rate limit reached for requests', type: 'requests', code: 'rate_limit_exceeded' } }) }] },
    short('after 429')] },
  { name: 'connection-interrupted', replies: [{ chunks: [created(), text('cut here')], end: 'destroy' }] },
  { name: 'cancel', abortAfterTextDeltas: 1, replies: [{ chunks: [created(), text('first')], end: 'hold' }] },
  { name: 'tool-args-repaired', replies: [{ chunks: [
    created(), fnAdded('fc_r', 'call_r', 'lookup', 0), argsDelta('fc_r', "{q: 'x', n: [1 2,],", 0), argsDone('fc_r', "{q: 'x', n: [1 2,],", 0), completed(),
  ] }] },
  { name: 'tool-args-invalid', replies: [{ chunks: [
    created(), fnAdded('fc_i', 'call_i', 'lookup', 0), argsDelta('fc_i', '{:1}', 0), argsDone('fc_i', '{:1}', 0), completed(),
  ] }] },
  { name: 'completed-flushes-open-calls', replies: [{ chunks: [
    created(), fnAdded('fc_o', 'call_o', 'lookup', 0), argsDelta('fc_o', '{"open":', 0), argsDelta('fc_o', 'true}', 0),
    sse({ type: 'response.function_call_arguments.delta', call_id: 'call_late', delta: '' }), completed(),
  ] }] },
  { name: 'incomplete-stream-continuation', replies: [{ chunks: [created(), text('Half')] }, short(' rest')] },
  { name: 'body-variants', request: {
      toolChoice: { type: 'function', name: 'lookup' }, systemPrompt: '', speed: 0.2,
      metadata: { n: 1, o: { a: 1 }, arr: [1, [2, 3], null], nul: null, b: false },
      outputSchema: { name: 'answer', description: 'Answer shape', strict: false,
        schema: { type: 'object', properties: { tags: { type: ['string', 'array'] }, kind: { enum: ['a', 'b'] }, n: { enum: [1, 2.5] } } } },
    },
    provider: { models: { 'gpt-5': { id: 'gpt-5', thinking: { state: 'disabled', efforts: [], format: 'provider' },
      capabilities: { supportsStreaming: true, supportsSystemPrompt: true, supportsToolUse: true, supportsSpeed: true, maxOutputTokens: 4096 },
      multimodal: { input: ['text', 'image', 'pdf'] } } } },
    replies: [short()] },
  { name: 'body-thinking-patch', provider: { models: { 'gpt-5': { id: 'gpt-5', thinking: { state: 'enabled', efforts: ['high'], format: 'qwen-cloud' },
      capabilities: { supportsStreaming: true, supportsSystemPrompt: true, supportsToolUse: true, supportsSpeed: true },
      multimodal: { input: ['text', 'image', 'pdf'] } } } },
    replies: [short()] },
  { name: 'unsupported-thinking', provider: { models: { 'gpt-5': { id: 'gpt-5', thinking: { state: 'enabled', efforts: ['low'], format: 'provider' },
      capabilities: { supportsStreaming: true, supportsSystemPrompt: true, supportsToolUse: true, supportsSpeed: true, maxOutputTokens: 64 },
      multimodal: { input: ['text', 'image', 'pdf'] } } } },
    replies: [] },
  // codex r3 P1: String(value) runs ToPrimitive — an own toString (JSON cannot
  // make it callable) throws before any fetch, also nested inside an array.
  { name: 'metadata-object-to-primitive', request: { metadata: { tag: { toString: 0 } } }, replies: [] },
  { name: 'metadata-array-nested-object', request: { metadata: { ok: 'x', tag: [1, { toString: null }] } }, replies: [] },
  { name: 'endpoint-fallback', url: '$MOCK', replies: [{ status: 404, chunks: [{ text: 'no v1 here' }] }, short('fallback')] },
];

const request = {
  provider: 'responses-fixture', model: 'gpt-5', systemPrompt: 'You are terse.',
  messages: [
    { role: 'user', content: [{ type: 'text', text: 'Hi 你好' }, { type: 'image', source: 'base64', mimeType: 'image/png', data: 'iVBORw0K', detail: 'low' }] },
    { role: 'assistant', content: [{ type: 'thinking', text: 'prior thought' }, { type: 'text', text: 'Calling' },
      { type: 'tool_call', id: 'call_prev', name: 'lookup', input: { q: 'x', deep: [1, { a: null }] } }] },
    { role: 'user', content: [{ type: 'tool_result', toolCallId: 'call_prev', content: [{ type: 'text', text: 'result' },
      { type: 'image', source: 'url', mimeType: 'image/jpeg', data: 'https://img.example/a.jpg' }] }, { type: 'text', text: 'And now?' }] },
  ],
  tools: [{ name: 'lookup', description: 'Look up', inputSchema: { type: 'object', properties: { q: { type: 'string' }, ids: { type: 'array' } } } },
    { name: 'fetch', inputSchema: { type: 'object', properties: { url: { const: 'x' } } } }],
  toolChoice: 'auto', maxOutputTokens: 256, metadata: { user_id: 'u-1', trace: 't' }, thinking: { mode: 'high' }, speed: 0.8,
};
const provider = {
  id: 'responses-fixture', protocol: 'openai-responses', url: '$MOCK/v1', apiKey: '  sk-fixture  ',
  headers: { 'x-fixture': '1', 'OpenAI-Beta': 'responses=v1' }, extraBody: { temperature: 0.2, truncation: 'auto' },
  speedMapping: 'openai_service_tier', retry: { baseDelayMs: 2, maxDelayMs: 30, streamMaxRetries: 2 },
  models: { 'gpt-5': { id: 'gpt-5',
    thinking: { state: 'enabled', efforts: ['low', 'high'], format: 'provider' },
    capabilities: { supportsStreaming: true, supportsSystemPrompt: true, supportsToolUse: true, supportsSpeed: true, maxOutputTokens: 32768 },
    multimodal: { input: ['text', 'image', 'pdf'] } } },
};

type Exchange = { replies: Reply[]; requests: any[]; origin: string };
let current: Exchange = { replies: [], requests: [], origin: '' };
const chunkBytes = (chunk: Chunk) => chunk.hex !== undefined ? Buffer.from(chunk.hex, 'hex') : Buffer.from(chunk.text!, 'utf8');
const pause = (ms: number) => new Promise((r) => setTimeout(r, ms));
function handler(req: IncomingMessage, res: ServerResponse) {
  const body: Buffer[] = [];
  req.on('data', (part) => body.push(part));
  req.on('end', async () => {
    const exchange = current;
    const headers = Object.entries(req.headers)
      .filter(([name]) => !TRANSPORT_HEADERS.includes(name))
      .map(([name, value]) => [name, String(value)])
      .sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0));
    const record: Record<string, unknown> = { method: req.method, path: req.url, headers, body: Buffer.concat(body).toString('utf8') };
    exchange.requests.push(record);
    const reply = exchange.replies[exchange.requests.length - 1];
    assert.ok(reply, `unexpected extra request in ${exchange.origin}`);
    const socket = res.socket!;
    // Registered before any write: a client may close mid-reply.
    const closed = new Promise<boolean>((done) => socket.once('close', () => done(true)));
    socket.setNoDelay(true);
    res.writeHead(reply.status ?? 200, reply.headers ?? { 'content-type': 'text/event-stream' });
    res.flushHeaders();
    for (const chunk of reply.chunks ?? []) {
      res.write(chunkBytes(chunk));
      await pause(4);
    }
    if (reply.end === 'destroy') {
      await pause(20);
      res.socket!.destroy();
    } else if (reply.end === 'hold') {
      let timer: NodeJS.Timeout | undefined;
      record.clientClosed = await Promise.race([closed, new Promise<boolean>((done) => { timer = setTimeout(() => done(false), 3000); })]);
      clearTimeout(timer);
      socket.destroy();
    } else {
      res.end();
    }
  });
}

const http = createHttpServer(handler);
await new Promise<void>((r) => http.listen(0, '127.0.0.1', r));
const MOCK = `http://127.0.0.1:${(http.address() as any).port}`;
const probe = createNetServer();
await new Promise<void>((r) => probe.listen(0, '127.0.0.1', r));
const CLOSED = `http://127.0.0.1:${(probe.address() as any).port}`;
await new Promise<void>((r) => probe.close(() => r()));

const origins: [string, string][] = [];
const placeholder = (text: string) => origins.reduce((acc, [real, name]) => acc.split(real).join(name), text);
const scrub = (value: unknown): unknown => JSON.parse(placeholder(JSON.stringify(value)));
function thrownRecord(error: any) {
  const record: Record<string, unknown> = { name: error?.name, message: error?.message };
  if (error?.name === 'ModelProviderError') record.error = error.error;
  if (error?.name === 'ModelRequestError') record.code = error.code;
  return record;
}

const cases = [];
let requestCount = 0;
for (const spec of specs) {
  const target = spec.target ?? 'mock';
  const origin = target === 'closed' ? CLOSED : MOCK;
  origins.length = 0;
  origins.push([origin, target === 'closed' ? '$CLOSED' : '$MOCK']);
  current = { replies: spec.replies, requests: [], origin };
  const caseProvider = { ...provider, ...(spec.provider ?? {}),
    retry: { ...provider.retry, ...((spec.provider?.retry as object) ?? {}) },
    url: (spec.url ?? provider.url).replace('$MOCK', origin) };
  const config = { providers: { [provider.id]: caseProvider } };
  const caseRequest = { ...request, ...(spec.request ?? {}) };
  const timeline: unknown[] = [];
  const controller = new AbortController();
  const previous = undici.getGlobalDispatcher();
  if (spec.bodyTimeoutMs !== undefined) undici.setGlobalDispatcher(new undici.Agent({ bodyTimeout: spec.bodyTimeoutMs }));
  try {
    let textDeltas = 0;
    for await (const event of streamModel(caseRequest, config, {
      signal: controller.signal, streamTimeoutMs: spec.streamTimeoutMs,
      onRetryProgress: (progress: unknown) => timeline.push({ progress }),
    })) {
      timeline.push({ event });
      if (event.type === 'text_delta' && spec.abortAfterTextDeltas !== undefined && ++textDeltas === spec.abortAfterTextDeltas) {
        controller.abort();
      }
    }
  } catch (error) {
    timeline.push({ thrown: thrownRecord(error) });
  } finally {
    if (spec.bodyTimeoutMs !== undefined) undici.setGlobalDispatcher(previous);
  }
  // Let a held reply observe the client close before reading its record.
  for (let i = 0; i < 400 && spec.replies.some((r) => r.end === 'hold') && current.requests.some((r) => !('clientClosed' in r)); i++) {
    await pause(10);
  }
  assert.equal(current.requests.length, spec.replies.length, `${spec.name}: every scripted reply is consumed exactly once`);
  requestCount += current.requests.length;
  cases.push(scrub({ name: spec.name, target, url: spec.url, request: spec.request, provider: spec.provider,
    replies: spec.replies, abortAfterTextDeltas: spec.abortAfterTextDeltas, streamTimeoutMs: spec.streamTimeoutMs,
    bodyTimeoutMs: spec.bodyTimeoutMs, requests: current.requests, timeline }));
}
http.close();
http.closeAllConnections?.();

const oracle = {
  sourceCommit, sourceSha256, node: process.version, v8: process.versions.v8, undici: JSON.parse(readFileSync(join(baseline, 'node_modules/undici/package.json'), 'utf8')).version,
  locale, random: 0.5, transportHeaders: TRANSPORT_HEADERS, request, provider, cases,
};
assert.equal(cases.length, 21);
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 1) + '\n');
console.log(JSON.stringify({ output: join(output, 'oracle.json'), cases: cases.length, requests: requestCount }));
