// Run from the repo root with Node 22 and the fixed baseline's tsx loader; see README.md.
// Real TS `complete()` (non-streaming HTTP) against an owned loopback HTTP mock.
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

type Reply = { status?: number; headers?: Record<string, string>; body: string };
type Spec = {
  name: string;
  protocol: 'anthropic' | 'openai' | 'openai-responses';
  reply?: Reply;
  /** Multiple replies for endpoint fallback; consumed in request order. */
  replies?: Reply[];
  /** Defaults to `/v1`; `/api` yields two candidate endpoints. */
  providerUrl?: string;
  request?: Record<string, unknown>;
  provider?: Record<string, unknown>;
};

const model = () => ({
  id: 'm',
  capabilities: { maxContextTokens: 1000, maxOutputTokens: 32 },
  multimodal: { input: ['text'] },
});
const provider = (protocol: string) => ({
  id: 'p',
  protocol,
  url: '$MOCK/v1',
  apiKey: 'fixture-key',
  headers: {},
  models: { m: model() },
});
const request = {
  provider: 'p',
  model: 'm',
  messages: [{ role: 'user', content: [{ type: 'text', text: 'hi' }] }],
  maxOutputTokens: 32,
};
const json = (value: unknown) => ({ body: JSON.stringify(value) });

const anthropicBody = {
  id: 'msg_1',
  type: 'message',
  role: 'assistant',
  content: [
    { type: 'thinking', thinking: 'plan', signature: 'sig==' },
    { type: 'text', text: 'hello' },
    { type: 'tool_use', id: 'toolu_1', name: 'lookup', input: { q: 'x' } },
  ],
  stop_reason: 'tool_use',
  usage: {
    input_tokens: 11,
    output_tokens: 7,
    cache_read_input_tokens: 3,
    cache_creation_input_tokens: 2,
  },
};
const openaiBody = {
  id: 'chatcmpl-1',
  choices: [{
    index: 0,
    message: {
      role: 'assistant',
      reasoning_content: 'think',
      reasoning_details: [
        { text: 'detail-a' },
        { content: 'detail-b' },
        { summary: 'detail-c' },
        {},
      ],
      content: [
        { type: 'text', text: 'part1' },
        { type: 'text', text: '' },
        { type: 'image', image_url: { url: 'ignored' } },
        { type: 'text', text: 'part2' },
      ],
      tool_calls: [
        { id: 'call_1', type: 'function', function: { name: 'lookup', arguments: '{"q":"x"}' } },
        { id: 'call_1', type: 'function', function: { name: 'lookup2', arguments: '{"q":"y"}' } },
        { type: 'function', function: { name: 'lookup3', arguments: '{}' } },
      ],
    },
    finish_reason: 'tool_calls',
  }],
  usage: {
    prompt_tokens: 100,
    completion_tokens: 20,
    total_tokens: 120,
    prompt_tokens_details: { cached_tokens: 10, cache_write_tokens: 5 },
    cost: 0.02,
  },
};
const responsesBody = {
  id: 'resp-1',
  output_text: 'Hi',
  output: [
    { type: 'message', content: [{ type: 'text', text: 'Hi' }] },
    {
      type: 'reasoning',
      summary: [
        { type: 'summary_text', text: 'why' },
        { type: 'summary_text', output_text: 'because' },
        { type: 'summary_text', text: '' },
      ],
    },
    {
      type: 'function_call',
      call_id: 'call_a',
      id: 'fc_1',
      name: 'lookup',
      arguments: '{"q":"x"}',
    },
    { type: 'function_call', id: 'fc_2', name: 'lookup2', arguments: "{q: 'y',}" },
  ],
  status: 'completed',
  usage: {
    input_tokens: 30,
    output_tokens: 10,
    total_tokens: 40,
    input_tokens_details: { cached_tokens: 4 },
  },
};
const okBody = {
  id: 'chatcmpl-ok',
  choices: [{
    index: 0,
    message: { role: 'assistant', content: 'ok' },
    finish_reason: 'stop',
  }],
};

