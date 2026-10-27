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
 * Upper-bound slack for the real request timer. The expected wait is
 * `provider.timeoutMs * attempts` (retries use a 0 base delay); `setTimeout`
 * cannot fire early. 500ms covers scheduler/loopback jitter for sub-second
 * waits while still rejecting a `timeout_ms * 10` mutation (1000ms/3000ms).
 * Derived from the baseline's requested timeout, not from a Rust run.
 */
const SCHEDULER_SLACK_MS = 500;

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
  causeCode?: string;
  abortBeforeSend?: boolean;
  /** Never settle the fetch until the request timer aborts it. */
  hang?: boolean;
};

const specs: Array<{
  name: string;
  maxRetries: number;
  behavior: Behavior;
  timeoutMs?: number;
  streamTimeoutMs?: number;
}> = [
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
  {
    // complete() uses provider.timeoutMs (not streamTimeoutMs) and retries a
    // retryable timeout.
    name: 'transport-timeout-100',
    maxRetries: 0,
    timeoutMs: 100,
    behavior: { failTimes: 1000, hang: true },
  },
  {
    name: 'transport-timeout-100-retried',
    maxRetries: 2,
    timeoutMs: 100,
    behavior: { failTimes: 1000, hang: true },
  },
  {
    // streamTimeoutMs would say 10ms; the baseline ignores it for complete().
    name: 'transport-timeout-stream-ignored',
    maxRetries: 0,
    timeoutMs: 300,
    streamTimeoutMs: 10,
    behavior: { failTimes: 1000, hang: true },
  },
  {
    // `isAbortError` wins over the retryable canonical flag: one send only.
    name: 'transport-abort-message-request-aborted',
    maxRetries: 2,
    behavior: { failTimes: 1000, message: 'request aborted', causeCode: 'ECONNRESET' },
  },
  {
    // `isAbortError` also wins over a premature-close message.
    name: 'transport-abort-plus-premature',
    maxRetries: 2,
    behavior: { failTimes: 1000, message: 'request aborted: premature close', causeCode: 'ECONNRESET' },
  },
  {
    // Contrast: premature close without "aborted" is retried three times.
    name: 'transport-premature-only',
    maxRetries: 2,
    behavior: { failTimes: 1000, message: 'premature close', causeCode: 'ECONNRESET' },
  },
];

function thrownRecord(error: any) {
  const record: Record<string, unknown> = { name: error?.name, message: error?.message };
  if (error?.name === 'ModelProviderError') record.error = error.error;
  return record;
}

const cases = [];
for (const spec of specs) {
  const behavior = spec.behavior;
  const provider: Record<string, unknown> = {
    id: 'p',
    protocol: 'openai',
    url: 'http://mock.local/v1',
    apiKey: 'fixture-key',
    headers: {},
    retry: { requestMaxRetries: spec.maxRetries, baseDelayMs: 0, maxDelayMs: 0 },
    models: { m: model() },
  };
  if (spec.timeoutMs !== undefined) provider.timeoutMs = spec.timeoutMs;
  let calls = 0;
  let lastError: any;
  const fetchImpl = async (_input: unknown, init: any) => {
    calls += 1;
    if (behavior.hang) {
      return new Promise((_resolve, reject) => {
        const rejectWithReason = () => reject(init.signal.reason);
        if (init.signal.aborted) rejectWithReason();
        else init.signal.addEventListener('abort', rejectWithReason);
      });
    }
    if (calls <= behavior.failTimes) {
      const cause = behavior.causeCode ? { cause: { code: behavior.causeCode } } : undefined;
      lastError = new Error(behavior.message ?? 'fetch failed', cause);
      throw lastError;
    }
    return new Response(successBody, {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  };
  const controller = new AbortController();
  if (behavior.abortBeforeSend) controller.abort();
  const options: Record<string, unknown> = { fetch: fetchImpl };
  if (behavior.abortBeforeSend) options.signal = controller.signal;
  if (spec.streamTimeoutMs !== undefined) options.streamTimeoutMs = spec.streamTimeoutMs;
  const result: Record<string, unknown> = {
    name: spec.name,
    request,
    provider,
    fetch: {
      failTimes: behavior.failTimes,
      message: behavior.message ?? 'fetch failed',
      causeCode: behavior.causeCode,
      abortBeforeSend: behavior.abortBeforeSend ?? false,
      hang: behavior.hang ?? false,
    },
    streamTimeoutMs: spec.streamTimeoutMs,
    // Real TS never waits longer than the requested timer per attempt. This
    // is absent for non-timeout specs, which are event-count based.
    elapsedAtMostMs: spec.timeoutMs === undefined
      ? undefined
      : spec.timeoutMs * (Math.floor(spec.maxRetries) + 1) + SCHEDULER_SLACK_MS,
    reply: behavior.failTimes === 1
      ? { status: 200, headers: { 'content-type': 'application/json' }, body: successBody }
      : undefined,
  };
  try {
    result.response = await complete(request as any, { providers: { p: provider } } as any, options as any);
  } catch (error) {
    result.thrown = thrownRecord(error);
    if ((error as any)?.name === 'ModelProviderError') {
      result.diagnostic = {
        ...(result.diagnostic as Record<string, unknown> | undefined),
        canonical: {
          code: (error as any).error?.code,
          message: (error as any).error?.message,
          retryable: (error as any).error?.retryable,
          raw: (error as any).error?.raw,
        },
      };
    }
  }
  result.fetchCalls = calls;
  if (lastError) {
    result.diagnostic = {
      ...(result.diagnostic as Record<string, unknown> | undefined),
      normalized: {
        name: lastError.name,
        message: lastError.message,
        cause: lastError.cause,
        causeCode: lastError.cause?.code,
      },
    };
  }
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
