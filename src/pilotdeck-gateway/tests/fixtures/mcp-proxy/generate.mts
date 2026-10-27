// Records the fixed TypeScript baseline's MCP client through the real
// `src/cli/proxy.ts` global dispatcher and a scripted local HTTP CONNECT proxy.
//
// The baseline's CLI installs an undici `EnvHttpProxyAgent` once at startup;
// `McpClient` itself never reads proxy configuration. To exercise the same
// path without inventing expectations, every scenario runs in a fresh Node
// child: the parent sets only that scenario's proxy environment variables and
// the child calls the real `installGlobalProxy()` before driving `McpClient`.
//
// The MCP target listens on 127.0.0.1 but is addressed as 0.0.0.0, which the
// dispatcher does not add to NO_PROXY implicitly (`buildNoProxy` appends only
// 127.0.0.1 and localhost); the proxy itself listens on 127.0.0.1.
//
// Run from the repository root with Node 22 and the baseline's tsx loader:
//   PDRS_BASELINE=/abs/path/PilotDeck LANG=en_US.UTF-8 \
//     node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/mcp-proxy/generate.mts \
//     src/pilotdeck-gateway/tests/fixtures/mcp-proxy
import assert from 'node:assert/strict';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import {
  existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync,
} from 'node:fs';
import { createServer as createHttpServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture directory');
// Error text and proxy environment handling are pinned to this runtime.
assert.equal(process.version, 'v22.22.0');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const MOCK = fileURLToPath(new URL('../mcp-client/mock_server.mjs', import.meta.url));
const PROXY = fileURLToPath(new URL('./connect_proxy.mjs', import.meta.url));
const TLS = fileURLToPath(new URL('./proxy-tls.json', import.meta.url));
const ENTRIES = [
  'src/cli/proxy.ts',
  'src/mcp/client/McpClient.ts',
  'src/network/fetch.ts',
];
const PROXY_ENV_KEYS = [
  'PILOTDECK_PROXY', 'https_proxy', 'HTTPS_PROXY', 'http_proxy', 'HTTP_PROXY',
  'no_proxy', 'NO_PROXY',
];

// ---------------------------------------------------------------- worker mode
// A child with exactly one scenario's environment. It imports the real
// modules, installs the proxy from env, runs the ops and writes the result.
if (process.argv[2] === '--worker') {
  const spec = JSON.parse(readFileSync(process.argv[3], 'utf8'));
  const load = (path) => import(pathToFileURL(join(baseline, path)).href);
  const { McpClient } = await load('src/mcp/client/McpClient.ts');
  const { installGlobalProxy, reinstallGlobalProxy, getGlobalProxyStateForTesting } = await load('src/cli/proxy.ts');
  const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
  // Same order as `pilotdeck.ts`: the CLI installs the env proxy at module
  // load, then the `server` command installs `config.proxy.url` only if that
  // did not already select an env proxy. Reload cases use
  // `reinstallGlobalProxy`, the only path that passes `proxy.noProxy`.
  await installGlobalProxy();
  if (spec.config) {
    if (spec.configMode === 'reload') {
      await reinstallGlobalProxy(spec.config.url, spec.config.noProxy);
    } else {
      await installGlobalProxy(spec.config.url);
    }
  }
  const state = getGlobalProxyStateForTesting();
  const installed = state?.mode === 'proxy';
  const errorValue = (error) => ({
    name: error?.name ?? null,
    code: error?.code ?? null,
    serverId: error?.serverId ?? null,
    message: String(error?.message),
  });
  const client = new McpClient(spec.server, spec.options ?? {});
  const steps = [];
  try {
    for (const op of spec.ops) {
      if (op.delayMs) await sleep(op.delayMs);
      let fields;
      try {
        switch (op.op) {
          case 'status':
            fields = { ok: client.getStatus() };
            break;
          case 'start':
            await client.start();
            fields = { ok: null, instructions: client.getInstructions() };
            break;
          case 'listTools':
            fields = { ok: await client.listTools() };
            break;
          case 'callTool': {
            const options = op.timeoutMs === undefined ? undefined : { timeoutMs: op.timeoutMs };
            fields = { ok: await client.callTool(op.name, 'args' in op ? op.args : undefined, options) };
            break;
          }
          case 'close':
            await client.close();
            fields = { ok: null };
            break;
          default:
            throw new Error(`unknown op ${op.op}`);
        }
      } catch (error) {
        fields = { error: errorValue(error) };
      }
      steps.push({ step: op, ...fields, status: client.getStatus() });
    }
    // Let detached posts land before the parent reads the server logs.
    await sleep(300);
  } finally {
    await client.close();
  }
  writeFileSync(process.argv[4], JSON.stringify({
    installed,
    proxyMode: state?.mode ?? null,
    source: state?.source ?? null,
    noProxy: state?.noProxy ?? null,
    steps,
  }));
  process.exit(0);
}

// Shared provenance guard (#280): explicit absolute Git, pinned HEAD, every
// tracked src byte equal to its Git blob, clean tracked tree.
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const sourceCommit = bindBaseline({ baseline, commit: BASELINE_COMMIT, gitExecutable }).provenance.ref;
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

const INIT = {
  jsonrpc: '2.0',
  id: '$id',
  result: {
    protocolVersion: '2025-06-18',
    capabilities: { tools: {} },
    serverInfo: { name: 'mock-proxy', version: '1.0.0' },
  },
};
const TOOLS = {
  jsonrpc: '2.0',
  id: '$id',
  result: {
    tools: [{
      name: 'echo',
      description: 'echo through a proxy',
      inputSchema: { type: 'object', properties: { q: { type: 'string' } } },
    }],
  },
};
const CALL = (text) => ({
  jsonrpc: '2.0',
  id: '$id',
  result: { content: [{ type: 'text', text }] },
});
const sseJson = (value) => JSON.stringify(value).split('"$id"').join('$ID');

const STREAMABLE_SCRIPT = (text) => ({
  initialize: [{ headers: { 'mcp-session-id': 'sess-1' }, json: INIT }],
  'tools/list': [{ json: TOOLS }],
  'tools/call:echo': [{ json: CALL(text) }],
});
const SSE_SCRIPT = (text) => ({
  'GET /sse': [{ sse: [': hello\n\n', 'event: endpoint\ndata: /messages?session=abc\n\n'], channel: true }],
  initialize: [{ status: 202, push: [`data: ${sseJson(INIT)}\n\n`] }],
  'tools/list': [{ status: 202, push: [`data: ${sseJson(TOOLS)}\n\n`] }],
  'tools/call:echo': [{ status: 202, push: [`data: ${sseJson(CALL(text))}\n\n`] }],
});

const SUCCESS_OPS = [
  { op: 'start' },
  { op: 'listTools' },
  { op: 'callTool', name: 'echo', args: { q: 1 } },
  { op: 'close' },
];
const FAILURE_OPS = [{ op: 'start' }, { op: 'status' }, { op: 'close' }];

const scenarios = [
  {
    // Lowest-priority variable, streamable HTTP.
    id: 'proxy-http-streamable',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    headers: { 'X-Api-Key': 'k' },
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    script: STREAMABLE_SCRIPT('over proxy http'),
    ops: SUCCESS_OPS,
  },
  {
    // `HTTPS_PROXY` is selected even though the URL is https (the baseline
    // installs the same proxy for http: and https:).
    id: 'proxy-https-streamable',
    transport: 'streamable_http',
    url: 'https://<mock>/mcp',
    headers: { 'X-Api-Key': 'k' },
    mockTls: true,
    proxy: { mode: 'open', env: 'HTTPS_PROXY' },
    script: STREAMABLE_SCRIPT('over proxy https'),
    ops: SUCCESS_OPS,
  },
  {
    // Lowercase `https_proxy` and the legacy SSE transport.
    id: 'proxy-http-sse',
    transport: 'sse',
    url: 'http://<mock>/sse',
    headers: { 'x-api-key': 'k' },
    proxy: { mode: 'open', env: 'https_proxy' },
    script: SSE_SCRIPT('over proxy sse'),
    ops: SUCCESS_OPS,
  },
  {
    // The combination the previous round left out: https target + legacy SSE
    // through an HTTP CONNECT proxy. The SSE transport opens its long-lived
    // GET stream and the POSTs as separate tunnels.
    id: 'proxy-https-sse',
    transport: 'sse',
    url: 'https://<mock>/sse',
    headers: { 'x-api-key': 'k' },
    mockTls: true,
    proxy: { mode: 'open', env: 'HTTPS_PROXY' },
    script: SSE_SCRIPT('over proxy https sse'),
    ops: SUCCESS_OPS,
  },
  {
    // Highest-priority `PILOTDECK_PROXY` plus proxy Basic auth.
    id: 'proxy-auth-https-streamable',
    transport: 'streamable_http',
    url: 'https://<mock>/mcp',
    mockTls: true,
    proxy: {
      mode: 'auth',
      env: 'PILOTDECK_PROXY',
      authUser: 'user',
      authPass: 'pass',
      urlAuth: 'good',
    },
    script: STREAMABLE_SCRIPT('over authenticated proxy'),
    ops: SUCCESS_OPS,
  },
  {
    // `HTTPS_PROXY` must outrank `HTTP_PROXY`: the latter points at a closed
    // port, so a wrong priority choice fails the handshake.
    id: 'proxy-precedence-https-over-http',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTPS_PROXY' },
    env: { HTTP_PROXY: '<closedProxy>' },
    script: STREAMABLE_SCRIPT('over precedence proxy'),
    ops: SUCCESS_OPS,
  },
  {
    // `PILOTDECK_PROXY` must outrank `HTTPS_PROXY` (closed port traps a wrong
    // order).
    id: 'proxy-precedence-pilotdeck-over-https',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'PILOTDECK_PROXY' },
    env: { HTTPS_PROXY: '<closedProxy>' },
    script: STREAMABLE_SCRIPT('over highest-precedence proxy'),
    ops: SUCCESS_OPS,
  },
  {
    // Empty env values are falsy (`||` in `getProxyUrl`), so the lowest
    // priority `http_proxy` is selected.
    id: 'proxy-empty-env-falls-through',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'http_proxy' },
    env: { PILOTDECK_PROXY: '', https_proxy: '', HTTPS_PROXY: '' },
    script: STREAMABLE_SCRIPT('over lower env after empty values'),
    ops: SUCCESS_OPS,
  },
  {
    // Uppercase NO_PROXY bypasses the proxy: the proxy is live, its log must
    // stay empty and the target still answers.
    id: 'no-proxy-http',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    noProxy: { name: 'NO_PROXY', value: '<mock>' },
    script: STREAMABLE_SCRIPT('direct, NO_PROXY hit'),
    ops: SUCCESS_OPS,
  },
  {
    // Lowercase `no_proxy` wins over `NO_PROXY` (which names the wrong host),
    // and the https target is bypassed too.
    id: 'no-proxy-https',
    transport: 'streamable_http',
    url: 'https://<mock>/mcp',
    mockTls: true,
    proxy: { mode: 'open', env: 'http_proxy' },
    noProxy: { name: 'no_proxy', value: '<mock>' },
    env: { NO_PROXY: '127.0.0.9' },
    script: STREAMABLE_SCRIPT('direct over https, no_proxy hit'),
    ops: SUCCESS_OPS,
  },
  {
    // A NO_PROXY entry that does not match the target must not bypass: the
    // proxy log pins that the proxy was used.
    id: 'no-proxy-miss',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    noProxy: { name: 'NO_PROXY', value: '127.0.0.9' },
    script: STREAMABLE_SCRIPT('through proxy, NO_PROXY misses'),
    ops: SUCCESS_OPS,
  },
  {
    // An empty lowercase `no_proxy` is falsy (`||`), so the uppercase
    // `NO_PROXY` still applies and the target is direct.
    id: 'no-proxy-empty-lowercase-uses-uppercase',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    noProxy: { name: 'no_proxy', value: '' },
    env: { NO_PROXY: '<mock>' },
    script: STREAMABLE_SCRIPT('direct, empty no_proxy falls back'),
    ops: SUCCESS_OPS,
  },
  {
    // Config source alone (`pilotdeck.yaml` `proxy.url`, no env): the server
    // command installs it after the env install found nothing.
    id: 'proxy-config-url-streamable',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open' },
    config: { url: '<proxy>' },
    configMode: 'startup',
    script: STREAMABLE_SCRIPT('over config proxy'),
    ops: SUCCESS_OPS,
  },
  {
    // `installGlobalProxy(config.url)` at startup drops `config.noProxy`
    // (only the reload path passes it), so the target still tunnels.
    id: 'proxy-config-startup-ignores-no-proxy',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open' },
    config: { url: '<proxy>', noProxy: '<mock>' },
    configMode: 'startup',
    script: STREAMABLE_SCRIPT('through config proxy, startup ignores noProxy'),
    ops: SUCCESS_OPS,
  },
  {
    // Env URL wins over `proxy.url` at startup (`installGlobalProxy`'s
    // "env source already installed" guard). The config URL is a closed
    // port, so a wrong priority fails the handshake.
    id: 'proxy-config-env-wins',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    config: { url: '<closedProxy>' },
    configMode: 'startup',
    script: STREAMABLE_SCRIPT('over env proxy, config ignored'),
    ops: SUCCESS_OPS,
  },
  {
    // Empty env URLs are skipped, so the config URL is used.
    id: 'proxy-config-after-empty-env',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open' },
    config: { url: '<proxy>' },
    configMode: 'startup',
    env: {
      PILOTDECK_PROXY: '', https_proxy: '', HTTPS_PROXY: '',
      http_proxy: '', HTTP_PROXY: '',
    },
    script: STREAMABLE_SCRIPT('over config after empty env'),
    ops: SUCCESS_OPS,
  },
  {
    // Hot-reload path: `reinstallGlobalProxy(config.url, config.noProxy)`.
    // The config noProxy entry bypasses the proxy for the target.
    id: 'proxy-config-no-proxy',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open' },
    config: { url: '<proxy>', noProxy: '<mock>' },
    configMode: 'reload',
    script: STREAMABLE_SCRIPT('direct, config noProxy hit'),
    ops: SUCCESS_OPS,
  },
  {
    // A config noProxy entry that misses still tunnels.
    id: 'proxy-config-no-proxy-miss',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open' },
    config: { url: '<proxy>', noProxy: '127.0.0.9' },
    configMode: 'reload',
    script: STREAMABLE_SCRIPT('through config proxy, noProxy misses'),
    ops: SUCCESS_OPS,
  },
  {
    // Env `no_proxy` is always part of `buildNoProxy`, even when the URL came
    // from config.
    id: 'proxy-config-env-no-proxy',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open' },
    config: { url: '<proxy>' },
    configMode: 'startup',
    noProxy: { name: 'no_proxy', value: '<mock>' },
    script: STREAMABLE_SCRIPT('direct, env no_proxy with config url'),
    ops: SUCCESS_OPS,
  },
  {
    // With an env URL installed, the config (including its noProxy) is not
    // applied at startup: the env proxy still tunnels.
    id: 'proxy-config-env-url-ignores-config',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTPS_PROXY' },
    config: { url: '<closedProxy>', noProxy: '<mock>' },
    configMode: 'startup',
    script: STREAMABLE_SCRIPT('env proxy, config noProxy ignored'),
    ops: SUCCESS_OPS,
  },
  {
    // Wrong proxy credentials: the proxy answers 407 to CONNECT. The
    // streamable transport only exposes `fetch failed`.
    id: 'proxy-auth-failure-streamable',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: {
      mode: 'auth',
      env: 'HTTP_PROXY',
      authUser: 'user',
      authPass: 'pass',
      urlAuth: 'bad',
    },
    script: {},
    ops: FAILURE_OPS,
  },
  {
    // Same 407 through the legacy SSE transport, whose EventSource flattens
    // the undici cause chain into the error message.
    id: 'proxy-auth-failure-sse',
    transport: 'sse',
    url: 'http://<mock>/sse',
    proxy: {
      mode: 'auth',
      env: 'HTTP_PROXY',
      authUser: 'user',
      authPass: 'pass',
      urlAuth: 'bad',
    },
    script: {},
    ops: FAILURE_OPS,
  },
  {
    // A proxy that rejects CONNECT with 403.
    id: 'proxy-rejected-sse',
    transport: 'sse',
    url: 'http://<mock>/sse',
    proxy: { mode: 'reject', status: 403, env: 'HTTP_PROXY' },
    script: {},
    ops: FAILURE_OPS,
  },
  {
    // Nothing listens on the configured proxy port.
    id: 'proxy-unreachable-streamable',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'closed', env: 'HTTP_PROXY' },
    script: {},
    ops: FAILURE_OPS,
  },
  {
    // The legacy SSE text must name the proxy address, not the MCP target.
    id: 'proxy-unreachable-sse',
    transport: 'sse',
    url: 'http://<mock>/sse',
    proxy: { mode: 'closed', env: 'HTTP_PROXY' },
    script: {},
    ops: FAILURE_OPS,
  },
  // ---------------------------------------------------------------- round 2
  {
    // A caller-supplied `Proxy-Authorization` header must never reach the
    // target: undici's ProxyAgent.dispatch rejects it (case-insensitively)
    // before tunnelling, so the fetch fails and no HTTP request is sent.
    id: 'proxy-auth-header-rejected-upper',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    headers: { 'Proxy-Authorization': 'Basic abc' },
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    script: {},
    ops: FAILURE_OPS,
  },
  {
    // Same check with the lowercase spelling; the legacy SSE transport
    // flattens the fetch cause into its EventSource error text.
    id: 'proxy-auth-header-rejected-lower-sse',
    transport: 'sse',
    url: 'http://<mock>/sse',
    headers: { 'proxy-authorization': 'Basic abc' },
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    script: {},
    ops: FAILURE_OPS,
  },
  {
    // `NO_PROXY` sends this request through the direct agent, which performs
    // no Proxy-Authorization check: the header reaches the target unchanged.
    // This pins that the rejection is proxy-dispatch-specific.
    id: 'proxy-auth-header-direct',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    headers: { 'Proxy-Authorization': 'Basic abc' },
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    noProxy: { name: 'NO_PROXY', value: '<mock>' },
    script: STREAMABLE_SCRIPT('direct with proxy auth header'),
    ops: SUCCESS_OPS,
  },
  {
    // `Number.parseInt('4294967296')` is a truthy port that never equals the
    // target's, so the entry must NOT bypass the proxy (Rust previously
    // treated the u32 overflow as the wildcard port 0).
    id: 'no-proxy-port-overflow',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    noProxy: { name: 'NO_PROXY', value: '0.0.0.0:4294967296' },
    script: STREAMABLE_SCRIPT('through proxy, overflow port never matches'),
    ops: SUCCESS_OPS,
  },
  {
    // `^(.+):(\d+)$` does not match a negative port, so the whole entry is a
    // hostname that cannot equal the target: the proxy is used.
    id: 'no-proxy-port-negative',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    noProxy: { name: 'NO_PROXY', value: '0.0.0.0:-1' },
    script: STREAMABLE_SCRIPT('through proxy, negative port never matches'),
    ops: SUCCESS_OPS,
  },
  {
    // Non-numeric port: same hostname fallback.
    id: 'no-proxy-port-non-numeric',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    noProxy: { name: 'NO_PROXY', value: '0.0.0.0:abc' },
    script: STREAMABLE_SCRIPT('through proxy, text port never matches'),
    ops: SUCCESS_OPS,
  },
  {
    // Empty port: `\d+` requires at least one digit, so the entry is a host.
    id: 'no-proxy-port-empty',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    noProxy: { name: 'NO_PROXY', value: '0.0.0.0:' },
    script: STREAMABLE_SCRIPT('through proxy, empty port never matches'),
    ops: SUCCESS_OPS,
  },
  {
    // `decodeURIComponent` validates `%ZZ` and throws URIError while the
    // EnvHttpProxyAgent is constructed; `applyGlobalProxy` catches it, warns,
    // and leaves no proxy installed, so the client goes direct.
    id: 'proxy-auth-decode-invalid-escape',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open' },
    config: { url: 'http://user%ZZ:pass@<proxyAuthority>' },
    configMode: 'startup',
    script: STREAMABLE_SCRIPT('direct after invalid percent escape'),
    ops: SUCCESS_OPS,
  },
  {
    // `%FF` decodes to bytes that are not valid UTF-8; decodeURIComponent
    // throws URIError, so the env proxy is never installed.
    id: 'proxy-auth-decode-invalid-utf8',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', env: 'HTTP_PROXY' },
    env: { HTTP_PROXY: 'http://user:%FF@<proxyAuthority>' },
    script: STREAMABLE_SCRIPT('direct after invalid UTF-8'),
    ops: SUCCESS_OPS,
  },
  {
    // Valid percent escapes must decode like decodeURIComponent (multi-byte
    // UTF-8 and `%2F`) before Basic auth is built; the proxy checks the exact
    // credential, so a lossy/different decode fails the CONNECT with 407.
    id: 'proxy-auth-decode-valid-escapes',
    transport: 'streamable_http',
    url: 'http://<mock>/mcp',
    proxy: { mode: 'open', authExpected: 'u/ser:päss' },
    config: { url: 'http://u%2Fser:p%C3%A4ss@<proxyAuthority>' },
    configMode: 'startup',
    script: STREAMABLE_SCRIPT('through proxy with decoded credentials'),
    ops: SUCCESS_OPS,
  },
];

