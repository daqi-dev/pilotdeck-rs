// Real TS project/provider lifecycle, model/router bundles, HTTP streamModel,
// and durable adapter. Only unrelated resource families and the network peer
// are fakes. No source is copied or patched; CJS is transpiled in memory.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { readFileSync, realpathSync } from 'node:fs';
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
  'src/cli/ProjectRuntimeRegistry.ts', 'src/cli/ProjectRuntimeResourcesBundle.ts',
  'src/cli/ProjectModelRuntimeBundle.ts', 'src/cli/ProjectRouterRuntimeBundle.ts',
  'src/model/providers/ModelInvocationProviderRegistry.ts',
  'src/agent/modules/llm/routerModelInvokerAdapter.ts',
  'src/agent/modules/llm/durableModelInvokerPort.ts',
]);
const load = (file: string) => import(pathToFileURL(join(baseline, file)).href);
const model = await load('src/model/providers/ModelInvocationProviderRegistry.ts');
const { ProjectModelRuntimeBundle } = await load('src/cli/ProjectModelRuntimeBundle.ts');
const { ProjectRouterRuntimeBundle } = await load('src/cli/ProjectRouterRuntimeBundle.ts');
const { createRegistryRouterModelInvocationPort } = await load('src/router/provider/RouterModelInvocationPort.ts');
const { resolvePilotDeckRuntimeProfile } = await load('src/cli/PilotDeckRuntimeProfile.ts');
const { loadPilotConfig } = await load('src/pilot/config/loadPilotConfig.ts');
const { isOptionalFeatureEnabled } = await load('src/pilot/config/optionalFeature.ts');
const schema = await load('src/router/config/schema.ts');
const { createRouterModelInvokerPort } = await load('src/agent/modules/llm/routerModelInvokerAdapter.ts');
const { createDurableModelInvokerPort } = await load('src/agent/modules/llm/durableModelInvokerPort.ts');
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
function compile(file: string, imports: (name: string) => any) {
  const code = transformSync(readFileSync(join(baseline, file), 'utf8'), { loader: 'ts', format: 'cjs', target: 'es2022' }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', code + (file === 'src/cli/ProjectRuntimeRegistry.ts' ? '\nmodule.exports.ensureRouterConfig = ensureRouterConfig;' : ''))(imports, module, module.exports);
  return module.exports;
}
const silent = { dispose() {}, shutdown() {} };
const bundleImports: Record<string, any> = {
  './ProjectModelRuntimeBundle.js': { ProjectModelRuntimeBundle },
  './ProjectRouterRuntimeBundle.js': { ProjectRouterRuntimeBundle },
  './PilotDeckRuntimeProfile.js': { resolvePilotDeckRuntimeProfile },
  '../pilot/index.js': { loadPilotConfig },
  '../extension/index.js': { PluginRuntime: class { loadSkillPrompt() {} dispose() {} } },
  './ProjectExecutionWorldBundle.js': { ProjectExecutionWorldBundle: class {
    stage() { return { tools: {}, executionWorld: {} }; } createTools() {} dispose() {}
  } },
  './ProjectContextStorageBundle.js': { ProjectContextStorageBundle: class { stage() { return {}; } } },
  './ProjectMemoryBundle.js': { ProjectMemoryBundle: class { stage() { return {}; } dispose() {} } },
  '../mcp/index.js': { ProjectMcpRuntimeProvider: class { dispose() {} } },
};
const { ProjectRuntimeResourcesBundle } = compile('src/cli/ProjectRuntimeResourcesBundle.ts', (name) => {
  if (name.startsWith('node:')) return requireBaseline(name);
  return bundleImports[name] ?? {};
});
const { ProjectRuntimeRegistry, ensureRouterConfig } = compile('src/cli/ProjectRuntimeRegistry.ts', (name) => {
  if (name.startsWith('node:')) return requireBaseline(name);
  if (name === './ProjectRuntimeResourcesBundle.js') return { ProjectRuntimeResourcesBundle };
  if (name === '../agent/sub/BackgroundSubagentRuntime.js') return { BackgroundSubagentRuntime: class { shutdown() {} async dispose() {} } };
  if (name === '../pilot/config/optionalFeature.js') return { isOptionalFeatureEnabled };
  if (name === '../router/config/schema.js') return schema;
  return {};
});
const scratch = await mkdtemp(join(tmpdir(), 'pdrs-584-model-factory-'));
const trace: any[] = [];
const pending: Record<string, ReadableStreamDefaultController<Uint8Array>> = {};
const encoder = new TextEncoder();
const sse = (v: any) => `event: ${v.type}\ndata: ${JSON.stringify(v)}\n\n`;
const frames = {
  start: sse({ type: 'message_start', message: { id: 'msg', type: 'message', role: 'assistant', content: [], model: 'm', usage: { input_tokens: 3, output_tokens: 0 } } }),
  text: sse({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }) + sse({ type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'ok' } }),
  end: sse({ type: 'message_delta', delta: { stop_reason: 'end_turn' }, usage: { output_tokens: 2 } }) + sse({ type: 'message_stop' }),
};
const fetchPeer = async (url: string, init: any) => {
  const label = new URL(url).pathname.split('/')[1];
  trace.push(`http:${label}`);
  assert.ok(init.signal, 'real streamModel must pass the cancel signal to fetch');
  const body = new ReadableStream<Uint8Array>({
    start(controller) { pending[label] = controller; controller.enqueue(encoder.encode(frames.start)); },
    cancel() { trace.push(`http-cancel:${label}`); },
  });
  return new Response(body, { status: 200, headers: { 'content-type': 'text/event-stream' } });
};
const settle = () => new Promise(resolve => setImmediate(resolve));
const config = (label: string, cap: number) => `schemaVersion: 1
agent: { model: a/m }
model:
  providers:
    a:
      protocol: anthropic
      url: http://fixture.test/${label}
      apiKey: mock-only
      retry: { streamMaxRetries: 0 }
      models:
        m:
          capabilities: { maxContextTokens: ${cap}, maxOutputTokens: 64 }
`;
const context = (label: string, signal?: AbortSignal) => ({ sessionId: label, turnId: 't', abortSignal: signal });
const request = { provider: 'a', model: 'm', messages: [{ role: 'user', content: [{ type: 'text', text: 'hi' }] }], metadata: { marker: 'fixture' } };
function port(runtime: any, label: string) {
  const delegate = createRouterModelInvokerPort(runtime.router);
  return createDurableModelInvokerPort(delegate, {
    async recordModelRequest(_session: string, _turn: string, prepared: any) { trace.push(`record-request:${label}:${prepared.request.metadata.marker}`); },
    async recordModelEvent(_session: string, _turn: string, event: any) { trace.push(`record-event:${label}:${event.type}`); },
  } as any);
}
try {
  const home = join(scratch, 'home'), project = join(scratch, 'project');
  await mkdir(home); await mkdir(project);
  await writeFile(join(home, 'pilotdeck.yaml'), config('old', 1000));
  const registry = Object.create(ProjectRuntimeRegistry.prototype);
  Object.assign(registry, {
    runtimes: new Map(), reloadTail: Promise.resolve(), runtimeDisposals: new Set(), disposed: false,
    options: { fallbackProjectRoot: project, pilotHome: home, env: { PILOT_HOME: home }, now: () => new Date(0), telemetry: {}, builtinPlugins: [],
      routerSessionCustomRouterFactory: () => silent, lspServiceFactory: () => silent,
      sessionTitleProviderFactory: () => silent,
      modelInvocationProviderFactory(snapshot: any) {
        const p = snapshot.config.model.providers.a;
        const label = new URL(p.url).pathname.slice(1);
        const provider = model.createNativeModelInvocationProvider(p, { fetch: fetchPeer });
        return [{ ...provider, dispose() { trace.push(`dispose:${label}`); } }];
      },
    }, permissionModes: silent, ownsSharedSessionState: false, routerEvents: { create: () => ({ emit() {} }) }, _extraTools: [],
  });
  const old = registry.resolve(project), releaseOld = registry.acquireRuntimeLease(old);
  const oldPort = port(old, 'old'), oldContext = context('old');
  const preparedOld = await oldPort.prepare({ request, context: oldContext });
  trace.push(`metadata:old:${old.model.getCapabilities('a', 'm').maxContextTokens}`);
  const oldStream = oldPort.stream({ prepared: preparedOld, context: oldContext })[Symbol.asyncIterator]();
  // request_started and message_start; the HTTP response is now in flight.
  for (let i = 0; i < 2; i++) { const event = await oldStream.next(); trace.push(`yield:old:${event.value.type}`); }
  await writeFile(join(home, 'pilotdeck.yaml'), config('new', 2000));
  await registry.reload(); await settle();
  const fresh = registry.resolve(project), releaseNew = registry.acquireRuntimeLease(fresh);
  trace.push(`metadata:old:${old.model.getCapabilities('a', 'm').maxContextTokens}`);
  trace.push(`metadata:new:${fresh.model.getCapabilities('a', 'm').maxContextTokens}`);
  trace.push(`generation:old:${old.modelProviders.getProviderGeneration('a')}`);
  trace.push(`generation:new:${fresh.modelProviders.getProviderGeneration('a')}`);
  const freshPort = port(fresh, 'new'), abort = new AbortController(), freshContext = context('new', abort.signal);
  const preparedNew = await freshPort.prepare({ request, context: freshContext });
  const freshStream = freshPort.stream({ prepared: preparedNew, context: freshContext })[Symbol.asyncIterator]();
  for (let i = 0; i < 2; i++) { const event = await freshStream.next(); trace.push(`yield:new:${event.value.type}`); }
  abort.abort(); await settle();
  trace.push(`cancel-observed:${trace.includes('http-cancel:new')}`);
  await freshStream.return();
  // The retired project generation remains alive until its turn releases.
  trace.push(`old-disposed-before-drain:${trace.includes('dispose:old')}`);
  pending.old.enqueue(encoder.encode(frames.text + frames.end)); pending.old.close();
  for await (const event of { [Symbol.asyncIterator]: () => oldStream }) trace.push(`yield:old:${event.type}`);
  await releaseOld(); await settle();
  trace.push(`old-disposed-after-drain:${trace.includes('dispose:old')}`);
  await releaseNew(); await registry.disposeProjectRuntimes();
  await settle();

  // Registry replacement also protects accepted work without a project lease.
  const replacements: any[] = [];
  const providers = new model.ModelInvocationProviderRegistry();
  const make = (label: string) => ({ providerId: 'a', stream: async function* () { yield { type: 'text_delta', text: label }; },
    complete: async () => ({ label }), getCapabilities: () => ({ label }), getMultimodal: () => ({ input: ['text'] }),
    getProviderProtocol: () => 'anthropic', getProviderBaseUrl: () => undefined, dispose: () => replacements.push(`dispose:${label}`) });
  const registration = providers.register(make('old'));
  const accepted = providers.stream(request)[Symbol.asyncIterator]();
  const replacement = providers.replace(make('new'));
  replacements.push(`current:${providers.getProviderGeneration('a')}`);
  await settle(); replacements.push(`old-disposed:${replacements.includes('dispose:old')}`);
  replacements.push((await accepted.next()).value.text);
  const latest = providers.stream(request)[Symbol.asyncIterator]();
  replacements.push((await latest.next()).value.text); await latest.return();
  await accepted.return(); await replacement.retired;
  await registration.dispose(); // Must not remove the replacement.
  replacements.push(`still-current:${providers.getProviderGeneration('a')}`);
  await providers.dispose();
  const api: any[] = [];
  const errorOf = (error: any) => ({ code: error.code, message: error.message });
  const errors = new model.ModelInvocationProviderRegistry();
  for (const id of ['', ' \t', '\uFEFF']) {
    try { errors.register({ ...make('invalid'), providerId: id }); } catch (error) { api.push(errorOf(error)); }
  }
  errors.register({ ...make('a'), dispose() {} });
  try { errors.register(make('duplicate')); } catch (error) { api.push(errorOf(error)); }
  const lease = errors.acquire('a'); lease.release(); lease.release(); api.push(`lease-generation:${lease.generation}`);
  api.push(`unregister:${await errors.unregister('a')}`); api.push(`unregister:${await errors.unregister('a')}`);
  try { errors.acquire('a'); } catch (error) { api.push(errorOf(error)); }
  await errors.dispose();
  try { errors.register(make('closed')); } catch (error) { api.push(errorOf(error)); }

  const disposalOrder: string[] = [];
  const disposal = new model.ModelInvocationProviderRegistry();
  const fail = (id: string, label: string) => ({ ...make(label), providerId: id,
    dispose() { disposalOrder.push(label); throw new Error(label); } });
  const first = disposal.register(fail('z', 'old-z'));
  const hold = disposal.acquire('z');
  disposal.register(fail('a', 'a'));
  const switched = disposal.replace(fail('z', 'new-z'));
  let failedOld: any;
  // Await and record the retired failure separately; active disposal excludes it.
  hold.release();
  try { await switched.retired; } catch (error) { failedOld = (error as Error).message; }
  let aggregate: any;
  try { await disposal.dispose(); } catch (error) { aggregate = { message: (error as Error).message, causes: (error as AggregateError).errors.map((error: any) => error.message) }; }

  const completeTrace: string[] = [];
  const completes = new model.ModelInvocationProviderRegistry();
  let enter: () => void; const entered = new Promise<void>(resolve => enter = resolve);
  const registrationComplete = completes.register({ ...make('complete'),
    complete(_request: any, options: any) { completeTrace.push('complete-start'); enter(); return new Promise((_resolve, reject) => options.signal.addEventListener('abort', () => { completeTrace.push('complete-abort'); reject(new Error('cancelled')); }, { once: true })); },
    dispose() { completeTrace.push('complete-dispose'); },
  });
  const completeAbort = new AbortController();
  const completing = completes.complete(request, { signal: completeAbort.signal });
  await entered;
  let drained = false; const drain = registrationComplete.whenDrained().then(() => { drained = true; });
  const disposingComplete = registrationComplete.dispose();
  await settle(); completeTrace.push(`drained-before:${drained}`);
  completeAbort.abort();
  let completeError: string | undefined;
  try { await completing; } catch (error) { completeError = (error as Error).message; }
  await drain; await disposingComplete;
  completeTrace.push(`drained-after:${drained}`); await completes.dispose();
  const mainTrace = [...trace]; trace.length = 0;
  // Default provider branch, without a modelInvocationProviderFactory.
  await writeFile(join(home, 'pilotdeck.yaml'), config('native', 3000));
  const nativeSnapshot = loadPilotConfig({ projectRoot: project, env: { PILOT_HOME: home } });
  const nativeBundle = new ProjectModelRuntimeBundle({ snapshot: nativeSnapshot });
  const nativeResources = nativeBundle.stage();
  const nativeRouterBundle = new ProjectRouterRuntimeBundle({ snapshot: nativeSnapshot, ...nativeResources,
    // Call-level fetch seam keeps the default provider-construction branch real.
    model: { ...nativeResources.model, stream: (request: any, options: any) => nativeResources.model.stream(request, { ...options, fetch: fetchPeer }) },
    useModelRuntime: true, extensions: { async loadSkillPrompt() { return undefined; } }, now: () => new Date(0), telemetry: {},
    createRouterConfig: () => ({ enabled: false }), createRouterEventBus: () => ({ emit() {} }),
  });
  const nativeRouter = nativeRouterBundle.stage();
  {
    const nativePort = port(nativeRouter, 'native'), controller = new AbortController(), ctx = context('native', controller.signal);
    const nativePrepared = await nativePort.prepare({ request, context: ctx });
    trace.push(`metadata:native:${nativeResources.model.getCapabilities('a', 'm').maxContextTokens}`);
    const iterator = nativePort.stream({ prepared: nativePrepared, context: ctx })[Symbol.asyncIterator]();
    for (let i = 0; i < 2; i++) trace.push(`yield:native:${(await iterator.next()).value.type}`);
    controller.abort(); await settle(); await iterator.return();
    await nativeRouterBundle.dispose(); await nativeBundle.dispose();
  }
  const nativeTrace = [...trace]; trace.length = 0; trace.push(...mainTrace);
  const selection = { id: 'a/m', provider: 'a', model: 'm' };
  const routerConfigs = [null, { enabled: false }, {}, { enabled: true, tokenSaver: { enabled: false }, autoOrchestrate: { enabled: false }, scenarios: { default: { provider: 'b', model: 'other' } }, fallback: { default: [] }, stats: { enabled: false } }].map(router => ({
    input: { config: { agent: { model: selection }, ...(router === null ? {} : { router }) } },
    result: ensureRouterConfig(router === null ? undefined : router, selection),
  }));
  // Actual bundle staging with token-saver enabled. Capability belongs to
  // the registry port, not to every provider currently registered in it.
  const registryRouterCases: any[] = [];
  for (const name of ['mixed', 'native', 'empty', 'empty-missing-provider']) {
    const judge = name === 'native' ? 'a' : 'b';
    const routerYaml = `router:
  enabled: true
  tokenSaver:
    enabled: true
    judge: ${judge}/m
    defaultTier: medium
    tiers:
      medium: { model: a/m }
  autoOrchestrate: { enabled: false }
  stats: { enabled: false }
`;
    const providerYaml = `    b:
      protocol: anthropic
      url: http://fixture.test/b
      apiKey: mock-only
      models:
        m:
          capabilities: { maxContextTokens: 3000, maxOutputTokens: 64 }
`;
    await writeFile(join(home, 'pilotdeck.yaml'), config(name, 3000) + providerYaml + routerYaml);
    const snapshot = loadPilotConfig({ projectRoot: project, env: { PILOT_HOME: home } });
    const modelBundle = new ProjectModelRuntimeBundle({ snapshot,
      ...(name === 'native' ? {} : { modelInvocationProviderFactory: () => name.startsWith('empty') ? [] : [
        model.createNativeModelInvocationProvider(snapshot.config.model.providers.a),
        { ...make('judge-b'), providerId: 'b', dispose() {} },
      ] }),
    });
    const resources = modelBundle.stage();
    const invocation = createRegistryRouterModelInvocationPort(resources.modelProviders);
    const routerBundle = new ProjectRouterRuntimeBundle({ snapshot, ...resources,
      extensions: { async loadSkillPrompt() { return undefined; } }, now: () => new Date(0), telemetry: {},
      createRouterConfig: (s: any) => ensureRouterConfig(s.config.router, s.config.agent.model),
      createRouterEventBus: () => ({ emit() {} }),
    });
    const staged = routerBundle.stage();
    const result: any = { name, providerYaml, routerYaml, completeAvailable: typeof invocation.complete === 'function', staged: !!staged.router };
    // Native non-streaming HTTP is a separate Rust gap: this case compares
    // staging only. Mixed/empty also exercise the actual registry dispatch.
    if (name !== 'native') {
      const judgeRequest: any = { ...request, provider: judge };
      // Deliberately cross the typed API boundary to exercise JS's undefined
      // route lookup when the request has no provider property at all.
      if (name === 'empty-missing-provider') delete judgeRequest.provider;
      result.request = judgeRequest;
      try { result.completion = await invocation.complete!(judgeRequest); }
      catch (error) { result.error = errorOf(error); }
    }
    registryRouterCases.push(result);
    await routerBundle.dispose(); await modelBundle.dispose();
  }
  const output = resolve(process.argv[2]); await mkdir(output, { recursive: true });
  await writeFile(join(output, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, frames, request, trace, replacements, api, disposal: { order: disposalOrder, failedOld, aggregate }, completeTrace, completeError, nativeTrace, routerConfigs, registryRouterCases }, null, 2) + '\n', { flag: 'wx' });
  console.log(`MODEL_FACTORY_ORACLE_PASS trace=${trace.length} replacements=${replacements.length}`);
} finally { await rm(scratch, { recursive: true, force: true }); }
