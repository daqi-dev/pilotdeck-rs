// Records the fixed TypeScript baseline's MCP client over a real local TLS
// server: `src/mcp/client/McpClient.ts` with the real SDK
// `StreamableHTTPClientTransport` / `SSEClientTransport`, driven against the
// same scripted server the Rust replay in `tests/mcp_https.rs` starts.
//
// The baseline's streamable transport fetches through `src/network/fetch.ts`
// (the baseline's bundled undici) and the legacy SSE transport through the
// global `fetch`; both trust `NODE_EXTRA_CA_CERTS`. The generator therefore
// re-executes itself once with that variable pointing at the shared throwaway
// fixture CA, so the oracle is recorded over real TLS with certificate
// verification on. No production trust store is touched.
//
// Run from the repository root with Node 22 and the baseline's tsx loader:
//   PDRS_BASELINE=/abs/path/PilotDeck LANG=en_US.UTF-8 \
//     node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/mcp-https/generate.mts \
//     src/pilotdeck-gateway/tests/fixtures/mcp-https
import assert from 'node:assert/strict';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import {
  existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync,
} from 'node:fs';
import { createServer as createHttpServer } from 'node:http';
import { createServer as createHttpsServer } from 'node:https';
import { createServer as createNetServer } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
// The oracle's bytes depend on this runtime (error text, redirect and
// connect-timeout behaviour), so pin it like the hooks HTTPS fixture does.
assert.equal(process.version, 'v22.22.0');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const MOCK = fileURLToPath(new URL('../mcp-client/mock_server.mjs', import.meta.url));
const TLS = fileURLToPath(new URL('../model-stream/tls.json', import.meta.url));
const ENTRIES = ['src/mcp/client/McpClient.ts', 'src/network/fetch.ts'];

// The legacy SSE transport uses the global `fetch`, whose trust store only
// honours NODE_EXTRA_CA_CERTS at process start. Re-exec once with that
// variable set to a temporary PEM holding the fixture CA, then record.
if (process.env.PDRS_MCP_HTTPS_REEXEC !== '1') {
  const material = JSON.parse(readFileSync(TLS, 'utf8'));
  const dir = realpathSync(mkdtempSync(join(tmpdir(), 'pdrs-mcp-https-ca-')));
  const ca = join(dir, 'ca.pem');
  writeFileSync(ca, material.ca);
  const child = spawnSync(
    process.execPath,
    [...process.execArgv, fileURLToPath(import.meta.url), output],
    {
      env: {
        ...process.env,
        NODE_EXTRA_CA_CERTS: ca,
        PDRS_MCP_HTTPS_REEXEC: '1',
      },
      stdio: 'inherit',
    },
  );
  rmSync(dir, { recursive: true, force: true });
  process.exit(child.status ?? 1);
}

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

const load = (path) => import(pathToFileURL(join(baseline, path)).href);
const { McpClient } = await load('src/mcp/client/McpClient.ts');

const material = JSON.parse(readFileSync(TLS, 'utf8'));

const sleep = (ms) => new Promise((done) => setTimeout(done, ms));

const INIT = {
  jsonrpc: '2.0',
  id: '$id',
  result: {
    protocolVersion: '2025-06-18',
    capabilities: { tools: {} },
    serverInfo: { name: 'mock-tls', version: '1.0.0' },
  },
};
const TOOLS = {
  jsonrpc: '2.0',
  id: '$id',
  result: {
    tools: [{
      name: 'echo',
      description: 'echo over TLS',
      inputSchema: { type: 'object', properties: { q: { type: 'string' } } },
    }],
  },
};
const CALL = (text) => ({
  jsonrpc: '2.0',
  id: '$id',
  result: { content: [{ type: 'text', text }] },
});
/** JSON text for an SSE `data:` line; the mock substitutes `$ID`. */
const sseJson = (value) => JSON.stringify(value).split('"$id"').join('$ID');

