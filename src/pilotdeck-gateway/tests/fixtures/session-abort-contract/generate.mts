// Real baseline classes; fake owned resources supply only controllable futures.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/mcp/runtime/SessionMcpRuntimeRegistry.ts',
  'src/agent/scope/AgentHandle.ts', 'src/agent/session/createAgentSession.ts',
  'src/cli/ProjectRuntimeRegistry.ts', 'src/cli/ProjectSessionFactory.ts',
  'src/cli/GatewaySessionResourceLeaseBundle.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
// Transpile entire, unchanged modules. The production cleanup order is owned
// by the real composition functions loaded below; unrelated imports fail closed.
async function load(source: string, imports: Record<string, any> = {}) {
  const compiled = transformSync(await readFile(join(baseline, source), 'utf8'), {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
  }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', compiled)((name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name in imports) return imports[name];
    return new Proxy({}, { get(_target, property) {
      if (property === '__esModule') return true;
      throw new Error(`unexercised import accessed: ${source} -> ${name}.${String(property)}`);
    } });
  }, module, module.exports);
  return module.exports;
}
const handles = await load('src/agent/scope/AgentHandle.ts');
const { ProjectRuntimeRegistry } = await load('src/cli/ProjectRuntimeRegistry.ts');
const { GatewaySessionResourceLeaseBundle } = await load('src/cli/GatewaySessionResourceLeaseBundle.ts');
const { ProjectSessionFactory } = await load('src/cli/ProjectSessionFactory.ts', {
  '../mcp/runtime/SessionMcpRuntimeRegistry.js':
    await load('src/mcp/runtime/SessionMcpRuntimeRegistry.ts'),
});
class Stub {}
const stubDependencies = { now: () => new Date(0), uuid: () => 'abort-contract', lifecycle: {} };
// createAgentSession's buildAgentSession is the production composition that
// wires AgentHandle.onDispose to disposeAgentSessionLifecycle. Only its
// uninstantiated collaborators are stubs; the two disposal steps under test
// stay the real configuredDisposer and the real runtimeResources.dispose.
const scenario: { resources: any; session: any } = { resources: {}, session: {} };
const { createAgentSessionWithStorage } = await load('src/agent/session/createAgentSession.ts', {
  '../loop/AgentLoop.js': { AgentLoop: Stub },
  '../../session/projection/AgentTranscriptProjections.js': { AGENT_TRANSCRIPT_PROJECTION_NAMES: {} },
  '../../session/projection/SessionProjection.js': { requireSessionProjectionValue: () => undefined },
  '../../session/metadata/SessionMetadataStore.js': { SessionMetadataStore: Stub },
  '../turn/TurnRunner.js': { TurnRunner: Stub },
  '../turn/TurnInputProcessor.js': { TurnInputProcessor: Stub },
  '../scope/AgentHandle.js': handles,
  './AgentSession.js': { AgentSession: class { constructor(_init: unknown) { return scenario.session; } } },
  './ManualCompactionController.js': { ManualCompactionController: Stub },
  './AgentSessionRuntimeBundle.js': {
    AgentSessionRuntimeBundle: class { compose() { return scenario.resources; } },
  },
});
function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(done => { resolve = done; });
  return { promise, resolve };
}
const cases: any[] = [];
for (const active of [false, true]) {
  const events: string[] = [];
  const disposeEntered = deferred(), disposeGate = deferred();
  const leaseEntered = deferred(), leaseGate = deferred();
  const runEntered = deferred(), runGate = deferred();
  let sessionDisposed = false, abortCalls = 0;
  // The handle's session only supplies the turn/abort surface. The gateway
  // composition never calls AgentSession.dispose; the session-owned resources
  // are the runtimeResources bundle disposed by disposeAgentSessionLifecycle.
  scenario.session = {
    sessionId: 'abort-contract',
    pendingTurns: () => [],
    async *submit() { runEntered.resolve(); await runGate.promise; },
    abort(reason: string) {
      abortCalls++;
      events.push(`abort:${reason}:${sessionDisposed ? 'disposed' : 'live'}`);
    },
  };
  scenario.resources = {
    capabilities: {}, context: undefined, dependencies: stubDependencies,
    eventRecorder: undefined, projections: undefined, scope: {}, storage: undefined,
    transcript: {}, sidecarModules: undefined, sidecarTransportContext: undefined,
    internalSessionPorts: { model: {}, tools: {} },
    dispose: async () => {
      events.push('session_dispose_start'); disposeEntered.resolve();
      await disposeGate.promise;
      sessionDisposed = true; events.push('session_dispose_done');
    },
  };
  // Keep the real lease acquisition, retirement and release implementations.
  // Bypass the unrelated provider constructor; only the owned resource is fake.
  const registry = Object.assign(Object.create(ProjectRuntimeRegistry.prototype), {
    runtimeDisposals: new Set(),
  });
  const runtime = { runtimeState: 'active', sessionLeases: 0,
    backgroundSubagents: { shutdown() {} },
    resourcesBundle: { async dispose() {
      events.push('lease_release_start'); leaseEntered.resolve();
      await leaseGate.promise; events.push('lease_release_done');
    } },
  };
  const release = registry.acquireRuntimeLease(runtime);
  registry.retireRuntime(runtime);
  // ProjectSessionRuntimeBundle.ts:217 retains the project runtime lease in a
  // real GatewaySessionResourceLeaseBundle; ProjectSessionFactory.composeDisposer
  // is the production __configure that releases it inside configuredDisposer.
  const leaseBundle = new GatewaySessionResourceLeaseBundle();
  leaseBundle.add('project runtime lease', release);
  const factory = new ProjectSessionFactory({} as any);
  const { handle } = createAgentSessionWithStorage({
    sessionId: 'abort-contract',
    config: { cwd: '/virtual/abort-contract' },
    dependencies: stubDependencies,
    __agentLoopFactory: () => ({}),
    __configure: factory.composeDisposer({ resources: leaseBundle }, () => undefined),
  });
  const running = active ? handle.submit({}).next() : undefined;
  if (active) await runEntered.promise;
  let done = false;
  const disposing = handle.dispose('dispose').then(() => { done = true; });
  if (active) { runGate.resolve(); await running; }
  await leaseEntered.promise;
  const checkpoints: any[] = [];
  const checkpoint = (phase: string) => checkpoints.push({ phase, state: handle.state,
    done, sessionDisposed, abortCalls, leaseCount: runtime.sessionLeases,
    runtimeState: runtime.runtimeState, events: [...events] });
  checkpoint('lease_pending');
  handle.abort('during-lease'); handle.abort('during-lease');
  checkpoint('lease_aborts');
  leaseGate.resolve(); await disposeEntered.promise;
  checkpoint('session_pending');
  handle.abort('during-session'); handle.abort('during-session');
  checkpoint('session_aborts');
  disposeGate.resolve(); await disposing;
  checkpoint('disposed');
  handle.abort('after-disposed');
  checkpoint('late_abort');
  cases.push({ active, checkpoints });
}
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'),
  JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n');
console.log(`recorded ${cases.length} real TS dispose/lease interleavings`);
