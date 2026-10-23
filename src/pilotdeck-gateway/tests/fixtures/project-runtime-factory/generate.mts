// The real TypeScript staging sequence of ProjectRuntimeResourcesBundle under
// the real ProjectRuntimeRegistry lifecycle. Only the per-family resource
// constructors are replaced with recording fakes; the registry, the resources
// bundle, the runtime-profile resolution, and loadPilotConfig are the
// unmodified baseline, reading a fixture config tree from a scratch home.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide a fresh output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sourcePaths = [
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/cli/ProjectRuntimeResourcesBundle.ts',
  'src/cli/PilotDeckRuntimeProfile.ts',
  'src/context/RuntimeContextSurface.ts',
  'src/interaction/InteractionProfile.ts',
  'src/tool/execution-world/SandboxPort.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sourcePaths);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');

async function compile(path: string): Promise<string> {
  const code = await readFile(join(baseline, path), 'utf8');
  return transformSync(code, {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: path,
  }).code;
}
function runModule(code: string, requireImpl: (name: string) => unknown): any {
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', code)(requireImpl, module, module.exports);
  return module.exports;
}

// Real config loader; the fixture home and project roots live under scratch.
const { loadPilotConfig } = await import(
  pathToFileURL(join(baseline, 'src/pilot/config/loadPilotConfig.ts')).href
);
// Real profile resolution (pure); its own imports are the three small modules.
const surfaceModule = runModule(
  await compile('src/context/RuntimeContextSurface.ts'),
  (name: string) => { throw new Error(`unexpected RuntimeContextSurface require: ${name}`); },
);
const interactionModule = runModule(
  await compile('src/interaction/InteractionProfile.ts'),
  (name: string) => { throw new Error(`unexpected InteractionProfile require: ${name}`); },
);
const sandboxModule = runModule(
  await compile('src/tool/execution-world/SandboxPort.ts'),
  (name: string) => { throw new Error(`unexpected SandboxPort require: ${name}`); },
);
const profileModule = runModule(await compile('src/cli/PilotDeckRuntimeProfile.ts'), (name: string) => {
  if (name === '../context/index.js') {
    return { resolveRuntimeContextSurface: surfaceModule.resolveRuntimeContextSurface };
  }
  if (name === '../interaction/index.js') {
    return { resolveInteractionProfile: interactionModule.resolveInteractionProfile };
  }
  if (name === '../tool/execution-world/SandboxPort.js') return sandboxModule;
  throw new Error(`unexpected PilotDeckRuntimeProfile require: ${name}`);
});

const scratch = await mkdtemp(join(tmpdir(), 'pdrs-project-runtime-factory-'));
// Retain this per-run scratch directory; never remove a fixed path.
const output = resolve(process.argv[2]);

const events: string[] = [];
let nextId = 0;
let failAt: { family: string; phase: 'construct' | 'stage' } | null = null;
let failCleanup: string[] = [];
// Per-step cross-family disposal dependency: the waiter family's disposal
// only starts once the signal family's disposal has. Both must be members of
// `disposeResources`' `Promise.allSettled` group, where all eight disposals
// start together (ProjectRuntimeResourcesBundle.ts:416-425).
const PARALLEL_DISPOSE_GROUP = [
  'pluginRuntime', 'routerSessionCustomRouters', 'compaction', 'sessionTitleProvider',
  'lspService', 'executionWorld', 'routerRuntime', 'modelRuntime',
];
let disposeDependency: { waiter: string; signal: string } | null = null;
let disposeStarted: Set<string> = new Set();
// Stateful resource clock (`new Date(1000 * ++nowCalls)`): every recorded
// compaction tick proves how many times `options.now` was consumed before it.
// `loadPilotConfig` reads its own wall clock (`new Date()`,
// loadPilotConfig.ts:65) — the bundle passes it no clock
// (ProjectRuntimeResourcesBundle.ts:201-204) — so the ticks only advance when
// a family reads the injected clock.
let nowCalls = 0;
const statefulNow = (): Date => new Date(1000 * ++nowCalls);

