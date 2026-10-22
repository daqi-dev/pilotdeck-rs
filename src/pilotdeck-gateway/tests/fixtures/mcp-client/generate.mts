// Records the fixed TypeScript baseline's MCP client protocol layer against a
// scripted stdio server (mock_server.mjs): src/mcp/client/McpClient.ts with
// the real @modelcontextprotocol/sdk Client/StdioClientTransport, plus the
// pure helpers in src/mcp/runtime/{sanitize,wireName,truncate}.ts and
// src/mcp/config/{loadMcpServerConfig,expandPlaceholders}.ts.
//
// Run from the repo root with Node 22 and the baseline's installed tsx loader:
//   PDRS_BASELINE=/abs/path/PilotDeck LANG=en_US.UTF-8 \
//     node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/mcp-client/generate.mts \
//     src/pilotdeck-gateway/tests/fixtures/mcp-client
import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import { createServer } from 'node:net';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const MOCK = fileURLToPath(new URL('./mock_server.mjs', import.meta.url));
const ENTRIES = [
  'src/mcp/client/McpClient.ts',
  'src/mcp/config/loadMcpServerConfig.ts',
  'src/mcp/runtime/wireName.ts',
];

// Shared provenance guard (#280): explicit absolute Git, pinned HEAD, every
// tracked src byte equal to its Git blob, clean tracked tree.
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const trackedChanges = execFileSync(gitExecutable,
  ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'],
  { env: gitEnv }).toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, ENTRIES);
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');
const sdkVersion = JSON.parse(readFileSync(
  join(baseline, 'node_modules/@modelcontextprotocol/sdk/package.json'), 'utf8')).version;
assert.equal(sdkVersion, '1.29.0', 'the oracle is calibrated against @modelcontextprotocol/sdk 1.29.0');

const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { McpClient } = await load('src/mcp/client/McpClient.ts');
const { recursivelySanitizeUnicode, sanitizeUnicodeString } = await load('src/mcp/runtime/sanitize.ts');
const { buildMcpToolWireName, parseMcpToolWireName } = await load('src/mcp/runtime/wireName.ts');
const { truncateMcpToolDescription } = await load('src/mcp/runtime/truncate.ts');
const { expandMcpString } = await load('src/mcp/config/expandPlaceholders.ts');
const { loadMcpServerConfig } = await load('src/mcp/config/loadMcpServerConfig.ts');

// The run owns one mkdtemp tree; every delete is confined to it.
const runRoot = realpathSync(mkdtempSync(join(tmpdir(), 'pdrs-mcp-client-')));
function removeOwned(path: string) {
  const rel = relative(runRoot, path);
  if (rel === '..' || rel.startsWith(`..${sep}`) || isAbsolute(rel)) {
    throw new Error(`remove refused: ${path} is outside the run directory ${runRoot}`);
  }
  rmSync(path, { recursive: true, force: true });
}

const sleep = (ms: number) => new Promise((done) => setTimeout(done, ms));
const INIT = (extra: Record<string, unknown> = {}) => [{
  send: {
    jsonrpc: '2.0',
    id: '$id',
    result: {
      protocolVersion: '2025-06-18',
      capabilities: { tools: { listChanged: true }, unknownCap: {} },
      serverInfo: { name: 'mock', version: '1.0.0' },
      instructions: 'server instructions are not surfaced by McpClient',
      ...extra,
    },
  },
}];
const ok = (result: unknown) => [{ send: { jsonrpc: '2.0', id: '$id', result } }];
const rpcError = (code: number, message: string, data?: unknown) => [{
  send: { jsonrpc: '2.0', id: '$id', error: data === undefined ? { code, message } : { code, message, data } },
}];
const text = (value: string) => ({ content: [{ type: 'text', text: value }] });
/** JSON text for an SSE `data:` line; the mock substitutes `$ID`. */
const sseJson = (value: unknown) => JSON.stringify(value).split('"$id"').join('$ID');
const HTTP_INIT = {
  jsonrpc: '2.0',
  id: '$id',
  result: {
    protocolVersion: '2025-06-18',
    capabilities: { tools: {} },
    serverInfo: { name: 'mock-http', version: '1.0.0' },
  },
};

type Op = Record<string, any> & { op: string };
type Scenario = {
  id: string;
  spec?: Record<string, unknown>;
  options?: Record<string, unknown>;
  procs: Record<string, unknown>[];
  /** HTTP mode: the mock serves this script over plain HTTP instead. */
  http?: Record<string, unknown>;
  ops: Op[];
  /** A process the client never closes (TS handshake timeout leaves it to the 60s request timer). */
  orphan?: boolean;
};

