// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
// Real TS `streamModel` (anthropic) against an owned loopback HTTP/HTTPS mock.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createServer as createHttpServer, type IncomingMessage, type ServerResponse } from 'node:http';
import { createServer as createHttpsServer } from 'node:https';
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
  name: string; target?: 'mock' | 'closed' | 'tls-trusted' | 'tls-untrusted'; url?: string;
  request?: Record<string, unknown>; provider?: Record<string, unknown>; replies: Reply[];
  abortAfterTextDeltas?: number; streamTimeoutMs?: number; bodyTimeoutMs?: number;
};

const sse = (event: Record<string, unknown>, name = String(event.type)) =>
  ({ text: `event: ${name}\ndata: ${JSON.stringify(event)}\n\n` });
const messageStart = sse({ type: 'message_start', message: { id: 'msg_1', type: 'message', role: 'assistant', content: [],
  model: 'claude-sonnet-4-6', usage: { input_tokens: 11, output_tokens: 1 } } });
const textStart = (index = 0) => sse({ type: 'content_block_start', index, content_block: { type: 'text', text: '' } });
const textDelta = (text: string, index = 0) => sse({ type: 'content_block_delta', index, delta: { type: 'text_delta', text } });
const blockStop = (index = 0) => sse({ type: 'content_block_stop', index });
const end = (stop = 'end_turn') => [
  sse({ type: 'message_delta', delta: { stop_reason: stop, stop_sequence: null }, usage: { output_tokens: 7 } }),
  sse({ type: 'message_stop' }),
];
const toolStart = (index: number, id: string) =>
  sse({ type: 'content_block_start', index, content_block: { type: 'tool_use', id, name: 'lookup', input: {} } });
const toolDelta = (index: number, partial_json: string) =>
  sse({ type: 'content_block_delta', index, delta: { type: 'input_json_delta', partial_json } });
const short = (text = 'ok'): Reply => ({ chunks: [messageStart, textStart(), textDelta(text), blockStop(), ...end()] });
const errorEvent = (type: string, message: string) => sse({ type: 'error', error: { type, message } });
const utf8Hex = (text: string) => Buffer.from(text, 'utf8').toString('hex');
const json = (value: unknown) => ({ text: JSON.stringify(value) });

