// Real TS golden for the external skills management port (#929).
// Run from the repo root with Node 22 and the fixed baseline's tsx loader:
//   PDRS_BASELINE=/path/to/PilotDeck node --import .../tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/skills-external/generate.mts <scratch>
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import { createServer } from 'node:http';
import { mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/composition/domainPorts.ts',
  'src/composition/HttpModuleClient.ts',
  'src/composition/types.ts',
  'src/agent/modules/protocol.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const { createSkillManagementPort } = await import(
  pathToFileURL(join(baseline, sources[0])).href
);
assert.equal(typeof createSkillManagementPort, 'function', 'createSkillManagementPort export');

await mkdir(output, { recursive: true });
const home = join(output, 'home');
await mkdir(home, { recursive: true });
process.env.HOME = home;
delete process.env.PDRS_929_SKILLS_TOKEN;

const METHODS = ['list', 'read', 'write', 'create', 'delete', 'import', 'validate', 'scan'];
const INPUTS: Record<string, unknown> = {
  list: { projectKey: '/p' },
  read: { projectKey: '/p', slug: 'x', scope: 'user' },
  write: { projectKey: '/p', slug: 'x', scope: 'user', content: '# x\n' },
  create: { projectKey: '/p', slug: 'x', scope: 'user', name: 'X', description: 'd' },
  delete: { projectKey: '/p', slug: 'x', scope: 'user' },
  import: { projectKey: '/p', slug: 'x', scope: 'user', sourcePath: '/src', mode: 'copy' },
  validate: { sourcePath: '/src' },
  scan: { parentPath: '/src' },
};

type Reply = { status: number; body: string; destroy?: boolean };
type Recorded = {
  method: string;
  path: string;
  contentType: string | null;
  authorization: string | null;
  body: string | null;
};
type Current = {
  binding: any;
  manifest: any;
  manifestText?: string;
  reply: (request: any) => Reply;
};
let current: Current;
const requests: Recorded[] = [];
const http: Reply[] = [];

const server = createServer(async (req, res) => {
  const chunks: Buffer[] = [];
  for await (const chunk of req) chunks.push(chunk as Buffer);
  const body = Buffer.concat(chunks).toString('utf8');
  requests.push({
    method: req.method ?? '',
    path: req.url ?? '',
    contentType: (req.headers['content-type'] as string | undefined) ?? null,
    authorization: (req.headers.authorization as string | undefined) ?? null,
    body: body === '' ? null : body,
  });
  if (req.url === current.binding.manifestPath) {
    const reply = { status: 200, body: current.manifestText ?? JSON.stringify(current.manifest) };
    http.push({ status: reply.status, body: reply.body });
    res.statusCode = reply.status;
    res.setHeader('content-type', 'application/json');
    res.end(reply.body);
    return;
  }
  assert.equal(req.url, current.binding.callPath, 'unexpected module path');
  const parsed = JSON.parse(body);
  const reply = current.reply(parsed);
  // `destroy` records a response whose declared body never completes; it is a
  // transport instruction, not part of the replayable HTTP reply.
  http.push({ status: reply.status, body: reply.body });
  res.statusCode = reply.status;
  res.setHeader('content-type', 'application/json');
  if (reply.destroy) {
    // Let the head + partial body reach the client before breaking the
    // connection; destroying in the same tick can reject fetch() itself.
    res.write(reply.body);
    await new Promise((ready) => setTimeout(ready, 50));
    res.destroy();
    return;
  }
  res.end(reply.body);
});
await new Promise<void>((ready) => server.listen(0, '127.0.0.1', ready));
const address = server.address();
assert.ok(address && typeof address === 'object', 'server address');
const endpoint = `http://127.0.0.1:${address.port}`;

const baseBinding = () => ({
  enabled: true,
  implementationId: 'test.skills',
  contract: 'pilotdeck.skills/v1',
  transport: 'module-http-v2',
  endpoint,
  manifestPath: '/module-manifest',
  callPath: '/v2/module/call',
  timeoutMs: 2000,
  methods: [...METHODS],
});
const baseManifest = () => ({
  protocolVersion: '2.0',
  implementationId: 'test.skills',
  contract: 'pilotdeck.skills/v1',
  transport: 'module-http-v2',
  methods: [...METHODS],
});
const successReply = (request: any): Reply => ({
  status: 200,
  body: JSON.stringify({
    kind: 'response',
    messageId: 'provider-response',
    inReplyTo: request.messageId,
    requestId: request.requestId,
    ok: true,
    payload: { result: { operation: request.payload.operation, marker: 'success' } },
  }),
});
const providerErrorReply = (request: any): Reply => ({
  status: 200,
  body: JSON.stringify({
    kind: 'response',
    messageId: 'provider-response',
    inReplyTo: request.messageId,
    requestId: request.requestId,
    ok: false,
    code: 'SKILL_PROVIDER_FAILED',
    error: {
      code: 'SKILL_PROVIDER_FAILED',
      message: 'provider exploded',
      retryability: 'safe',
      details: { retryAfterMs: 1000 },
    },
  }),
});

