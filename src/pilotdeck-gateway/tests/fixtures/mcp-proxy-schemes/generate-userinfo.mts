// Real baseline install paths; every socket is 127.0.0.1. No copied TS parser.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { execFileSync, spawn } from 'node:child_process';
import { resolve, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { start } from './net.mjs';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

const baseline = resolve(process.env.PDRS_BASELINE!);
const script = fileURLToPath(import.meta.url);
const PROXY_ENV_KEYS = ['PILOTDECK_PROXY', 'https_proxy', 'HTTPS_PROXY', 'http_proxy', 'HTTP_PROXY', 'no_proxy', 'NO_PROXY'];
assert.equal(process.version, 'v22.22.0');
if (process.argv[2] === '--worker') {
  const [spec, target, proxy] = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
  const load = path => import(pathToFileURL(join(baseline, path)).href);
  const { installGlobalProxy, reinstallGlobalProxy, getGlobalProxyStateForTesting } = await load('src/cli/proxy.ts');
  const { McpClient } = await load('src/mcp/client/McpClient.ts');
  const fill = url => url?.replaceAll('<proxy>', proxy);
  if (spec.env) process.env[spec.envKey ?? 'PILOTDECK_PROXY'] = fill(spec.env);
  await installGlobalProxy();
  if (spec.config !== undefined) {
    if (spec.mode === 'reload') await reinstallGlobalProxy(fill(spec.config));
    else await installGlobalProxy(fill(spec.config));
  }
  const state = getGlobalProxyStateForTesting();
  const client = new McpClient({ id: 'userinfo', transport: 'streamable_http', url: `http://127.0.0.1:${target}/mcp` },
    { handshakeTimeoutMs: 2000, callTimeoutMs: 2000 });
  let result;
  try { await client.start(); result = { ok: true, tools: (await client.listTools()).map(tool => tool.toolName) }; }
  catch (error) { result = { ok: false, error: String(error.message) }; }
  await client.close();
  fs.writeFileSync(process.argv[4], JSON.stringify({ ...result, state: state?.mode ?? 'unset',
    source: state?.source ?? null, proxyUrl: state?.proxyUrl?.replaceAll(proxy, '<proxy>') ?? null }));
  process.exit(0);
}

const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = fs.realpathSync(execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
bindSourceTree(baseline, commit, git);
assert.equal(JSON.parse(fs.readFileSync(join(baseline, 'node_modules/undici/package.json'), 'utf8')).version, '8.3.0');
assert.equal(JSON.parse(fs.readFileSync(join(baseline, 'node_modules/@modelcontextprotocol/sdk/package.json'), 'utf8')).version, '1.29.0');
const sourceSha256 = sourceClosureSha256(baseline, commit, git,
  ['src/cli/proxy.ts', 'src/mcp/client/McpClient.ts', 'src/network/fetch.ts']);
const cases = [];
for (const scheme of ['http', 'https', 'socks5', 'socks']) {
  for (const [id, auth] of [
    ['invalid-escape', 'user%ZZ:pass'], ['invalid-utf8', 'user:%FF'],
    ['bare-percent', 'user%:pass'], ['short-escape', 'user%2:pass'],
    ['short-utf8', 'user:%C3'], ['surrogate', 'user:%ED%A0%80'],
    ['overlong', 'user:%C0%AF'], ['out-of-range', 'user:%F4%90%80%80'],
    ['password-escape', 'user:pass%GG'], ['username-utf8', '%FF:pass'],
    ['empty-username', ':%FF'], ['encoded-valid', 'us%20er:p%40ss'],
    ['escaped-percent', 'user%25ZZ:pass'],
  ]) {
    const url = `${scheme}://${auth}@<proxy>`;
    // Keep the two original failures' exact source paths: config %ZZ / env %FF.
    cases.push({ id: `${scheme}-${id}`, mode: 'startup', ...(id === 'invalid-utf8' ? { env: url, envKey: 'HTTP_PROXY' } : { config: url }) });
  }
}
cases.push(
  { id: 'bad-env-valid-config', mode: 'startup', env: 'http://user:%FF@<proxy>', config: 'http://<proxy>' },
  { id: 'bad-env-bad-config', mode: 'startup', env: 'http://user:%FF@<proxy>', config: 'http://user%ZZ:pass@<proxy>' },
  { id: 'valid-env-bad-config', mode: 'startup', env: 'http://<proxy>', config: 'http://user%ZZ:pass@<proxy>' },
  { id: 'bad-env-empty-config', mode: 'startup', env: 'http://user:%FF@<proxy>', config: '' },
  { id: 'bad-env-structural-config', mode: 'startup', env: 'http://user:%FF@<proxy>', config: 'not a url' },
  { id: 'structural-env-bad-config', mode: 'startup', env: 'not a url', config: 'http://user%ZZ:pass@<proxy>' },
  { id: 'valid-env-bad-reload', mode: 'reload', env: 'http://<proxy>', config: 'http://user%ZZ:pass@<proxy>' },
  { id: 'bad-userinfo-invalid-path', mode: 'startup', config: 'http://user%ZZ:pass@<proxy>/path' },
  { id: 'bad-userinfo-unsupported', mode: 'startup', config: 'ftp://user%ZZ:pass@<proxy>' },
);
const output = resolve(process.argv[2]); fs.mkdirSync(output, { recursive: true });
const lines = path => fs.existsSync(path) ? fs.readFileSync(path, 'utf8').trim().split('\n').filter(Boolean).map(JSON.parse) : [];
for (const spec of cases) {
  const dir = join(output, spec.id); fs.mkdirSync(dir);
  const server = await start({ scheme: 'http', transport: 'streamable_http' }, dir);
  try {
    const env = { ...process.env };
    for (const key of [...PROXY_ENV_KEYS, 'NODE_EXTRA_CA_CERTS', 'NODE_TLS_REJECT_UNAUTHORIZED']) delete env[key];
    const input = join(dir, 'input.json'), resultPath = join(dir, 'result.json');
    fs.writeFileSync(input, JSON.stringify([spec, server.target, `127.0.0.1:${server.proxy}`]));
    const child = spawn(process.execPath, [...process.execArgv, script, '--worker', input, resultPath], { env });
    let raw = ''; child.stdout.on('data', data => raw += data); child.stderr.on('data', data => raw += data);
    const timer = setTimeout(() => child.kill(), 12000);
    const status = await new Promise(resolve => child.on('exit', resolve)); clearTimeout(timer);
    fs.writeFileSync(join(dir, 'node.log'), raw); assert.equal(status, 0, `${spec.id}: ${raw}`);
    spec.ts = { ...JSON.parse(fs.readFileSync(resultPath, 'utf8')),
      methods: [...new Set(lines(join(dir, 'target.jsonl')).map(row => row.method))].sort(),
      connections: lines(join(dir, 'connection.jsonl')).length };
    assert.equal(spec.ts.ok, true, spec.id);
    console.log(JSON.stringify(spec));
  } finally { server.stop(); }
}
fs.writeFileSync(join(output, 'oracle.json'), JSON.stringify({ sourceCommit: commit, sourceSha256, node: process.version, cases }, null, 2) + '\n');
