// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
// Real TS `streamModel` (google protocol, @google/genai SDK) against an owned
// loopback HTTP mock. No external traffic.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync, writeFileSync } from 'node:fs';
import { createServer as createHttpServer, STATUS_CODES, type IncomingMessage, type ServerResponse } from 'node:http';
import { createServer as createNetServer } from 'node:net';
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
// Native retry jitter uses Math.random; pin it so delays replay.
Math.random = () => 0.5;
const { streamModel } = await import(pathToFileURL(join(baseline, ENTRY)).href);

// Fetch/undici transport headers, not provider projection. Excluded on both sides.
const TRANSPORT_HEADERS = ['host', 'connection', 'content-length', 'user-agent', 'accept',
  'accept-language', 'sec-fetch-mode', 'accept-encoding', 'transfer-encoding', 'keep-alive'];
type Chunk = { text?: string; hex?: string };
type Reply = { status?: number; statusText?: string; headers?: Record<string, string>; chunks?: Chunk[]; end?: 'end' | 'destroy' | 'hold' };
type Spec = {
  name: string; target?: 'mock' | 'closed'; url?: string;
  request?: Record<string, unknown>; provider?: Record<string, unknown>; replies: Reply[];
  abortAfterTextDeltas?: number; streamTimeoutMs?: number;
};

const sse = (value: unknown) => ({ text: `data: ${JSON.stringify(value)}\n\n` });
const textChunk = (text: string, finishReason?: string) => sse({
  candidates: [{ content: { role: 'model', parts: [{ text }] }, ...(finishReason ? { finishReason } : {}) }],
});
const finishChunk = (finishReason: string) => sse({ candidates: [{ finishReason }] });
const usageChunk = (usageMetadata: Record<string, unknown>) => sse({ usageMetadata });
const short = (text = 'ok') => ({ chunks: [textChunk(text), finishChunk('STOP')] });
const jsonReply = (status: number, value: unknown): Reply => ({
  status, headers: { 'content-type': 'application/json' }, chunks: [{ text: JSON.stringify(value) }],
});
const htmlReply = (status: number, html: string): Reply => ({
  status, headers: { 'content-type': 'text/html' }, chunks: [{ text: html }],
});

const weirdSchema: Record<string, unknown> = {
  $defs: {
    Item: {
      type: 'object',
      properties: { id: { type: 'integer' }, tag: { anyOf: [{ const: 'a' }, { const: 'b' }] } },
      required: ['id', 'gone'],
    },
  },
  title: 'Root',
  description: 'root description',
  type: ['object', 'null'],
  properties: {
    answer: { type: ['string', 'null'], minLength: 2, maxLength: 99, description: 'answer', default: 'x' },
    mode: { anyOf: [{ const: 'a' }, { const: 'b' }, { type: 'null' }] },
    item: { $ref: '#/$defs/Item' },
    list: { type: 'array', minItems: 1 },
    nested: { type: 'object', properties: { 'bad-name': { type: 'string' }, good_name: { type: 'string' } }, additionalProperties: false },
    scalar: { const: 7 },
    any: {},
  },
  required: ['answer', 'missing', 'bad-name', 'mode'],
  additionalProperties: false,
  patternProperties: { '^x': { type: 'string' } },
  examples: [{ answer: 'x' }],
  $schema: 'https://json-schema.org/draft/2020-12/schema',
};

