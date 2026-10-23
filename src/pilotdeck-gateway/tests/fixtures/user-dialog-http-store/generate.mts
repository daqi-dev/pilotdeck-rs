// Real TS recordings for the A7.4-pr2 Gateway user-dialog HTTP store.
//
// Source of truth: resources/PilotDeck @cc7c4e6
//   src/gateway/user-dialog/HttpGatewayUserDialogStore.ts
// Every byte below is produced by the real TS client/handler/server; nothing is
// hand-written. Run with the fixed baseline checkout and Node 22:
//   PDRS_BASELINE=/abs/fixed/PilotDeck LANG=en_US.UTF-8 \
//     node --import /abs/fixed/PilotDeck/node_modules/tsx/dist/loader.mjs \
//     src/pilotdeck-gateway/tests/fixtures/user-dialog-http-store/generate.mts /abs/output-parent
//
// Sections:
//   clientRequests   TS client -> injected fetch: raw request bytes + happy-path value
//   clientResponses  injected response -> TS client outcome (validation/transport)
//   clientBodyFailures real TS client -> real raw server whose response body
//                    fails after a complete head (disconnect/stall/oversized)
//   handlerCases     raw Request -> TS handler status/body + store-call arguments
//   socketRequests   real loopback TS server -> raw status/body for framing cases
//   flow             real TS client <-> real TS server (fixed clock/uuid), with the
//                    exact request each call sent and the response it received
//
// The Rust replay in tests/user_dialog_http_store.rs compares these recordings
// byte-for-byte. Cases whose value cannot be expressed in JSON (an `undefined`
// property) are marked in the fixture and built programmatically on the Rust
// side; see README.md.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { createServer, request as httpRequest } from 'node:http';
import { createServer as createTcpServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide an output parent directory; each run writes into its own mkdtemp child');
assert.ok(process.version.startsWith('v22.'), 'generate with Node 22');
const baseline = resolve(process.env.PDRS_BASELINE!);
const outputParent = resolve(process.argv[2]);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
const bound = bindSourceTree(baseline, commit, git);
const entries = [
  'src/gateway/user-dialog/HttpGatewayUserDialogStore.ts',
  'src/gateway/user-dialog/FileGatewayUserDialogStore.ts',
];
const sourceSha256 = sourceClosureSha256(baseline, commit, git, entries);

const load = (name: string) => import(pathToFileURL(join(baseline, name)).href);
const {
  HttpGatewayUserDialogStore,
  createGatewayUserDialogStoreHttpHandler,
  startGatewayUserDialogStoreHttpServer,
} = await load('src/gateway/user-dialog/HttpGatewayUserDialogStore.ts');
const { FileGatewayUserDialogStore } = await load('src/gateway/user-dialog/FileGatewayUserDialogStore.ts');

const TOKEN = 'test-dialog-store-token';
const key = { projectRoot: '/workspace/project', pilotHome: '/workspace/home', sessionId: 'sdk:http-dialog-store' };
const dialog = {
  request: {
    type: 'user_dialog_request',
    requestId: 'http-dialog-1',
    dialogKind: 'form',
    toolCallId: 'tool-1',
    toolName: 'request_user_form',
    prompt: 'Configure the deployment.',
    schema: {
      type: 'object',
      properties: { region: { type: 'string' }, count: { type: 'number' } },
      required: ['region'],
      additionalProperties: false,
    },
  },
  createdAt: '2026-09-11T00:00:00.000Z',
};

async function capture(run: () => Promise<any>) {
  try {
    const value = await run();
    return { kind: 'value', defined: value !== undefined, value: value === undefined ? null : JSON.parse(JSON.stringify(value)) };
  } catch (error: any) {
    return {
      kind: 'error',
      name: error?.name ?? typeof error,
      thrown: typeof error === 'string' ? 'string' : 'error',
      message: error?.message ?? String(error),
    };
  }
}
const response = (status: number, body: string) => new Response(body, { status, headers: { 'content-type': 'application/json; charset=utf-8' } });

// ---------------------------------------------------------------------------
// clientRequests: every operation, payload edges and endpoint joining
// ---------------------------------------------------------------------------
const clientRequests: any[] = [];
function client(handler: (url: any, init: any) => Promise<Response>, timeoutMs?: number) {
  return new HttpGatewayUserDialogStore({
    url: 'http://127.0.0.1:9/base',
    authorizationToken: TOKEN,
    ...(timeoutMs === undefined ? {} : { timeoutMs }),
    fetch: handler,
  });
}
async function recordRequest(
  name: string,
  operation: string,
  endpoint: string,
  responseBody: string,
  run: (store: any) => Promise<any>,
  extra: any = {},
) {
  const captured: any[] = [];
  const store = new HttpGatewayUserDialogStore({
    url: endpoint,
    authorizationToken: TOKEN,
    fetch: async (url: any, init: any) => {
      captured.push({
        method: init.method,
        url: String(url),
        headers: Object.entries(init.headers ?? {}) as [string, string][],
        body: init.body,
      });
      return response(200, responseBody);
    },
  });
  const value = await capture(() => run(store));
  assert.equal(captured.length, 1, `${name}: exactly one request`);
  clientRequests.push({
    name,
    operation,
    endpoint,
    request: captured[0],
    serverResponse: { status: 200, body: responseBody },
    outcome: value,
    ...extra,
  });
}
const nullBody = '{"version":1,"result":null}';
const emptyList = '{"version":1,"result":[]}';
const leasePending = '{"version":1,"result":{"claimed":false,"reason":"not_pending"}}';
const ownerPending = '{"version":1,"result":{"owned":false,"reason":"not_pending"}}';
const trueBody = '{"version":1,"result":true}';

await recordRequest('put-form', 'put', 'http://127.0.0.1:9/base', nullBody, (store) => store.put(key, dialog));
await recordRequest('put-undefined-member', 'put', 'http://127.0.0.1:9/base', nullBody,
  (store) => store.put(key, { request: { ...dialog.request, schema: undefined }, createdAt: dialog.createdAt }),
  { special: 'dialog-schema-undefined' });
await recordRequest('put-unicode-and-numbers', 'put', 'http://127.0.0.1:9/base', nullBody, (store) => store.put(
  { projectRoot: '/项目/😀', pilotHome: '/home/\u00e9', sessionId: 's\t1' },
  {
    request: {
      type: 'user_dialog_request',
      requestId: 'r-"quoted"\n',
      dialogKind: 'confirm',
      toolCallId: 'tool-😀',
      toolName: 'request_user_confirm',
      prompt: 'line1\nline2\t"quoted" \\ back',
      meta: { big: 1e21, small: 1.5, negativeZero: -0, safe: 9007199254740991, array: [1, 'two', null, { deep: true }] },
    },
    createdAt: '2026-09-11T00:00:00.000Z',
  },
));
await recordRequest('list', 'list', 'http://127.0.0.1:9/base', emptyList, (store) => store.list(key));
await recordRequest('remove', 'remove', 'http://127.0.0.1:9/base', nullBody, (store) => store.remove(key, 'http-dialog-1'));
await recordRequest('clear', 'clear', 'http://127.0.0.1:9/base', nullBody, (store) => store.clear(key));
await recordRequest('listLive', 'listLive', 'http://127.0.0.1:9/base', emptyList, (store) => store.listLive(key));
await recordRequest('claimLive-minimal', 'claimLive', 'http://127.0.0.1:9/base', leasePending,
  (store) => store.claimLive(key, { requestId: 'http-dialog-1', ttlMs: 5000 }));
await recordRequest('claimLive-renew-lease', 'claimLive', 'http://127.0.0.1:9/base', leasePending,
  (store) => store.claimLive(key, { requestId: 'http-dialog-1', ttlMs: 5000, leaseId: 'lease-1' }));
await recordRequest('claimLive-fractional-ttl', 'claimLive', 'http://127.0.0.1:9/base', leasePending,
  (store) => store.claimLive(key, { requestId: 'http-dialog-1', ttlMs: 1.5 }));
await recordRequest('releaseLive', 'releaseLive', 'http://127.0.0.1:9/base', trueBody,
  (store) => store.releaseLive(key, { requestId: 'http-dialog-1', leaseId: 'lease-1' }));
await recordRequest('submitLiveAnswer-answered', 'submitLiveAnswer', 'http://127.0.0.1:9/base', trueBody,
  (store) => store.submitLiveAnswer(key, { requestId: 'http-dialog-1', leaseId: 'lease-1', result: { behavior: 'answered', value: { region: 'us-east-1', count: 2 } } }));
await recordRequest('submitLiveAnswer-cancelled-reason', 'submitLiveAnswer', 'http://127.0.0.1:9/base', trueBody,
  (store) => store.submitLiveAnswer(key, { requestId: 'http-dialog-1', result: { behavior: 'cancelled', reason: 'user closed' } }));
await recordRequest('submitLiveAnswer-cancelled-bare', 'submitLiveAnswer', 'http://127.0.0.1:9/base', trueBody,
  (store) => store.submitLiveAnswer(key, { requestId: 'http-dialog-1', result: { behavior: 'cancelled' } }));
await recordRequest('takeLiveAnswer', 'takeLiveAnswer', 'http://127.0.0.1:9/base', nullBody, (store) => store.takeLiveAnswer(key, 'http-dialog-1'));
await recordRequest('claimLiveOwner', 'claimLiveOwner', 'http://127.0.0.1:9/base', ownerPending,
  (store) => store.claimLiveOwner(key, { requestId: 'http-dialog-1', ownerId: 'gateway-a', ttlMs: 5000 }));
await recordRequest('renewLiveOwner', 'renewLiveOwner', 'http://127.0.0.1:9/base', trueBody,
  (store) => store.renewLiveOwner(key, { requestId: 'http-dialog-1', ownerId: 'gateway-a', ttlMs: 5000 }));
await recordRequest('releaseLiveOwner', 'releaseLiveOwner', 'http://127.0.0.1:9/base', trueBody,
  (store) => store.releaseLiveOwner(key, { requestId: 'http-dialog-1', ownerId: 'gateway-a' }));
// An `undefined` property inside an answered value (JSON.stringify drops it).
await recordRequest('submitLiveAnswer-undefined-value', 'submitLiveAnswer', 'http://127.0.0.1:9/base', trueBody,
  (store) => store.submitLiveAnswer(key, { requestId: 'http-dialog-1', result: { behavior: 'answered', value: { keep: 1, drop: undefined } } }),
  { special: 'answer-value-undefined' });
// Blank key members are *not* validated by the client; they reach the wire.
await recordRequest('put-blank-key', 'put', 'http://127.0.0.1:9/base', nullBody,
  (store) => store.put({ projectRoot: '  ', pilotHome: '', sessionId: 's' }, dialog));

for (const [name, endpoint] of [
  ['endpoint-no-slash', 'http://127.0.0.1:1234'],
  ['endpoint-base-path', 'http://127.0.0.1:1234/base'],
  ['endpoint-base-slash', 'http://127.0.0.1:1234/base/'],
  ['endpoint-query', 'http://127.0.0.1:1234/base?x=1'],
  ['endpoint-fragment', 'http://127.0.0.1:1234/a/b#frag'],
  ['endpoint-uppercase-host', 'HTTP://Example.COM/Path'],
  ['endpoint-escaped-path', 'http://h/a%20b'],
  ['endpoint-unicode-path', 'http://h/\u00e4'],
  ['endpoint-double-slashes', 'https://h//x//'],
  ['endpoint-empty-path', 'http://127.0.0.1:1234?query=1'],
] as const) {
  await recordRequest(name, 'clear', endpoint, nullBody, (store) => store.clear(key));
}

// Constructor outcomes (url, token, timeoutMs) captured through the real class.
// `input` records the exact value shape: Rust's typed options cannot express
// `undefined` or a non-number `timeoutMs`, so those cases are type-level skips.
const timeoutInput = (value: any) => value === undefined ? { kind: 'undefined' }
  : value === null ? { kind: 'null' }
  : typeof value === 'number' ? { kind: 'number', text: String(value) }
  : { kind: 'string', value: String(value) };
const tokenInput = (value: any) => value === undefined ? { kind: 'undefined' } : { kind: 'string', value };
const constructors: any[] = [];
for (const url of [
  'http://127.0.0.1:1234', 'http://127.0.0.1:1234/base', 'HTTP://Example.COM/Path',
  'http://h/a%20b', 'http://h/\u00e4', 'https://h//x//', 'http://h?query=1',
  'ftp://h/x', 'not-a-url', 'http://', 'file:///tmp/x',
]) {
  try {
    const store: any = new HttpGatewayUserDialogStore({ url, authorizationToken: TOKEN });
    constructors.push({ name: `url ${url}`, kind: 'endpoint', input: url, url, href: store.endpoint.href, resolved: new URL('clear', store.endpoint).href });
  } catch (error: any) {
    constructors.push({ name: `url ${url}`, kind: 'error', input: url, message: error?.message ?? String(error), errorName: error?.name });
  }
}
for (const timeoutMs of [undefined, 1, 1.5, 0, -1, '1000', null, Number.NaN, Number.POSITIVE_INFINITY, 2 ** 53, 2 ** 53 - 1]) {
  try {
    const store: any = new HttpGatewayUserDialogStore({ url: 'http://h/', authorizationToken: TOKEN, timeoutMs: timeoutMs as any });
    constructors.push({ name: `timeoutMs ${String(timeoutMs)}`, kind: 'timeout', input: timeoutInput(timeoutMs), ms: store.timeoutMs });
  } catch (error: any) {
    constructors.push({ name: `timeoutMs ${String(timeoutMs)}`, kind: 'error', input: timeoutInput(timeoutMs), message: error?.message ?? String(error), errorName: error?.name });
  }
}
for (const authorizationToken of [undefined, '', '   ', '\t', '\u00a0', '\uFEFF', 'ok']) {
  try {
    new HttpGatewayUserDialogStore({ url: 'http://h/', authorizationToken: authorizationToken as any });
    constructors.push({ name: `token ${JSON.stringify(authorizationToken)}`, kind: 'token', input: tokenInput(authorizationToken), ok: true });
  } catch (error: any) {
    constructors.push({ name: `token ${JSON.stringify(authorizationToken)}`, kind: 'error', input: tokenInput(authorizationToken), message: error?.message ?? String(error), errorName: error?.name });
  }
}

// ---------------------------------------------------------------------------
// clientResponses: injected wire responses and transport failures
// ---------------------------------------------------------------------------
const clientResponses: any[] = [];
const canonicalCalls: Record<string, (store: any) => Promise<any>> = {
  put: (store) => store.put(key, dialog),
  list: (store) => store.list(key),
  remove: (store) => store.remove(key, 'http-dialog-1'),
  clear: (store) => store.clear(key),
  listLive: (store) => store.listLive(key),
  claimLive: (store) => store.claimLive(key, { requestId: 'http-dialog-1', ttlMs: 5000 }),
  releaseLive: (store) => store.releaseLive(key, { requestId: 'http-dialog-1', leaseId: 'lease-1' }),
  submitLiveAnswer: (store) => store.submitLiveAnswer(key, { requestId: 'http-dialog-1', leaseId: 'lease-1', result: { behavior: 'answered', value: { a: 1 } } }),
  takeLiveAnswer: (store) => store.takeLiveAnswer(key, 'http-dialog-1'),
  claimLiveOwner: (store) => store.claimLiveOwner(key, { requestId: 'http-dialog-1', ownerId: 'gateway-a', ttlMs: 5000 }),
  renewLiveOwner: (store) => store.renewLiveOwner(key, { requestId: 'http-dialog-1', ownerId: 'gateway-a', ttlMs: 5000 }),
  releaseLiveOwner: (store) => store.releaseLiveOwner(key, { requestId: 'http-dialog-1', ownerId: 'gateway-a' }),
};
// The injected `fetch` behaviour is part of the fixture: the Rust replay must
// not reconstruct these bodies from the case names.
type Injection =
  | { kind: 'reply'; status: number; body: string }
  | { kind: 'throw'; type: 'Error' | 'TypeError' | 'string'; message: string }
  | { kind: 'abort' }
  | { kind: 'jsonRejects'; status: number };
function injectedFetch(injection: Injection): any {
  switch (injection.kind) {
    case 'reply': return (async () => response(injection.status, injection.body)) as any;
    case 'throw': return (async () => {
      if (injection.type === 'string') throw injection.message;
      if (injection.type === 'TypeError') throw new TypeError(injection.message);
      throw new Error(injection.message);
    }) as any;
    case 'abort': return ((input: any, init: any) => new Promise((_, reject) => {
      init.signal.addEventListener('abort', () => reject(init.signal.reason));
    })) as any;
    case 'jsonRejects': return (async () => ({
      ok: true,
      status: injection.status,
      json: async () => { throw new SyntaxError('bad'); },
    })) as any;
  }
}
async function recordResponse(name: string, operation: string, injection: Injection, extra: any = {}) {
  const store = new HttpGatewayUserDialogStore({ url: 'http://127.0.0.1:9/base', authorizationToken: TOKEN, fetch: injectedFetch(injection), timeoutMs: extra.timeoutMs });
  clientResponses.push({ name, operation, timeoutMs: extra.timeoutMs ?? null, injection, outcome: await capture(() => canonicalCalls[operation](store)), ...extra.fixture });
}
const bodyOf = (value: any) => JSON.stringify(value);
const reply = (status: number, body: string): Injection => ({ kind: 'reply', status, body });
const replyValue = (value: any): Injection => reply(200, bodyOf({ version: 1, result: value }));
const dialogs = (value: any): Injection => replyValue(value);

for (const [name, status, body] of [
  ['nonok-message', 400, '{"version":1,"error":"x","message":"boom"}'],
  ['nonok-no-message', 500, '{"version":1,"error":"x"}'],
  ['nonok-bad-json', 502, 'oops'],
  ['nonok-empty-body', 400, ''],
  ['nonok-message-number', 400, '{"message":5}'],
  ['nonok-array', 400, '[1]'],
  ['nonok-null', 400, 'null'],
  ['ok-null', 200, 'null'],
  ['ok-array', 200, '[]'],
  ['ok-empty-object', 200, '{}'],
  ['ok-version-2', 200, '{"version":2,"result":null}'],
  ['ok-version-string', 200, '{"version":"1","result":null}'],
  ['ok-version-true', 200, '{"version":true,"result":null}'],
  ['ok-version-float', 200, '{"version":1.0,"result":null}'],
  ['ok-no-result', 200, '{"version":1}'],
  ['ok-result-null', 200, '{"version":1,"result":null}'],
  ['ok-extra-field', 200, '{"version":1,"result":null,"extra":1}'],
  ['ok-bad-json', 200, 'nope'],
  ['ok-bom', 200, '\uFEFF{"version":1,"result":null}'],
  ['ok-trailing-garbage', 200, '{"version":1,"result":null}x'],
] as const) {
  await recordResponse(name, 'clear', reply(status, body));
}
await recordResponse('duplicate-keys', 'clear', reply(200, '{"version":1,"version":1,"result":null}'));

// dialogs (client `assertDialogs` / `requiredDialog`)
await recordResponse('list-not-array', 'list', replyValue({}));
await recordResponse('list-null-entry', 'list', dialogs([null]));
await recordResponse('list-extra-fields', 'list', dialogs([{ ...dialog, extra: 1, request: { ...dialog.request, extraRequest: 2 } }]));
await recordResponse('list-wrong-type', 'list', dialogs([{ ...dialog, request: { ...dialog.request, type: 'x' } }]));
await recordResponse('list-missing-createdAt', 'list', dialogs([{ request: dialog.request }]));
await recordResponse('list-missing-toolCallId', 'list', dialogs([{ ...dialog, request: { ...dialog.request, toolCallId: undefined } }]));
await recordResponse('list-bad-kind', 'listLive', dialogs([{ ...dialog, request: { ...dialog.request, dialogKind: 'x' } }]));
await recordResponse('list-empty', 'list', dialogs([]));

// lease claims
for (const [name, value] of [
  ['claim-true', { claimed: true, leaseId: 'lease-1', expiresAt: '2026-01-01T00:00:05.000Z' }],
  ['claim-true-no-lease', { claimed: true, expiresAt: 'e' }],
  ['claim-true-no-expiry', { claimed: true, leaseId: 'lease-1' }],
  ['claim-false-claimed', { claimed: false, reason: 'claimed', expiresAt: 'e' }],
  ['claim-false-claimed-no-expiry', { claimed: false, reason: 'claimed' }],
  ['claim-false-notpending', { claimed: false, reason: 'not_pending' }],
  ['claim-false-notpending-expiry', { claimed: false, reason: 'not_pending', expiresAt: 'e' }],
  ['claim-false-bad-reason', { claimed: false, reason: 'other' }],
  ['claim-false-reason-number', { claimed: false, reason: 5 }],
  ['claim-false-expiry-number', { claimed: false, reason: 'claimed', expiresAt: 5 }],
  ['claim-null', null],
  ['claim-string', 'x'],
  ['claim-empty', {}],
  ['claim-claimed-string', { claimed: 'true', leaseId: 'l', expiresAt: 'e' }],
] as const) await recordResponse(name, 'claimLive', replyValue(value));
for (const [name, value] of [
  ['owner-true', { owned: true, ownerId: 'gateway-a', expiresAt: 'e' }],
  ['owner-false', { owned: false, reason: 'owned', expiresAt: 'e' }],
  ['owner-false-no-expiry', { owned: false, reason: 'owned' }],
  ['owner-false-notpending', { owned: false, reason: 'not_pending' }],
  ['owner-bad', { owned: false, reason: 'x' }],
  ['owner-null', null],
] as const) await recordResponse(name, 'claimLiveOwner', replyValue(value));

// booleans
for (const [name, value] of [['bool-true', true], ['bool-false', false], ['bool-null', null], ['bool-string', 'true'], ['bool-zero', 0]] as const)
  await recordResponse(name, 'releaseLive', replyValue(value));

// answer
for (const [name, value] of [
  ['answer-null', null],
  ['answer-ok-answered', { requestId: 'http-dialog-1', submittedAt: 's', result: { behavior: 'answered', value: { a: 1 } } }],
  ['answer-ok-cancelled', { requestId: 'http-dialog-1', submittedAt: 's', result: { behavior: 'cancelled', reason: 'r' } }],
  ['answer-ok-cancelled-blank-reason', { requestId: 'http-dialog-1', submittedAt: 's', result: { behavior: 'cancelled', reason: ' ' } }],
  ['answer-bad-behavior', { requestId: 'http-dialog-1', submittedAt: 's', result: { behavior: 'x' } }],
  ['answer-extra-fields', { requestId: 'http-dialog-1', submittedAt: 's', extra: 1, result: { behavior: 'answered', value: 1, extra: 2 } }],
  ['answer-missing-result', { requestId: 'http-dialog-1', submittedAt: 's' }],
  ['answer-missing-requestId', { submittedAt: 's', result: { behavior: 'cancelled' } }],
  ['answer-missing-submittedAt', { requestId: 'http-dialog-1', result: { behavior: 'cancelled' } }],
  ['answer-undefined-value', { requestId: 'http-dialog-1', submittedAt: 's', result: { behavior: 'answered' } }],
] as const) await recordResponse(name, 'takeLiveAnswer', replyValue(value));

// transport failures
await recordResponse('network-typeerror', 'clear', { kind: 'throw', type: 'TypeError', message: 'fetch failed' });
await recordResponse('network-error', 'clear', { kind: 'throw', type: 'Error', message: 'socket hang up' });
await recordResponse('network-string', 'clear', { kind: 'throw', type: 'string', message: 'weird' });
await recordResponse('abort-reason', 'clear', { kind: 'abort' }, { timeoutMs: 30 });
// A `json()` rejection on an ok response leaves the body undefined.
await recordResponse('ok-json-rejects', 'clear', { kind: 'jsonRejects', status: 200 });

// ---------------------------------------------------------------------------
// clientBodyFailures: real TS client against a real raw server that sends a
// complete response head and then fails the body. TS
// `response.json().catch(() => undefined)` swallows the body failure, so the
// observable error is decided by the status code alone:
//   2xx -> "returned an invalid response."
//   non-2xx -> "failed: HTTP <status>"
// `oversized` exceeds V8 MAX_STRING_LENGTH (0x1fffffe8); this is a real
// 512 MiB loopback transfer, not an injected rejection.
// ---------------------------------------------------------------------------
const clientBodyFailures: any[] = [];
{
  async function recordBodyFailure(status: number, failure: 'disconnect' | 'stall' | 'oversized') {
    const sockets = new Set<any>();
    const server = createTcpServer((socket) => {
      sockets.add(socket);
      socket.on('error', () => {});
      let request = Buffer.alloc(0);
      const onData = (chunk: Buffer) => {
        request = Buffer.concat([request, chunk]);
        if (!request.includes('\r\n\r\n')) return;
        socket.off('data', onData);
        const reason = status === 200 ? 'OK' : 'Internal Server Error';
        const declared = failure === 'oversized' ? 536_870_889 : 100;
        socket.write(`HTTP/1.1 ${status} ${reason}\r\ncontent-type: application/json\r\ncontent-length: ${declared}\r\nconnection: close\r\n\r\n`);
        if (failure === 'disconnect') {
          socket.write('{"version":1');
          setTimeout(() => socket.destroy(), 30);
        } else if (failure === 'stall') {
          socket.write('{"version":1');
        } else {
          const chunk = Buffer.alloc(1024 * 1024, 0x20);
          let sent = 0;
          const pump = () => {
            while (sent < declared) {
              const take = Math.min(chunk.length, declared - sent);
              sent += take;
              if (!socket.write(take === chunk.length ? chunk : chunk.subarray(0, take))) {
                socket.once('drain', pump);
                return;
              }
            }
            socket.end();
          };
          pump();
        }
      };
      socket.on('data', onData);
    });
    await new Promise<void>((resolveListen, rejectListen) => {
      server.once('error', rejectListen);
      server.listen(0, '127.0.0.1', () => resolveListen());
    });
    const port = (server.address() as any).port;
    const timeoutMs = failure === 'stall' ? 250 : 30_000;
    const store = new HttpGatewayUserDialogStore({
      url: `http://127.0.0.1:${port}/v1/gateway-user-dialog-store/`,
      authorizationToken: TOKEN,
      timeoutMs,
    });
    let outcome;
    try {
      outcome = await capture(() => store.clear(key));
    } finally {
      for (const socket of sockets) socket.destroy();
      await new Promise<void>((resolveClose) => server.close(() => resolveClose()));
    }
    clientBodyFailures.push({ name: `${failure}-${status}`, status, failure, timeoutMs, outcome });
  }
  for (const status of [200, 500]) {
    for (const failure of ['disconnect', 'stall', 'oversized'] as const) {
      await recordBodyFailure(status, failure);
    }
  }
}

// ---------------------------------------------------------------------------
// handlerCases: raw Request -> response bytes and store-call arguments
// ---------------------------------------------------------------------------
const handlerCases: any[] = [];
const handlerUrl = (operation: string) => `http://localhost:1234/v1/gateway-user-dialog-store/${operation}`;
function recordingStore() {
  const calls: any[] = [];
  const push = (value: any) => { calls.push(value); };
  return {
    calls,
    store: {
      put: (k: any, d: any) => { push({ op: 'put', key: k, dialog: d }); },
      list: (k: any) => { push({ op: 'list', key: k }); return [dialog]; },
      remove: (k: any, requestId: any) => { push({ op: 'remove', key: k, requestId }); },
      clear: (k: any) => { push({ op: 'clear', key: k }); },
      listLive: (k: any) => { push({ op: 'listLive', key: k }); return [dialog]; },
      claimLive: (k: any, input: any) => {
        push({ op: 'claimLive', key: k, input });
        return { claimed: true, leaseId: 'lease-1', expiresAt: '2026-01-01T00:00:05.000Z' };
      },
      releaseLive: (k: any, input: any) => { push({ op: 'releaseLive', key: k, input }); return true; },
      submitLiveAnswer: (k: any, input: any) => { push({ op: 'submitLiveAnswer', key: k, input }); return true; },
      takeLiveAnswer: (k: any, requestId: any) => {
        push({ op: 'takeLiveAnswer', key: k, requestId });
        // Same record shape the real FileGatewayUserDialogStore persists
        // (`requestId`, `result`, `submittedAt`); the handler passes it through.
        return { requestId: 'http-dialog-1', result: { behavior: 'cancelled', reason: 'why' }, submittedAt: '2026-01-01T00:00:04.000Z' };
      },
      claimLiveOwner: (k: any, input: any) => {
        push({ op: 'claimLiveOwner', key: k, input });
        return { owned: true, ownerId: 'gateway-a', expiresAt: '2026-01-01T00:00:05.000Z' };
      },
      renewLiveOwner: (k: any, input: any) => { push({ op: 'renewLiveOwner', key: k, input }); return true; },
      releaseLiveOwner: (k: any, input: any) => { push({ op: 'releaseLiveOwner', key: k, input }); return true; },
    },
  };
}
async function runHandler(name: string, request: any, mode: 'default' | 'failing' = 'default', extra: any = {}) {
  const recorder = recordingStore();
  const store = mode === 'failing' ? Object.assign(recorder.store, {
    list: () => { throw new Error('disk exploded'); },
    clear: () => { throw new TypeError('bad type'); },
    put: () => { throw 'plain failure'; },
  }) : recorder.store;
  const instance = createGatewayUserDialogStoreHttpHandler({ store, authorizationToken: TOKEN });
  const headers = new Headers();
  for (const [header, value] of request.headers ?? []) headers.append(header, value);
  const body = request.bodyHex !== undefined ? Buffer.from(request.bodyHex, 'hex') : request.body;
  const result = await instance(new Request(request.url, {
    method: request.method,
    headers,
    ...(body !== undefined ? { body } : {}),
  }));
  const text = Buffer.from(await result.arrayBuffer()).toString('utf8');
  handlerCases.push({
    name,
    store: mode,
    // `bodyHex` is kept so the Rust replay can reproduce the exact bytes.
    request,
    response: { status: result.status, contentType: result.headers.get('content-type'), body: text },
    calls: JSON.parse(JSON.stringify(recorder.calls)),
    ...extra,
  });
}
const auth = ['authorization', `Bearer ${TOKEN}`] as [string, string];
const jsonHeaders = [auth, ['content-type', 'application/json'] as [string, string]];
const post = (operation: string, body: any, headers: any[] = jsonHeaders) => ({
  method: 'POST', url: handlerUrl(operation), headers, body: JSON.stringify(body),
});
const base = { version: 1, key };

await runHandler('method-get', { method: 'GET', url: handlerUrl('put'), headers: [] });
await runHandler('method-put', { method: 'PUT', url: handlerUrl('put'), headers: [auth], body: '{}' });
await runHandler('method-head', { method: 'HEAD', url: handlerUrl('put'), headers: [] });
await runHandler('method-delete-with-body', { method: 'DELETE', url: handlerUrl('clear'), headers: [auth], body: '{}' });
await runHandler('unknown-operation', { method: 'POST', url: handlerUrl('nope'), headers: [auth], body: '{}' });
await runHandler('unknown-operation-case', { method: 'POST', url: handlerUrl('PUT'), headers: [auth], body: '{}' });
await runHandler('prefix-mismatch', { method: 'POST', url: 'http://localhost:1234/v2/gateway-user-dialog-store/put', headers: [auth], body: '{}' });
await runHandler('no-prefix', { method: 'POST', url: 'http://localhost:1234/put', headers: [auth], body: '{}' });
await runHandler('operation-subpath', { method: 'POST', url: handlerUrl('clear/x'), headers: [auth], body: '{}' });
await runHandler('operation-trailing-slash', { method: 'POST', url: handlerUrl('clear') + '/', headers: [auth], body: '{}' });
await runHandler('operation-escaped', { method: 'POST', url: 'http://localhost:1234/v1/gateway-user-dialog-store/%70ut', headers: [auth], body: '{}' });
await runHandler('operation-query', post('clear', base));
await runHandler('no-auth', { method: 'POST', url: handlerUrl('clear'), headers: [], body: '{}' });
await runHandler('wrong-auth', { method: 'POST', url: handlerUrl('clear'), headers: [['authorization', 'Bearer wrong']], body: '{}' });
await runHandler('basic-auth', { method: 'POST', url: handlerUrl('clear'), headers: [['authorization', `Basic ${TOKEN}`]], body: '{}' });
await runHandler('lowercase-bearer', { method: 'POST', url: handlerUrl('clear'), headers: [['authorization', `bearer ${TOKEN}`]], body: '{}' });
await runHandler('bearer-prefix-only', { method: 'POST', url: handlerUrl('clear'), headers: [['authorization', 'Bearer ']], body: '{}' });
await runHandler('bearer-extra-space', { method: 'POST', url: handlerUrl('clear'), headers: [['authorization', `Bearer  ${TOKEN}`]], body: '{}' });
await runHandler('duplicate-auth', { method: 'POST', url: handlerUrl('clear'), headers: [auth, auth], body: '{}' });
await runHandler('auth-uppercase-header-name', { method: 'POST', url: handlerUrl('clear'), headers: [['Authorization', `Bearer ${TOKEN}`]], body: '{}' });

for (const [name, body, bodyHex] of [
  ['body-empty', '', undefined],
  ['body-brace', '{', undefined],
  ['body-trailing-garbage', '{"version":1} x', undefined],
  ['body-null', 'null', undefined],
  ['body-array', '[]', undefined],
  ['body-number', '1', undefined],
  ['body-string', '"x"', undefined],
  ['body-true', 'true', undefined],
  ['body-whitespace', '   ', undefined],
  ['body-bom-empty-object', '\uFEFF{}', undefined],
  ['body-bom-valid', `\uFEFF${JSON.stringify({ ...base, dialog })}`, undefined],
  ['body-leading-space', `  ${JSON.stringify(base)}`, undefined],
  ['body-invalid-utf8', undefined, '7b2276657273696f6e223a312c226b6579223a7b2270726f6a656374526f6f74223a22ff227d7d'],
  ['body-nul', '\u0000', undefined],
] as const) {
  await runHandler(name, { method: 'POST', url: handlerUrl(name.startsWith('body-bom-valid') ? 'put' : 'clear'), headers: jsonHeaders, ...(bodyHex !== undefined ? { bodyHex } : { body }) });
}

const raw = (name: string, payload: any, operation = 'put') => runHandler(name, post(operation, payload));
await raw('version-missing', { key });
await raw('version-0', { version: 0, key });
await raw('version-2', { version: 2, key });
await raw('version-string', { version: '1', key });
await raw('version-true', { version: true, key });
await raw('key-missing', { version: 1 });
await raw('key-null', { version: 1, key: null });
await raw('key-array', { version: 1, key: [] });
await raw('key-empty', { version: 1, key: {} });
await raw('key-blank', { version: 1, key: { projectRoot: ' ', pilotHome: '/h', sessionId: 's' } });
await raw('key-bom-blank', { version: 1, key: { projectRoot: '\uFEFF', pilotHome: '/h', sessionId: 's' } });
await raw('key-nbsp-blank', { version: 1, key: { projectRoot: '\u00a0', pilotHome: '/h', sessionId: 's' } });
await raw('key-number', { version: 1, key: { projectRoot: 1, pilotHome: '/h', sessionId: 's' } });
await raw('key-extra', { version: 1, key: { projectRoot: '/p', pilotHome: '/h', sessionId: 's', extra: 1 } });
await raw('dialog-missing', { version: 1, key });
await raw('dialog-null', { version: 1, key, dialog: null });
await raw('dialog-array', { version: 1, key, dialog: [] });
await raw('dialog-wrong-type', { version: 1, key, dialog: { request: { ...dialog.request, type: 'x' }, createdAt: 'x' } });
await raw('dialog-request-missing', { version: 1, key, dialog: { createdAt: 'x' } });
await raw('dialog-request-null', { version: 1, key, dialog: { request: null, createdAt: 'x' } });
await raw('dialog-requestId-missing', { version: 1, key, dialog: { request: { ...dialog.request, requestId: undefined }, createdAt: 'x' } });
await raw('dialog-toolCallId-blank', { version: 1, key, dialog: { request: { ...dialog.request, toolCallId: '  ' }, createdAt: 'x' } });
await raw('dialog-toolName-null', { version: 1, key, dialog: { request: { ...dialog.request, toolName: null }, createdAt: 'x' } });
await raw('dialog-prompt-missing', { version: 1, key, dialog: { request: { ...dialog.request, prompt: undefined }, createdAt: 'x' } });
await raw('dialog-kind-invalid', { version: 1, key, dialog: { request: { ...dialog.request, dialogKind: 'other' }, createdAt: 'x' } });
await raw('dialog-kind-missing', { version: 1, key, dialog: { request: { ...dialog.request, dialogKind: undefined }, createdAt: 'x' } });
await raw('dialog-createdAt-missing', { version: 1, key, dialog: { request: dialog.request } });
await raw('dialog-createdAt-blank', { version: 1, key, dialog: { request: dialog.request, createdAt: ' ' } });
await raw('dialog-extra-fields', { version: 1, key, dialog: { request: { ...dialog.request, schema: { b: 2 }, extra: true }, createdAt: 'c', extraDialog: 1 } });
await raw('put-extra-body', { version: 1, key, dialog, extraBody: 1 });
await raw('input-null-on-put', { version: 1, key, dialog, input: null });
await raw('input-array-on-list', { version: 1, key, input: [] }, 'list');
await raw('requestId-number-on-list', { version: 1, key, requestId: 5 }, 'list');

await raw('remove-missing', { version: 1, key }, 'remove');
await raw('remove-blank', { version: 1, key, requestId: '  ' }, 'remove');
await raw('remove-number', { version: 1, key, requestId: 5 }, 'remove');
await raw('remove-ok', { version: 1, key, requestId: 'http-dialog-1' }, 'remove');
await raw('clear-ok', { version: 1, key }, 'clear');
await raw('list-ok', { version: 1, key }, 'list');
await raw('listLive-ok', { version: 1, key }, 'listLive');
await raw('claim-ok', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: 5000 } }, 'claimLive');
await raw('claim-with-lease', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: 5000, leaseId: 'lease-0' } }, 'claimLive');
await raw('claim-no-input', { version: 1, key }, 'claimLive');
await raw('claim-input-null', { version: 1, key, input: null }, 'claimLive');
await raw('claim-ttl-missing', { version: 1, key, input: { requestId: 'http-dialog-1' } }, 'claimLive');
await raw('claim-ttl-zero', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: 0 } }, 'claimLive');
await raw('claim-ttl-negative', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: -5 } }, 'claimLive');
await raw('claim-ttl-float', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: 1.5 } }, 'claimLive');
await raw('claim-ttl-string', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: '5000' } }, 'claimLive');
await raw('claim-ttl-null', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: null } }, 'claimLive');
await raw('claim-ttl-max', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: 9007199254740991 } }, 'claimLive');
await raw('claim-ttl-over-max', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: 9007199254740992 } }, 'claimLive');
await raw('claim-lease-blank', { version: 1, key, input: { requestId: 'http-dialog-1', ttlMs: 5, leaseId: '' } }, 'claimLive');
await raw('claim-requestId-blank', { version: 1, key, input: { requestId: '', ttlMs: 5 } }, 'claimLive');
await raw('release-ok', { version: 1, key, input: { requestId: 'http-dialog-1', leaseId: 'lease-1' } }, 'releaseLive');
await raw('release-missing-lease', { version: 1, key, input: { requestId: 'http-dialog-1' } }, 'releaseLive');
await raw('answer-ok', { version: 1, key, input: { requestId: 'http-dialog-1', result: { behavior: 'answered', value: { a: 1 } } } }, 'submitLiveAnswer');
await raw('answer-cancelled', { version: 1, key, input: { requestId: 'http-dialog-1', leaseId: 'lease-1', result: { behavior: 'cancelled', reason: 'why', extra: 1 } } }, 'submitLiveAnswer');
await raw('answer-cancelled-no-reason', { version: 1, key, input: { requestId: 'http-dialog-1', result: { behavior: 'cancelled' } } }, 'submitLiveAnswer');
await raw('answer-bad-behavior-no-requestId', { version: 1, key, input: { result: { behavior: 'x' } } }, 'submitLiveAnswer');
await raw('answer-no-result', { version: 1, key, input: { requestId: 'http-dialog-1' } }, 'submitLiveAnswer');
await raw('answer-result-null', { version: 1, key, input: { requestId: 'http-dialog-1', result: null } }, 'submitLiveAnswer');
await raw('answer-answered-no-value', { version: 1, key, input: { requestId: 'http-dialog-1', result: { behavior: 'answered' } } }, 'submitLiveAnswer');
await raw('answer-reason-blank', { version: 1, key, input: { requestId: 'http-dialog-1', result: { behavior: 'cancelled', reason: ' ' } } }, 'submitLiveAnswer');
await raw('answer-reason-number', { version: 1, key, input: { requestId: 'http-dialog-1', result: { behavior: 'cancelled', reason: 5 } } }, 'submitLiveAnswer');
await raw('answer-value-undefined-wire', { version: 1, key, input: { requestId: 'http-dialog-1', result: { behavior: 'answered', value: undefined } } }, 'submitLiveAnswer');
await raw('answer-extra-input-fields', { version: 1, key, input: { requestId: 'http-dialog-1', leaseId: 'lease-1', extra: 1, result: { behavior: 'answered', value: 1, extra: 2 } } }, 'submitLiveAnswer');
await raw('take-missing', { version: 1, key }, 'takeLiveAnswer');
await raw('take-ok', { version: 1, key, requestId: 'http-dialog-1' }, 'takeLiveAnswer');
await raw('owner-claim', { version: 1, key, input: { requestId: 'http-dialog-1', ownerId: 'gateway-a', ttlMs: 7 } }, 'claimLiveOwner');
await raw('owner-renew-missing-ttl', { version: 1, key, input: { requestId: 'http-dialog-1', ownerId: 'gateway-a' } }, 'renewLiveOwner');
await raw('owner-renew-ok', { version: 1, key, input: { requestId: 'http-dialog-1', ownerId: 'gateway-a', ttlMs: 7 } }, 'renewLiveOwner');
await raw('owner-release-ok', { version: 1, key, input: { requestId: 'http-dialog-1', ownerId: 'gateway-a' } }, 'releaseLiveOwner');
await raw('owner-release-missing-owner', { version: 1, key, input: { requestId: 'http-dialog-1' } }, 'releaseLiveOwner');