const specs: Spec[] = [
  { name: 'normal-stream', replies: [{ chunks: [
    { text: ': ping\n\n' }, messageStart,
    sse({ type: 'content_block_start', index: 0, content_block: { type: 'thinking', thinking: '' } }),
    sse({ type: 'content_block_delta', index: 0, delta: { type: 'thinking_delta', thinking: 'Think 一下' } }),
    sse({ type: 'content_block_delta', index: 0, delta: { type: 'signature_delta', signature: 'sig==' } }),
    blockStop(0), textStart(1), textDelta('Hello', 1),
    { text: `id: 7\nevent: content_block_delta\ndata:   ${JSON.stringify({ type: 'content_block_delta', index: 1, delta: { type: 'text_delta', text: ' 世界' } })}  \nretry: 10\n\n` },
    blockStop(1),
    sse({ type: 'content_block_start', index: 2, content_block: { type: 'tool_use', id: 'toolu_1', name: 'lookup', input: {} } }),
    sse({ type: 'content_block_delta', index: 2, delta: { type: 'input_json_delta', partial_json: '{"q":"天' } }),
    sse({ type: 'content_block_delta', index: 2, delta: { type: 'input_json_delta', partial_json: '气","n":[1,2.5e3]}' } }),
    blockStop(2),
    sse({ type: 'message_delta', delta: { stop_reason: 'tool_use' }, usage: { input_tokens: 11, output_tokens: 30, cache_read_input_tokens: 5, cache_creation_input_tokens: 2 } }),
    sse({ type: 'message_stop' }), { text: 'data: [DONE]\n\n' },
  ] }] },
  { name: 'utf8-boundaries', replies: [{ chunks: (() => {
    const body = `﻿${messageStart.text}${textStart().text}`
      + `event: content_block_delta\r\ndata: ${JSON.stringify({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'crlf' } })}\r\n\r\n`
      + `${textDelta('中文😀é').text}${textDelta('tail').text}`;
    const bytes = Buffer.from(body, 'utf8');
    const cuts = [1, 2, bytes.indexOf(Buffer.from('中')) + 1, bytes.indexOf(Buffer.from('中')) + 4,
      bytes.indexOf(Buffer.from('😀')) + 2, bytes.indexOf(Buffer.from('😀')) + 3, bytes.indexOf(Buffer.from('é')) + 1];
    const chunks: Chunk[] = [];
    let at = 0;
    for (const cut of cuts) { chunks.push({ hex: bytes.subarray(at, cut).toString('hex') }); at = cut; }
    chunks.push({ hex: bytes.subarray(at).toString('hex') });
    // A lone invalid byte inside a JSON string decodes to U+FFFD.
    chunks.push({ hex: utf8Hex('event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x') + 'ff' + utf8Hex('y"}}\n\n') });
    chunks.push(blockStop(), ...end());
    return chunks;
  })() }] },
  { name: 'stream-error-retry', replies: [{ chunks: [messageStart, errorEvent('overloaded_error', 'Overloaded')] }, short('after retry')] },
  { name: 'stream-error-continuation', replies: [
    { chunks: [messageStart, textStart(), textDelta('Partial answer'), errorEvent('api_error', 'Internal error, please try again in 5ms')] },
    short(' continued')] },
  { name: 'stream-error-fatal', replies: [{ chunks: [messageStart, errorEvent('invalid_request_error', 'messages: field required')] }] },
  { name: 'http-429-retry-after', replies: [
    { status: 429, headers: { 'retry-after': '0.02', 'content-type': 'application/json' },
      chunks: [json({ type: 'error', error: { type: 'rate_limit_error', message: 'Number of request tokens has exceeded your per-minute rate limit' } })] },
    short()] },
  { name: 'http-529-overloaded', replies: [
    { status: 529, headers: { 'content-type': 'application/json' }, chunks: [json({ type: 'error', error: { type: 'overloaded_error', message: 'Overloaded' } })] },
    short()] },
  { name: 'http-500-exhausted', replies: Array.from({ length: 3 }, () => ({ status: 500, headers: { 'content-type': 'text/html' },
    chunks: [{ text: '<!DOCTYPE html><html><head><title> 502 Bad Gateway </title></head><body>x</body></html>' }] })) },
  { name: 'http-401-no-retry', replies: [{ status: 401, headers: { 'content-type': 'application/json' },
    chunks: [json({ type: 'error', error: { type: 'authentication_error', message: 'invalid x-api-key' } })] }] },
  { name: 'connection-interrupted', replies: [{ chunks: [messageStart, textStart(), textDelta('cut')], end: 'destroy' }] },
  { name: 'incomplete-continuation', replies: [{ chunks: [messageStart, textStart(), textDelta('Half')] }, short(' rest')] },
  { name: 'incomplete-tool-interruption', replies: [{ chunks: [messageStart,
    sse({ type: 'content_block_start', index: 0, content_block: { type: 'tool_use', id: 'toolu_x', name: 'lookup', input: {} } }),
    sse({ type: 'content_block_delta', index: 0, delta: { type: 'input_json_delta', partial_json: '{"q":"ab' } })] }] },
  { name: 'cancel', abortAfterTextDeltas: 1, replies: [{ chunks: [messageStart, textStart(), textDelta('first')], end: 'hold' }] },
  { name: 'idle-timeout', streamTimeoutMs: 80, provider: { retry: { streamMaxRetries: 0 } },
    replies: [{ chunks: [messageStart], end: 'hold' }] },
  { name: 'invalid-sse-json', replies: [{ chunks: [messageStart, { text: 'event: x\ndata: not json\n\n' }, ...end()] }] },
  { name: 'connection-refused', target: 'closed', replies: [] },
  { name: 'endpoint-fallback', url: '$MOCK', replies: [{ status: 404, chunks: [{ text: 'not here' }] }, short('fallback')] },
  { name: 'request-validation', request: { provider: 'missing-provider' }, replies: [] },
  { name: 'tls-trusted', target: 'tls-trusted', replies: [short('secure')] },
  { name: 'tls-untrusted', target: 'tls-untrusted', provider: { retry: { streamMaxRetries: 0 } }, replies: [] },
  // Repair round 1: jsonrepair and deferred tool-argument failures in a stream.
  { name: 'tool-args-repaired', replies: [{ chunks: [messageStart, toolStart(0, 'toolu_r'),
    toolDelta(0, "{q: 'ab', n: [1 2,],"), blockStop(0), ...end('tool_use')] }] },
  { name: 'tool-args-truncated-max-tokens', replies: [{ chunks: [messageStart, toolStart(0, 'toolu_t'),
    toolDelta(0, '{"q":"ab'), toolDelta(0, 'c"} x'), blockStop(0), ...end('max_tokens')] }] },
  { name: 'tool-args-invalid-message-stop', replies: [{ chunks: [messageStart, toolStart(0, 'toolu_i'),
    toolDelta(0, '{:1}'), blockStop(0), sse({ type: 'message_stop' })] }] },
  // Error body: headers flushed, half a body, then silence until bodyTimeout.
  { name: 'error-body-stall', bodyTimeoutMs: 150, replies: [{ status: 500, headers: { 'content-type': 'application/json' },
    chunks: [{ text: '{"type":"error","error":{"type":"api_error",' }], end: 'hold' }] },
  // 204: fetch exposes a null body -> "Missing response body." error event.
  { name: 'no-content', replies: [{ status: 204, headers: {} }] },
];

