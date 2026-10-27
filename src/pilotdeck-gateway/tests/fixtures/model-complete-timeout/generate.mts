// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see
// README.md. Real TS `complete()` against an owned loopback HTTP server that
// either never answers, answers only after the request timer, or sends headers
// and then delays/truncates the body. This is the only fixture that drives a
// real loopback socket and wall-clock timers; every other model-complete oracle
// is event-count based.
//
// `delayMs` always calls `flushHeaders()` before sleeping. That distinction
// matters: a bare `writeHead()` does not put the headers on the wire, so a
// delay before `flushHeaders()` is really a *headers* delay and is bounded by
// the request timer. Once `flushHeaders()` has run, `networkFetch` has already
// returned and detached its parent-abort listener, so the remaining body read
// outlives both `provider.timeoutMs` and a parent abort. See the r4b probe in
// the issue's `_tmp` directory.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the output directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8');
const gitExecutable = process.env.PDRS_GIT
  || execFileSync('which', ['git'], { encoding: 'utf8' }).trim();
const sourceCommit = bindBaseline({ baseline, commit: BASELINE_COMMIT, gitExecutable }).provenance.ref;
const ENTRY = 'src/model/streaming/streamModel.ts';
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [ENTRY]);
const { complete } = await import(pathToFileURL(join(baseline, ENTRY)).href);

/**
 * Scheduling/loopback tolerance for the *upper* wall-clock bound. The expected
 * wait is the Node timer/body delay the fixture specifies; `setTimeout` never
 * fires early, so the lower bound is that delay minus 10ms of Date.now
 * granularity. 500ms comfortably covers a loaded CI host for sub-second waits
 * while still rejecting a `timeout_ms * 10` mutation at 100ms (1000ms) and
 * 300ms (3000ms). Thresholds are derived from the baseline's requested delays,
 * never from a Rust run.
 */
const SCHEDULER_SLACK_MS = 500;
/** Delay before destroying a truncated socket, so undici observes the error. */
const TRUNCATE_DESTROY_MS = 20;

type Reply = {
  status?: number;
  headers?: Record<string, string>;
  body: string;
  /** Flush headers first, then send the body after this many ms. */
  delayMs?: number;
  /** Delay before `writeHead()` at all; the request timer still bounds this. */
  delayHeadersMs?: number;
  /** Advertise a longer content-length, write the prefix, then destroy. */
  truncate?: boolean;
};

type Spec = {
  name: string;
  behavior: 'hang' | 'delayed-body' | 'abort' | 'reply';
  timeoutMs?: number;
  streamTimeoutMs?: number;
  abortAfterMs?: number;
  /** Defaults to `/v1`; `/api` yields two candidate endpoints. */
  providerUrl?: string;
  replies?: Reply[];
  /** Baseline-requested wait used to derive the upper bound. */
  expectedWaitMs: number;
  elapsedAtLeastMs: number;
};

const model = () => ({
  id: 'm',
  capabilities: { maxContextTokens: 1000, maxOutputTokens: 32 },
  multimodal: { input: ['text'] },
});
const request = {
  provider: 'p',
  model: 'm',
  messages: [{ role: 'user', content: [{ type: 'text', text: 'hi' }] }],
  maxOutputTokens: 32,
};
const delayedBody = JSON.stringify({
  id: 'chatcmpl-delayed',
  choices: [{
    index: 0,
    message: { role: 'assistant', content: 'done' },
    finish_reason: 'stop',
  }],
});
const okBody = JSON.stringify({
  id: 'chatcmpl-ok',
  choices: [{
    index: 0,
    message: { role: 'assistant', content: 'ok' },
    finish_reason: 'stop',
  }],
});
const validOpenai = (content: string) => JSON.stringify({
  choices: [{
    index: 0,
    message: { role: 'assistant', content },
    finish_reason: 'stop',
  }],
});