for (const [name, operation] of [['failure-list', 'list'], ['failure-clear', 'clear'], ['failure-put', 'put']] as const)
  await runHandler(name, post(operation, { version: 1, key, ...(operation === 'put' ? { dialog } : {}) }), 'failing');

// constructor guards
const handlerConstruction: any[] = [];
for (const [name, store, token, missing] of [
  ['missing-put', (() => { const s = recordingStore().store; delete s.put; return s; })(), TOKEN, 'put'],
  ['missing-clear', (() => { const s = recordingStore().store; delete s.clear; return s; })(), TOKEN, 'clear'],
  ['missing-releaseLiveOwner', (() => { const s = recordingStore().store; delete s.releaseLiveOwner; return s; })(), TOKEN, 'releaseLiveOwner'],
] as const) {
  try {
    createGatewayUserDialogStoreHttpHandler({ store, authorizationToken: token });
    handlerConstruction.push({ name, missing, ok: true });
  } catch (error: any) {
    handlerConstruction.push({ name, missing, ok: false, message: error?.message ?? String(error) });
  }
}
for (const token of [undefined, '', '   ', 'ok']) {
  try {
    createGatewayUserDialogStoreHttpHandler({ store: recordingStore().store, authorizationToken: token as any });
    handlerConstruction.push({ name: `token ${JSON.stringify(token)}`, input: tokenInput(token), ok: true });
  } catch (error: any) {
    handlerConstruction.push({ name: `token ${JSON.stringify(token)}`, input: tokenInput(token), ok: false, message: error?.message ?? String(error) });
  }
}