function errorShape(error: any): any {
  const shape: any = { name: error.name };
  if (error.code !== undefined) shape.code = error.code;
  shape.message = error.message;
  if (error.retryability !== undefined) shape.retryability = error.retryability;
  if (error.details !== undefined) shape.details = error.details;
  if (error.outcome !== undefined) shape.outcome = error.outcome;
  return shape;
}
function normalize(value: any): any {
  if (typeof value === 'string') return value.split(endpoint).join('http://127.0.0.1:<port>');
  if (Array.isArray(value)) return value.map(normalize);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, normalize(item)]));
  }
  return value;
}

async function runCase(
  name: string,
  method: string,
  options: {
    binding?: any;
    manifest?: any;
    manifestText?: string;
    reply?: (request: any) => Reply;
  } = {},
): Promise<any> {
  const binding = options.binding ?? baseBinding();
  current = {
    binding,
    manifest: options.manifest ?? baseManifest(),
    manifestText: options.manifestText,
    reply: options.reply ?? successReply,
  };
  requests.length = 0;
  http.length = 0;
  const port = createSkillManagementPort(binding);
  let outcome: any;
  try {
    outcome = { result: await (port as any)[method](INPUTS[method]) };
  } catch (error: any) {
    outcome = { error: errorShape(error) };
  }
  return {
    name,
    method,
    binding: normalize(binding),
    ...(binding.credentialEnv ? { env: {} } : {}),
    input: INPUTS[method],
    http: http.map(normalize),
    requests: requests.map(normalize),
    outcome: normalize(outcome),
  };
}

const cases: any[] = [];
for (const method of METHODS) {
  cases.push(await runCase(`${method}-success`, method));
  const undeclared = baseBinding();
  undeclared.methods = METHODS.filter((candidate) => candidate !== method);
  cases.push(await runCase(`${method}-undeclared`, method, { binding: undeclared }));
  cases.push(
    await runCase(`${method}-provider-error`, method, { reply: providerErrorReply }),
  );
}
cases.push(
  await runCase('missing-credential', 'list', {
    binding: { ...baseBinding(), credentialEnv: 'PDRS_929_SKILLS_TOKEN' },
  }),
);
cases.push(
  await runCase('manifest-missing-method', 'list', {
    manifest: { ...baseManifest(), methods: ['list'] },
  }),
);
cases.push(
  await runCase('result-not-object', 'list', {
    reply: (request) => ({
      status: 200,
      body: JSON.stringify({
        kind: 'response',
        messageId: 'provider-response',
        inReplyTo: request.messageId,
        requestId: request.requestId,
        ok: true,
        payload: { result: 'nope' },
      }),
    }),
  }),
);
cases.push(
  await runCase('http-500', 'list', {
    reply: () => ({ status: 500, body: 'oops' }),
  }),
);
cases.push(
  await runCase('invalid-json', 'list', {
    reply: () => ({ status: 200, body: 'not json' }),
  }),
);
cases.push(
  await runCase('correlation-mismatch', 'list', {
    reply: (request) => ({
      status: 200,
      body: JSON.stringify({
        kind: 'response',
        messageId: 'provider-response',
        inReplyTo: 'wrong',
        requestId: request.requestId,
        ok: true,
        payload: { result: { marker: 'success' } },
      }),
    }),
  }),
);
cases.push(
  await runCase('ok-with-failure-fields', 'list', {
    reply: (request) => ({
      status: 200,
      body: JSON.stringify({
        kind: 'response',
        messageId: 'provider-response',
        inReplyTo: request.messageId,
        requestId: request.requestId,
        ok: true,
        code: 'X',
        payload: { result: { marker: 'success' } },
      }),
    }),
  }),
);

