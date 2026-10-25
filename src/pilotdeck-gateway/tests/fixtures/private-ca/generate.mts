// Real cc7c4e6 clients, fresh Node process per trust-source/hostname cell.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import { spawn, execFileSync } from 'node:child_process';
import { join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { start } from './server.mjs';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';
const baseline = resolve(process.env.PDRS_BASELINE!);
const script = fileURLToPath(import.meta.url);
assert.equal(process.version, 'v22.22.0');
const load = path => import(pathToFileURL(join(baseline, path)).href);
if (process.argv[2] === '--worker') {
  const spec = JSON.parse(fs.readFileSync(process.argv[3], 'utf8'));
  if (spec.mode === 'early-file') fs.writeFileSync(spec.ca, fs.readFileSync(spec.restore));
  if (spec.mode === 'late-env') process.env.NODE_EXTRA_CA_CERTS = spec.ca;
  const { installGlobalProxy } = await load('src/cli/proxy.ts');
  await installGlobalProxy();
  const { McpClient } = await load('src/mcp/client/McpClient.ts');
  const { HttpHookExecutor } = await load('src/extension/hooks/execution/HttpHookExecutor.ts');
  const { parseModelConfig } = await load('src/model/config/parseModelConfig.ts');
  const { complete } = await load('src/model/streaming/streamModel.ts');
  const { TokenAccountingRuntime } = await load('src/context/budget/TokenAccountingRuntime.ts');
  const ca = spec.mode === 'config' ? fs.readFileSync(spec.ca, 'utf8') : undefined;
  const config = parseModelConfig({ providers: { tls: { protocol: 'anthropic', url: spec.url, apiKey: 'fixture-only', timeoutMs: 1500, retry: { requestMaxRetries: 0 }, models: { probe: {} }, ca } } });
  const request = { provider: 'tls', model: 'probe', messages: [{ role: 'user', content: [{ type: 'text', text: 'local TLS test' }] }] };
  const results = {};
  const record = async (name, run) => { try { results[name] = await run(); } catch (e) { results[name] = { ok: false, error: e.message, code: e.code }; } };
  await record('mcp', async () => {
    const client = new McpClient({ id: 'tls', transport: 'streamable_http', url: `${spec.url}/mcp`, ca }, { handshakeTimeoutMs: 1500, callTimeoutMs: 1500 });
    try { await client.start(); return { ok: true }; } finally { await client.close(); }
  });
  await record('hook', async () => {
    const value = await new HttpHookExecutor().execute({ hook: { type: 'http', url: `${spec.url}/hook`, ca }, hookInput: { hook_event_name: 'PreToolUse' }, signal: AbortSignal.timeout(1500) });
    return { ok: value.outcome === 'success', value };
  });
  await record('model', async () => ({ ok: true, value: await complete(request, config, { signal: AbortSignal.timeout(1500) }) }));
  await record('count', async () => {
    const value = await new TokenAccountingRuntime({ modelConfig: config, timeoutMs: 1500, cacheSize: 0 }).countRequestInput(request);
    return { ok: value.source === 'provider', value };
  });
  if (spec.mode === 'cached-file') {
    fs.writeFileSync(spec.ca, '');
    await record('cached', async () => ({ ok: (await new HttpHookExecutor().execute({ hook: { type: 'http', url: `${spec.fresh}/hook` }, hookInput: {}, signal: AbortSignal.timeout(1500) })).outcome === 'success' }));
  }
  console.log(JSON.stringify(results)); process.exit(0);
}
const output = resolve(process.argv[2]), work = resolve(process.argv[3]);
fs.mkdirSync(output, { recursive: true }); fs.mkdirSync(work, { recursive: true });
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = fs.realpathSync(execFileSync('which', ['git'], { encoding: 'utf8' }).trim());
bindSourceTree(baseline, commit, git);
const sourceSha256 = sourceClosureSha256(baseline, commit, git, ['src/cli/proxy.ts', 'src/mcp/client/McpClient.ts', 'src/extension/hooks/execution/HttpHookExecutor.ts', 'src/model/streaming/streamModel.ts', 'src/context/budget/TokenAccountingRuntime.ts']);
const server = await start(work), ca = join(work, 'ca.pem');
const caBytes = fs.readFileSync(ca);
const modes = ['bundled', 'extra', 'bundle', 'partial', 'missing', 'empty', 'bad', 'other', 'system-flag', 'system-env', 'system-extra', 'system-empty', 'system-dir', 'system-off', 'ssl-unflagged', 'system-options', 'forced-bundled', 'bundled-system', 'dir-unflagged', 'dir-plain', 'bad-first', 'bad-der', 'early-file', 'config', 'late-env', 'cached-file', 'proxy-extra', 'proxy-none'];
const cases = [];
const selected = process.env.PDRS_MODES?.split(',') ?? modes;
try {
  for (const mode of selected) for (const mismatch of [false, true]) {
    fs.writeFileSync(ca, caBytes);
    const spec = { mode, mismatch, ca, restore: join(work, 'initial-ca.pem'), fresh: mismatch ? server.freshBad : server.freshGood, url: mode.startsWith('proxy-') ? server.target : mismatch ? server.bad : server.good };
    const env = { ...process.env };
    for (const key of ['PILOTDECK_PROXY', 'https_proxy', 'HTTPS_PROXY', 'http_proxy', 'HTTP_PROXY', 'no_proxy', 'NO_PROXY', 'NODE_EXTRA_CA_CERTS', 'NODE_OPTIONS', 'NODE_USE_SYSTEM_CA', 'NODE_TLS_REJECT_UNAUTHORIZED', 'SSL_CERT_FILE', 'SSL_CERT_DIR']) delete env[key];
    if (['extra', 'early-file', 'cached-file', 'system-extra', 'proxy-extra'].includes(mode)) env.NODE_EXTRA_CA_CERTS = ca;
    if (['bundle', 'partial', 'missing', 'empty', 'bad', 'other', 'bad-first', 'bad-der'].includes(mode)) env.NODE_EXTRA_CA_CERTS = join(work, `${mode}.pem`);
    if (mode.startsWith('system-') || mode === 'ssl-unflagged') { env.SSL_CERT_FILE = ca; env.SSL_CERT_DIR = join(work, 'empty-dir'); }
    if (['system-empty', 'system-dir'].includes(mode)) env.SSL_CERT_FILE = join(work, 'empty.pem');
    if (mode === 'system-dir') env.SSL_CERT_DIR = join(work, 'cert-dir');
    if (mode === 'system-off') env.NODE_USE_SYSTEM_CA = '0';
    if (mode === 'system-options') env.NODE_OPTIONS = '--use-system-ca';
    if (['system-env', 'system-extra', 'system-empty', 'system-dir'].includes(mode)) env.NODE_USE_SYSTEM_CA = '1';
    if (mode.startsWith('proxy-')) env.PILOTDECK_PROXY = mismatch ? server.badProxy : server.proxy;
    if (['forced-bundled', 'bundled-system', 'dir-unflagged', 'dir-plain'].includes(mode)) { env.SSL_CERT_FILE = ca; env.SSL_CERT_DIR = join(work, 'empty-dir'); }
    if (mode.startsWith('dir-')) { env.SSL_CERT_FILE = join(work, 'empty.pem'); env.SSL_CERT_DIR = join(work, mode === 'dir-plain' ? 'plain-dir' : 'cert-dir'); }
    if (mode === 'early-file') fs.writeFileSync(ca, '');
    const input = join(work, `${mode}-${mismatch}.json`); fs.writeFileSync(input, JSON.stringify(spec));
    const args = [...(mode === 'system-flag' ? ['--use-system-ca'] : mode === 'bundled-system' ? ['--use-bundled-ca', '--use-system-ca'] : mode === 'forced-bundled' ? ['--use-bundled-ca'] : []), '--import', join(baseline, 'node_modules/tsx/dist/loader.mjs'), script, '--worker', input];
    const raw = await new Promise<{ stdout: string, stderr: string }>((resolve, reject) => {
      const child = spawn(process.execPath, args, { env }); let stdout = '', stderr = '';
      const timer = setTimeout(() => { child.kill('SIGKILL'); reject(Error(`timeout: ${mode}`)); }, 30000);
      child.stdout.on('data', x => stdout += x); child.stderr.on('data', x => stderr += x);
      child.on('error', reject); child.on('close', code => { clearTimeout(timer); code === 0 ? resolve({ stdout, stderr }) : reject(Error(`${mode}: ${code}\n${stderr}`)); });
    });
    const values = JSON.parse(raw.stdout.trim().split('\n').at(-1)!);
    const normalize = text => [work, baseline, server.good, server.bad, server.target, server.proxy, server.badProxy, server.freshGood, server.freshBad].reduce((text, value, index) => text.replaceAll(value, `<fixture-${index}>`), text);
    cases.push({ mode, mismatch, expected: Object.fromEntries(Object.entries(values).map(([key, value]: any) => [key, value.ok])) });
    fs.writeFileSync(join(output, `${mode}-${mismatch}.json`), normalize(JSON.stringify({ ...spec, stdout: raw.stdout, stderr: raw.stderr }, null, 2)) + '\n', { flag: 'wx' });
    console.log(JSON.stringify(cases.at(-1)));
  }
  fs.writeFileSync(join(output, 'oracle.json'), JSON.stringify({ sourceCommit: commit, nodeVersion: process.version, platform: process.platform, sourceSha256, cases }, null, 2) + '\n', { flag: 'wx' });
} finally { server.stop(); }
