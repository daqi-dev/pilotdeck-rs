// Real baseline `ApiServerChannel` over a loopback listener. The only mock is
// `deps.gateway.submitTurn` (an async generator), so no account, model or user
// home is touched. Every response is the baseline's own bytes.
//
//   PDRS_BASELINE=/abs/PilotDeck LANG=en_US.UTF-8 PDRS_GIT=/usr/bin/git \
//   node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/api-server-channel/generate.mts \
//     /tmp/api-server-channel-out
//
// Wall-clock fields are masked by the generator itself (Date -> fixed, uuid ->
// fixed, `chatcmpl-<ms>` -> fixed, `created` -> 0) so the committed oracle is
// byte-identical across regenerations.
import assert from 'node:assert/strict';
import http from 'node:http';
import { mkdir, rename, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.equal(process.version, 'v22.22.0', '#1333 Node baseline');
assert.equal(process.versions.icu, '78.2', '#1333 ICU baseline');
assert.ok(process.env.PDRS_BASELINE, '#1333 PDRS_BASELINE');
assert.ok(process.argv[2], '#1333 output directory');
const baseline = resolve(process.env.PDRS_BASELINE!);
const outputDir = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/adapters/channel/api-server/ApiServerChannel.ts',
  'src/adapters/channel/api-server/ApiServerSessionMapper.ts',
  'src/adapters/channel/api-server/api-server-render.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { ApiServerChannel } = await import(
  pathToFileURL(join(baseline, 'src/adapters/channel/api-server/ApiServerChannel.ts')).href
);
const { ApiServerSessionMapper } = await import(
  pathToFileURL(join(baseline, 'src/adapters/channel/api-server/ApiServerSessionMapper.ts')).href
);

const FIXED_UUID = '00000000-0000-4000-8000-000000000000';
const FIXED_DATE = 'Thu, 01 Jan 1970 00:00:00 GMT';
const MODEL = 'parity-model';

function maskText(text: string): string {
  return text
    .replace(/"id":"chatcmpl-\d+"/g, '"id":"chatcmpl-FIXED"')
    .replace(/"created":\d+/g, '"created":0')
    .replace(
      /api-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/g,
      'api-FIXED',
    );
}

function maskHeaders(headers: http.IncomingHttpHeaders): Record<string, string> {
  const out: Record<string, string> = {};
  for (const name of Object.keys(headers).sort()) {
    const raw = headers[name];
    let value = Array.isArray(raw) ? raw.join(', ') : String(raw ?? '');
    if (name.toLowerCase() === 'date') value = FIXED_DATE;
    out[name.toLowerCase()] = maskText(value);
  }
  return out;
}

type RequestSpec = {
  method?: string;
  path?: string;
  headers?: Record<string, string>;
  body?: string;
};

function send(port: number, spec: RequestSpec): Promise<any> {
  return new Promise((resolve) => {
    const headers: Record<string, string> = { ...(spec.headers ?? {}) };
    const keepAlive = /keep-alive/i.test(headers.connection ?? '');
    if (!keepAlive) headers.connection = 'close';
    let settled = false;
    const done = (value: any) => {
      if (!settled) {
        settled = true;
        resolve(value);
      }
    };
    const req = http.request(
      {
        host: '127.0.0.1',
        port,
        method: spec.method ?? 'GET',
        path: spec.path ?? '/',
        headers,
        agent: false,
      },
      (response) => {
        const chunks: Buffer[] = [];
        response.on('data', (chunk: Buffer) => chunks.push(chunk));
        response.on('end', () =>
          done({
            status: response.statusCode,
            headers: maskHeaders(response.headers),
            body: maskText(Buffer.concat(chunks).toString('utf8')),
          }),
        );
        response.on('error', (error: any) => done({ error: String(error.code ?? error.message), bytes: 0 }));
      },
    );
    req.on('error', (error: any) => done({ error: String(error.code ?? error.message), bytes: 0 }));
    try {
      if (spec.body !== undefined) req.write(spec.body);
      req.end();
    } catch (error: any) {
      done({ error: String(error.code ?? error.message), bytes: 0 });
    }
  });
}

function scriptedGateway(turns: any[], calls: any[]) {
  let index = 0;
  return {
    async *submitTurn(input: any) {
      calls.push(input);
      const turn = turns[Math.min(index, turns.length - 1)] ?? { events: [] };
      index += 1;
      for (const event of turn.events ?? []) yield event;
      if (turn.error) throw new Error(turn.error);
    },
  };
}