function fill(value, hosts) {
  if (typeof value === 'string') {
    let out = value;
    for (const [placeholder, replacement] of hosts) {
      out = out.split(placeholder).join(replacement);
    }
    return out;
  }
  if (Array.isArray(value)) return value.map((item) => fill(item, hosts));
  if (value && typeof value === 'object') {
    const out = {};
    for (const key of Object.keys(value)) out[key] = fill(value[key], hosts);
    return out;
  }
  return value;
}

function scrub(value, hosts) {
  if (typeof value === 'string') {
    let out = value;
    for (const [needle, replacement] of hosts) out = out.split(needle).join(replacement);
    return out;
  }
  if (Array.isArray(value)) return value.map((item) => scrub(item, hosts));
  if (value && typeof value === 'object') {
    const out = {};
    for (const key of Object.keys(value)) out[key] = scrub(value[key], hosts);
    return out;
  }
  return value;
}

function startMock(dir, tlsFile) {
  const child = spawn('node', [MOCK, '--http'], {
    env: {
      ...process.env,
      MOCK_DIR: dir,
      MOCK_HOST: '0.0.0.0',
      ...(tlsFile ? { MOCK_TLS: tlsFile } : {}),
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
      if (match) ready({ port: Number(match[1]), stop: () => stopChild(child) });
    });
    child.on('exit', () => fail(new Error('HTTP mock exited before listening')));
  });
}