const specs: Spec[] = [
  { name: 'anthropic-normal', protocol: 'anthropic', reply: json(anthropicBody) },
  {
    name: 'anthropic-refusal',
    protocol: 'anthropic',
    reply: json({
      id: 'msg_2',
      type: 'message',
      role: 'assistant',
      content: [{ type: 'text', text: 'declined' }],
      stop_reason: 'refusal',
    }),
  },
  { name: 'openai-normal', protocol: 'openai', reply: json(openaiBody) },
  {
    // The second tool call has no id, so it generates the same id the first
    // call already claimed; the shared `nextUniqueToolCallId` must use `_2`.
    name: 'openai-tool-id-suffix-collision',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-suffix',
      choices: [{
        index: 0,
        message: {
          role: 'assistant',
          tool_calls: [
            { id: 'call_chatcmpl-suffix_0_1', type: 'function', function: { name: 'first', arguments: '{}' } },
            { type: 'function', function: { name: 'second', arguments: '{}' } },
          ],
        },
        finish_reason: 'tool_calls',
      }],
    }),
  },
  {
    // `safeToolCallIdPart`: trims, collapses a run of illegal characters into
    // one `_`, and strips leading/trailing underscores.
    name: 'openai-tool-id-sanitize-run',
    protocol: 'openai',
    reply: json({
      id: 'chat cmpl..1_',
      choices: [{
        index: 0,
        message: {
          role: 'assistant',
          tool_calls: [{ type: 'function', function: { name: 'lookup', arguments: '{}' } }],
        },
        finish_reason: 'tool_calls',
      }],
    }),
  },
  {
    // Same `_2` suffix rule on the Responses id state (generated from the
    // output index), not just the chat-completions state.
    name: 'responses-tool-id-suffix-collision',
    protocol: 'openai-responses',
    reply: json({
      id: 'resp-suffix',
      output: [
        { type: 'function_call', call_id: 'call_resp-suffix_1', name: 'first', arguments: '{}' },
        { type: 'function_call', name: 'second', arguments: '{}' },
      ],
      status: 'completed',
    }),
  },
  {
    name: 'openai-string-content',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-2',
      choices: [{
        index: 0,
        message: { role: 'assistant', content: 'plain' },
        finish_reason: 'stop',
      }],
    }),
  },
  {
    name: 'openai-reasoning-object',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-2b',
      choices: [{
        index: 0,
        message: {
          role: 'assistant',
          reasoning: { content: 'fallback-think' },
          content: 'nested ok',
        },
        finish_reason: 'stop',
      }],
    }),
  },
  {
    name: 'openai-content-filter',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-3',
      choices: [{
        index: 0,
        message: { role: 'assistant', content: 'blocked' },
        finish_reason: 'content_filter_results',
      }],
    }),
  },
  {
    name: 'openai-unknown-finish',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-4',
      choices: [{
        index: 0,
        message: { role: 'assistant', content: 'odd' },
        finish_reason: 'mystery',
      }],
    }),
  },
  { name: 'responses-normal', protocol: 'openai-responses', reply: json(responsesBody) },
  {
    name: 'responses-message-completed',
    protocol: 'openai-responses',
    reply: json({
      id: 'resp-2',
      output_text: 'Hello',
      output: [{ type: 'message', content: [{ type: 'output_text', text: 'Hello' }] }],
      status: 'completed',
    }),
  },
  {
    name: 'responses-incomplete',
    protocol: 'openai-responses',
    reply: json({
      id: 'resp-3',
      output: [{ type: 'message', content: [{ type: 'text', text: 'partial' }] }],
      status: 'incomplete',
    }),
  },
  {
    name: 'responses-failed',
    protocol: 'openai-responses',
    reply: json({ id: 'resp-4', output: [], status: 'failed' }),
  },
  {
    name: 'responses-queued',
    protocol: 'openai-responses',
    reply: json({ id: 'resp-5', output: [], status: 'queued' }),
  },
  {
    name: 'responses-reasoning-only',
    protocol: 'openai-responses',
    reply: json({
      id: 'resp-6',
      output: [{
        type: 'reasoning',
        summary: [
          { type: 'summary_text', text: 'think1' },
          { type: 'summary_text', text: 'think2' },
          { type: 'summary_text', text: '' },
        ],
      }],
      status: 'completed',
    }),
  },
  { name: 'anthropic-empty-body', protocol: 'anthropic', reply: { body: '' } },
  { name: 'openai-empty-body', protocol: 'openai', reply: { body: '' } },
  { name: 'responses-empty-body', protocol: 'openai-responses', reply: { body: '' } },
  { name: 'anthropic-non-json', protocol: 'anthropic', reply: { body: 'not json' } },
  { name: 'openai-non-json', protocol: 'openai', reply: { body: '<html>oops</html>' } },
  { name: 'responses-non-json', protocol: 'openai-responses', reply: { body: '[1,2' } },
  {
    name: 'anthropic-429-retry-after',
    protocol: 'anthropic',
    reply: {
      status: 429,
      headers: { 'retry-after': '7', 'content-type': 'application/json' },
      body: JSON.stringify({
        type: 'error',
        error: {
          type: 'rate_limit_error',
          message: 'Number of request tokens has exceeded your per-minute rate limit',
        },
      }),
    },
  },
  {
    name: 'openai-429-body-retry-after',
    protocol: 'openai',
    reply: {
      status: 429,
      headers: { 'retry-after': '1', 'content-type': 'application/json' },
      body: JSON.stringify({
        error: { message: 'slow down', retryAfterMs: 40, retryable: true },
      }),
    },
  },
  {
    name: 'openai-invalid-tool-args',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-bad',
      choices: [{
        index: 0,
        message: {
          role: 'assistant',
          tool_calls: [{
            id: 'call_bad',
            type: 'function',
            function: { name: 'lookup', arguments: '{:}' },
          }],
        },
        finish_reason: 'tool_calls',
      }],
    }),
  },
  {
    name: 'responses-invalid-tool-args',
    protocol: 'openai-responses',
    reply: json({
      id: 'resp-bad',
      output: [{
        type: 'function_call',
        call_id: 'call_bad',
        name: 'lookup',
        arguments: '{:}',
      }],
      status: 'completed',
    }),
  },
  {
    name: 'responses-body-parsed-as-openai',
    protocol: 'openai',
    reply: json(responsesBody),
  },
  {
    name: 'openai-body-parsed-as-responses',
    protocol: 'openai-responses',
    reply: json(openaiBody),
  },
  {
    // `readString(record.thinking) ?? readString(record.text)`: a thinking
    // block with only `text` falls back; the unknown block is ignored.
    name: 'anthropic-thinking-text-fallback-end-turn',
    protocol: 'anthropic',
    reply: json({
      id: 'msg_think',
      content: [
        { type: 'thinking', text: 'fallback-plan' },
        { type: 'text', text: 'ok' },
        { type: 'redacted_thinking', data: 'ignored' },
      ],
      stop_reason: 'end_turn',
    }),
  },
  {
    name: 'anthropic-stop-sequence',
    protocol: 'anthropic',
    reply: json({ id: 'msg_stop', content: [], stop_reason: 'stop_sequence' }),
  },
  {
    name: 'anthropic-max-tokens',
    protocol: 'anthropic',
    reply: json({ id: 'msg_len', content: [], stop_reason: 'max_tokens' }),
  },
  {
    name: 'anthropic-unknown-stop',
    protocol: 'anthropic',
    reply: json({ id: 'msg_unknown', content: [], stop_reason: 'mystery' }),
  },
  {
    // `readNonEmptyString`: whitespace-only trims to empty -> generated id.
    name: 'openai-tool-id-whitespace',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-ws',
      choices: [{
        index: 0,
        message: {
          role: 'assistant',
          tool_calls: [{ id: '   ', type: 'function', function: { name: 'lookup', arguments: '{}' } }],
        },
        finish_reason: 'tool_calls',
      }],
    }),
  },
  {
    // A non-blank id is returned untrimmed, including its surrounding spaces.
    name: 'openai-tool-id-padded',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-pad',
      choices: [{
        index: 0,
        message: {
          role: 'assistant',
          tool_calls: [{ id: '  call_pad  ', type: 'function', function: { name: 'lookup', arguments: '{}' } }],
        },
        finish_reason: 'tool_calls',
      }],
    }),
  },
  {
    // `choice.index` (not the array position) feeds the generated id.
    name: 'openai-choice-index-2',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-idx',
      choices: [{
        index: 2,
        message: {
          role: 'assistant',
          tool_calls: [{ type: 'function', function: { name: 'lookup', arguments: '{}' } }],
        },
        finish_reason: 'tool_calls',
      }],
    }),
  },
  {
    // Missing arguments -> JSON.parse("{}") path.
    name: 'openai-arguments-missing',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-miss',
      choices: [{
        index: 0,
        message: {
          role: 'assistant',
          tool_calls: [{ id: 'call_miss', type: 'function', function: { name: 'lookup' } }],
        },
        finish_reason: 'tool_calls',
      }],
    }),
  },
  {
    // Empty arguments -> JSON.parse("") and jsonrepair("") both fail.
    name: 'openai-arguments-empty',
    protocol: 'openai',
    reply: json({
      id: 'chatcmpl-empty',
      choices: [{
        index: 0,
        message: {
          role: 'assistant',
          tool_calls: [{ id: 'call_empty', type: 'function', function: { name: 'lookup', arguments: '' } }],
        },
        finish_reason: 'tool_calls',
      }],
    }),
  },
  {
    // `dedupeInitialOutputText` keeps both blocks when the remaining message
    // text differs from `output_text`.
    name: 'responses-output-text-mismatch',
    protocol: 'openai-responses',
    reply: json({
      id: 'resp-mismatch',
      output_text: 'Hi',
      output: [{ type: 'message', content: [{ type: 'text', text: 'Ho' }] }],
      status: 'completed',
    }),
  },
  {
    // A response id that sanitizes to `response` (the synthetic base fallback)
    // plus a real nextUniqueToolCallId `_2` collision on the generated id.
    name: 'responses-response-id-sanitized-collision',
    protocol: 'openai-responses',
    reply: json({
      id: '!!!',
      output: [
        { type: 'function_call', call_id: 'call_response_1', name: 'first', arguments: '{}' },
        { type: 'function_call', name: 'second', arguments: '{}' },
      ],
      status: 'completed',
    }),
  },
  {
    // 401 is not an endpoint-fallback status: `/api` has two candidates but
    // only the first request is sent.
    name: 'openai-401-auth',
    protocol: 'openai',
    providerUrl: '/api',
    reply: {
      status: 401,
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ error: { message: 'bad key' } }),
    },
  },
  {
    name: 'openai-500-html',
    protocol: 'openai',
    reply: {
      status: 500,
      headers: { 'content-type': 'text/html' },
      body: '<html>oops</html>',
    },
  },
  {
    name: 'responses-503-plain',
    protocol: 'openai-responses',
    reply: {
      status: 503,
      headers: { 'content-type': 'text/plain' },
      body: 'upstream unavailable',
    },
  },
  {
    name: 'openai-400-endpoint-fallback',
    protocol: 'openai',
    providerUrl: '/api',
    replies: [{ status: 400, body: '{}' }, json(okBody)],
  },
  {
    name: 'openai-404-endpoint-fallback',
    protocol: 'openai',
    providerUrl: '/api',
    replies: [{ status: 404, body: '{}' }, json(okBody)],
  },
  {
    name: 'openai-405-endpoint-fallback',
    protocol: 'openai',
    providerUrl: '/api',
    replies: [{ status: 405, body: '{}' }, json(okBody)],
  },
];