async function startChannel(options: any, gateway: any) {
  const logs: any[] = [];
  const mapper = new ApiServerSessionMapper({ activeByChatId: {} }, () => FIXED_UUID);
  const channel = new ApiServerChannel({
    host: '127.0.0.1',
    port: 0,
    modelName: MODEL,
    apiKey: '',
    corsOrigins: [],
    mapper,
    ...(options ?? {}),
  });
  const handle = await channel.start({
    ...(gateway ? { gateway } : {}),
    logger: {
      info: (message: string) => logs.push(['info', maskText(message)]),
      warn: (message: string) => logs.push(['warn', maskText(message)]),
      error: (message: string) => logs.push(['error', maskText(message)]),
    },
  });
  const port = (channel as any).server.address().port as number;
  return { channel, handle, logs, port };
}

const cases: any[] = [];

async function surfaceCase(id: string, options: any, requests: RequestSpec[], turns?: any[]) {
  const calls: any[] = [];
  const gateway = turns ? scriptedGateway(turns, calls) : undefined;
  const started = await startChannel(options, gateway);
  const expect: any[] = [];
  for (const spec of requests) expect.push(await send(started.port, spec));
  await started.handle.stop('surface-done');
  cases.push({
    id,
    options: options ?? {},
    ...(turns ? { gateway: { turns } } : {}),
    requests,
    expect,
    ...(turns ? { submits: calls } : {}),
  });
}

const chat = (content: unknown) =>
  JSON.stringify({ messages: [{ role: 'user', content }] });

// 1. Listener + method dispatch. Node's WHATWG URL normalization is part of
// the cell: `//host/health`, `/v1/../health` both resolve to `/health`.
await surfaceCase('method-dispatch', {}, [
  { method: 'GET', path: '/health' },
  { method: 'POST', path: '/health', body: 'ignored' },
  { method: 'HEAD', path: '/health' },
  { method: 'GET', path: '/v1/models' },
  { method: 'POST', path: '/v1/models' },
  { method: 'GET', path: '/v1/chat/completions' },
  { method: 'PUT', path: '/v1/chat/completions' },
  { method: 'OPTIONS', path: '/anything' },
  { method: 'GET', path: '/health/' },
  { method: 'GET', path: '/HEALTH' },
  { method: 'GET', path: '/v1/../health' },
  { method: 'GET', path: '//elsewhere/health' },
  { method: 'GET', path: '/%68ealth' },
  { method: 'GET', path: '/nope' },
]);

// 2/3. CORS preflight and simple responses, exact allow-list and `*`.
await surfaceCase(
  'cors-exact',
  { corsOrigins: ['https://app.example'] },
  [
    { method: 'GET', path: '/health', headers: { origin: 'https://app.example' } },
    { method: 'GET', path: '/health', headers: { origin: 'https://other.example' } },
    { method: 'GET', path: '/health' },
    {
      method: 'OPTIONS',
      path: '/v1/chat/completions',
      headers: { origin: 'https://app.example' },
    },
  ],
);
await surfaceCase(
  'cors-star',
  { corsOrigins: ['*'] },
  [
    { method: 'GET', path: '/health', headers: { origin: 'https://anything.example' } },
    { method: 'OPTIONS', path: '/anything', headers: { origin: 'https://anything.example' } },
  ],
);

// 4. Bearer auth is checked before the body is read; valid auth falls through
// to the missing-gateway cell.
await surfaceCase(
  'auth',
  { apiKey: 's3cret-key' },
  [
    { method: 'POST', path: '/v1/chat/completions', body: chat('hi') },
    {
      method: 'POST',
      path: '/v1/chat/completions',
      headers: { authorization: 'Bearer wrong' },
      body: chat('hi'),
    },
    {
      method: 'POST',
      path: '/v1/chat/completions',
      headers: { authorization: 'Bearer s3cret-key' },
      body: chat('hi'),
    },
    {
      method: 'POST',
      path: '/v1/chat/completions',
      headers: { authorization: 'Bearer  s3cret-key ' },
      body: chat('hi'),
    },
  ],
);

// 5. Body validation cells (no gateway, so a valid turn would be 503).
await surfaceCase('chat-validation', {}, [
  { method: 'POST', path: '/v1/chat/completions', body: '' },
  { method: 'POST', path: '/v1/chat/completions', body: '{' },
  { method: 'POST', path: '/v1/chat/completions', body: '{}' },
  { method: 'POST', path: '/v1/chat/completions', body: '[]' },
  { method: 'POST', path: '/v1/chat/completions', body: '{"messages":[]}' },
  {
    method: 'POST',
    path: '/v1/chat/completions',
    body: '{"messages":[{"role":"user","content":""}]}',
  },
  { method: 'POST', path: '/v1/chat/completions', body: '{"messages":["x"]}' },
  { method: 'POST', path: '/v1/chat/completions', body: '{"messages":[{"role":"user"}]}' },
]);

