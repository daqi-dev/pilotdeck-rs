// Real fixed TS `streamModel.ts` retry-word oracle (cc7c4e6).
// Run from the repo root with Node 22 and the baseline's tsx loader; see README.md.
// No implementation is copied: the probe imports the baseline sources and drives
// them over a loopback-free Response / RouterRuntime invocation port.
import assert from 'node:assert/strict';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const STREAM_SOURCE = 'src/model/streaming/streamModel.ts';
const ERRORS_SOURCE = 'src/model/protocol/errors.ts';
const ROUTER_SOURCE = 'src/router/RouterRuntime.ts';
const sourceSha256 = await baselineSourceHashes(baseline, [
  STREAM_SOURCE,
  ERRORS_SOURCE,
  ROUTER_SOURCE,
]);

const { streamModel } = await import(
  pathToFileURL(join(baseline, STREAM_SOURCE)).href
);
const { parseModelConfig } = await import(
  pathToFileURL(join(baseline, 'src/model/config/parseModelConfig.ts')).href
);
const { createRouterRuntime } = await import(
  pathToFileURL(join(baseline, ROUTER_SOURCE)).href
);

const streamText = await readFile(join(baseline, STREAM_SOURCE), 'utf8');
const routerText = await readFile(join(baseline, ROUTER_SOURCE), 'utf8');