function construct(name: string, extra?: string): number {
  const id = ++nextId;
  events.push(`construct:${name}:${id}${extra === undefined ? '' : `:${extra}`}`);
  if (failAt?.family === name && failAt.phase === 'construct') {
    throw new Error(`construct failed:${name}:${id}`);
  }
  return id;
}
function stage(name: string, id: number): void {
  events.push(`stage:${name}:${id}`);
  if (failAt?.family === name && failAt.phase === 'stage') {
    throw new Error(`stage failed:${name}:${id}`);
  }
}
// Each fake is its family's dispose owner; disposal failures reject like the
// baseline's per-resource cleanup. A disposal waiting on a sibling's start
// only progresses because `Promise.allSettled` started every group member.
async function disposeSequence(name: string, id: number): Promise<void> {
  const dependency = disposeDependency;
  if (dependency?.waiter === name) {
    while (!disposeStarted.has(dependency.signal)) {
      await new Promise(settle => setImmediate(settle));
    }
  }
  disposeStarted.add(name);
  checkDispose(name, id);
}
function checkDispose(name: string, id: number): void {
  events.push(`dispose:${name}:${id}`);
  if (failCleanup.includes(name)) {
    throw new Error(`dispose failed:${name}:${id}`);
  }
}
// `loadPilotConfig`'s own wall clock stamps `snapshot.loadedAt` even under
// the stateful resource clock; a config read drawing on `options.now` (or a
// frozen epoch clock) would record false here.
function loadedAtRecent(loadedAt: unknown): boolean {
  return typeof loadedAt === 'object' && loadedAt !== null
    && Math.abs((loadedAt as Date).getTime() - Date.now()) < 3_600_000;
}

// The per-family fakes mirror what the real bundle passes to each constructor.
// Family names are the TS resource/field names, so the Rust replay can be
// compared event for event.
class FakeModelRuntimeBundle {
  readonly fixtureId: number;
  constructor(options: { snapshot: any }) {
    this.fixtureId = construct(
      'modelRuntime',
      `agent=${options.snapshot.config.agent.model.id}:loadedAtRecent=${loadedAtRecent(options.snapshot.loadedAt)}`,
    );
  }
  stage() {
    stage('modelRuntime', this.fixtureId);
    return { model: { fixtureId: this.fixtureId }, modelProviders: { fixtureId: this.fixtureId } };
  }
  async dispose() { await disposeSequence('modelRuntime', this.fixtureId); }
}
class FakePluginRuntime {
  readonly fixtureId: number;
  constructor(options: { builtinPluginsEnabled: unknown }) {
    // The parsed extension section fills a plugin-name map; record its size.
    const enabled = options.builtinPluginsEnabled;
    const shown = enabled === undefined ? 'undefined' : `${Object.keys(enabled as object).length}`;
    this.fixtureId = construct('pluginRuntime', `builtinPluginsEnabled=${shown}`);
  }
  async dispose() { await disposeSequence('pluginRuntime', this.fixtureId); }
}
function fakeCustomRouters() {
  const fixtureId = construct('routerSessionCustomRouters');
  return {
    fixtureId,
    async dispose() { await disposeSequence('routerSessionCustomRouters', fixtureId); },
  };
}
class FakeRouterRuntimeBundle {
  readonly fixtureId: number;
  constructor(options: { model: any; extensions: any; customRouterRegistry: any; useModelRuntime: boolean }) {
    this.fixtureId = construct('routerRuntime', `model=${options.model.fixtureId}:extensions=${options.extensions.fixtureId}:registry=${options.customRouterRegistry.fixtureId}:useModelRuntime=${options.useModelRuntime}`);
  }
  stage() {
    stage('routerRuntime', this.fixtureId);
    return { tokenAccounting: { fixtureId: this.fixtureId }, router: { fixtureId: this.fixtureId } };
  }
  async dispose() { await disposeSequence('routerRuntime', this.fixtureId); }
}
function fakeLspService() {
  const fixtureId = construct('lspService');
  return { fixtureId, async dispose() { await disposeSequence('lspService', fixtureId); } };
}
class FakeExecutionWorldBundle {
  readonly fixtureId: number;
  constructor(options: { profile: any; lsp: any }) {
    const profile = options.profile;
    this.fixtureId = construct('executionWorld', `profile=${profile.sandboxMode}/${profile.interaction.name}/${profile.runtimeContextSurface}:lsp=${options.lsp.fixtureId}`);
  }
  stage() {
    stage('executionWorld', this.fixtureId);
    return { executionWorld: { fixtureId: this.fixtureId }, tools: { fixtureId: this.fixtureId } };
  }
  async dispose() { await disposeSequence('executionWorld', this.fixtureId); }
}
class FakeContextStorageBundle {
  readonly fixtureId: number;
  constructor(_options: unknown) {
    this.fixtureId = construct('contextStorage');
  }
  stage() {
    stage('contextStorage', this.fixtureId);
    return { instructionStorage: {}, toolResultSpill: {} };
  }
  // No dispose: the baseline never disposes context storage (an unexpected
  // call surfaces as a recorded failure instead of passing silently).
}
function fakeCompaction(input: { now: () => Date }) {
  const fixtureId = construct('compaction', `now=${input.now().getTime()}`);
  return { fixtureId, async dispose() { await disposeSequence('compaction', fixtureId); } };
}
function fakePromptCacheCoordinator() {
  // No dispose: the baseline never disposes the prompt-cache coordinator.
  return { fixtureId: construct('promptCacheCoordinator') };
}
function fakeSessionTitleProvider(input: { modelRuntime: any }) {
  const fixtureId = construct('sessionTitleProvider', `model=${input.modelRuntime.fixtureId}`);
  return { fixtureId, async dispose() { await disposeSequence('sessionTitleProvider', fixtureId); } };
}
class FakeMcpRuntimeProvider {
  readonly fixtureId: number;
  constructor(_options: unknown) {
    this.fixtureId = construct('mcpProvider');
  }
  async dispose() { await disposeSequence('mcpProvider', this.fixtureId); }
}
class FakeMemoryBundle {
  readonly fixtureId: number;
  constructor(options: { agentModel: string }) {
    this.fixtureId = construct('memoryBundle', `agentModel=${options.agentModel}`);
  }
  stage() {
    stage('memoryBundle', this.fixtureId);
    return {};
  }
  async dispose() { await disposeSequence('memoryBundle', this.fixtureId); }
}
class SilentBackgroundSubagentRuntime {
  shutdown() {}
}