const specs: Spec[] = [
  { name: 'text', replies: [{ chunks: [
    sse({ responseId: 'resp 1/2', candidates: [{ content: { role: 'model', parts: [{ text: 'Hello' }] } }],
      usageMetadata: { promptTokenCount: 5, candidatesTokenCount: 2, totalTokenCount: 7 } }),
    sse({ candidates: [{ content: { role: 'model', parts: [{ text: ' 世界' }] }, finishReason: 'STOP' }],
      usageMetadata: { promptTokenCount: 5, candidatesTokenCount: 4, totalTokenCount: 9 } }),
  ] }] },
  { name: 'multimodal-contents', request: { messages: [
    { role: 'user', content: [
      { type: 'text', text: 'look' },
      { type: 'image', source: 'base64', data: 'QUJD', mimeType: 'image/png' },
      { type: 'image', source: 'url', data: 'https://example.invalid/a.png', mimeType: 'image/png' },
      { type: 'pdf', source: 'base64', data: 'UERG', mimeType: 'application/pdf', bytes: 3, pages: 1 },
      { type: 'audio', source: 'url', data: 'https://example.invalid/a.ogg', mimeType: 'audio/ogg' },
    ] },
    { role: 'assistant', content: [
      { type: 'tool_call', id: 'call 1', name: 'lookup', input: { q: 'prior', n: 1 } },
      { type: 'text', text: 'calling' },
    ] },
    { role: 'user', content: [
      { type: 'tool_result', toolCallId: 'call 1', content: [
        { type: 'text', text: 'stdout' },
        { type: 'image', source: 'base64', data: 'QUJD', mimeType: 'image/png' },
      ] },
      { type: 'tool_result', toolCallId: 'call 1', isError: true, content: [] },
      { type: 'tool_result_reference', toolCallId: 'call 1', path: '/tmp/x', originalBytes: 10, preview: 'preview', hasMore: false },
      { type: 'media_reference', path: '/tmp/y', originalBytes: 4, preview: '[media]', hasMore: false, mimeType: 'image/png', mediaType: 'image' },
    ] },
  ] }, replies: [short('seen')] },
  // Review round 1 P1-3: lone surrogates in tool_result text and in the
  // tool_result_reference preview/path must survive to the request bytes
  // (JSON.stringify escapes them as \ud800; no U+FFFD substitution).
  { name: 'tool-result-lone-surrogate', request: { messages: [
    { role: 'user', content: [{ type: 'text', text: 'go' }] },
    { role: 'assistant', content: [
      { type: 'tool_call', id: 'call 1', name: 'lookup', input: { q: 'x' } },
    ] },
    { role: 'user', content: [
      { type: 'tool_result', toolCallId: 'call 1', content: [
        { type: 'text', text: 'r\ud800' },
        { type: 'text', text: 's' },
      ] },
      { type: 'tool_result_reference', toolCallId: 'call 1', path: '/tmp/\ud800',
        originalBytes: 12, preview: 'p\ud800', hasMore: true },
    ] },
  ] }, replies: [short()] },
  { name: 'multi-tool-calls', replies: [{ chunks: [
    sse({ responseId: 'resp/one', candidates: [{ content: { role: 'model', parts: [
      { functionCall: { id: 'a b', name: 'lookup', args: { q: 'x' } } },
      { functionCall: { id: 'a-b', name: 'lookup', args: { q: 'y' } } },
      { functionCall: { name: 'lookup', args: [1, 2] } },
    ] } }] }),
    sse({ candidates: [{ content: { parts: [
      { functionCall: { id: 'a_b', name: 'lookup', args: { q: 'z' } } },
    ] }, finishReason: 'STOP' }] }),
  ] }] },
  { name: 'thinking-delta', replies: [{ chunks: [
    sse({ candidates: [{ content: { role: 'model', parts: [
      { text: 'Think 一下', thought: true, thoughtSignature: 'sig==' },
      { text: '' },
      { text: 'answer' },
    ] }, finishReason: 'STOP' }] }),
  ] }] },
  { name: 'usage-only', replies: [{ chunks: [
    usageChunk({ promptTokenCount: 10, cachedContentTokenCount: 4, candidatesTokenCount: 3, thoughtsTokenCount: 2 }),
    sse({ candidates: [{ finishReason: 'STOP' }] }),
  ] }] },
  { name: 'finish-max-tokens', replies: [{ chunks: [
    textChunk('cut'),
    finishChunk('MAX_TOKENS'),
  ] }] },
  { name: 'finish-safety', replies: [{ chunks: [
    sse({ candidates: [{ content: { parts: [{ text: 'partial' }] }, finishReason: 'SAFETY',
      safetyRatings: [{ category: 'HARM_CATEGORY_HARASSMENT', probability: 'HIGH' }] }] }),
  ] }] },
  { name: 'finish-malformed', replies: [{ chunks: [finishChunk('MALFORMED_FUNCTION_CALL')] }] },
  { name: 'finish-unknown', replies: [{ chunks: [finishChunk('FUTURE_REASON')] }] },
  { name: 'schema-cleanup', request: {
    outputSchema: { name: 'answer', schema: weirdSchema },
    tools: [{ name: 'lookup', description: 'Look up', inputSchema: weirdSchema }],
  }, replies: [short()] },
  { name: 'extra-body', provider: { extraBody: {
    generationConfig: { topP: 0.9, temperature: 0.3 },
    safetySettings: [{ category: 'HARM_CATEGORY_HARASSMENT', threshold: 'BLOCK_NONE' }],
    topLevel: { a: 1 },
    temperature: 0.1,
  } }, replies: [short()] },
  { name: 'endpoint-v1beta', url: '$MOCK/v1beta', replies: [short()] },
  { name: 'endpoint-path', url: '$MOCK/api', replies: [short()] },
  { name: 'tool-choice-forced', request: { toolChoice: { name: 'lookup' } }, replies: [short()] },
  { name: 'thinking-unsupported', request: { thinking: { mode: 'xhigh' } }, replies: [] },
  { name: 'error-json-401', request: { thinking: { mode: 'default' } }, replies: [
    jsonReply(401, { error: { code: 401, message: 'invalid api key', status: 'UNAUTHENTICATED' } }),
  ] },
  // Review round 1 P1-1: the SDK's synthesized error body uses the server's
  // actual HTTP statusText (response.statusText), not the canonical reason.
  { name: 'error-custom-status-text', provider: { retry: { streamMaxRetries: 0 } }, replies: [
    { status: 502, statusText: 'Custom Bad', headers: { 'content-type': 'text/plain' },
      chunks: [{ text: 'upstream broke' }] },
  ] },
  // Review round 1 P1-2: throwErrorIfNotOK tests the content-type value
  // case-sensitively; `Application/JSON` is not the JSON branch.
  { name: 'error-content-type-case', provider: { retry: { streamMaxRetries: 0 } }, replies: [
    { status: 400, headers: { 'content-type': 'Application/JSON' },
      chunks: [{ text: '{"error":{"code":400,"message":"upper case","status":"INVALID_ARGUMENT"}}' }] },
  ] },
  { name: 'http-429-retry', replies: [
    jsonReply(429, { error: { code: 429, message: 'quota exceeded', status: 'RESOURCE_EXHAUSTED' } }),
    short('after retry'),
  ] },
  { name: 'http-500-html-exhausted', provider: { retry: { streamMaxRetries: 2 } }, replies: [
    htmlReply(500, '<!DOCTYPE html><html><head><title> 502 Bad Gateway </title></head><body>x</body></html>'),
    htmlReply(500, '<!DOCTYPE html><html><head><title> 502 Bad Gateway </title></head><body>x</body></html>'),
    htmlReply(500, '<!DOCTYPE html><html><head><title> 502 Bad Gateway </title></head><body>x</body></html>'),
  ] },
  { name: 'connection-cut', provider: { retry: { streamMaxRetries: 0 } }, replies: [
    { chunks: [textChunk('cut')], end: 'destroy' },
  ] },
  { name: 'empty-stream', provider: { retry: { streamMaxRetries: 0 } }, replies: [
    { chunks: [sse({ responseId: 'r0' })] },
  ] },
  { name: 'cancel', abortAfterTextDeltas: 1, replies: [
    { chunks: [textChunk('first')], end: 'hold' },
  ] },
  { name: 'request-validation', request: { provider: 'missing-provider' }, replies: [] },
  { name: 'connection-refused', target: 'closed', provider: { retry: { streamMaxRetries: 0 } }, replies: [] },
];

