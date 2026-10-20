// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
// Real TS `streamModel` (OpenAI chat completions) against an owned loopback HTTP mock.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
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
// The OpenAI request builder and stream normalizer are the calibrated closure;
// streamModel.ts carries the shared retry/checkpoint loop.
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [
  'src/model/streaming/streamModel.ts',
  'src/model/providers/openai/request.ts',
  'src/model/providers/openai/stream.ts',
]);
// The native retry policy jitters with Math.random; pin it so delays replay.
Math.random = () => 0.5;
const { streamModel } = await import(pathToFileURL(join(baseline, 'src/model/streaming/streamModel.ts')).href);

// Fetch/undici transport headers, not provider projection. Excluded on both sides.
const TRANSPORT_HEADERS = ['host', 'connection', 'content-length', 'user-agent', 'accept',
  'accept-language', 'sec-fetch-mode', 'accept-encoding', 'transfer-encoding', 'keep-alive'];
type Chunk = { text?: string; hex?: string };
type Reply = { status?: number; headers?: Record<string, string>; chunks?: Chunk[]; end?: 'end' | 'destroy' | 'hold' };
type Spec = {
  name: string; target?: 'mock' | 'closed'; url?: string;
  request?: Record<string, unknown>; provider?: Record<string, unknown>; replies: Reply[];
  abortAfterTextDeltas?: number; streamTimeoutMs?: number;
};

const sse = (event: Record<string, unknown>) => ({ text: `data: ${JSON.stringify(event)}\n\n` });
const done = { text: 'data: [DONE]\n\n' };
const chunk = (delta: Record<string, unknown>, extra: Record<string, unknown> = {}) => ({
  id: 'chatcmpl-fixture', object: 'chat.completion.chunk', created: 1700000000, model: 'gpt-5.4',
  choices: [{ index: 0, delta, finish_reason: null }], ...extra,
});
const finishing = (reason: string, extra: Record<string, unknown> = {}) => ({
  id: 'chatcmpl-fixture', object: 'chat.completion.chunk', created: 1700000000, model: 'gpt-5.4',
  choices: [{ index: 0, delta: {}, finish_reason: reason }], ...extra,
});
const short = (text = 'ok'): Reply => ({ chunks: [
  sse(chunk({ role: 'assistant', content: text })),
  sse(finishing('stop', { usage: { prompt_tokens: 9, completion_tokens: 2, total_tokens: 11 } })),
  done,
] });
const errorEvent = (error: Record<string, unknown>) => sse({ error });
const utf8Hex = (text: string) => Buffer.from(text, 'utf8').toString('hex');
const json = (value: unknown) => ({ text: JSON.stringify(value) });