const LONG = 'x'.repeat(2047) + '\u{1F600}' + 'tail';
const TOOLS = {
  tools: [
    {
      zzz: 'stripped',
      name: 'a.b',
      description: 'hello‮evil​',
      inputSchema: { b: 1, type: 'object', properties: { q: { type: 'string' } }, a: 2 },
      annotations: { extra: 1, readOnlyHint: true, title: 'T' },
      _meta: { 'k‍': 'v﻿' },
    },
    { name: 'a b', inputSchema: { type: 'object' } },
    { name: 'long', description: LONG, inputSchema: { type: 'object', required: ['q'] } },
    { name: 'naïve/工具', description: 'x'.repeat(2048), inputSchema: { type: 'object' }, icons: [] },
    { name: 'structured', inputSchema: { type: 'object' }, outputSchema: { type: 'object' } },
    { name: 'task', inputSchema: { type: 'object' }, execution: { taskSupport: 'required' } },
  ],
  nextCursor: 'ignored',
};

const scenarios: Scenario[] = [
  {
    id: 'basic',
    procs: [{
      initialize: INIT(),
      'tools/list': ok(TOOLS),
      'tools/call:echo': ok({
        extra: 'kept by ResultSchema, dropped by McpClient',
        content: [
          { type: 'text', text: 'hi‮�', annotations: { priority: 0.5, audience: ['user'] }, zz: 1 },
          { mimeType: 'image/png', type: 'image', data: 'QUJD', _meta: { m: 1 } },
          { type: 'resource', resource: { uri: 'file:///x', text: 't', blob: 'QQ==' } },
          { type: 'resource_link', uri: 'u', name: 'n', size: 3 },
        ],
        isError: false,
      }),
      'tools/call:noContent': ok({}),
      'tools/call:flagged': ok({ content: [], isError: true }),
      'tools/call:structured': ok({ content: [], structuredContent: { a: 1 } }),
    }],
    ops: [
      { op: 'status' },
      { op: 'start' },
      { op: 'start' },
      { op: 'listTools' },
      { op: 'listTools' },
      { op: 'callTool', name: 'echo', args: { q: 'x', n: [1, 2] } },
      { op: 'callTool', name: 'noContent' },
      { op: 'callTool', name: 'flagged', args: null },
      { op: 'callTool', name: 'structured', args: {} },
      { op: 'callTool', name: 'structured', args: {}, timeoutMs: 5000 },
      { op: 'close' },
      { op: 'close' },
    ],
  },
  {
    id: 'call-errors',
    procs: [{
      initialize: INIT(),
      'tools/list': ok(TOOLS),
      'tools/call:rpc': rpcError(-32602, 'bad args', { detail: 1 }),
      'tools/call:badBlock': ok({ content: [{ type: 'text' }] }),
      'tools/call:badFlag': ok({ content: [], isError: 'yes' }),
      'tools/call:badPriority': ok({ content: [{ type: 'text', text: 'x', annotations: { priority: -1 } }] }),
      'tools/call:badBase64': ok({ content: [{ type: 'image', data: '!!!', mimeType: 'x' }] }),
      'tools/call:structured': ok({ content: [] }),
      'tools/call:structuredError': ok({ content: [], isError: true }),
      'tools/call:after': ok(text('still connected')),
    }],
    ops: [
      { op: 'callTool', name: 'rpc', args: {} },
      { op: 'callTool', name: 'badBlock', args: {} },
      { op: 'callTool', name: 'badFlag', args: {} },
      { op: 'callTool', name: 'badPriority', args: {} },
      { op: 'callTool', name: 'badBase64', args: {} },
      { op: 'listTools' },
      { op: 'callTool', name: 'task', args: {} },
      { op: 'callTool', name: 'structured', args: {} },
      { op: 'callTool', name: 'structuredError', args: {} },
      { op: 'callTool', name: 'after', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'list-errors',
    procs: [
      { initialize: INIT(), 'tools/list': ok({ tools: [{ name: 1, inputSchema: { type: 'x' } }] }) },
      { initialize: INIT(), 'tools/list': ok({ nextCursor: 3 }) },
      { initialize: INIT(), 'tools/list': ok({ tools: [] }) },
    ],
    ops: [
      { op: 'listTools' },
      { op: 'close' },
      { op: 'listTools' },
      { op: 'close' },
      { op: 'listTools' },
      { op: 'close' },
    ],
  },
  {
    id: 'invalid-frames',
    procs: [{
      initialize: [
        { line: 'not json' },
        { line: '{"foo":1}' },
        { line: '{"jsonrpc":"2.0","id":99,"result":{}}' },
        { line: '{"jsonrpc":"2.0","id":7,"method":"ping"}' },
        { line: '{"jsonrpc":"2.0","id":"s1","method":"elicitation/create","params":{}}' },
        { line: '{"jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":5,"progress":1}}' },
        { line: '{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"x"}}' },
        { sleep: 50 },
        { line: '{"jsonrpc":"2.0","id":"0","result":{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"m","version":"1"}}}\r' },
      ],
      'tools/call:split': [
        { line: '{"jsonrpc":"2.0","id":$ID,"result":{"content":[{"type":"text","text":"\\ud83d\\ude00 \\ud800"}]},"extra":1}' },
        { line: '{"jsonrpc":"2.0","id":$ID,"result":{"content":[{"type":"text","text":"second answer wins"}]}}' },
        { line: '{"jsonrpc":"2.0","id":$ID,"result":{"content":[{"type":"text","text":"late duplicate"}]}}' },
      ],
      'tools/call:unicode': [
        { line: '{"jsonrpc":"2.0","id":$ID,"result":{"content":[{"type":"text","text":"\\ud83d\\ude00 \\ud800 \\u200b\\ufffd","_meta":{"\\u202ek":["\\ufeff"]}}]}}' },
        { line: '{"jsonrpc":"2.0","id":$ID,"result":{"content":[]},"bad":"\xff"}' },
      ],
    }],
    options: { callTimeoutMs: 1000 },
    ops: [
      { op: 'start' },
      { op: 'callTool', name: 'split', args: {} },
      { op: 'callTool', name: 'unicode', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'handshake-error',
    procs: [
      { initialize: rpcError(-32603, 'boom') },
      { initialize: INIT() },
    ],
    ops: [{ op: 'start' }, { op: 'status' }, { op: 'start' }, { op: 'close' }],
  },
  {
    id: 'handshake-version',
    procs: [{ initialize: INIT({ protocolVersion: '1999-01-01' }) }],
    ops: [{ op: 'start' }, { op: 'close' }],
  },
  {
    id: 'handshake-schema',
    procs: [{ initialize: INIT({ protocolVersion: 1, serverInfo: { name: 's' } }) }],
    ops: [{ op: 'listTools' }, { op: 'close' }],
  },
  {
    id: 'handshake-crash',
    procs: [{ initialize: [{ exit: 3 }] }],
    ops: [{ op: 'callTool', name: 'x', args: {} }, { op: 'close' }],
  },
  {
    id: 'handshake-timeout',
    options: { handshakeTimeoutMs: 300 },
    procs: [{}],
    ops: [{ op: 'start' }, { op: 'close' }],
    orphan: true,
  },
  {
    id: 'spawn-enoent',
    spec: { command: '/nonexistent/pdrs-mcp-mock' },
    procs: [{}],
    ops: [{ op: 'start' }, { op: 'status' }],
  },
  {
    id: 'call-timeout',
    options: { callTimeoutMs: 300 },
    procs: [
      { initialize: INIT(), 'tools/list': ok(TOOLS), 'tools/call:slow': [] },
      { initialize: INIT(), 'tools/call:slow': ok(text('fresh process')) },
    ],
    ops: [
      { op: 'listTools' },
      { op: 'callTool', name: 'slow', args: {} },
      { op: 'status' },
      { op: 'callTool', name: 'slow', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'timeout-message',
    procs: [
      { initialize: INIT(), 'tools/call:x': rpcError(-32000, 'upstream Timeout') },
      { initialize: INIT(), 'tools/call:x': ok(text('respawned')) },
    ],
    ops: [
      { op: 'callTool', name: 'x', args: {}, timeoutMs: 5000 },
      { op: 'status' },
      { op: 'callTool', name: 'x', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'server-crash',
    procs: [{ initialize: INIT(), 'tools/call:crash': [{ exit: 3 }] }],
    ops: [
      { op: 'callTool', name: 'crash', args: {} },
      { op: 'status' },
      { op: 'callTool', name: 'crash', args: {} },
      { op: 'listTools' },
      { op: 'close' },
      { op: 'status' },
    ],
  },
  {
    id: 'session-expired',
    procs: [
      { initialize: INIT(), 'tools/list': ok({ tools: [] }), 'tools/call:x': rpcError(-32000, 'Session expired') },
      {
        initialize: INIT(),
        'tools/list': ok({ tools: [] }),
        'tools/call:x': ok(text('after reconnect')),
        'tools/call:y': rpcError(-32000, 'session has EXPIRED'),
      },
      { initialize: INIT(), 'tools/call:y': rpcError(-32000, 'Session expired again') },
    ],
    ops: [
      { op: 'listTools' },
      { op: 'callTool', name: 'x', args: {} },
      { op: 'listTools' },
      { op: 'callTool', name: 'y', args: {} },
      { op: 'status' },
      { op: 'close' },
    ],
  },
  {
    id: 'spawn-options',
    spec: {
      env: { MOCK_DIR: '<dir>', MOCK_FLAG: 'on', PILOTDECK_SESSION_TMPDIR: '<dir>/tmp/nested' },
      cwd: '<dir>/work',
      args: ['<mock>', '--flag'],
      perSession: true,
    },
    procs: [{ initialize: INIT() }],
    ops: [{ op: 'start' }, { op: 'close' }, { op: 'perSessionDirs' }],
  },
  {
    id: 'storage-root',
    spec: {
      env: { MOCK_DIR: '<dir>', PILOTDECK_SESSION_STORAGE_ROOT: '<dir>/root' },
      perSession: true,
      callTimeoutMs: 250,
    },
    procs: [{ initialize: INIT(), 'tools/call:slow': [] }],
    ops: [{ op: 'start' }, { op: 'callTool', name: 'slow', args: {} }, { op: 'close' }],
  },
  {
    id: 'close-pending',
    procs: [{ initialize: INIT() }],
    ops: [
      { op: 'start' },
      { op: 'parallel', ops: [{ op: 'callTool', name: 'hang', args: {} }, { op: 'close', delayMs: 100 }] },
      { op: 'status' },
    ],
  },
  // P1-2: one stdout write carries several frames; the replies follow the
  // JS microtask depths (Method not found, then the settled request's
  // continuation, then ping replies).
  {
    id: 'same-chunk',
    procs: [{
      initialize: [
        { line: '{"jsonrpc":"2.0","id":"p1","method":"ping"}' },
        { line: '{"jsonrpc":"2.0","id":"r1","method":"roots/list"}' },
        ...INIT(),
        { line: '{"jsonrpc":"2.0","id":"p2","method":"ping"}' },
      ],
      'tools/call:x': [
        ...ok(text('same chunk')),
        { line: '{"jsonrpc":"2.0","id":"p3","method":"ping"}' },
        { line: '{"jsonrpc":"2.0","id":"r2","method":"sampling/createMessage"}' },
      ],
      'tools/call:y': ok(text('next')),
    }],
    ops: [
      { op: 'start' },
      { op: 'callTool', name: 'x', args: {} },
      { op: 'callTool', name: 'y', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'same-chunk-failed-handshake',
    procs: [
      { initialize: [{ line: '{"jsonrpc":"2.0","id":"p1","method":"ping"}' }, ...INIT({ protocolVersion: '1999-01-01' })] },
      { initialize: [...rpcError(-32603, 'nope'), { line: '{"jsonrpc":"2.0","id":"p2","method":"ping"}' }] },
    ],
    ops: [{ op: 'start' }, { op: 'start' }, { op: 'close' }],
  },
  // Plain HTTP transports (streamable_http and legacy sse) against the same
  // mock in `--http` mode.
  {
    id: 'http-json',
    spec: { transport: 'streamable_http', url: 'http://<host>/mcp', headers: { 'X-Api-Key': 'k' } },
    procs: [],
    http: {
      initialize: [{ headers: { 'mcp-session-id': 'sess-1' }, json: HTTP_INIT }],
      'tools/list': [{ json: { jsonrpc: '2.0', id: '$id', result: { tools: TOOLS.tools.slice(0, 2) } } }],
      'tools/call:echo': [{ json: { jsonrpc: '2.0', id: '$id', result: text('over http') } }],
    },
    ops: [{ op: 'start' }, { op: 'listTools' }, { op: 'callTool', name: 'echo', args: { q: 1 } }, { op: 'close' }],
  },
  {
    id: 'http-sse',
    spec: { transport: 'streamable_http', url: 'http://<host>/mcp' },
    procs: [],
    http: {
      initialize: [{ sse: [`id: e1\nevent: message\ndata: ${sseJson(HTTP_INIT)}\n\n`] }],
      'GET /mcp': [{ sse: ['retry: 50\n\n'], hold: true }],
      'tools/call:echo': [{
        sse: [
          ': comment\n\n',
          'data: {"jsonrpc":"2.0","id":77,"method":"ping"}\n\n',
          'data: {"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info","data":"x"}}\n\n',
          'event: other\ndata: ignored\n\n',
          'data: not json\n\n',
          'data: {"jsonrpc":"2.0","id":$ID,\ndata: "result":{"content":[{"type":"text","text":"multi\\u200bline"}]}}\r\n\r\n',
        ],
      }],
      'tools/call:batch': [{
        json: [
          { jsonrpc: '2.0', method: 'notifications/message', params: {} },
          { jsonrpc: '2.0', id: '$id', result: text('batched') },
        ],
      }],
    },
    ops: [
      { op: 'start' },
      { op: 'callTool', name: 'echo', args: {} },
      { op: 'callTool', name: 'batch', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'http-errors',
    spec: { transport: 'streamable_http', url: 'http://<host>/mcp' },
    options: { callTimeoutMs: 5000 },
    procs: [],
    http: {
      initialize: [{ status: 500, text: 'boom' }, { json: HTTP_INIT }],
      'tools/call:contentType': [{ headers: { 'content-type': 'text/plain' }, text: 'x' }],
      'tools/call:badJson': [{ headers: { 'content-type': 'application/json' }, text: 'not json' }],
      'tools/call:badMessage': [{ json: { jsonrpc: '1.0', id: '$id', result: {} } }],
      'tools/call:rejected': [{ status: 400, text: 'bad request' }],
      'tools/call:expired': [
        { status: 404, text: 'Session expired' },
        { json: { jsonrpc: '2.0', id: '$id', result: text('after reconnect') } },
      ],
    },
    ops: [
      { op: 'start' },
      { op: 'start' },
      { op: 'callTool', name: 'contentType', args: {} },
      { op: 'callTool', name: 'badJson', args: {} },
      { op: 'callTool', name: 'badMessage', args: {} },
      { op: 'callTool', name: 'rejected', args: {} },
      { op: 'callTool', name: 'expired', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'http-refused',
    spec: { transport: 'streamable_http', url: 'http://<refused>/mcp' },
    procs: [],
    http: {},
    ops: [{ op: 'start' }, { op: 'status' }, { op: 'close' }],
  },
  {
    id: 'sse-legacy',
    spec: { transport: 'sse', url: 'http://<host>/sse', headers: { 'x-api-key': 'k' } },
    procs: [],
    http: {
      'GET /sse': [{ sse: [': hello\n\n', 'event: endpoint\ndata: /messages?session=abc\n\n'], channel: true }],
      initialize: [{ status: 202, push: [`data: ${sseJson(HTTP_INIT)}\n\n`] }],
      'tools/list': [{ status: 202, push: ['event: message\ndata: {"jsonrpc":"2.0","id":$ID,"result":{"tools":[]}}\n\n'] }],
      'tools/call:echo': [{
        status: 202,
        push: [
          'data: {"jsonrpc":"2.0","id":5,"method":"ping"}\n\n',
          'data: {"jsonrpc":"2.0","id":$ID,"result":{"content":[{"type":"text","text":"legacy"}]}}\n\n',
        ],
      }],
      'tools/call:rejected': [{ status: 500, text: 'nope' }],
    },
    ops: [
      { op: 'start' },
      { op: 'listTools' },
      { op: 'callTool', name: 'echo', args: {} },
      { op: 'callTool', name: 'rejected', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'sse-legacy-errors',
    spec: { transport: 'sse', url: 'http://<host>/sse' },
    procs: [],
    http: {
      'GET /sse': [
        { status: 500 },
        { sse: ['event: endpoint\ndata: http://other.example/x\n\n'], hold: true },
        { headers: { 'content-type': 'text/plain' }, text: 'x' },
        { status: 204 },
      ],
    },
    ops: [{ op: 'start' }, { op: 'start' }, { op: 'start' }, { op: 'start' }, { op: 'close' }],
  },
  {
    id: 'sse-legacy-refused',
    spec: { transport: 'sse', url: 'http://<refused>/sse' },
    procs: [],
    http: {},
    ops: [{ op: 'start' }, { op: 'close' }],
  },
];

/** Placeholders for the per-run HTTP endpoints, as `[text, placeholder]`. */
let hosts: [string, string][] = [];
function scrub(value: any, dir: string): any {
  if (typeof value === 'string') {
    let out = value.split(dir).join('<dir>');
    for (const [host, placeholder] of hosts) out = out.split(host).join(placeholder);
    return out.replace(/(pilotdeck-mcp-[^/]*-)[A-Za-z0-9]{6}/g, '$1XXXXXX');
  }
  if (Array.isArray(value)) return value.map((item) => scrub(item, dir));
  if (value && typeof value === 'object') {
    const out: Record<string, any> = {};
    for (const key of Object.keys(value)) out[key] = scrub(value[key], dir);
    return out;
  }
  return value;
}

function fill(value: any, dir: string): any {
  if (typeof value === 'string') {
    let out = value.split('<dir>').join(dir).split('<mock>').join(MOCK);
    for (const [host, placeholder] of hosts) out = out.split(placeholder).join(host);
    return out;
  }
  if (Array.isArray(value)) return value.map((item) => fill(item, dir));
  if (value && typeof value === 'object') {
    const out: Record<string, any> = {};
    for (const key of Object.keys(value)) out[key] = fill(value[key], dir);
    return out;
  }
  return value;
}

function errorValue(error: any) {
  return {
    name: error?.name ?? null,
    code: error?.code ?? null,
    serverId: error?.serverId ?? null,
    message: String(error?.message),
  };
}

async function runOp(client: any, op: Op, dir: string): Promise<any> {
  if (op.delayMs) await sleep(op.delayMs);
  try {
    switch (op.op) {
      case 'status':
        return { ok: client.getStatus() };
      case 'start':
        await client.start();
        return { ok: null, instructions: client.getInstructions() };
      case 'listTools':
        return { ok: await client.listTools() };
      case 'callTool': {
        const options = op.timeoutMs === undefined ? undefined : { timeoutMs: op.timeoutMs };
        const args = 'args' in op ? op.args : undefined;
        return { ok: await client.callTool(op.name, args, options) };
      }
      case 'close':
        await client.close();
        return { ok: null };
      case 'perSessionDirs': {
        const base = join(dir, 'tmp/nested');
        return { ok: existsSync(base) ? readdirSync(base).map((name) => scrub(name, dir)) : null };
      }
      case 'parallel':
        return { ok: await Promise.all(op.ops.map((inner: Op) => runOp(client, inner, dir))) };
      default:
        throw new Error(`unknown op ${op.op}`);
    }
  } catch (error) {
    return { error: errorValue(error) };
  }
}

function readLogs(dir: string) {
  const logs: any[] = [];
  for (let index = 0; existsSync(join(dir, `log-${index}.jsonl`)); index += 1) {
    const lines = readFileSync(join(dir, `log-${index}.jsonl`), 'utf8').split('\n').filter(Boolean);
    logs.push(lines.map((line) => scrub(JSON.parse(line), dir)));
  }
  return logs;
}

async function settle(dir: string, orphan: boolean) {
  for (let waited = 0; waited < 10_000; waited += 25) {
    const logs = readLogs(dir);
    const done = orphan
      ? logs.length > 0 && logs.every((log) => log.some((entry: any) => entry.event === 'frame'))
      : logs.every((log) => log.some((entry: any) => entry.event === 'exit'));
    if (done) return;
    await sleep(25);
  }
  throw new Error(`mock processes did not settle in ${dir}`);
}

/** A port nothing listens on: bind, read, close. */
async function refusedPort(): Promise<number> {
  const server = createServer();
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  const port = (server.address() as { port: number }).port;
  await new Promise((done) => server.close(done));
  return port;
}

/** Starts the mock in `--http` mode and resolves its port. */
async function startHttpMock(dir: string) {
  const child = spawn('node', [MOCK, '--http'], {
    env: { ...process.env, MOCK_DIR: dir },
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  // A pipe closed by an exiting recorder can surface as EPIPE; that is not a
  // transport failure.
  child.stdin.on('error', () => undefined);
  let port: number;
  try {
    port = await new Promise<number>((done, fail) => {
      let out = '';
      child.stdout.setEncoding('utf8');
      child.stdout.on('data', (chunk) => {
        out += chunk;
        const match = /^PORT (\d+)\n/.exec(out);
        if (match) done(Number(match[1]));
      });
      child.on('exit', () => fail(new Error('HTTP mock exited before listening')));
    });
  } catch (error) {
    // Do not keep a half-started mock alive when startup fails.
    child.kill('SIGKILL');
    throw error;
  }
  // Bounded: a mock that is already gone must not turn cleanup into a hang.
  const stop = () => new Promise<void>((done) => {
    if (child.exitCode !== null || child.signalCode !== null) return done();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const finish = () => { if (timer) clearTimeout(timer); done(); };
    timer = setTimeout(() => { child.kill('SIGKILL'); finish(); }, 5_000);
    child.once('exit', finish);
    try { child.stdin.end(); } catch {
      // A destroyed pipe may never deliver EOF; the 5 s deadline above still
      // kills the mock, so do not settle early and leave it running.
    }
  });
  return { port, stop };
}

function readHttpLog(dir: string) {
  const file = join(dir, 'http.jsonl');
  if (!existsSync(file)) return [];
  // Requests on separate connections may arrive in any order; the replay
  // compares the sorted multiset (ids and bodies keep each one distinct).
  return readFileSync(file, 'utf8').split('\n').filter(Boolean)
    .map((line) => JSON.stringify(scrub(JSON.parse(line), dir))).sort().map((line) => JSON.parse(line));
}

const recorded: any[] = [];
let mock: Awaited<ReturnType<typeof startHttpMock>> | undefined;
let loopDone = false;
try {
  for (const scenario of scenarios) {
    const dir = join(runRoot, scenario.id);
    mkdirSync(join(dir, 'work'), { recursive: true });
    writeFileSync(join(dir, 'script.json'), JSON.stringify({ procs: scenario.procs, http: scenario.http }));
    mock = scenario.http ? await startHttpMock(dir) : undefined;
    try {
      hosts = mock ? [[`127.0.0.1:${mock.port}`, '<host>'], [`127.0.0.1:${await refusedPort()}`, '<refused>']] : [];
      const spec = scenario.http
        ? { id: 'mock', ...scenario.spec }
        : {
          id: 'mock',
          transport: 'stdio',
          command: 'node',
          args: ['<mock>'],
          env: { MOCK_DIR: '<dir>' },
          ...scenario.spec,
        };
      const client = new McpClient(fill(spec, dir), scenario.options ?? {});
      const steps: any[] = [];
      for (const op of scenario.ops) {
        steps.push({ step: op, ...(await runOp(client, op, dir)), status: client.getStatus() });
      }
      let httpLog: any[] | undefined;
      if (mock) {
        // Let detached posts (ping replies, cancellations) land.
        await sleep(300);
        httpLog = readHttpLog(dir);
      } else {
        await settle(dir, !!scenario.orphan);
      }
      recorded.push({
        id: scenario.id,
        spec,
        options: scenario.options ?? {},
        script: { procs: scenario.procs, ...(scenario.http ? { http: scenario.http } : {}) },
        orphan: !!scenario.orphan,
        steps: scrub(steps, dir),
        processes: readLogs(dir),
        ...(httpLog ? { http: httpLog } : {}),
      });
    } finally {
      // Stop the listening mock even when a step/assertion above throws; the
      // success path also stops it here, after its log has been read.
      const current = mock;
      mock = undefined;
      if (current) await current.stop();
    }
  }
  loopDone = true;
} finally {
  // A scenario that fails before its own stop must not leave the mock (or the
  // run tree) behind.
  if (mock) await mock.stop();
  mock = undefined;
  hosts = [];
  if (!loopDone) removeOwned(runRoot);
}

// Pure helpers. Strings that JSON cannot carry exactly are avoided: every case
// round-trips through JSON.stringify unchanged.
process.env.PDRS_MCP_SET = 'value';
process.env.HOME = '/pdrs-home';
delete process.env.PDRS_MCP_UNSET;
const sanitize = [
  '',
  'plain',
  '​zero‌‍‎‏ width',
  '‪‫‬‭‮bidi⁦⁧⁨⁩',
  '﻿bom￼�',
  '中文 é \u{1F600} kept',
].map((input) => ({ input, output: sanitizeUnicodeString(input) }));
const recursive = [
  { 'a​': ['x‮', 1, null, true, { '﻿b': 'c�' }], 'a': 'collides', '2': 'n', '1​': 'm' },
  ['​', [['‌']]],
  42,
  null,
].map((input) => ({ input, output: recursivelySanitizeUnicode(input) }));
const wire = [
  ['srv', 'tool'],
  ['my server', 'a.b'],
  ['my server', 'a b'],
  ['srv', 'naïve/工具'],
  ['srv', '\u{1F600}'],
  ['', ''],
  ['a-b_c', 'd-e_f'],
].map(([server, tool]) => ({ server, tool, output: buildMcpToolWireName(server, tool) }));
const parse = [
  'mcp__srv__tool',
  'mcp__srv__a__b',
  'mcp____tool',
  'mcp__srv__',
  'mcp__srv',
  'other__srv__tool',
  'mcp___srv__tool',
].map((input) => ({ input, output: parseMcpToolWireName(input) }));
const truncate = [
  '',
  'short',
  'y'.repeat(2048),
  'y'.repeat(2049),
  LONG,
].map((input) => ({ input, output: truncateMcpToolDescription(input) }));
const expand = [
  '${env:PDRS_MCP_SET}',
  '${env:PDRS_MCP_UNSET}x',
  '${env:}',
  '${env:PDRS_MCP_SET',
  'a${env:PDRS_MCP_SET}b${env:PDRS_MCP_SET}c',
  '${userHome}/x',
  '~/a',
  '~\\a',
  '~',
  '~a',
  'x~/a',
].map((input) => ({ input, output: expandMcpString(input) }));

const configRoot = join(runRoot, 'config');
const configCases = [
  { id: 'none' },
  { id: 'merge', global: { mcpServers: { a: { command: '${env:PDRS_MCP_SET}' }, b: { url: 'g' }, 2: { n: 1 } } },
    project: { mcpServers: { b: { url: 'p', args: ['~/x', 3, null] }, c: { command: '~' }, 1: {} } } },
  { id: 'invalid-json', global: '{"mcpServers": ', project: { mcpServers: { a: { command: 'x' } } } },
  { id: 'root-array', global: [], project: 'null' },
  { id: 'servers-array', global: { mcpServers: [] }, project: { other: 1 } },
  { id: 'bom', project: '﻿{"mcpServers":{}}' },
  { id: 'directory', globalDir: true },
];
let config: any;
try {
  config = configCases.map((item: any) => {
    const dir = join(configRoot, item.id);
    const home = join(dir, 'home');
    const project = join(dir, 'project');
    mkdirSync(home, { recursive: true });
    mkdirSync(join(project, '.pilotdeck'), { recursive: true });
    const write = (path: string, value: unknown) => {
      if (value === undefined) return;
      writeFileSync(path, typeof value === 'string' ? value : JSON.stringify(value));
    };
    write(join(home, 'mcp.json'), item.global);
    write(join(project, '.pilotdeck/mcp.json'), item.project);
    if (item.globalDir) mkdirSync(join(home, 'mcp.json'));
    const result = loadMcpServerConfig(project, home);
    return { ...item, output: scrub(result, dir) };
  });
} finally {
  removeOwned(runRoot);
}

const scenario = (id: string) => recorded.find((item) => item.id === id)!;
const stepCount = recorded.reduce((sum, item) => sum + item.steps.length, 0);
// Spot checks on the behaviours the replay must reproduce.
assert.equal(scenario('basic').steps[1].instructions, '');
assert.equal(scenario('basic').processes.length, 1);
assert.equal(scenario('basic').steps[3].ok[0].wireName, scenario('basic').steps[3].ok[1].wireName);
assert.equal(scenario('call-timeout').steps[1].error.code, 'mcp_call_timeout');
assert.equal(scenario('call-timeout').processes.length, 2);
assert.equal(scenario('server-crash').steps[2].error.message, 'MCP call failed (server=mock): Not connected');
assert.equal(scenario('session-expired').processes.length, 3);
assert.equal(scenario('session-expired').steps[3].error.name, 'McpError');
assert.equal(scenario('spawn-enoent').steps[0].error.message, 'MCP handshake failed: spawn /nonexistent/pdrs-mcp-mock ENOENT');
assert.equal(scenario('handshake-timeout').processes.length, 1);
// Exact corpus size: a zero-scenario or truncated oracle must never pass.
// P1-2: in one stdout chunk, Method not found < initialized < ping replies.
const sameChunk = scenario('same-chunk').processes[0].map((entry: any) => entry.line).filter(Boolean);
assert.deepEqual(sameChunk.slice(1, 5).map((line: string) => JSON.parse(line).id ?? JSON.parse(line).method),
  ['r1', 'notifications/initialized', 'p1', 'p2']);
assert.equal(scenario('same-chunk-failed-handshake').processes[0].length, 4);
assert.equal(scenario('http-json').http.length, 5);
assert.equal(scenario('http-errors').steps[6].ok.content[0].text, 'after reconnect');
assert.equal(scenario('sse-legacy-refused').steps[0].error.message,
  'MCP handshake failed: SSE error: TypeError: fetch failed: connect ECONNREFUSED <refused>');
assert.equal(recorded.length, 26);
assert.equal(stepCount, 115);
assert.deepEqual(
  [sanitize, recursive, wire, parse, truncate, expand, config].map((cases) => cases.length),
  [6, 4, 7, 7, 5, 11, 7],
);

const oracle = {
  sourceCommit,
  sourceSha256,
  sdkVersion,
  scenarios: recorded,
  pure: { sanitize, recursive, wire, parse, truncate, expand, config },
};
writeFileSync(join(output, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.log(`PASS: ${recorded.length} scenarios, ${stepCount} steps, ${Object.keys(sourceSha256).length} source files`);
// The handshake-timeout orphan keeps its 60s initialize timer; do not wait for it.
process.exit(0);