const scrub = (value: unknown): unknown => {
  const text = JSON.stringify(value);
  return JSON.parse(text.split(MOCK).join('$MOCK'));
};
function thrownRecord(error: any) {
  const record: Record<string, unknown> = { name: error?.name, message: error?.message };
  if (error?.name === 'ModelProviderError') record.error = error.error;
  if (error?.name === 'ModelRequestError') record.code = error.code;
  return record;
}

type Current = {
  replies: Reply[];
  used: Reply[];
  requests: Array<Record<string, unknown>>;
};
let current: Current;
const server = createServer((incoming, outgoing) => {
  const chunks: Buffer[] = [];
  incoming.on('data', (chunk) => chunks.push(chunk));
  incoming.on('end', () => {
    current.requests.push({
      method: incoming.method,
      path: incoming.url,
      body: Buffer.concat(chunks).toString('utf8'),
    });
    const reply = current.replies[Math.min(current.used.length, current.replies.length - 1)];
    current.used.push(reply);
    outgoing.writeHead(reply.status ?? 200, reply.headers ?? { 'content-type': 'application/json' });
    outgoing.end(reply.body);
  });
});
await new Promise<void>((ready) => server.listen(0, '127.0.0.1', ready));
const MOCK = `http://127.0.0.1:${(server.address() as any).port}`;