const bundleModule = { exports: {} as any };
new Function('require', 'module', 'exports', await compile('src/cli/ProjectRuntimeResourcesBundle.ts'))(
  (name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name === './ProjectModelRuntimeBundle.js') return { ProjectModelRuntimeBundle: FakeModelRuntimeBundle };
    if (name === './ProjectExecutionWorldBundle.js') return { ProjectExecutionWorldBundle: FakeExecutionWorldBundle };
    if (name === './ProjectContextStorageBundle.js') return { ProjectContextStorageBundle: FakeContextStorageBundle };
    if (name === './ProjectMemoryBundle.js') return { ProjectMemoryBundle: FakeMemoryBundle };
    if (name === './ProjectRouterRuntimeBundle.js') return { ProjectRouterRuntimeBundle: FakeRouterRuntimeBundle };
    if (name === './PilotDeckRuntimeProfile.js') return profileModule;
    if (name === '../pilot/index.js') return { loadPilotConfig };
    if (name === '../extension/index.js') return { PluginRuntime: FakePluginRuntime };
    if (name === '../mcp/index.js') return { ProjectMcpRuntimeProvider: FakeMcpRuntimeProvider };
    if (name === '../router/index.js') {
      return { createNativeRouterSessionCustomRouterPort() { throw new Error('unexpected native custom-router default'); } };
    }
    if (name === '../lsp/index.js') {
      return { LspService: class { constructor() { throw new Error('unexpected native LspService default'); } } };
    }
    if (name === '../session/index.js') {
      return { createNativeSessionTitleProvider() { throw new Error('unexpected native session-title default'); } };
    }
    if (name === '../sandbox/SessionExecutionLease.js') {
      return { SessionExecutionLease: class { constructor() { throw new Error('unexpected SessionExecutionLease'); } } };
    }
    if (name === '../sandbox/SessionExecutionProvider.js') {
      return { SessionExecutionProviderError: class SessionExecutionProviderError extends Error {} };
    }
    // Type-only imports (TokenAccountingRuntime, model/tool/telemetry types).
    if (name === '../context/index.js' || name === '../model/index.js'
      || name === '../tool/index.js' || name === '../telemetry/index.js') return {};
    throw new Error(`unexpected ProjectRuntimeResourcesBundle require: ${name}`);
  },
  bundleModule,
  bundleModule.exports,
);
const RealResourcesBundle = bundleModule.exports.ProjectRuntimeResourcesBundle;
assert.equal(typeof RealResourcesBundle, 'function', 'bundle class must load');

const registryModule = { exports: {} as any };
new Function('require', 'module', 'exports', await compile('src/cli/ProjectRuntimeRegistry.ts'))(
  (name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name === './ProjectRuntimeResourcesBundle.js') {
      return { ProjectRuntimeResourcesBundle: RealResourcesBundle };
    }
    if (name === '../agent/sub/BackgroundSubagentRuntime.js') {
      return { BackgroundSubagentRuntime: SilentBackgroundSubagentRuntime };
    }
    // Imports for session/config/storage methods never exercised in this
    // oracle; access would fail rather than silently run a fake method.
    return {};
  },
  registryModule,
  registryModule.exports,
);
const Registry = registryModule.exports.ProjectRuntimeRegistry;
assert.equal(typeof Registry.prototype.buildRuntime, 'function');