const specs: Spec[] = [
  {
    name: 'loopback-provider-timeout-100',
    behavior: 'hang',
    timeoutMs: 100,
    expectedWaitMs: 100,
    elapsedAtLeastMs: 50,
  },
  {
    // streamTimeoutMs=10 would fire at 10ms if complete() used the stream
    // idle resolver; the baseline uses provider.timeoutMs=300 instead.
    name: 'loopback-stream-timeout-ignored',
    behavior: 'hang',
    timeoutMs: 300,
    streamTimeoutMs: 10,
    expectedWaitMs: 300,
    elapsedAtLeastMs: 150,
  },
  {
    // No provider.timeoutMs: the 600000ms fallback is still pending at 150ms,
    // so a 10ms streamTimeoutMs must not abort complete().
    name: 'loopback-default-fallback-not-stream-timeout',
    behavior: 'abort',
    streamTimeoutMs: 10,
    abortAfterMs: 150,
    expectedWaitMs: 150,
    elapsedAtLeastMs: 100,
  },
  {
    // Single endpoint: headers arrive before provider.timeoutMs, the body
    // after it. send() returns at headers and clears the timer, so the later
    // response.text() is not capped. No shape probe runs for one endpoint.
    name: 'loopback-body-outlives-request-timeout',
    behavior: 'delayed-body',
    timeoutMs: 50,
    expectedWaitMs: 200,
    elapsedAtLeastMs: 150,
  },
  {
    // Multi-endpoint shape check, first candidate valid but slow: the clone's
    // body read starts after headers and is NOT bounded by the request timer,
    // so it succeeds even though provider.timeoutMs=100 elapsed.
    name: 'loopback-multi-first-shape-valid-slow-body',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 100,
    replies: [
      { status: 200, body: validOpenai('slow-first'), delayMs: 120 },
      { status: 200, body: okBody },
    ],
    expectedWaitMs: 120,
    elapsedAtLeastMs: 110,
  },
  {
    // Last candidate wrong shape and slow: the body is read to the end after
    // the timer, the last original Response is returned, and complete() parses
    // its raw body.
    name: 'loopback-multi-last-shape-wrong-slow-body',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 100,
    replies: [
      { status: 200, body: '{}' },
      { status: 200, body: '{"last":true}', delayMs: 120 },
    ],
    expectedWaitMs: 120,
    elapsedAtLeastMs: 110,
  },
  {
    // Last candidate valid shape and slow: same lifecycle, accepted canonical.
    name: 'loopback-multi-last-shape-valid-slow-body',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 100,
    replies: [
      { status: 200, body: '{}' },
      { status: 200, body: validOpenai('slow-last'), delayMs: 120 },
    ],
    expectedWaitMs: 120,
    elapsedAtLeastMs: 110,
  },
  {
    // First candidate wrong shape and slow: the first body is still read to
    // the end. Only then does the next fetch observe the already-aborted outer
    // signal, so it fails with the timeout and never reaches the server.
    name: 'loopback-multi-first-shape-wrong-slow-body',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 100,
    replies: [
      { status: 200, body: '{}', delayMs: 120 },
      { status: 200, body: okBody },
    ],
    expectedWaitMs: 120,
    elapsedAtLeastMs: 110,
  },
  {
    // Single endpoint non-JSON body that outlives the request timer: no shape
    // check, headers return first, and complete() keeps the raw text.
    name: 'loopback-single-endpoint-slow-non-json',
    behavior: 'reply',
    providerUrl: '/v1',
    timeoutMs: 100,
    replies: [
      { status: 200, headers: { 'content-type': 'text/html' }, body: '<html>oops</html>', delayMs: 120 },
    ],
    expectedWaitMs: 120,
    elapsedAtLeastMs: 110,
  },
  {
    // Control for the flushHeaders distinction: delaying headers themselves
    // keeps the request timer armed, so the first fetch times out and no
    // second candidate is attempted.
    name: 'loopback-delay-headers-beyond-timeout',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 100,
    replies: [
      { status: 200, body: okBody, delayHeadersMs: 200 },
      { status: 200, body: okBody },
    ],
    expectedWaitMs: 100,
    elapsedAtLeastMs: 90,
  },
  {
    // Parent abort after headers, first candidate valid slow: the abort cannot
    // reach the detached body reader, so the shape probe succeeds.
    name: 'loopback-parent-abort-after-headers-shape-valid',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 1000,
    abortAfterMs: 50,
    replies: [
      { status: 200, body: validOpenai('after-abort'), delayMs: 120 },
      { status: 200, body: okBody },
    ],
    expectedWaitMs: 120,
    elapsedAtLeastMs: 110,
  },
  {
    // Parent abort after headers, first candidate wrong slow: the body still
    // finishes, then the next fetch sees the aborted signal and throws the
    // parent abort reason without a second request.
    name: 'loopback-parent-abort-after-headers-shape-wrong',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 1000,
    abortAfterMs: 50,
    replies: [
      { status: 200, body: '{}', delayMs: 120 },
      { status: 200, body: okBody },
    ],
    expectedWaitMs: 120,
    elapsedAtLeastMs: 110,
  },
  {
    // Both the parent abort (50ms) and the request timer (100ms) fire while a
    // wrong-shape body is still arriving. At body end the next fetch fails and
    // sendProviderRequest's catch sees signal.aborted, so it throws the parent
    // AbortError rather than the timeout.
    name: 'loopback-parent-abort-wins-over-timeout-shape-wrong',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 100,
    abortAfterMs: 50,
    replies: [
      { status: 200, body: '{}', delayMs: 120 },
      { status: 200, body: okBody },
    ],
    expectedWaitMs: 120,
    elapsedAtLeastMs: 110,
  },
  {
    // Reverse ordering: the request timer (100ms) fires first, then the parent
    // abort at 150ms while the body drains to 200ms. The next fetch fails with
    // the timeout, but sendProviderRequest's catch rechecks signal.aborted and
    // replaces it with the later parent AbortError.
    name: 'loopback-later-parent-abort-masks-timeout',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 100,
    abortAfterMs: 150,
    replies: [
      { status: 200, body: '{}', delayMs: 200 },
      { status: 200, body: okBody },
    ],
    expectedWaitMs: 200,
    elapsedAtLeastMs: 190,
  },
  {
    // A clone().json() read failure falls back to the next candidate.
    name: 'loopback-body-read-error-then-second',
    behavior: 'reply',
    providerUrl: '/api',
    replies: [
      { status: 200, body: '{"choices":', truncate: true },
      { status: 200, body: okBody },
    ],
    expectedWaitMs: TRUNCATE_DESTROY_MS,
    elapsedAtLeastMs: 10,
  },
  {
    // A read failure on the last candidate is returned as the last Response;
    // complete()'s own response.text() then throws TypeError: terminated.
    name: 'loopback-last-body-read-error',
    behavior: 'reply',
    providerUrl: '/api',
    replies: [
      { status: 200, body: '{}' },
      { status: 200, body: '{"choices":', truncate: true },
    ],
    expectedWaitMs: TRUNCATE_DESTROY_MS,
    elapsedAtLeastMs: 10,
  },
  {
    // Slow fallback that still finishes before the request deadline keeps the
    // ordinary two-endpoint success path.
    name: 'loopback-multi-first-shape-wrong-within-timeout',
    behavior: 'reply',
    providerUrl: '/api',
    timeoutMs: 1000,
    replies: [
      { status: 200, body: '{}', delayMs: 50 },
      { status: 200, body: okBody },
    ],
    expectedWaitMs: 50,
    elapsedAtLeastMs: 40,
  },
];

