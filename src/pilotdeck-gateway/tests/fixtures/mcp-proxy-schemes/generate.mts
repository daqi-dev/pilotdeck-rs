// Real cc7c4e6 imports; no copied TS implementation. All sockets are loopback.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { spawn, execFileSync } from 'node:child_process';
import { resolve, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { start } from './net.mjs';
import {sourceClosureSha256} from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
const baseline = resolve(process.env.PDRS_BASELINE!);
const script = fileURLToPath(import.meta.url);
const keys = ['PILOTDECK_PROXY', 'https_proxy', 'HTTPS_PROXY', 'http_proxy', 'HTTP_PROXY', 'no_proxy', 'NO_PROXY'];
assert.equal(process.version, 'v22.22.0');
if (process.argv[2] === '--worker') {
  const [spec, url, proxy] = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
  const load = path => import(pathToFileURL(join(baseline, path)).href);
  const { installGlobalProxy, reinstallGlobalProxy, getGlobalProxyStateForTesting } = await load('src/cli/proxy.ts');
  const { McpClient } = await load('src/mcp/client/McpClient.ts');
  process.env.PILOTDECK_PROXY = spec.install === 'fallback' || spec.install === 'invalid' ? spec.invalid : proxy;
  await installGlobalProxy();
  if (spec.install === 'fallback') await installGlobalProxy(proxy);
  if (spec.install === 'reload') await reinstallGlobalProxy(spec.invalid);
  const client = new McpClient({ id: 'local', transport: spec.transport, url }, { handshakeTimeoutMs: 2000, callTimeoutMs: 2000 });
  let result;
  try {
    await client.start();
    const tools = await client.listTools();
    result = { ok: true, tools: tools.map(tool => tool.toolName) };
  } catch (e) { result = { ok: false, error: { code: e.code, message: e.message } }; }
  await client.close();
  result.state = getGlobalProxyStateForTesting()?.mode ?? 'unset';
  fs.writeFileSync(process.argv[4], JSON.stringify(result));
  process.exit(0);
}
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = fs.realpathSync(execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
bindBaseline({ baseline, commit, gitExecutable: git });
const sourceSha256 = sourceClosureSha256(baseline, commit, git, ['src/cli/proxy.ts', 'src/mcp/client/McpClient.ts', 'src/network/fetch.ts']);
assert.equal(JSON.parse(fs.readFileSync(join(baseline, 'node_modules/undici/package.json'), 'utf8')).version, '8.3.0');
const output = resolve(process.argv[2]); fs.mkdirSync(output, { recursive: true });
const ca = join(output, 'ca.pem'); fs.writeFileSync(ca, JSON.parse(fs.readFileSync(new URL('./tls.json', import.meta.url))).ca);
const cases = [
  { id: 'http', scheme: 'http' },
  { id: 'https-stream', scheme: 'https', targetTls: true },
  { id: 'https-sse', scheme: 'https', targetTls: true, transport: 'sse' },
  { id: 'https-auth', scheme: 'https', auth: 'user:pass' },
  { id: 'https-untrusted', scheme: 'https', trust: false, failure: true },
  { id: 'https-reject', scheme: 'https', reject: true, failure: true },
  { id: 'socks5', scheme: 'socks5' },
  { id: 'socks', scheme: 'socks', targetTls: true, transport: 'sse' },
  { id: 'socks-auth', scheme: 'socks5', auth: 'user:pass' },
  { id: 'socks-encoded', scheme: 'socks', auth: 'us%20er:p%40ss' },
  { id: 'socks-auth-reject', scheme: 'socks5', auth: 'user:pass', badAuth: true, failure: true },
  { id: 'socks-reject', scheme: 'socks5', reject: true, failure: true },
  { id: 'socks-method-reject', scheme: 'socks', badMethod: true, failure: true },
  { id: 'invalid-reload', scheme: 'http', install: 'reload', invalid: 'not a url', failClosed: true },
  { id: 'unsupported-reload', scheme: 'http', install: 'reload', invalid: 'ftp://127.0.0.1:9', failClosed: true },
  { id: 'bad-userinfo-reload', scheme: 'http', install: 'reload', invalid: 'http://user%ZZ:pass@127.0.0.1:9', failClosed: true },
  { id: 'invalid-path-reload', scheme: 'http', install: 'reload', invalid: 'http://127.0.0.1:9/path', failClosed: true },
  { id: 'invalid-query-reload', scheme: 'http', install: 'reload', invalid: 'https://127.0.0.1:9/?q', failClosed: true },
  { id: 'invalid-fragment-reload', scheme: 'http', install: 'reload', invalid: 'http://127.0.0.1:9/#fragment', failClosed: true },
  { id: 'invalid-startup', scheme: 'http', install: 'invalid', invalid: 'not a url', failClosed: true },
  { id: 'unsupported-startup', scheme: 'http', install: 'invalid', invalid: 'ftp://127.0.0.1:9', failClosed: true },
  { id: 'invalid-env-config-fallback', scheme: 'https', install: 'fallback', invalid: 'not a url' },
].map(spec => ({ transport: 'streamable_http', ...spec }));
const lines = path => fs.existsSync(path) ? fs.readFileSync(path, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : [];
for (const spec of cases) {
  const dir = join(output, spec.id); fs.mkdirSync(dir);
  const server = await start(spec, dir);
  try {
    const host = spec.install === 'invalid' ? '127.0.0.1' : 'mcp.test';
    const url = `${spec.targetTls ? 'https' : 'http'}://${host}:${server.target}/mcp`;
    const auth = spec.auth ? `${spec.badAuth ? 'wrong:wrong' : spec.auth}@` : '';
    const proxy = `${spec.scheme}://${auth}127.0.0.1:${server.proxy}`;
    const env = { ...process.env }; for (const key of [...keys, 'NODE_EXTRA_CA_CERTS', 'NODE_TLS_REJECT_UNAUTHORIZED']) delete env[key];
    if (spec.trust !== false) env.NODE_EXTRA_CA_CERTS = ca;
    fs.writeFileSync(join(dir, 'input.json'), JSON.stringify([spec, url, proxy]));
    const child = spawn(process.execPath, [...process.execArgv, script, '--worker', join(dir, 'input.json'), join(dir, 'result.json')], { env });
    let raw = ''; child.stdout.on('data', chunk => raw += chunk); child.stderr.on('data', chunk => raw += chunk);
    const timer = setTimeout(() => child.kill(), 12000);
    const status = await new Promise(resolve => child.on('exit', resolve)); clearTimeout(timer);
    fs.writeFileSync(join(dir, 'node.log'), raw); assert.equal(status, 0, `${spec.id}: ${raw}`);
    const result = JSON.parse(fs.readFileSync(join(dir, 'result.json'), 'utf8'));
    if (result.error) result.error.message = result.error.message.replaceAll(String(server.proxy), '<proxy>').replaceAll(String(server.target), '<target>');
    spec.ts = { ...result, proxy: [...new Set(lines(join(dir, 'proxy.jsonl')).map(x => JSON.stringify(x)))].map(JSON.parse),
      methods: [...new Set(lines(join(dir, 'target.jsonl')).map(x => x.method).filter(x => ['initialize', 'tools/list'].includes(x)))].sort() };
    for (const name of ['handshake', 'auth', 'connection']) {
      spec.ts[name] = [...new Set(lines(join(dir, `${name}.jsonl`)).map(JSON.stringify))].map(JSON.parse);
    }
    assert.equal(result.ok, !spec.failure, spec.id);
    console.log(JSON.stringify(spec));
  } finally { server.stop(); }
}
fs.writeFileSync(join(output, 'oracle.json'), JSON.stringify({ sourceCommit: commit, sourceSha256, node: process.version, cases }) + '\n');