const scenarios = [
  {
    id: 'https-streamable',
    transport: 'streamable_http',
    url: 'https://<mock>/mcp',
    headers: { 'X-Api-Key': 'k' },
    mockTls: true,
    script: {
      initialize: [{ headers: { 'mcp-session-id': 'sess-1' }, json: INIT }],
      'tools/list': [{ json: TOOLS }],
      'tools/call:echo': [{ json: CALL('over https') }],
    },
    ops: [
      { op: 'start' },
      { op: 'listTools' },
      { op: 'callTool', name: 'echo', args: { q: 1 } },
      { op: 'close' },
    ],
  },
  {
    id: 'https-sse',
    transport: 'sse',
    url: 'https://<mock>/sse',
    headers: { 'x-api-key': 'k' },
    mockTls: true,
    script: {
      'GET /sse': [{ sse: [': hello\n\n', 'event: endpoint\ndata: /messages?session=abc\n\n'], channel: true }],
      initialize: [{ status: 202, push: [`data: ${sseJson(INIT)}\n\n`] }],
      'tools/list': [{ status: 202, push: [`data: ${sseJson(TOOLS)}\n\n`] }],
      'tools/call:echo': [{ status: 202, push: [`data: ${sseJson(CALL('over sse https'))}\n\n`] }],
    },
    ops: [
      { op: 'start' },
      { op: 'listTools' },
      { op: 'callTool', name: 'echo', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'http-to-https-redirect',
    transport: 'streamable_http',
    url: 'http://<redirect>/mcp',
    // 307 keeps the POST method and body; 301/302/303 would turn the
    // initialize POST into a bodyless GET (fetch's redirect rules).
    redirect: { status: 307, mockTls: true },
    headers: { 'X-Api-Key': 'k', Authorization: 'Bearer secret' },
    mockTls: true,
    script: {
      initialize: [{ json: INIT }],
      'tools/list': [{ json: TOOLS }],
      'tools/call:echo': [{ json: CALL('over redirect') }],
    },
    ops: [
      { op: 'start' },
      { op: 'listTools' },
      { op: 'callTool', name: 'echo', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'https-to-http-downgrade',
    transport: 'streamable_http',
    url: 'https://<redirect>/mcp',
    redirect: { status: 307, mockTls: false },
    headers: { 'X-Api-Key': 'k', Authorization: 'Bearer secret' },
    mockTls: false,
    script: {
      initialize: [{ json: INIT }],
      'tools/list': [{ json: TOOLS }],
      'tools/call:echo': [{ json: CALL('over downgrade') }],
    },
    ops: [
      { op: 'start' },
      { op: 'listTools' },
      { op: 'callTool', name: 'echo', args: {} },
      { op: 'close' },
    ],
  },
  {
    id: 'https-handshake-stall',
    transport: 'streamable_http',
    url: 'https://<stall>/mcp',
    stall: true,
    // Deliberately longer than undici's 10 s connectTimeout: the error must
    // be the connect deadline, not the client's own handshake timer.
    options: { handshakeTimeoutMs: 30_000 },
    script: {},
    ops: [{ op: 'start' }, { op: 'status' }, { op: 'close' }],
  },
  {
    id: 'https-refused',
    transport: 'streamable_http',
    url: 'https://<closed>/mcp',
    closed: true,
    script: {},
    ops: [{ op: 'start' }, { op: 'status' }, { op: 'close' }],
  },
];

function startMock(dir, tls) {
  const child = spawn('node', [MOCK, '--http'], {
    env: {
      ...process.env,
      MOCK_DIR: dir,
      ...(tls ? { MOCK_TLS: TLS } : {}),
    },
    stdio: ['pipe', 'pipe', 'inherit'],
  });
  child.stdin.on('error', () => undefined);
  return new Promise((ready, fail) => {
    let out = '';
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', (chunk) => {
      out += chunk;
      const match = /^PORT (\d+)\n/.exec(out);
      if (match) {
        ready({
          port: Number(match[1]),
          stop: () => new Promise((done) => {
            if (child.exitCode !== null || child.signalCode !== null) return done();
            const timer = setTimeout(() => { child.kill('SIGKILL'); done(); }, 5_000);
            child.once('exit', () => { clearTimeout(timer); done(); });
            try { child.stdin.end(); } catch { /* already gone */ }
          }),
        });
      }
    });
    child.on('exit', () => fail(new Error('HTTP mock exited before listening')));
  });
}

/** A loopback server that answers every request with a redirect. */
async function startRedirect(tls, status, target) {
  const handler = (req, res) => {
    const chunks = [];
    req.on('data', (chunk) => chunks.push(chunk));
    req.on('end', () => {
      res.writeHead(status, { location: `${target}${req.url ?? '/'}` });
      res.end();
    });
  };
  const server = tls
    ? createHttpsServer({ key: material.key, cert: material.cert }, handler)
    : createHttpServer(handler);
  await new Promise((ready) => server.listen(0, '127.0.0.1', ready));
  return {
    port: server.address().port,
    // undici keeps its keep-alive socket open; `close()` alone would wait.
    stop: () => new Promise((done) => {
      server.close(() => done());
      server.closeAllConnections?.();
    }),
  };
}

/** Accepts TCP but never answers the TLS ClientHello. */
async function startStall() {
  const sockets = [];
  const server = createNetServer((socket) => {
    sockets.push(socket);
    socket.on('error', () => undefined);
  });
  await new Promise((ready) => server.listen(0, '127.0.0.1', ready));
  return {
    port: server.address().port,
    stop: async () => {
      for (const socket of sockets) socket.destroy();
      await new Promise((done) => server.close(() => done()));
    },
  };
}

/** A loopback port that is closed again before use. */
async function closedPort() {
  const server = createHttpServer();
  await new Promise((ready) => server.listen(0, '127.0.0.1', ready));
  const port = server.address().port;
  await new Promise((done) => server.close(() => done()));
  return port;
}

/** Placeholders for this run's loopback endpoints. */
let hosts = [];
/** Every real origin this run used, to prove none leaked into the oracle. */
const origins = [];
function scrub(value) {
  if (typeof value === 'string') {
    let out = value;
    for (const host of hosts) {
      out = out.split(host.origin).join(host.placeholder)
        .split(host.other).join(host.placeholder)
        .split(host.authority).join(host.placeholder);
    }
    return out;
  }
  if (Array.isArray(value)) return value.map(scrub);
  if (value && typeof value === 'object') {
    const out = {};
    for (const key of Object.keys(value)) out[key] = scrub(value[key]);
    return out;
  }
  return value;
}
/** Templates carry the scheme (`https://<mock>/mcp`); fill the authority. */
function fill(value) {
  if (typeof value === 'string') {
    let out = value;
    for (const host of hosts) out = out.split(host.placeholder).join(host.authority);
    return out;
  }
  if (Array.isArray(value)) return value.map(fill);
  if (value && typeof value === 'object') {
    const out = {};
    for (const key of Object.keys(value)) out[key] = fill(value[key]);
    return out;
  }
  return value;
}

function errorValue(error) {
  return {
    name: error?.name ?? null,
    code: error?.code ?? null,
    serverId: error?.serverId ?? null,
    message: String(error?.message),
  };
}

async function runOp(client, op) {
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
        return { ok: await client.callTool(op.name, 'args' in op ? op.args : undefined, options) };
      }
      case 'close':
        await client.close();
        return { ok: null };
      default:
        throw new Error(`unknown op ${op.op}`);
    }
  } catch (error) {
    return { error: errorValue(error) };
  }
}