const request = {
  provider: 'anthropic-fixture', model: 'claude-sonnet-4-6', systemPrompt: 'You are terse.',
  messages: [
    { role: 'user', content: [{ type: 'text', text: 'Say hi 你好' }] },
    { role: 'assistant', content: [{ type: 'thinking', text: 'prior', signature: 'old-sig' }, { type: 'text', text: 'Hi' }] },
    { role: 'user', content: [{ type: 'text', text: 'Look it up' }] },
  ],
  tools: [{ name: 'lookup', description: 'Look up', inputSchema: { type: 'object', properties: { q: { type: 'string' } } } }],
  toolChoice: 'auto', maxOutputTokens: 256, metadata: { user_id: 'u-1', trace: 't' },
  cachePlan: { system: true, tools: true, messages: [0, 1, 2] }, thinking: { mode: 'high' }, speed: 0.8,
};
const provider = {
  id: 'anthropic-fixture', protocol: 'anthropic', url: '$MOCK/v1', apiKey: '  sk-fixture  ',
  headers: { 'x-fixture': '1', 'anthropic-beta': 'tools-2024' }, extraBody: { temperature: 0.2, top_k: 5 },
  speedMapping: 'anthropic_speed', retry: { baseDelayMs: 2, maxDelayMs: 30, streamMaxRetries: 2 },
  models: { 'claude-sonnet-4-6': { id: 'claude-sonnet-4-6',
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

// Throwaway test-only CA + leaf (`tls.json`, shared with the Rust replay).
// Created once with openssl in an owned temp dir; reused afterwards so both
// generator runs are byte-identical. Nothing about the certs enters oracle.json.
const tlsPath = new URL('./tls.json', import.meta.url);
if (!existsSync(tlsPath)) {
  const certDir = mkdtempSync(join(tmpdir(), 'pdrs-model-stream-tls-'));
  try {
    const openssl = (...args: string[]) => execFileSync('openssl', args, { cwd: certDir, stdio: 'pipe' });
    openssl('req', '-x509', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes', '-keyout', 'ca.key',
      '-out', 'ca.pem', '-days', '36500', '-subj', '/CN=pdrs model-stream test CA');
    openssl('req', '-newkey', 'ec', '-pkeyopt', 'ec_paramgen_curve:P-256', '-nodes', '-keyout', 'leaf.key',
      '-out', 'leaf.csr', '-subj', '/CN=127.0.0.1');
    writeFileSync(join(certDir, 'ext.cnf'), 'subjectAltName=IP:127.0.0.1\nbasicConstraints=critical,CA:FALSE\n');
    openssl('x509', '-req', '-in', 'leaf.csr', '-CA', 'ca.pem', '-CAkey', 'ca.key', '-CAcreateserial', '-out', 'leaf.pem',
      '-days', '36500', '-extfile', 'ext.cnf');
    const read = (name: string) => readFileSync(join(certDir, name), 'utf8');
    writeFileSync(tlsPath, JSON.stringify({ note: 'test-only throwaway CA/leaf for 127.0.0.1; not a credential',
      ca: read('ca.pem'), cert: read('leaf.pem'), key: read('leaf.key') }, null, 1) + '\n');
  } finally {
    rmSync(certDir, { recursive: true, force: true });
  }
}
const material = JSON.parse(readFileSync(tlsPath, 'utf8'));
const caPem = Buffer.from(material.ca);
const tls = createHttpsServer({ key: material.key, cert: material.cert }, handler);
await new Promise<void>((r) => tls.listen(0, '127.0.0.1', r));
const TLS = `https://127.0.0.1:${(tls.address() as any).port}`;

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
  const origin = target === 'closed' ? CLOSED : target === 'mock' ? MOCK : TLS;
  origins.length = 0;
  origins.push([origin, target === 'closed' ? '$CLOSED' : target === 'mock' ? '$MOCK' : '$TLS']);
  current = { replies: spec.replies, requests: [], origin };
  const caseProvider = { ...provider, ...(spec.provider ?? {}),
    retry: { ...provider.retry, ...((spec.provider?.retry as object) ?? {}) },
    url: (spec.url ?? provider.url).replace('$MOCK', origin) };
  const config = { providers: { [provider.id]: caseProvider } };
  const caseRequest = { ...request, ...(spec.request ?? {}) };
  const timeline: unknown[] = [];
  const controller = new AbortController();
  const previous = undici.getGlobalDispatcher();
  if (target === 'tls-trusted') undici.setGlobalDispatcher(new undici.Agent({ connect: { ca: caPem } }));
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
    if (target === 'tls-trusted' || spec.bodyTimeoutMs !== undefined) undici.setGlobalDispatcher(previous);
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
tls.close();
http.closeAllConnections?.();
tls.closeAllConnections?.();

const oracle = {
  sourceCommit, sourceSha256, node: process.version, v8: process.versions.v8, undici: JSON.parse(readFileSync(join(baseline, 'node_modules/undici/package.json'), 'utf8')).version,
  locale, random: 0.5, transportHeaders: TRANSPORT_HEADERS, request, provider, cases,
};
assert.equal(cases.length, 25);
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 1) + '\n');
console.log(JSON.stringify({ output: join(output, 'oracle.json'), cases: cases.length, requests: requestCount }));