const specs: Spec[] = [
  { name: 'normal-text', replies: [{ chunks: [
    { text: ': keep-alive\n\n' },
    sse(chunk({ role: 'assistant', content: 'Hello' })),
    { text: `id: 7\ndata:  ${JSON.stringify(chunk({ content: ' 世界' }))}  \nretry: 10\n\n` },
    sse(chunk({ content: '😀' })),
    sse(finishing('stop', {
      usage: { prompt_tokens: 11, completion_tokens: 7, total_tokens: 18,
        prompt_tokens_details: { cached_tokens: 3, cache_write_tokens: 2 } },
    })),
    done,
  ] }] },
  { name: 'utf8-boundaries', replies: [{ chunks: (() => {
    const body = `\uFEFF${sse(chunk({ role: 'assistant', content: 'start' })).text}`
      + `data: ${JSON.stringify(chunk({ content: 'crlf' }))}\r\n\r\n`
      + `${sse(chunk({ content: '中文😀é' })).text}${sse(chunk({ content: 'tail' })).text}`;
    const bytes = Buffer.from(body, 'utf8');
    const cuts = [1, 2, bytes.indexOf(Buffer.from('中')) + 1, bytes.indexOf(Buffer.from('中')) + 4,
      bytes.indexOf(Buffer.from('😀')) + 2, bytes.indexOf(Buffer.from('😀')) + 3, bytes.indexOf(Buffer.from('é')) + 1];
    const chunks: Chunk[] = [];
    let at = 0;
    for (const cut of cuts) { chunks.push({ hex: bytes.subarray(at, cut).toString('hex') }); at = cut; }
    chunks.push({ hex: bytes.subarray(at).toString('hex') });
    // A lone invalid byte inside a JSON string decodes to U+FFFD.
    chunks.push({ hex: utf8Hex('data: {"id":"chatcmpl-fixture","choices":[{"index":0,"delta":{"content":"x') + 'ff' + utf8Hex('y"}}]}\n\n') });
    chunks.push(sse(finishing('stop')), done);
    return chunks;
  })() }] },
  { name: 'reasoning-progressive', replies: [{ chunks: [
    sse(chunk({ role: 'assistant', reasoning_content: 'Think' })),
    sse(chunk({ reasoning_content: 'Think 一下' })),
    sse(chunk({ reasoning: 'Think 一下', content: 'Answer' })),
    sse(finishing('stop')),
    done,
  ] }] },
  { name: 'tool-calls-interleaved', replies: [{ chunks: [
    sse(chunk({ role: 'assistant', tool_calls: [
      { index: 0, id: 'call_a', type: 'function', function: { name: 'lookup', arguments: '{"q":' } },
      { index: 1, id: 'call_b', type: 'function', function: { name: 'calc', arguments: '{"n":1' } },
    ] })),
    sse(chunk({ tool_calls: [
      { index: 1, function: { name: '', arguments: '}' } },
      { index: 0, function: { arguments: '"天","n":[1,2.5e3]}' } },
    ] })),
    sse(finishing('tool_calls', { usage: { prompt_tokens: 9, completion_tokens: 4, total_tokens: 13 } })),
    done,
  ] }] },
  { name: 'tool-call-id-fallback', replies: [{ chunks: [
    sse(chunk({ tool_calls: [
      { index: 0, function: { name: 'lookup', arguments: '{"q":"a"}' } },
      { index: 1, id: 'call_dup', function: { name: 'calc', arguments: '{"n":2}' } },
    ] })),
    sse(chunk({ tool_calls: [
      { index: 2, id: 'call_dup', function: { name: 'lookup', arguments: '{"q":"b"}' } },
    ] })),
    sse(finishing('tool_calls')),
    done,
  ] }] },
  { name: 'finish-reasons', replies: [{ chunks: [
    sse(chunk({ role: 'assistant', content: 'x' })),
    sse({ id: 'chatcmpl-fixture', choices: [
      { index: 0, delta: {}, finish_reason: 'stop' },
      { index: 1, delta: {}, finish_reason: 'length' },
      { index: 2, delta: {}, finish_reason: 'tool_calls' },
      { index: 3, delta: {}, finish_reason: 'function_call' },
      { index: 4, delta: {}, finish_reason: 'content_filter' },
      { index: 5, delta: {}, finish_reason: 'content_filter_results' },
      { index: 6, delta: {}, finish_reason: 'weird' },
      { index: 7, delta: {}, finish_reason: '' },
    ] }),
    done,
  ] }] },
  { name: 'usage-only-final', replies: [{ chunks: [
    sse(chunk({ role: 'assistant', content: 'partial' })),
    sse({ id: 'chatcmpl-fixture', choices: [], usage: {
      prompt_tokens: 20, completion_tokens: 3, total_tokens: 23, cost: 0.001,
    } }),
    done,
  ] }] },
  { name: 'stream-error-retry', replies: [
    { chunks: [errorEvent({ type: 'server_error', message: 'upstream boom' }), done] },
    short('after retry'),
  ] },
  { name: 'stream-error-code-fallback', replies: [
    { chunks: [errorEvent({ code: 'timeout_error', message: '' }), done] },
    short(),
  ] },
  { name: 'stream-error-fatal', replies: [{ chunks: [
    errorEvent({ type: 'invalid_request_error', message: 'messages: field required' }),
  ] }] },
  { name: 'http-429-retry-after', replies: [
    { status: 429, headers: { 'retry-after': '0.02', 'content-type': 'application/json' },
      chunks: [json({ error: { message: 'Rate limit reached for requests', type: 'rate_limit_error', code: 'rate_limit_exceeded' } })] },
    short(),
  ] },
  { name: 'http-401-no-retry', replies: [
    { status: 401, headers: { 'content-type': 'application/json' },
      chunks: [json({ error: { message: 'Incorrect API key provided: sk-x', type: 'invalid_request_error', code: 'invalid_api_key' } })] },
  ] },
  { name: 'http-500-html-exhausted', replies: Array.from({ length: 3 }, () => ({
    status: 500, headers: { 'content-type': 'text/html' },
    chunks: [{ text: '<!DOCTYPE html><html><head><title> 502 Bad Gateway </title></head><body>x</body></html>' }],
  })) },
  { name: 'mid-stream-disconnect', replies: [{ chunks: [
    sse(chunk({ role: 'assistant', content: 'cut' })), sse(chunk({ content: ' off' })),
  ], end: 'destroy' }] },
  { name: 'incomplete-continuation', replies: [
    { chunks: [sse(chunk({ role: 'assistant', content: 'Half' }))] },
    short(' rest'),
  ] },
  { name: 'tool-call-invalid-json-length', provider: { retry: { streamMaxRetries: 0 } }, replies: [{ chunks: [
    sse(chunk({ tool_calls: [{ index: 0, id: 'call_bad', function: { name: 'lookup', arguments: '{{' } }] })),
    sse(finishing('length')),
  ] }] },
  { name: 'tool-call-jsonrepair', replies: [{ chunks: [
    // Trailing comma: JSON.parse fails, jsonrepair succeeds, wasRepaired=true.
    sse(chunk({ tool_calls: [{ index: 0, id: 'call_repair', function: { name: 'lookup', arguments: '{"q":"x",}' } }] })),
    sse(finishing('tool_calls')),
    done,
  ] }] },
  { name: 'cancel', abortAfterTextDeltas: 1, replies: [{ chunks: [
    sse(chunk({ role: 'assistant', content: 'first' })),
  ], end: 'hold' }] },
  { name: 'idle-timeout', streamTimeoutMs: 80, provider: { retry: { streamMaxRetries: 0 } }, replies: [
    { chunks: [sse(chunk({ role: 'assistant', content: 'stall' }))], end: 'hold' },
  ] },
  { name: 'endpoint-fallback', url: '$MOCK', replies: [
    { status: 404, chunks: [{ text: 'not here' }] },
    short('fallback'),
  ] },
  { name: 'request-tool-pairing', replies: [short('paired')], request: { messages: [
    { role: 'assistant', content: [
      { type: 'tool_call', name: 'lookup', input: { q: 1 } },
      { type: 'tool_call', id: 'dup', name: 'calc', input: {} },
      { type: 'tool_call', id: 'dup', name: 'lookup', input: {} },
    ] },
    { role: 'user', content: [
      { type: 'tool_result', toolCallId: 'dup', content: [{ type: 'text', text: 'ok-dup' }] },
      { type: 'tool_result_reference', toolCallId: 'dup_2', hasMore: true, preview: 'preview',
        path: '/tmp/ref', originalBytes: 12 },
    ] },
    { role: 'user', content: [{ type: 'text', text: 'middle' }] },
    { role: 'user', content: [
      { type: 'tool_result', toolCallId: 'call_0_0', content: [{ type: 'text', text: 'orphan' }] },
    ] },
  ] } },
  { name: 'request-validation', request: { provider: 'missing-provider' }, replies: [] },
  { name: 'connection-refused', target: 'closed', replies: [] },
];