type Current = {
  behavior: Spec['behavior'];
  httpBody: string;
  replies: Reply[];
  used: Reply[];
  requests: Array<{ method: string; path: string; body: string }>;
};
let current: Current;

function sendReply(outgoing: import('node:http').ServerResponse, reply: Reply) {
  const headers: Record<string, string> = {
    'content-type': 'application/json',
    ...(reply.headers ?? {}),
  };
  if (reply.truncate) {
    headers['content-length'] = String(Buffer.byteLength(reply.body) + 10);
  }
  const write = () => {
    if (outgoing.destroyed) return;
    outgoing.writeHead(reply.status ?? 200, headers);
    if (reply.truncate) {
      outgoing.write(reply.body);
      setTimeout(() => outgoing.destroy(), TRUNCATE_DESTROY_MS);
      return;
    }
    if (reply.delayMs) {
      outgoing.flushHeaders();
      setTimeout(() => {
        if (!outgoing.destroyed) outgoing.end(reply.body);
      }, reply.delayMs);
      return;
    }
    outgoing.end(reply.body);
  };
  if (reply.delayHeadersMs) setTimeout(write, reply.delayHeadersMs);
  else write();
}

const server = createServer((incoming, outgoing) => {
  const chunks: Buffer[] = [];
  incoming.on('data', (chunk) => chunks.push(chunk));
  incoming.on('end', () => {
    current.requests.push({
      method: incoming.method,
      path: incoming.url,
      body: Buffer.concat(chunks).toString('utf8'),
    });
    if (current.behavior === 'hang' || current.behavior === 'abort') return;
    if (current.behavior === 'delayed-body') {
      const split = Math.floor(current.httpBody.length / 2);
      outgoing.writeHead(200, {
        'content-type': 'application/json',
        'content-length': String(Buffer.byteLength(current.httpBody)),
      });
      outgoing.write(current.httpBody.slice(0, split));
      setTimeout(() => outgoing.end(current.httpBody.slice(split)), 200);
      return;
    }
    const reply = current.replies[Math.min(current.used.length, current.replies.length - 1)];
    current.used.push(reply);
    sendReply(outgoing, reply);
  });
});
await new Promise<void>((ready) => server.listen(0, '127.0.0.1', ready));
const MOCK = `http://127.0.0.1:${(server.address() as any).port}`;

