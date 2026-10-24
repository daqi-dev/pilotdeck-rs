// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see
// README.md. Real TS `complete()` against an owned loopback HTTP server that
// accepts the request and either never answers or answers after the request
// timer was cleared. This is the only fixture that uses wall-clock timers;
// every other model-complete oracle is event-count based.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdirSync, writeFileSync } from 'node:fs';
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
const gitExecutable = process.env.PDRS_GIT
  || execFileSync('which', ['git'], { encoding: 'utf8' }).trim();
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const ENTRY = 'src/model/streaming/streamModel.ts';
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [ENTRY]);
const { complete } = await import(pathToFileURL(join(baseline, ENTRY)).href);

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

let mode: 'hang' | 'delayed-body' = 'hang';
let requests: Array<{ method: string; path: string; body: string }> = [];
const server = createServer((incoming, outgoing) => {
  const chunks: Buffer[] = [];
  incoming.on('data', (chunk) => chunks.push(chunk));
  incoming.on('end', () => {
    requests.push({
      method: incoming.method,
      path: incoming.url,
      body: Buffer.concat(chunks).toString('utf8'),
    });
    if (mode === 'hang') return;
    outgoing.writeHead(200, { 'content-type': 'application/json' });
    const split = Math.floor(delayedBody.length / 2);
    outgoing.write(delayedBody.slice(0, split));
    setTimeout(() => outgoing.end(delayedBody.slice(split)), 200);
  });
});
await new Promise<void>((ready) => server.listen(0, '127.0.0.1', ready));
const MOCK = `http://127.0.0.1:${(server.address() as any).port}`;

const thrownRecord = (error: any) => {
  const record: Record<string, unknown> = { name: error?.name, message: error?.message };
  if (error?.name === 'ModelProviderError') record.error = error.error;
  return record;
};

const specs: Array<{
  name: string;
  behavior: 'hang' | 'delayed-body' | 'abort';
  timeoutMs?: number;
  streamTimeoutMs?: number;
  abortAfterMs?: number;
  elapsedAtLeastMs: number;
}> = [
  {
    name: 'loopback-provider-timeout-100',
    behavior: 'hang',
    timeoutMs: 100,
    elapsedAtLeastMs: 50,
  },
  {
    // streamTimeoutMs=10 would fire at 10ms if complete() used the stream
    // idle resolver; the baseline uses provider.timeoutMs=300 instead.
    name: 'loopback-stream-timeout-ignored',
    behavior: 'hang',
    timeoutMs: 300,
    streamTimeoutMs: 10,
    elapsedAtLeastMs: 150,
  },
  {
    // No provider.timeoutMs: the 600000ms fallback is still pending at 150ms,
    // so a 10ms streamTimeoutMs must not abort complete().
    name: 'loopback-default-fallback-not-stream-timeout',
    behavior: 'abort',
    streamTimeoutMs: 10,
    abortAfterMs: 150,
    elapsedAtLeastMs: 100,
  },
  {
    // Headers arrive before provider.timeoutMs, the body after it: the timer
    // is cleared when send returns, so the body read is not capped.
    name: 'loopback-body-outlives-request-timeout',
    behavior: 'delayed-body',
    timeoutMs: 50,
    elapsedAtLeastMs: 150,
  },
];

const cases = [];
for (const spec of specs) {
  mode = spec.behavior === 'delayed-body' ? 'delayed-body' : 'hang';
  requests = [];
  const provider: Record<string, unknown> = {
    id: 'p',
    protocol: 'openai',
    url: `${MOCK}/v1`,
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
    provider: { ...provider, url: 'http://mock.local/v1' },
    streamTimeoutMs: spec.streamTimeoutMs,
    abortAfterMs: spec.abortAfterMs,
    elapsedAtLeastMs: spec.elapsedAtLeastMs,
  };
  if (spec.behavior === 'delayed-body') result.httpBody = delayedBody;
  try {
    result.response = await complete(request as any, { providers: { p: provider } } as any, options as any);
  } catch (error) {
    result.thrown = thrownRecord(error);
  }
  result.requestCount = requests.length;
  result.requests = requests;
  cases.push(result);
}
server.close();
server.closeAllConnections?.();

const oracle = {
  sourceCommit,
  sourceSha256,
  node: process.version,
  locale,
  cases,
};
mkdirSync(output, { recursive: true });
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 1) + '\n', { flag: 'wx' });
console.log(JSON.stringify({ output: join(output, 'oracle.json'), cases: cases.length }));