const request = {
  provider: 'openai-fixture', model: 'gpt-5.4', systemPrompt: 'You are terse.',
  messages: [
    { role: 'user', content: [{ type: 'text', text: 'Say hi 你好' }] },
    { role: 'assistant', content: [
      { type: 'thinking', text: 'prior', reasoningContent: 'prior-reasoning' },
      { type: 'text', text: 'Hi' },
      { type: 'tool_call', id: 'call_a', name: 'lookup', input: { q: 'x' } },
    ] },
    { role: 'user', content: [
      { type: 'tool_result', toolCallId: 'call_a', content: [
        { type: 'text', text: 'result-a' },
        { type: 'image', mimeType: 'image/png', data: 'iVBORw0=', source: 'base64' },
      ] },
      { type: 'text', text: 'continue' },
    ] },
  ],
  tools: [
    { name: 'lookup', description: 'Look up', inputSchema: { type: 'object', properties: { q: { type: 'string' } }, required: ['q'] } },
    { name: 'calc', description: 'Calculate', inputSchema: { type: 'array' } },
  ],
  toolChoice: { name: 'lookup' }, maxOutputTokens: 256, metadata: { user_id: 'u-1', n: 2 },
  outputSchema: { name: 'Answer', description: 'Final answer', strict: false,
    schema: { type: 'object', properties: { text: { const: 'x' } } } },
  thinking: { mode: 'high' }, speed: 0.8,
};
const provider = {
  id: 'openai-fixture', protocol: 'openai', url: '$MOCK/v1', apiKey: '  sk-fixture  ',
  headers: { 'x-fixture': '1' }, extraBody: { temperature: 0.2, top_k: 5 },
  speedMapping: 'openai_service_tier', retry: { baseDelayMs: 2, maxDelayMs: 30, streamMaxRetries: 2 },
  models: { 'gpt-5.4': { id: 'gpt-5.4',
    thinking: { state: 'enabled', efforts: ['high'], format: 'provider' },
    capabilities: { supportsStreaming: true, supportsSystemPrompt: true, supportsToolUse: true, supportsSpeed: true, maxOutputTokens: 32768 },
    multimodal: { input: ['text', 'image'] } } },
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
  }
  // Let a held reply observe the client close before reading its record.
  for (let i = 0; i < 400 && spec.replies.some((r) => r.end === 'hold') && current.requests.some((r) => !('clientClosed' in r)); i++) {
    await pause(10);
  }
  assert.equal(current.requests.length, spec.replies.length, `${spec.name}: every scripted reply is consumed exactly once`);
  requestCount += current.requests.length;
  cases.push(scrub({ name: spec.name, target, url: spec.url, request: spec.request, provider: spec.provider,
    replies: spec.replies, abortAfterTextDeltas: spec.abortAfterTextDeltas, streamTimeoutMs: spec.streamTimeoutMs,
    requests: current.requests, timeline }));
}
http.close();
http.closeAllConnections?.();

const oracle = {
  sourceCommit, sourceSha256, node: process.version, v8: process.versions.v8, locale, random: 0.5,
  transportHeaders: TRANSPORT_HEADERS, request, provider, cases,
};
assert.equal(cases.length, 23);
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 1) + '\n');
console.log(JSON.stringify({ output: join(output, 'oracle.json'), cases: cases.length, requests: requestCount }));