function startProxy(dir, scenarioProxy) {
  const log = join(dir, 'proxy.jsonl');
  const child = spawn('node', [PROXY], {
    env: {
      ...process.env,
      PROXY_LOG: log,
      ...(scenarioProxy.authExpected
        ? { PROXY_AUTH: scenarioProxy.authExpected }
        : scenarioProxy.mode === 'auth'
          ? { PROXY_AUTH: `${scenarioProxy.authUser}:${scenarioProxy.authPass}` }
          : {}),
      ...(scenarioProxy.mode === 'reject' ? { PROXY_STATUS: String(scenarioProxy.status) } : {}),
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
      if (match) ready({ port: Number(match[1]), log, stop: () => stopChild(child) });
    });
    child.on('exit', () => fail(new Error('CONNECT proxy exited before listening')));
  });
}

function stopChild(child) {
  return new Promise((done) => {
    if (child.exitCode !== null || child.signalCode !== null) return done();
    const timer = setTimeout(() => { child.kill('SIGKILL'); done(); }, 2_000);
    child.once('exit', () => { clearTimeout(timer); done(); });
    try { child.stdin.end(); } catch { /* already gone */ }
    child.kill('SIGTERM');
  });
}

/** A loopback port that is closed again before use. */
async function closedPort() {
  const server = createHttpServer();
  await new Promise((ready) => server.listen(0, '127.0.0.1', ready));
  const port = server.address().port;
  await new Promise((done) => server.close(() => done()));
  return port;
}