// P1-2: WHATWG UTF-8 decode strips one BOM before JSON.parse, for both the
// manifest GET and the call response.
const withBom = (text: string) => `\uFEFF${text}`;
cases.push(
  await runCase('manifest-bom', 'list', {
    manifestText: withBom(JSON.stringify(baseManifest())),
  }),
);
cases.push(
  await runCase('response-bom', 'list', {
    reply: (request) => ({ status: 200, body: withBom(successReply(request).body) }),
  }),
);

// P1-3: a 2xx body that ends before its declared length makes
// `response.json()` reject; TS swallows that into `undefined` and reports the
// protocol error, not a transport error.
cases.push(
  await runCase('response-truncated', 'list', {
    reply: () => ({ status: 200, body: '{"kind":', destroy: true }),
  }),
);

// P1-4: provider error fields are JS strings and keep unpaired surrogates.
const providerErrorBody = (request: any, code: string, message: string): Reply => ({
  status: 200,
  body: JSON.stringify({
    kind: 'response',
    messageId: 'provider-response',
    inReplyTo: request.messageId,
    requestId: request.requestId,
    ok: false,
    code,
    error: { code, message, retryability: 'safe' },
  }),
});
cases.push(
  await runCase('lone-surrogate-code', 'list', {
    reply: (request) => providerErrorBody(request, 'PROVIDER\ud800', 'provider exploded'),
  }),
);
cases.push(
  await runCase('lone-surrogate-message', 'list', {
    reply: (request) => providerErrorBody(request, 'SKILL_PROVIDER_FAILED', 'bad\ud800'),
  }),
);

// P1-5: every kind runs `validateModuleMessage` before the response-envelope
// check, so a non-response message reports its own protocol error.
const messageReply = (body: string): ((request: any) => Reply) => () => ({ status: 200, body });
cases.push(
  await runCase('message-error-missing-id', 'list', {
    reply: messageReply(JSON.stringify({ kind: 'error' })),
  }),
);
cases.push(
  await runCase('message-request-missing-field', 'list', {
    reply: messageReply(JSON.stringify({
      kind: 'request', messageId: 'provider-response', method: 'module_call', payload: {},
    })),
  }),
);
cases.push(
  await runCase('message-event-missing-field', 'list', {
    reply: messageReply(JSON.stringify({ kind: 'event' })),
  }),
);
cases.push(
  await runCase('message-error-invalid-retryability', 'list', {
    reply: messageReply(JSON.stringify({
      kind: 'error', messageId: 'provider-response', code: 'E', message: 'm', retryability: 'maybe',
    })),
  }),
);
cases.push(
  await runCase('message-unknown-kind', 'list', {
    reply: messageReply(JSON.stringify({ kind: 'bogus' })),
  }),
);

// Coverage guards: every method has all three required classes, and the port
// itself (not only config validation) covers its not-configured branches.
assert.equal(METHODS.length, 8);
for (const method of METHODS) {
  for (const suffix of ['success', 'undeclared', 'provider-error']) {
    assert.ok(cases.some((row) => row.name === `${method}-${suffix}`), `missing ${method}-${suffix}`);
  }
}
for (const method of METHODS) {
  const row = cases.find((candidate) => candidate.name === `${method}-success`);
  assert.deepEqual(row.outcome.result, { operation: method, marker: 'success' });
  assert.deepEqual(row.requests.map((request: Recorded) => request.method), ['GET', 'POST']);
  assert.equal(row.requests[1].contentType, 'application/json');
  const undeclared = cases.find((candidate) => candidate.name === `${method}-undeclared`);
  assert.equal(undeclared.outcome.error.code, 'MODULE_PROTOCOL_INCOMPATIBLE');
  assert.equal(undeclared.requests.length, 0);
  const provider = cases.find((candidate) => candidate.name === `${method}-provider-error`);
  assert.deepEqual(provider.outcome.error, {
    name: 'Error',
    code: 'SKILL_PROVIDER_FAILED',
    message: 'provider exploded',
    retryability: 'safe',
    details: { retryAfterMs: 1000 },
  });
}
assert.equal(
  cases.find((row) => row.name === 'missing-credential').outcome.error.message,
  'Knowledge module account credential is not configured.',
);

await new Promise<void>((closed) => server.close(() => closed()));
await writeFile(
  join(output, 'oracle.json'),
  JSON.stringify({ sourceCommit, sourceSha256, node: process.version, cases }, null, 2) + '\n',
  { flag: 'wx' },
);
console.log(`PASS: ${cases.length} real TS external skill port cases`);