const GLOBAL_YAML = `schemaVersion: 1
agent: {model: parity/deterministic}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: mock-only
      models:
        deterministic: {}
        alpha: {}
        beta: {}
`;
const PROJECT_CONFIGS: Record<string, string> = {
  v1: 'agent: {model: parity/alpha}\n',
  v2: 'agent: {model: parity/beta, sandboxMode: read-only, interactionProfile: headless}\n',
  // agent.subagents must be an object: a direct PilotConfigError throw.
  broken: 'agent: {subagents: 3}\n',
};

function makeRegistry() {
  const registry = Object.create(Registry.prototype);
  Object.assign(registry, {
    runtimes: new Map(), reloadTail: Promise.resolve(), runtimeDisposals: new Set(), disposed: false,
    options: {
      fallbackProjectRoot: join(scratch, 'fallback'),
      pilotHome: join(scratch, 'home'),
      env: { PILOT_HOME: join(scratch, 'config-home') },
      now: statefulNow,
      telemetry: {},
      builtinPlugins: [],
      routerEvents: { create() { throw new Error('unexpected router event bus'); } },
      routerSessionCustomRouterFactory: fakeCustomRouters,
      lspServiceFactory: fakeLspService,
      compactionProviderFactory: fakeCompaction,
      promptCacheCoordinatorFactory: fakePromptCacheCoordinator,
      sessionTitleProviderFactory: fakeSessionTitleProvider,
      contextStorage: {},
    },
    _extraTools: [], permissionModes: { dispose() {} }, ownsSharedSessionState: false,
  });
  return registry;
}