function proxyUrl(scenarioProxy, port) {
  const auth = scenarioProxy.mode !== 'auth'
    ? ''
    : scenarioProxy.urlAuth === 'good'
      ? `${scenarioProxy.authUser}:${scenarioProxy.authPass}@`
      : `${scenarioProxy.authUser}:wrong@`;
  return `http://${auth}127.0.0.1:${port}`;
}

const runRoot = realpathSync(mkdtempSync(join(tmpdir(), 'pdrs-mcp-proxy-')));
const caPath = join(runRoot, 'ca.pem');
writeFileSync(caPath, JSON.parse(readFileSync(TLS, 'utf8')).ca);
const recorded = [];
try {
  for (const scenario of scenarios) {
    const dir = join(runRoot, scenario.id);
    mkdirSync(dir, { recursive: true });
    writeFileSync(join(dir, 'script.json'), JSON.stringify({ http: scenario.script }));
    const mock = await startMock(dir, scenario.mockTls ? TLS : undefined);
    const proxy = scenario.proxy.mode === 'closed'
      ? undefined
      : await startProxy(dir, scenario.proxy);
    // Allocate after the live proxy holds its port; the two must differ.
    const closed = await closedPort();
    const proxyAddress = proxy?.port ?? closed;

    const replacements = [
      ['<mock>', `0.0.0.0:${mock.port}`],
      ['<proxy>', proxyUrl(scenario.proxy, proxyAddress)],
      ['<closedProxy>', `http://127.0.0.1:${closed}`],
      ['<proxyAuthority>', `127.0.0.1:${proxyAddress}`],
      ['<closedAuthority>', `127.0.0.1:${closed}`],
      ['<mockAuthority>', `0.0.0.0:${mock.port}`],
    ];
    const env = {};
    if (scenario.proxy.env) env[scenario.proxy.env] = '<proxy>';
    Object.assign(env, scenario.env ?? {});
    if (scenario.noProxy) env[scenario.noProxy.name] = scenario.noProxy.value;
    const scrubPairs = replacements.map(([placeholder, real]) => [real, placeholder]);
    const filledEnv = fill(env, replacements);
    const url = fill(scenario.url, replacements);
    const config = scenario.config ? fill(scenario.config, replacements) : null;
    const server = {
      id: 'mock',
      transport: scenario.transport,
      url,
      ...(scenario.headers ? { headers: scenario.headers } : {}),
    };
    const specPath = join(dir, 'worker-spec.json');
    const outPath = join(dir, 'worker-out.json');
    writeFileSync(specPath, JSON.stringify({
      server,
      options: scenario.options ?? {},
      ops: scenario.ops,
      config,
      configMode: scenario.configMode ?? null,
    }));
    const childEnv = { ...process.env };
    for (const key of PROXY_ENV_KEYS) delete childEnv[key];
    Object.assign(childEnv, filledEnv, { LANG: 'en_US.UTF-8' });
    if (scenario.mockTls) childEnv.NODE_EXTRA_CA_CERTS = caPath;
    const child = spawnSync(
      process.execPath,
      [...process.execArgv, fileURLToPath(import.meta.url), '--worker', specPath, outPath],
      { env: childEnv, stdio: 'inherit' },
    );
    assert.equal(child.status, 0, `${scenario.id}: worker failed`);
    const result = JSON.parse(readFileSync(outPath, 'utf8'));

    const logPath = join(dir, 'http.jsonl');
    const requests = existsSync(logPath)
      ? readFileSync(logPath, 'utf8')
        .split('\n').filter(Boolean)
        .map((line) => JSON.stringify(scrub(JSON.parse(line), scrubPairs))).sort()
        .map((line) => JSON.parse(line))
      : [];
    const proxyLogPath = proxy?.log;
    const connects = proxyLogPath && existsSync(proxyLogPath)
      ? [...new Map(
        readFileSync(proxyLogPath, 'utf8').split('\n').filter(Boolean)
          .map((line) => JSON.stringify(scrub(JSON.parse(line), scrubPairs)))
          .map((line) => [line, JSON.parse(line)]),
      ).values()].sort((a, b) => JSON.stringify(a).localeCompare(JSON.stringify(b)))
      : [];
    recorded.push({
      id: scenario.id,
      transport: scenario.transport,
      url: scenario.url,
      headers: scenario.headers ?? {},
      options: scenario.options ?? {},
      mockTls: !!scenario.mockTls,
      proxy: scenario.proxy,
      env: scenario.env ?? {},
      noProxy: scenario.noProxy ?? null,
      config: scenario.config ?? null,
      configMode: scenario.configMode ?? null,
      script: scenario.script,
      ops: scenario.ops,
      installed: result.installed,
      proxyMode: result.proxyMode,
      source: result.source,
      noProxyValue: scrub(result.noProxy, scrubPairs),
      steps: scrub(result.steps, scrubPairs),
      requests,
      connects,
    });
    await mock.stop();
    await proxy?.stop();
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
assert.ok(!text.includes(runRoot), 'oracle leaked the run root');
// A real authority is `host:port` with a 1-5 digit port; the NO_PROXY cases
// intentionally record fake oversized ports (`4294967296`) which must not be
// mistaken for one.
assert.ok(!/0\.0\.0\.0:\d{1,5}(?!\d)/.test(text), 'oracle leaked a target authority');
assert.ok(!/127\.0\.0\.1:\d{1,5}(?!\d)/.test(text), 'oracle leaked a proxy authority');
mkdirSync(output, { recursive: true });
writeFileSync(join(output, 'oracle.json'), text);
console.log(`mcp-proxy oracle: ${recorded.length} real TS proxy cases`);
process.exit(0);
