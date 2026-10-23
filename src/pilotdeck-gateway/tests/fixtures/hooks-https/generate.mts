// Real TS `HttpHookExecutor` against a local TLS server. The oracle proves
// that HTTPS hook URLs (and a redirect from HTTP to HTTPS) execute through
// the baseline's global `fetch`; the Rust replay starts the same TLS server
// with the shared throwaway CA from `../model-stream/tls.json`.
//
// Run from the repository root with Node 22 and the fixed baseline's tsx
// loader; see README.md.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { realpathSync } from 'node:fs';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer as createHttpServer } from 'node:http';
import { createServer as createHttpsServer } from 'node:https';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.equal(process.version, 'v22.22.0');
assert.ok(process.argv[2], 'provide the fixture directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);

const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const git = (...args: string[]) =>
  execFileSync(gitExecutable, ['--no-replace-objects', '-C', baseline, ...args], {
    env: gitEnv,
    maxBuffer: 32 * 1024 * 1024,
  });
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;

// Files whose behavior this oracle calibrates.
const sources = [
  'src/extension/hooks/execution/HttpHookExecutor.ts',
  'src/extension/hooks/execution/CommandHookExecutor.ts',
  'src/extension/hooks/execution/parseHookOutput.ts',
];
const sourceSha256: Record<string, string> = {};
for (const path of sources) {
  const committed = git('show', `${BASELINE_COMMIT}:${path}`);
  const working = await readFile(join(baseline, path));
  assert.deepEqual(working, committed, `${path} differs from ${BASELINE_COMMIT}`);
  sourceSha256[path] = createHash('sha256').update(committed).digest('hex');
}

const load = (path: string) => import(pathToFileURL(join(baseline, path)).href);
const { HttpHookExecutor } = await load('src/extension/hooks/execution/HttpHookExecutor.ts');
const undici = await import(pathToFileURL(join(baseline, 'node_modules/undici/index.js')).href);

// Throwaway test CA/leaf shared with the Rust replay (not a credential).
const material = JSON.parse(await readFile(new URL('../model-stream/tls.json', import.meta.url), 'utf8'));
const caPem = Buffer.from(material.ca);
const hookInput = { hook_event_name: 'PreToolUse', session_id: 'sess-https' };

const readBody = async (request: any): Promise<string> => {
  const chunks: Buffer[] = [];
  for await (const chunk of request) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString('utf8');
};

const httpsServer = createHttpsServer({ key: material.key, cert: material.cert }, async (request, response) => {
  const body = await readBody(request);
  const echo = JSON.stringify({
    method: request.method,
    body,
    contentType: request.headers['content-type'] ?? '',
  });
  response.writeHead(200, { 'content-type': 'application/json' });
  response.end(echo);
});
await new Promise<void>((ready) => httpsServer.listen(0, '127.0.0.1', ready));
const tlsOrigin = `https://127.0.0.1:${(httpsServer.address() as any).port}`;

const httpServer = createHttpServer(async (request, response) => {
  await readBody(request);
  response.writeHead(302, { location: `${tlsOrigin}${request.url ?? '/hook'}` });
  response.end('redirect');
});
await new Promise<void>((ready) => httpServer.listen(0, '127.0.0.1', ready));
const httpOrigin = `http://127.0.0.1:${(httpServer.address() as any).port}`;

// A port that is closed before the case runs: TLS connect fails like fetch.
const closedServer = createHttpServer();
await new Promise<void>((ready) => closedServer.listen(0, '127.0.0.1', ready));
const closedPort = (closedServer.address() as any).port;
await new Promise<void>((closed) => closedServer.close(() => closed()));
const closedOrigin = `https://127.0.0.1:${closedPort}`;

const cases: any[] = [];
async function record(name: string, url: string, trusted: boolean) {
  const executor = new HttpHookExecutor();
  const previous = undici.getGlobalDispatcher();
  const agent = trusted ? new undici.Agent({ connect: { ca: caPem } }) : undefined;
  if (agent) undici.setGlobalDispatcher(agent);
  try {
    const result = await executor.execute({
      hook: { type: 'http', url },
      hookInput,
      env: {},
    });
    const masked = url
      .replace(tlsOrigin, '<tls>')
      .replace(httpOrigin, '<http>')
      .replace(closedOrigin, '<closed>');
    cases.push({ name, url: masked, trusted, expected: JSON.stringify(result) });
  } finally {
    if (agent) {
      undici.setGlobalDispatcher(previous);
      await agent.close();
    }
  }
}

await record('https-direct', `${tlsOrigin}/hook`, true);
await record('http-to-https-redirect', `${httpOrigin}/hook`, true);
await record('https-untrusted-certificate', `${tlsOrigin}/hook`, false);
await record('https-connection-refused', `${closedOrigin}/hook`, true);

await new Promise<void>((closed) => httpsServer.close(() => closed()));
await new Promise<void>((closed) => httpServer.close(() => closed()));

const oracle = {
  sourceCommit,
  nodeVersion: process.version,
  sourceSha256,
  hookInput,
  cases,
};
const text = JSON.stringify(oracle, null, 2) + '\n';
assert.ok(!text.includes(baseline), 'oracle leaked the baseline path');
assert.ok(!text.includes(tlsOrigin) && !text.includes(httpOrigin) && !text.includes(closedOrigin),
  'oracle leaked a run-varying origin');
await mkdir(output, { recursive: true });
await writeFile(join(output, 'oracle.json'), text);
console.log(`hooks-https oracle: ${cases.length} real TS TLS cases`);