// ---------------------------------------------------------------------------
// socketRequests: real loopback TS server framing
// ---------------------------------------------------------------------------
const socketRequests: any[] = [];
function rawHttp(port: number, { method, path, headers = [], body }: any): Promise<{ status: number; contentType: string | null; body: string }> {
  return new Promise((resolveRequest, rejectRequest) => {
    // A raw header array suppresses Node's automatic Host header, which makes
    // the HTTP/1.1 parser reject the request before the handler runs. Add the
    // Host a normal client sends while still allowing raw duplicate headers.
    const request = httpRequest({ host: '127.0.0.1', port, method, path, headers: [['host', `127.0.0.1:${port}`], ...headers] as any }, (response) => {
      const chunks: Buffer[] = [];
      response.on('data', (chunk) => chunks.push(chunk));
      response.on('end', () => resolveRequest({
        status: response.statusCode!,
        contentType: response.headers['content-type'] ?? null,
        body: Buffer.concat(chunks).toString('utf8'),
      }));
    });
    request.on('error', rejectRequest);
    if (body !== undefined) request.write(body);
    request.end();
  });
}
{
  const root = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-user-dialog-http-store-'));
  const server = await startGatewayUserDialogStoreHttpServer({
    store: new FileGatewayUserDialogStore({ directory: root }),
    authorizationToken: TOKEN,
  });
  try {
    const port = Number(new URL(server.url).port);
    const authHeader = ['authorization', `Bearer ${TOKEN}`];
    const validPut = JSON.stringify({ version: 1, key, dialog });
    const hugeValid = JSON.stringify({ version: 1, key, extra: 'x'.repeat(1024 * 1024) });
    const cases = [
      ['get', { method: 'GET', path: '/v1/gateway-user-dialog-store/clear' }],
      ['head', { method: 'HEAD', path: '/v1/gateway-user-dialog-store/clear' }],
      ['unknown-operation', { method: 'POST', path: '/v1/gateway-user-dialog-store/nope', headers: [authHeader], body: '{}' }],
      ['wrong-prefix', { method: 'POST', path: '/v2/gateway-user-dialog-store/put', headers: [authHeader], body: '{}' }],
      ['no-auth', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', body: '{}' }],
      ['bad-auth', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', headers: [['authorization', 'Bearer wrong']], body: '{}' }],
      ['duplicate-auth', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', headers: [authHeader, authHeader], body: '{}' }],
      ['invalid-json', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', headers: [authHeader], body: '{' }],
      ['empty-body', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', headers: [authHeader] }],
      ['huge-invalid-json', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', headers: [authHeader], body: ' '.repeat(1024 * 1024) }],
      ['huge-valid-json', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', headers: [authHeader], body: hugeValid }],
      ['put-valid', { method: 'POST', path: '/v1/gateway-user-dialog-store/put', headers: [authHeader], body: validPut }],
      ['list-valid', { method: 'POST', path: '/v1/gateway-user-dialog-store/list', headers: [authHeader], body: JSON.stringify({ version: 1, key }) }],
      ['trailing-slash', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear/', headers: [authHeader], body: '{}' }],
      ['query', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear?x=1', headers: [authHeader], body: '{}' }],
      ['bom-body', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', headers: [authHeader], body: `\uFEFF${JSON.stringify({ version: 1, key })}` }],
      ['invalid-utf8', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', headers: [authHeader], body: Buffer.from([0x7b, 0xff, 0x7d]) }],
      ['content-type-text', { method: 'POST', path: '/v1/gateway-user-dialog-store/clear', headers: [authHeader, ['content-type', 'text/plain']], body: JSON.stringify({ version: 1, key }) }],
    ] as const;
    for (const [name, request] of cases) {
      const result = await rawHttp(port, request);
      const recorded: any = { ...request };
      if (Buffer.isBuffer(recorded.body)) {
        recorded.bodyHex = recorded.body.toString('hex');
        delete recorded.body;
      }
      socketRequests.push({ name, request: recorded, response: result });
    }
  } finally {
    await server.close();
    await rm(root, { recursive: true, force: true });
  }
}

// ---------------------------------------------------------------------------
// flow: real TS client <-> real TS server, fixed clock and uuid sequence
// ---------------------------------------------------------------------------
const flow: any[] = [];
{
  const root = await mkdtemp(join(realpathSync(tmpdir()), 'pdrs-user-dialog-http-store-flow-'));
  const fixedNow = new Date(Date.parse('2026-01-01T00:00:00.000Z'));
  let uuidIndex = 0;
  const uuids = ['lease-1', 'lease-2'];
  const server = await startGatewayUserDialogStoreHttpServer({
    store: new FileGatewayUserDialogStore({
      directory: root,
      now: () => new Date(fixedNow.getTime()),
      uuid: () => uuids[uuidIndex++ % uuids.length],
    }),
    authorizationToken: TOKEN,
  });
  try {
    let recorded: any = null;
    const recordingFetch = async (url: any, init: any) => {
      recorded = {
        request: {
          method: init.method,
          path: new URL(String(url)).pathname,
          headers: Object.entries(init.headers ?? {}) as [string, string][],
          body: init.body,
        },
      };
      const result = await globalThis.fetch(url, init);
      const body = await result.text();
      recorded.response = { status: result.status, body };
      return new Response(body, { status: result.status, headers: { 'content-type': result.headers.get('content-type') ?? '' } });
    };
    const client = new HttpGatewayUserDialogStore({ url: server.url, authorizationToken: TOKEN, fetch: recordingFetch });
    const steps: Array<[string, (store: any) => Promise<any>]> = [
      ['put', (store) => store.put(key, dialog)],
      ['list', (store) => store.list(key)],
      ['listLive-empty', (store) => store.listLive(key)],
      ['claimLiveOwner', (store) => store.claimLiveOwner(key, { requestId: 'http-dialog-1', ownerId: 'gateway-a', ttlMs: 5000 })],
      ['listLive', (store) => store.listLive(key)],
      ['renewLiveOwner', (store) => store.renewLiveOwner(key, { requestId: 'http-dialog-1', ownerId: 'gateway-a', ttlMs: 5000 })],
      ['claimLive', (store) => store.claimLive(key, { requestId: 'http-dialog-1', ttlMs: 5000 })],
      ['submitLiveAnswer-wrong-lease', (store) => store.submitLiveAnswer(key, { requestId: 'http-dialog-1', leaseId: 'wrong-lease', result: { behavior: 'answered', value: { region: 'us-east-1' } } })],
      ['submitLiveAnswer', (store) => store.submitLiveAnswer(key, { requestId: 'http-dialog-1', leaseId: 'lease-1', result: { behavior: 'answered', value: { region: 'us-east-1' } } })],
      ['takeLiveAnswer', (store) => store.takeLiveAnswer(key, 'http-dialog-1')],
      ['takeLiveAnswer-again', (store) => store.takeLiveAnswer(key, 'http-dialog-1')],
      ['releaseLive', (store) => store.releaseLive(key, { requestId: 'http-dialog-1', leaseId: 'lease-1' })],
      ['releaseLiveOwner', (store) => store.releaseLiveOwner(key, { requestId: 'http-dialog-1', ownerId: 'gateway-a' })],
      ['listLive-after-release', (store) => store.listLive(key)],
      ['clear', (store) => store.clear(key)],
      ['list-after-clear', (store) => store.list(key)],
    ];
    for (const [name, run] of steps) {
      recorded = null;
      const outcome = await capture(() => run(client));
      assert.ok(recorded, `${name}: captured a real request`);
      flow.push({ name, request: recorded.request, response: recorded.response, outcome });
    }
  } finally {
    await server.close();
    await rm(root, { recursive: true, force: true });
  }
}

// ---------------------------------------------------------------------------
// Emit
// ---------------------------------------------------------------------------
const oracle = {
  sourceCommit: commit,
  sourceSha256,
  baselineCount: bound.count,
  counts: {
    clientRequests: clientRequests.length,
    clientResponses: clientResponses.length,
    clientBodyFailures: clientBodyFailures.length,
    handlerCases: handlerCases.length,
    socketRequests: socketRequests.length,
    flow: flow.length,
    constructors: constructors.length,
    handlerConstruction: handlerConstruction.length,
  },
  constructors,
  clientRequests,
  clientResponses,
  clientBodyFailures,
  handlerCases,
  handlerConstruction,
  socketRequests,
  flow,
};
const output = join(await mkdtemp(join(outputParent, 'user-dialog-http-store-')), 'oracle.json');
await writeFile(output, `${JSON.stringify(oracle, null, 2)}\n`, { flag: 'wx' });
console.log(output);