const thrownRecord = (error: any) => {
  const record: Record<string, unknown> = { name: error?.name, message: error?.message };
  if (error?.name === 'ModelProviderError') record.error = error.error;
  return record;
};
const scrub = (value: unknown): unknown => JSON.parse(JSON.stringify(value).split(MOCK).join('$MOCK'));

const cases = [];
const observations = [];
for (const spec of specs) {
  const replies = spec.replies ?? [];
  current = {
    behavior: spec.behavior,
    httpBody: delayedBody,
    replies,
    used: [],
    requests: [],
  };
  const providerPath = spec.providerUrl ?? '/v1';
  const provider: Record<string, unknown> = {
    id: 'p',
    protocol: 'openai',
    url: `${MOCK}${providerPath}`,
    apiKey: 'fixture-key',
    headers: {},
    retry: { requestMaxRetries: 0, baseDelayMs: 0, maxDelayMs: 0 },
    models: { m: model() },
  };
  if (spec.timeoutMs !== undefined) provider.timeoutMs = spec.timeoutMs;
  const options: Record<string, unknown> = {};
  if (spec.streamTimeoutMs !== undefined) options.streamTimeoutMs = spec.streamTimeoutMs;
  if (spec.abortAfterMs !== undefined) {
    const controller = new AbortController();
    setTimeout(() => controller.abort(), spec.abortAfterMs);
    options.signal = controller.signal;
  }
  const result: Record<string, unknown> = {
    name: spec.name,
    behavior: spec.behavior,
    request,
    provider: { ...provider, url: `http://mock.local${providerPath}` },
    streamTimeoutMs: spec.streamTimeoutMs,
    abortAfterMs: spec.abortAfterMs,
    expectedWaitMs: spec.expectedWaitMs,
    elapsedAtLeastMs: spec.elapsedAtLeastMs,
    elapsedAtMostMs: spec.expectedWaitMs + SCHEDULER_SLACK_MS,
  };
  if (spec.behavior === 'delayed-body') result.httpBody = delayedBody;
  if (spec.behavior === 'reply') {
    result.replies = replies.map((reply) => ({ ...reply, body: scrub(reply.body) }));
  }
  const started = Date.now();
  try {
    result.response = await complete(request as any, { providers: { p: provider } } as any, options as any);
  } catch (error) {
    result.thrown = thrownRecord(error);
  }
  const elapsedMs = Date.now() - started;
  result.requestCount = current.requests.length;
  result.requests = current.requests.map((entry) => ({
    method: entry.method,
    path: entry.path,
    body: scrub(entry.body),
  }));
  // Wall-clock observations stay out of the byte-compared oracle; otherwise
  // the regeneration test could never match.
  observations.push({
    name: spec.name,
    elapsedMs,
    elapsedAtLeastMs: spec.elapsedAtLeastMs,
    elapsedAtMostMs: spec.expectedWaitMs + SCHEDULER_SLACK_MS,
    requestCount: result.requestCount,
  });
  cases.push(result);
}
server.close();
server.closeAllConnections?.();

const oracle = {
  sourceCommit,
  sourceSha256,
  node: process.version,
  locale,
  schedulerSlackMs: SCHEDULER_SLACK_MS,
  cases,
};
mkdirSync(output, { recursive: true });
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 1) + '\n', { flag: 'wx' });
writeFileSync(
  join(output, 'observations.json'),
  JSON.stringify({ node: process.version, observations }, null, 1) + '\n',
  { flag: 'wx' },
);
console.log(JSON.stringify({
  output: join(output, 'oracle.json'),
  cases: cases.length,
  observations,
}));