const runRoot = realpathSync(mkdtempSync(join(tmpdir(), 'pdrs-mcp-https-')));
const recorded = [];
try {
  for (const scenario of scenarios) {
    const dir = join(runRoot, scenario.id);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'script.json'), JSON.stringify({ http: scenario.script }));
    hosts = [];
    const addHost = (scheme, port, placeholder) => {
      const authority = `127.0.0.1:${port}`;
      const origin = `${scheme}://${authority}`;
      const other = `${scheme === 'https' ? 'http' : 'https'}://${authority}`;
      hosts.push({ origin, other, authority, placeholder });
      origins.push(origin, other, authority);
    };
    const mock = scenario.mockTls || scenario.redirect ? await startMock(dir, scenario.mockTls) : undefined;
    let redirect;
    let stall;
    if (mock) {
      addHost(scenario.mockTls ? 'https' : 'http', mock.port, '<mock>');
    }
    if (scenario.redirect) {
      const scheme = scenario.url.startsWith('https:') ? 'https' : 'http';
      redirect = await startRedirect(
        scheme === 'https',
        scenario.redirect.status,
        `${scenario.redirect.mockTls ? 'https' : 'http'}://127.0.0.1:${mock.port}`,
      );
      addHost(scheme, redirect.port, '<redirect>');
    }
    if (scenario.stall) {
      stall = await startStall();
      addHost('https', stall.port, '<stall>');
    }
    let closed;
    if (scenario.closed) {
      closed = await closedPort();
      addHost('https', closed, '<closed>');
    }
    let steps = [];
    try {
      const spec = {
        id: 'mock',
        transport: scenario.transport,
        url: fill(scenario.url),
        ...(scenario.headers ? { headers: scenario.headers } : {}),
      };
      const client = new McpClient(spec, scenario.options ?? {});
      for (const op of scenario.ops) {
        steps.push({ step: op, ...(await runOp(client, op)), status: client.getStatus() });
      }
      // Let detached posts land before reading the server log.
      await sleep(300);
    } finally {
      const stops = [
        mock?.stop(),
        redirect?.stop(),
        stall?.stop(),
      ].filter(Boolean);
      await Promise.all(stops);
    }
    const logPath = join(dir, 'http.jsonl');
    const requests = existsSync(logPath)
      ? readFileSync(logPath, 'utf8')
        .split('\n').filter(Boolean)
        .map((line) => JSON.stringify(scrub(JSON.parse(line)))).sort()
        .map((line) => JSON.parse(line))
      : [];
    recorded.push({
      id: scenario.id,
      transport: scenario.transport,
      url: scenario.url,
      headers: scenario.headers ?? {},
      options: scenario.options ?? {},
      mockTls: !!scenario.mockTls,
      redirect: scenario.redirect ?? null,
      stall: !!scenario.stall,
      closed: !!scenario.closed,
      script: scenario.script,
      ops: scenario.ops,
      steps: scrub(steps),
      requests,
    });
  }
} finally {
  rmSync(runRoot, { recursive: true, force: true });
}

const oracle = {
  sourceCommit,
  nodeVersion: process.version,
  sourceSha256,
  sdkVersion,
  cases: recorded,
};
const text = JSON.stringify(oracle, null, 2) + '\n';
assert.ok(!text.includes(baseline), 'oracle leaked the baseline path');
for (const origin of origins) {
  assert.ok(!text.includes(origin), `oracle leaked the run-varying origin ${origin}`);
}
mkdirSync(output, { recursive: true });
writeFileSync(join(output, 'oracle.json'), text);
console.log(`mcp-https oracle: ${recorded.length} real TS TLS cases`);
process.exit(0);