// 6. Buffered turn: fragments concatenate, then the reply is trimmed.
await surfaceCase(
  'buffered',
  {},
  [
    {
      method: 'POST',
      path: '/v1/chat/completions',
      headers: { 'x-hermes-session-id': 'chat-buffered' },
      body: chat('hello'),
    },
  ],
  [
    {
      events: [
        { type: 'assistant_text_delta', text: '  Hello ' },
        { type: 'tool_call_started', name: 'search' },
        { type: 'tool_call_finished', toolName: 'search', ok: true },
        { type: 'assistant_text_delta', text: 'world  ' },
      ],
    },
  ],
);

// 7. SSE: empty fragments are skipped, a visible failure suppresses the
// following generic error event, and the done chunk precedes `[DONE]`.
await surfaceCase(
  'stream',
  {},
  [
    {
      method: 'POST',
      path: '/v1/chat/completions',
      headers: { 'x-hermes-session-id': 'chat-stream', connection: 'keep-alive' },
      body: JSON.stringify({ stream: true, messages: [{ role: 'user', content: 'stream' }] }),
    },
  ],
  [
    {
      events: [
        { type: 'assistant_text_delta', text: 'Hi' },
        { type: 'assistant_thinking_delta', text: 'hidden' },
        { type: 'tool_call_started', name: 'calc' },
        {
          type: 'agent_status',
          event: 'turn_failed',
          detail: { visible: true, severity: 'error', message: 'boom' },
        },
        { type: 'error', message: 'suppressed duplicate' },
        { type: 'agent_status', event: 'turn_failed', detail: { visible: false } },
        { type: 'assistant_text_delta', text: '!' },
      ],
    },
  ],
);

// 8. `/new` switches the per-chat session key and acks; the next turn reuses
// it. Streaming `/new` writes the ack through the SSE path without a session
// header (baseline `writeChatCompletionStream` does not set one).
await surfaceCase(
  'new-command',
  {},
  [
    {
      method: 'POST',
      path: '/v1/chat/completions',
      headers: { 'x-hermes-session-id': 'chat-new' },
      body: chat('/new'),
    },
    {
      method: 'POST',
      path: '/v1/chat/completions',
      headers: { 'x-hermes-session-id': 'chat-new' },
      body: chat('hello again'),
    },
    {
      method: 'POST',
      path: '/v1/chat/completions',
      headers: { 'x-hermes-session-id': 'chat-new-stream', connection: 'keep-alive' },
      body: JSON.stringify({ stream: true, messages: [{ role: 'user', content: '/new' }] }),
    },
  ],
  [
    { events: [{ type: 'assistant_text_delta', text: 'ack ignored' }] },
    { events: [{ type: 'assistant_text_delta', text: 'ok' }] },
    { events: [{ type: 'assistant_text_delta', text: 'ack ignored' }] },
  ],
);

// 9. Missing gateway: JSON 503 on both paths, before any SSE headers.
await surfaceCase('gateway-missing', {}, [
  {
    method: 'POST',
    path: '/v1/chat/completions',
    headers: { 'x-hermes-session-id': 'chat-missing' },
    body: chat('hi'),
  },
  {
    method: 'POST',
    path: '/v1/chat/completions',
    headers: { 'x-hermes-session-id': 'chat-missing-stream', connection: 'keep-alive' },
    body: JSON.stringify({ stream: true, messages: [{ role: 'user', content: 'hi' }] }),
  },
]);

// 10. Over the 1 MB cap the baseline destroys the request: the client sees a
// zero-byte close, never a 413. Three runs pin the observed shape.
for (let run = 0; run < 3; run += 1) {
  const started = await startChannel({}, undefined);
  const result = await send(started.port, {
    method: 'POST',
    path: '/v1/chat/completions',
    body: 'x'.repeat(1_000_001),
  });
  await started.handle.stop('surface-done');
  cases.push({
    id: `oversized-${run}`,
    options: {},
    requests: [{ method: 'POST', path: '/v1/chat/completions', body: '<1000001 bytes>' }],
    expect: [result],
  });
}

// 11. keep-alive response headers come from Node's server, not the handler.
await surfaceCase('keep-alive', {}, [
  { method: 'GET', path: '/health', headers: { connection: 'keep-alive' } },
]);

const oracle = { sourceCommit, sourceSha256, node: process.version, icu: process.versions.icu, cases };
const text = `${JSON.stringify(oracle, null, 2)}\n`;
await mkdir(outputDir, { recursive: true });
const temporary = join(outputDir, 'oracle.json.tmp');
await writeFile(temporary, text, { encoding: 'utf8', flag: 'wx' });
await rename(temporary, join(outputDir, 'oracle.json'));
console.log(`#1333 wrote ${cases.length} cases to ${join(outputDir, 'oracle.json')}`);