/// Extract the literal `msg.includes("...")` needles from one top-level
/// function. This records the source list independently of the runtime result.
function sourceList(fn: string, text = streamText): string[] {
  const start = text.indexOf(`function ${fn}(`);
  assert.ok(start >= 0, `missing function ${fn}`);
  const end = text.indexOf('\n}', start);
  assert.ok(end > start, `missing body end for ${fn}`);
  const body = text.slice(start, end);
  return [...body.matchAll(/msg\.includes\("([^"]+)"\)/g)].map((match) => match[1]);
}

const streamWords = sourceList('isRetryableStreamError');
const requestWords = sourceList('isRetryableRequestError');
const routerWords = sourceList('isNetworkTransient', routerText);
assert.deepEqual(streamWords, [
  'network',
  'econnreset',
  'socket hang up',
  'fetch failed',
  'aborted',
  'timeout',
  'epipe',
  'econnrefused',
]);
assert.deepEqual(requestWords, [
  'network',
  'econnreset',
  'socket hang up',
  'fetch failed',
  'timeout',
  'etimedout',
  'epipe',
  'econnrefused',
]);
assert.deepEqual(routerWords, [
  'econnreset',
  'econnrefused',
  'etimedout',
  'epipe',
  'socket hang up',
  'network',
  'dns',
  'fetch failed',
  'abort',
]);

const errorsText = await readFile(join(baseline, ERRORS_SOURCE), 'utf8');
const premature = errorsText.match(
  /export const PREMATURE_STREAM_CLOSE_PATTERN =\s*\/(.+)\/[a-z]*;/,
);
assert.ok(premature, 'PREMATURE_STREAM_CLOSE_PATTERN');

const config = parseModelConfig({
  providers: {
    test: {
      protocol: 'openai',
      url: 'https://example.test/v1',
      apiKey: 'test-key',
      retry: { streamMaxRetries: 1, baseDelayMs: 0 },
      models: { m: {} },
    },
  },
});
const request = {
  provider: 'test',
  model: 'm',
  messages: [{ role: 'user', content: [{ type: 'text', text: 'hello' }] }],
};
const retryPolicy = { decide: () => ({ maxRetries: 1, delayMs: 0 }) };
const sse = (value: unknown) => `data: ${JSON.stringify(value)}\n\n`;
const successfulStream = () => new Response(
  sse({
    id: 'chatcmpl-fixture',
    object: 'chat.completion.chunk',
    created: 1700000000,
    model: 'm',
    choices: [{ index: 0, delta: { role: 'assistant', content: 'ok' }, finish_reason: null }],
  }) +
  sse({
    id: 'chatcmpl-fixture',
    object: 'chat.completion.chunk',
    created: 1700000000,
    model: 'm',
    choices: [{ index: 0, delta: {}, finish_reason: 'stop' }],
  }) +
  'data: [DONE]\n\n',
  { headers: { 'content-type': 'text/event-stream' } },
);
function failingStream(message: string, errorName: string): Response {
  const error = Object.assign(new Error(message), { name: errorName });
  return new Response(
    new ReadableStream({
      start(controller) {
        controller.error(error);
      },
    }),
    { headers: { 'content-type': 'text/event-stream' } },
  );
}

type Case = { name: string; errorName: string; message: string };
const wordCases: Case[] = streamWords.map((message) => ({
  name: `word-${message}`,
  errorName: 'Error',
  message,
}));
const variantCases: Case[] = [
  { name: 'case-uppercase-network', errorName: 'Error', message: 'NETWORK' },
  { name: 'case-mixed-socket-hang-up', errorName: 'Error', message: 'Socket HANG up' },
  { name: 'case-uppercase-econnreset', errorName: 'Error', message: 'ECONNRESET' },
  { name: 'case-substring-econnreset', errorName: 'Error', message: 'xxeconnresetyy' },
  { name: 'case-suffix-econnresetx', errorName: 'Error', message: 'ECONNRESETX' },
  { name: 'case-sentence-econnrefused', errorName: 'Error', message: 'connect ECONNREFUSED 127.0.0.1:80' },
  { name: 'case-sentence-timeout', errorName: 'Error', message: 'request timeout' },
  { name: 'case-sentence-aborted', errorName: 'Error', message: 'request aborted' },
  { name: 'case-not-dns', errorName: 'Error', message: 'dns' },
  { name: 'case-not-etimedout', errorName: 'Error', message: 'etimedout' },
  { name: 'case-not-abort', errorName: 'Error', message: 'abort' },
  { name: 'case-not-etimedout-uppercase', errorName: 'Error', message: 'ETIMEDOUT' },
  { name: 'case-not-timed-out', errorName: 'Error', message: 'timed out' },
  { name: 'case-not-connection-reset-by-peer', errorName: 'Error', message: 'connection reset by peer' },
  { name: 'case-not-prematurely-closed', errorName: 'Error', message: 'prematurely closed' },
  { name: 'case-premature-close-phrase', errorName: 'Error', message: 'Connection prematurely closed DURING response' },
  { name: 'case-abort-name-boom', errorName: 'AbortError', message: 'boom' },
  { name: 'case-abort-name-timeout', errorName: 'AbortError', message: 'timeout' },
  { name: 'case-abort-name-aborted', errorName: 'AbortError', message: 'aborted' },
  { name: 'case-timeout-name-boom', errorName: 'TimeoutError', message: 'boom' },
  { name: 'case-timeout-name-timeout', errorName: 'TimeoutError', message: 'timeout' },
  { name: 'case-typeerror-terminated', errorName: 'TypeError', message: 'terminated' },
];
const cases = [...wordCases, ...variantCases];

async function streamCase(test: Case) {
  let calls = 0;
  const events: string[] = [];
  const retryProgress: unknown[] = [];
  let thrown: { name: string; message: string } | null = null;
  try {
    for await (const event of streamModel(request as never, config, {
      fetch: async () => {
        calls += 1;
        return calls === 1
          ? failingStream(test.message, test.errorName)
          : successfulStream();
      },
      retryPolicy: retryPolicy as never,
      onRetryProgress: (progress: unknown) => retryProgress.push(progress),
    } as never)) {
      events.push(event.type);
    }
  } catch (error) {
    thrown = {
      name: (error as Error).name,
      message: (error as Error).message,
    };
  }
  const lower = test.message.toLowerCase();
  return {
    ...test,
    retryWord: streamWords.some((word) => lower.includes(word)),
    calls,
    retried: calls > 1,
    retryProgress,
    events,
    thrown,
  };
}

const capabilities = {
  supportsToolUse: true,
  supportsStreaming: true,
  supportsParallelToolCalls: false,
  supportsThinking: false,
  supportsJsonSchema: false,
  supportsSystemPrompt: true,
  supportsPromptCache: false,
  maxContextTokens: 8192,
  maxOutputTokens: 1024,
};
const modelRuntime = {
  async *stream() {},
  async complete() {
    throw new Error('not used');
  },
  getCapabilities: () => capabilities,
  getMultimodal: () => ({ input: ['text'] }),
  getProviderProtocol: () => 'openai',
  getProviderBaseUrl: () => 'https://example.invalid',
};
const routerConfig = {
  enabled: true,
  scenarios: { default: { id: 'test/m', provider: 'test', model: 'm' } },
  transientRetry: { enabled: false, maxAttempts: 1, baseDelayMs: 0, maxDelayMs: 0 },
  zeroUsageRetry: { enabled: false, maxAttempts: 1 },
  stats: { enabled: false },
};
const routerRequest = {
  provider: 'test',
  model: 'm',
  maxOutputTokens: 256,
  messages: [{ role: 'user', content: [{ type: 'text', text: 'hello' }] }],
};
const routerContext = { sessionId: 'session-1', turnId: 'turn-1' };

async function routerCase(test: Case) {
  const thrown = Object.assign(new Error(test.message), { name: test.errorName });
  const invoker = {
    async *stream() {
      throw thrown;
    },
    getCapabilities: () => capabilities,
    getMultimodal: () => ({ input: ['text'] }),
    getProviderProtocol: () => 'openai',
    getProviderBaseUrl: () => 'https://example.invalid',
  };
  const router = createRouterRuntime(routerConfig as never, {
    modelRuntime,
    modelInvoker: invoker,
  } as never);
  const events: Array<{ type?: string; error?: Record<string, unknown> }> = [];
  try {
    for await (const event of router.execute(
      {
        provider: 'test',
        model: 'm',
        scenarioType: 'default',
        isSubagent: false,
        orchestrating: false,
        resolvedFrom: 'scenario',
        mutations: {},
      } as never,
      routerRequest as never,
      routerContext as never,
    )) {
      events.push(event as never);
    }
  } finally {
    await router.shutdown();
  }
  const failure = events.find((event) => event.type === 'error')?.error;
  assert.ok(failure, `router error event for ${test.name}`);
  return {
    ...test,
    retryWord: routerWords.some((word) => test.message.toLowerCase().includes(word)),
    code: failure.code,
    retryable: failure.retryable,
  };
}

const streamCases = [];
for (const test of cases) streamCases.push(await streamCase(test));
const routerCases = [];
for (const test of cases) routerCases.push(await routerCase(test));

const out = resolve(output);
await mkdir(out, { recursive: true });
await writeFile(
  join(out, 'oracle.json'),
  `${JSON.stringify({
    sourceCommit,
    sourceSha256,
    sourceLists: {
      isRetryableStreamError: streamWords,
      isRetryableRequestError: requestWords,
      isNetworkTransient: routerWords,
    },
    prematureStreamClosePattern: premature[1],
    cases: streamCases,
    routerCases,
  }, null, 2)}\n`,
);
console.log(`${streamCases.length} stream cases, ${routerCases.length} router cases`);
