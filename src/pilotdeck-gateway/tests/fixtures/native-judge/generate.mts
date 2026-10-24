// Real TS native provider -> registry -> token-saver judge, over loopback HTTP.
// Only the remote model service is a fixture; no invocation port is substituted.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { realpathSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE && process.env.PDRS_GIT && process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const git = realpathSync(process.env.PDRS_GIT);
const sourceCommit = bindSourceTree(baseline, commit, git).ref;
const sourceSha256 = sourceClosureSha256(baseline, commit, git, [
  'src/router/RouterRuntime.ts', 'src/pilot/config/loadPilotConfig.ts',
  'src/model/providers/ModelInvocationProviderRegistry.ts',
  'src/router/provider/RouterModelInvocationPort.ts',
]);
const load = (file: string) => import(pathToFileURL(join(baseline, file)).href);
const { ModelInvocationProviderRegistry, createNativeModelInvocationProvider } =
  await load('src/model/providers/ModelInvocationProviderRegistry.ts');
const { createRegistryRouterModelInvocationPort } = await load('src/router/provider/RouterModelInvocationPort.ts');
const { createRouterRuntime } = await load('src/router/RouterRuntime.ts');
const { loadPilotConfig } = await load('src/pilot/config/loadPilotConfig.ts');
const { ModelProviderError, ModelRequestError } = await load('src/model/index.ts');

const cases = [
  { id: 'simple', status: 200, text: '<tier>simple</tier>' },
  { id: 'complex', status: 200, text: '<tier>complex</tier>' },
  { id: 'auth', status: 401, error: { type: 'authentication_error', message: 'Invalid API key for judge.' } },
  { id: 'bad-request', status: 400, error: { type: 'invalid_request_error', message: 'Malformed judge request.' } },
  { id: 'retryable', status: 503, error: { type: 'server_error', message: 'Judge service unavailable.' } },
  { id: 'missing-provider', status: 200, text: '<tier>simple</tier>' },
  { id: 'missing-model', status: 200, text: '<tier>simple</tier>' },
];
const user = [{ role: 'user', content: [{ type: 'text', text: 'Classify this small fixture task.' }] }];
const directRequest = { provider: 'judge', model: 'j', messages: user, stream: true };
const input = { request: { provider: 'route', model: 'complex', messages: user }, sessionId: 'native-judge', isMainAgent: true };
const scratch = await mkdtemp(join(tmpdir(), 'pdrs-native-judge-'));
const results = [];
try {
  for (const item of cases) {
    const requests: any[] = [];
    const response = item.status === 200
      ? { choices: [{ message: { role: 'assistant', content: item.text }, finish_reason: 'stop' }] }
      : { error: item.error };
    const server = createServer(async (request, reply) => {
      const chunks: Buffer[] = [];
      for await (const chunk of request) chunks.push(Buffer.from(chunk));
      requests.push({ method: request.method, path: request.url,
        body: JSON.parse(Buffer.concat(chunks).toString('utf8')) });
      reply.writeHead(item.status, { 'content-type': 'application/json', connection: 'close' });
      reply.end(JSON.stringify(response));
    });
    await new Promise<void>((yes, no) => {
      server.once('error', no);
      server.listen(0, '127.0.0.1', () => yes());
    });
    const port = (server.address() as any).port;
    const registry = new ModelInvocationProviderRegistry();
    try {
    const configPath = join(scratch, `${item.id}.yaml`);
    await writeFile(configPath, `schemaVersion: 1
agent: {model: route/complex}
model:
  providers:
    judge:
      protocol: openai
      url: http://127.0.0.1:${port}/v1
      apiKey: fixture-only
      retry: {requestMaxRetries: 0, streamMaxRetries: 0}
      models: {j: {}}
    route:
      protocol: openai
      url: http://127.0.0.1:${port}/v1
      apiKey: fixture-only
      models: {simple: {}, complex: {}}
router:
  enabled: true
  scenarios: {default: route/complex}
  stats: {enabled: false}
  tokenSaver:
    enabled: true
    judge: judge/j
    defaultTier: complex
    cacheAwareSwitching: {enabled: false}
    tiers:
      simple: {model: route/simple}
      complex: {model: route/complex}
`);
    const snapshot = await loadPilotConfig({ configPath, env: { PILOT_HOME: scratch }, version: 1 });
    assert.equal(snapshot.diagnostics.filter((d: any) => d.level === 'error').length, 0);
    for (const provider of Object.values(snapshot.config.model.providers) as any[]) {
      if (item.id === 'missing-provider' && provider.id === 'judge') continue;
      const selected = item.id === 'missing-model' && provider.id === 'judge'
        ? { ...provider, models: {} } : provider;
      registry.register(createNativeModelInvocationProvider(selected));
    }
      let completion: any;
      try {
        const value = await registry.complete(directRequest);
        completion = { content: value.content, finishReason: value.finishReason };
      } catch (error: any) {
        const provider = error instanceof ModelProviderError;
        const request = error instanceof ModelRequestError;
        const detail = provider ? error.error : error;
        completion = { error: { kind: provider ? 'provider' : request ? 'request' : 'other',
          code: detail.code, message: detail.message, retryable: provider ? detail.retryable : !request } };
      }
      const completeHttp = requests.splice(0);
      const invocation = createRegistryRouterModelInvocationPort(registry);
      const events: any[] = [];
      const runtime = createRouterRuntime(snapshot.config.router, { modelInvoker: invocation, judgeInvoker: invocation,
        now: () => new Date(0), events: { emit: (event: any) => events.push(event) } });
      const decision = await runtime.decide(input);
      const judgeHttp = requests.splice(0);
      const savedSnapshot = { config: { model: structuredClone(snapshot.config.model), router: snapshot.config.router } };
      for (const provider of Object.values(savedSnapshot.config.model.providers) as any[]) provider.url = '<loopback>/v1';
      assert.ok([...completeHttp, ...judgeHttp].every(r => r.body.stream === false));
      if (['auth', 'bad-request'].includes(item.id)) assert.equal(judgeHttp.length, 1);
      results.push({ ...item, response, snapshot: savedSnapshot, directRequest, input, completion,
        completeHttp, decision, events, judgeHttp });
    } finally {
      await registry.dispose();
      server.closeAllConnections();
      await new Promise<void>((yes, no) => server.close(error => error ? no(error) : yes()));
    }
  }
  const output = resolve(process.argv[2]);
  await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases: results }, null, 2) + '\n', { flag: 'wx' });
  console.log(`native-judge oracle: ${results.length} real TS HTTP cases`);
} finally {
  await rm(scratch, { recursive: true, force: true });
}