const cases: any[] = [
  { name: 'construct-success-and-dispose', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', as: 'a1' },
    { op: 'resolve', root: 'a', as: 'a2' },
    { op: 'dispose' },
  ] },
  { name: 'config-error-fails-before-families', steps: [
    { op: 'write', project: 'a', variant: 'broken' },
    { op: 'resolve', root: 'a' },
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'dispose' },
  ] },
  { name: 'stage-failure-first-family', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', fail: { family: 'modelRuntime', phase: 'stage' } },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'dispose' },
  ] },
  { name: 'construct-failure-middle-family', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', fail: { family: 'pluginRuntime', phase: 'construct' } },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'dispose' },
  ] },
  { name: 'stage-failure-middle-family', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', fail: { family: 'executionWorld', phase: 'stage' } },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'dispose' },
  ] },
  { name: 'stage-failure-last-family', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', fail: { family: 'memoryBundle', phase: 'stage' } },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'dispose' },
  ] },
  { name: 'dispose-failure-order-and-aggregation', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'acquire', runtime: 'a', as: 'l1' },
    { op: 'invalidate', root: 'a' },
    { op: 'release', lease: 'l1', failCleanup: ['mcpProvider', 'lspService', 'memoryBundle'] },
    { op: 'dispose' },
  ] },
  { name: 'dispose-single-failure-parallel-family', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'acquire', runtime: 'a', as: 'l1' },
    { op: 'invalidate', root: 'a' },
    { op: 'release', lease: 'l1', failCleanup: ['pluginRuntime'] },
    { op: 'dispose' },
  ] },
  { name: 'dispose-single-failure-mcp', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'acquire', runtime: 'a', as: 'l1' },
    { op: 'invalidate', root: 'a' },
    { op: 'release', lease: 'l1', failCleanup: ['mcpProvider'] },
    { op: 'dispose' },
  ] },
  { name: 'dispose-parallel-start-dependency', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'acquire', runtime: 'a', as: 'l1' },
    { op: 'invalidate', root: 'a' },
    // pluginRuntime's cleanup waits for lspService's cleanup to start; only
    // `Promise.allSettled`'s concurrent start lets both finish, and the
    // recorded event order shows pluginRuntime completing after the whole
    // group started (memoryBundle still waits for the group).
    { op: 'release', lease: 'l1', disposeWaits: { waiter: 'pluginRuntime', signal: 'lspService' } },
    { op: 'dispose' },
  ] },
  { name: 'reload-generation-switch-and-drain', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'resolve', root: 'a', as: 'old' },
    { op: 'acquire', runtime: 'old', as: 'l1' },
    { op: 'write', project: 'a', variant: 'v2' },
    { op: 'reload' },
    { op: 'resolve', root: 'a', as: 'new' },
    { op: 'release', lease: 'l1' },
    { op: 'dispose' },
  ] },
  { name: 'reload-partial-failure-keeps-old', steps: [
    { op: 'write', project: 'a', variant: 'v1' },
    { op: 'write', project: 'b', variant: 'v2' },
    { op: 'resolve', root: 'a', as: 'a' },
    { op: 'resolve', root: 'b', as: 'b' },
    { op: 'reload', fail: { family: 'routerRuntime', phase: 'stage' } },
    { op: 'resolve', root: 'a', as: 'sameA' },
    { op: 'resolve', root: 'b', as: 'sameB' },
    // Retire the old generations one at a time: `invalidate` a, then a
    // reload that only replaces b. Two concurrently retired bundles would
    // interleave `disposeResources`' await points across generations in
    // microtask order, which the native port does not reproduce (it cleans
    // each generation contiguously); per-generation order is covered above.
    { op: 'invalidate', root: 'a' },
    { op: 'reload' },
    { op: 'resolve', root: 'a', as: 'newA' },
    { op: 'dispose' },
  ] },
];
function failure(error: any): any {
  return { message: error.message, causes: error instanceof AggregateError ? error.errors.map(failure) : [] };
}
let count = 0;
try {
  const configHome = join(scratch, 'config-home');
  await mkdir(configHome, { recursive: true });
  await writeFile(join(configHome, 'pilotdeck.yaml'), GLOBAL_YAML, 'utf8');
  for (const item of cases) {
    events.length = 0; nextId = 0; nowCalls = 0;
    const registry = makeRegistry();
    const runtimes = new Map<string, any>();
    const leases = new Map<string, any>();
    const recorded: any[] = [];
    for (const step of item.steps) {
      failAt = step.fail ?? null;
      failCleanup = step.failCleanup ?? [];
      disposeDependency = step.disposeWaits ?? null;
      if (disposeDependency) {
        for (const family of [disposeDependency.waiter, disposeDependency.signal]) {
          assert.ok(PARALLEL_DISPOSE_GROUP.includes(family),
            `disposeWaits family ${family} must be in the parallel dispose group`);
        }
      }
      disposeStarted = new Set();
      let error = null;
      let id = null;
      try {
        switch (step.op) {
          case 'write': {
            const dir = join(scratch, step.project, '.pilotdeck');
            await mkdir(dir, { recursive: true });
            await writeFile(join(dir, 'pilotdeck.yaml'), PROJECT_CONFIGS[step.variant], 'utf8');
            break;
          }
          case 'resolve': {
            const runtime = registry.resolve(step.root === undefined ? undefined : `${scratch}/${step.root}`);
            if (step.as) runtimes.set(step.as, runtime);
            id = runtime.model?.fixtureId ?? null;
            break;
          }
          case 'acquire': leases.set(step.as, registry.acquireRuntimeLease(runtimes.get(step.runtime))); break;
          case 'release': await leases.get(step.lease)(); break;
          case 'invalidate': registry.invalidate(step.root === undefined ? undefined : `${scratch}/${step.root}`); break;
          case 'reload': await registry.reload(); break;
          case 'dispose': await registry.disposeProjectRuntimes(); break;
          default: throw new Error(`unknown operation ${step.op}`);
        }
      } catch (caught) { error = failure(caught); }
      // Finish fire-and-forget rollback before observation.
      await new Promise(settle => setImmediate(settle));
      recorded.push({
        input: step, id, error, events: [...events], runtimes: Object.fromEntries(
          [...runtimes].map(([label, runtime]) => [label, {
            id: runtime.model?.fixtureId ?? null, state: runtime.runtimeState, leases: runtime.sessionLeases,
          }]),
        ),
      });
      count++;
    }
    item.steps = recorded;
  }
  assert.equal(cases.length, 12);
  const text = JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n';
  assert.ok(!text.includes(scratch), 'scratch paths must not leak into the oracle');
  await writeFile(join(output, 'oracle.json'), text, { flag: 'wx' });
} finally {
  // This run's private config scratch is removed on success and failure
  // alike; the output directory belongs to the caller (rv-codex-r1 #3).
  await rm(scratch, { recursive: true, force: true });
}
console.log(`PROJECT_RUNTIME_FACTORY_ORACLE_PASS cases=${cases.length} steps=${count}`);
