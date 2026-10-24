// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
// Real TS `complete()` (non-streaming HTTP) with a controlled `fetch` that fails
// before or after headers. `complete()` itself (retry loop, error wrapping,
// body read) is the unmodified baseline; only the transport is controlled so
// pre-header failures can be replayed without a network.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
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
const successBody = JSON.stringify({
  id: 'chatcmpl-retry',
  choices: [{
    index: 0,
    message: { role: 'assistant', content: 'recovered' },
    finish_reason: 'stop',
  }],
});
const request = {
  provider: 'p',
  model: 'm',
  messages: [{ role: 'user', content: [{ type: 'text', text: 'hi' }] }],
  maxOutputTokens: 32,
};

type Behavior = {
  /** Fail this many sends; 1000 means every send fails. */
  failTimes: number;
  message?: string;
  abortBeforeSend?: boolean;
};

const specs: Array<{ name: string; maxRetries: number; behavior: Behavior }> = [
  { name: 'transport-max-retries-0', maxRetries: 0, behavior: { failTimes: 1000, message: 'fetch failed' } },
  { name: 'transport-max-retries-1', maxRetries: 1, behavior: { failTimes: 1000, message: 'fetch failed' } },
  { name: 'transport-max-retries-1.5', maxRetries: 1.5, behavior: { failTimes: 1000, message: 'fetch failed' } },
  { name: 'transport-max-retries-2', maxRetries: 2, behavior: { failTimes: 1000, message: 'fetch failed' } },
  { name: 'transport-non-retryable', maxRetries: 2, behavior: { failTimes: 1000, message: 'boom' } },
  {
    name: 'transport-abort-before-send',
    maxRetries: 2,
    behavior: { failTimes: 1000, message: 'fetch failed', abortBeforeSend: true },
  },
  { name: 'transport-fail-once-then-success', maxRetries: 2, behavior: { failTimes: 1, message: 'fetch failed' } },
];

function thrownRecord(error: any) {
  const record: Record<string, unknown> = { name: error?.name, message: error?.message };
  if (error?.name === 'ModelProviderError') record.error = error.error;
  return record;
}

const cases = [];
for (const spec of specs) {
  const provider = {
    id: 'p',
    protocol: 'openai',
    url: 'http://mock.local/v1',
    apiKey: 'fixture-key',
    headers: {},
    retry: { requestMaxRetries: spec.maxRetries, baseDelayMs: 0, maxDelayMs: 0 },
    models: { m: model() },
  };
  let calls = 0;
  const fetchImpl = async () => {
    calls += 1;
    if (calls <= spec.behavior.failTimes) {
      throw new Error(spec.behavior.message ?? 'fetch failed');
    }
    return new Response(successBody, {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  };
  const controller = new AbortController();
  if (spec.behavior.abortBeforeSend) controller.abort();
  const options: Record<string, unknown> = { fetch: fetchImpl };
  if (spec.behavior.abortBeforeSend) options.signal = controller.signal;
  const result: Record<string, unknown> = {
    name: spec.name,
    request,
    provider,
    fetch: {
      failTimes: spec.behavior.failTimes,
      message: spec.behavior.message ?? 'fetch failed',
      abortBeforeSend: spec.behavior.abortBeforeSend ?? false,
    },
    reply: spec.behavior.failTimes === 1
      ? { status: 200, headers: { 'content-type': 'application/json' }, body: successBody }
      : undefined,
  };
  try {
    result.response = await complete(request as any, { providers: { p: provider } } as any, options as any);
  } catch (error) {
    result.thrown = thrownRecord(error);
  }
  result.fetchCalls = calls;
  cases.push(result);
}

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