// The base request exercises the full canonical surface: thinking, tools,
// tool config, structured output, text and tool call/result history.
const request = {
  provider: 'google-fixture', model: 'gemini-3-pro', systemPrompt: 'You are terse.',
  messages: [
    { role: 'user', content: [{ type: 'text', text: 'Say hi 你好' }] },
    { role: 'assistant', content: [
      { type: 'thinking', text: 'prior', signature: 'old-sig' },
      { type: 'text', text: 'Hi' },
      { type: 'tool_call', id: 'call 1', name: 'lookup', input: { q: 'prior' } },
    ] },
    { role: 'user', content: [
      { type: 'tool_result', toolCallId: 'call 1', content: [{ type: 'text', text: 'result' }] },
    ] },
  ],
  tools: [{ name: 'lookup', description: 'Look up', inputSchema: { type: 'object', properties: { q: { type: 'string' } }, required: ['q'] } }],
  toolChoice: 'auto', maxOutputTokens: 256, thinking: { mode: 'high' },
  outputSchema: { name: 'answer', schema: { type: 'object', properties: { answer: { type: 'string' } }, required: ['answer'] } },
};
const provider = {
  id: 'google-fixture', protocol: 'google', url: '$MOCK', apiKey: '  sk-fixture  ',
  headers: { 'x-fixture': '1' }, extraBody: { generationConfig: { topP: 0.5 } },
  retry: { baseDelayMs: 2, maxDelayMs: 30, streamMaxRetries: 2 },
  models: { 'gemini-3-pro': {
    id: 'gemini-3-pro',
    thinking: { state: 'enabled', efforts: ['low', 'medium', 'high'], format: 'provider' },
    capabilities: { supportsStreaming: true, supportsSystemPrompt: true, supportsToolUse: true, maxOutputTokens: 32768 },
    multimodal: { input: ['text', 'image', 'audio', 'pdf'] },
  } },
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
    const closed = new Promise<boolean>((done) => socket.once('close', () => done(true)));
    socket.setNoDelay(true);
    const status = reply.status ?? 200;
    res.writeHead(status, reply.statusText ?? STATUS_CODES[status], reply.headers ?? { 'content-type': 'text/event-stream' });
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
// The SDK adds `sdkHttpResponse` (response headers, including a clock-derived
// date) to every chunk. That is transport metadata, not the provider protocol
// projection, so it is excluded on both sides like `transportHeaders`.
function stripSdkResponse(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stripSdkResponse);
  if (value && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [key, entry] of Object.entries(value)) {
      if (key === 'sdkHttpResponse') continue;
      out[key] = stripSdkResponse(entry);
    }
    return out;
  }
  return value;
}
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
      timeline.push({ event: stripSdkResponse(event) });
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
  sourceCommit, sourceSha256, node: process.version, v8: process.versions.v8,
  locale, random: 0.5, transportHeaders: TRANSPORT_HEADERS, request, provider, cases,
};
assert.equal(cases.length, 26);
assert.equal(requestCount, 26);
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 1) + '\n');
console.log(JSON.stringify({ output: join(output, 'oracle.json'), cases: cases.length, requests: requestCount }));