const cases = [];
for (const spec of specs) {
  const replies = spec.replies ?? [spec.reply!];
  if (!replies.length || replies.some((reply) => !reply)) {
    throw new Error(`${spec.name}: no reply`);
  }
  current = { replies, used: [], requests: [] };
  const providerPath = spec.providerUrl ?? '/v1';
  const caseProvider = { ...provider(spec.protocol), ...(spec.provider ?? {}),
    url: `$MOCK${providerPath}`.replace('$MOCK', MOCK) };
  const caseRequest = { ...request, ...(spec.request ?? {}) };
  const result: Record<string, unknown> = {
    name: spec.name,
    protocol: spec.protocol,
    request: caseRequest,
    provider: { ...caseProvider, url: `http://mock.local${providerPath}` },
  };
  try {
    result.response = await complete(caseRequest as any, { providers: { p: caseProvider } } as any);
  } catch (error) {
    result.thrown = thrownRecord(error);
  }
  const finalReply = current.used[current.used.length - 1];
  result.reply = { ...finalReply, body: scrub(finalReply.body) };
  if (current.used.length > 1) {
    result.replies = current.used.map((reply) => ({ ...reply, body: scrub(reply.body) }));
  }
  result.requestCount = current.requests.length;
  result.requests = scrub(current.requests);
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
