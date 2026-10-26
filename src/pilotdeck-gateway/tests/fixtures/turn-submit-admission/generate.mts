// Execute the real TS submitTurn admission windows against the real
// coordinators: GatewayTurnReplacementCoordinator, GatewayInteractionCoordinator
// (native reconnect port) and GatewayManualCompactionCoordinator are the real
// baseline modules; session, registry, authorizer, replacement storage and the
// compaction router are controlled ports. Session events are scripted, so a
// full turn observes exactly the admission windows and their ordering.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, writeFile, mkdir, mkdtemp, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/gateway/client/InProcessGateway.ts', 'src/gateway/SessionRouter.ts',
  'src/agent/scope/AgentHandle.ts', 'src/agent/scope/AgentRegistry.ts',
  'src/agent/scope/AgentFactoryProvider.ts', 'src/agent/scope/ScopedServiceRegistry.ts',
  'src/gateway/client/GatewayTurnCompletionFence.ts',
  'src/gateway/client/GatewayTurnReplayStore.ts', 'src/gateway/client/GatewayTurnEventCoordinator.ts',
  'src/gateway/client/GatewayAgentEventProjector.ts',
  'src/gateway/client/GatewayToolResultArtifactStore.ts',
  'src/tool/protocol/result.ts', 'src/gateway/util/AsyncQueue.ts',
  'src/gateway/run/RunRegistry.ts', 'src/status/agentStatus.ts',
  'src/gateway/dialog/errors.ts', 'src/router/protocol/errors.ts',
  'src/agent/policy/FailureGuard.ts', 'src/agent/policy/classify.ts',
  'src/gateway/client/GatewayTurnReplacementCoordinator.ts',
  'src/gateway/client/GatewayInteractionCoordinator.ts',
  'src/interaction/InteractionReconnect.ts',
  'src/gateway/client/GatewayManualCompactionCoordinator.ts',
  'src/gateway/dialog/modelCatalog.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
const unexpectedImports: string[] = [];
let controlledClock: Map<object, () => void> | undefined;
const controlledSetTimeout = (callback: () => void, ms: number) => {
  if (!controlledClock) return setTimeout(callback, ms);
  const key = {}; controlledClock.set(key, callback); return key;
};
const controlledClearTimeout = (key: any) => {
  controlledClock?.delete(key); clearTimeout(key);
};
async function load(source: string, imports: Record<string, any> = {}) {
  const compiled = transformSync(await readFile(join(baseline, source), 'utf8'), {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
  }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', 'setTimeout', 'clearTimeout', compiled)((name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name in imports) return imports[name];
    return new Proxy({}, { get(_t, key) {
      if (key === '__esModule') return true;
      const error = `unexercised import: ${source} -> ${name}.${String(key)}`;
      unexpectedImports.push(error); throw Error(error);
    } });
  }, module, module.exports, controlledSetTimeout, controlledClearTimeout);
  return module.exports;
}
const handleModule = await load(sources[2]);
const scopedModule = await load(sources[5]);
const agentsModule = await load(sources[3], { './AgentHandle.js': handleModule, './ScopedServiceRegistry.js': scopedModule });
const factoryModule = await load(sources[4], { './AgentHandle.js': handleModule, './AgentRegistry.js': agentsModule });
const routerModule = await load(sources[1], { '../agent/index.js': { ...handleModule, ...agentsModule, ...factoryModule } });
const fenceModule = await load(sources[6]);
const replayModule = await load(sources[7]);
const coordinatorModule = await load(sources[8], { './GatewayTurnReplayStore.js': replayModule });
const artifactsModule = await load(sources[10]);
const toolResultModule = await load(sources[11]);
const projectorModule = await load(sources[9], { './GatewayToolResultArtifactStore.js': artifactsModule, '../../tool/index.js': toolResultModule });
const queueModule = await load(sources[12]);
const registryModule = await load(sources[13]);
const statusModule = await load(sources[14]);
const dialogErrors = await load(sources[15]);
const routerErrors = await load(sources[16]);
const classifyModule = await load(sources[18]);
const guardModule = await load(sources[17], { './classify.js': classifyModule });
const replacementModule = await load(sources[19], { '../dialog/errors.js': dialogErrors });
const reconnectModule = await load(sources[21]);
const interactionCalls: any[] = [];
class RecordingElicitationBus {
  rejectSession(sessionKey: string, reason: string) { interactionCalls.push({ bus: 'elicitation', op: 'rejectSession', sessionKey, reason }); }
  hasPendingSession() { return false; }
}
class RecordingPermissionBus {
  denySession(sessionKey: string, reason: string) { interactionCalls.push({ bus: 'permission', op: 'denySession', sessionKey, reason }); }
  hasPendingSession() { return false; }
}
class RecordingGrants {
  allowRules() { interactionCalls.push({ bus: 'grants', op: 'allowRules', sessionKey: 's' }); return []; }
  grant() {}
  dispose() {}
  closeSession() {}
}
const interactionModule = await load(sources[20], {
  '../../interaction/index.js': { createNativeInteractionReconnectPort: reconnectModule.createNativeInteractionReconnectPort },
  '../../permission/index.js': { permissionEntryToRule: () => ({}) },
  '../elicitation/GatewayElicitationBus.js': { GatewayElicitationBus: RecordingElicitationBus },
  '../permission/GatewayPermissionBus.js': { GatewayPermissionBus: RecordingPermissionBus },
  '../permission/GatewaySessionPermissionRuleSetRegistry.js': { GatewaySessionPermissionRuleSetRegistry: RecordingGrants },
});
const compactionModule = await load(sources[22]);
const modelCatalogModule = await load(sources[23], {
  '../../pilot/config/loadPilotConfig.js': { loadPilotConfig: () => ({ config: { model: { providers: {} } } }) },
  './errors.js': dialogErrors,
});
const { InProcessGateway } = await load(sources[0], {
  '../../agent/policy/FailureGuard.js': guardModule,
  './GatewayTurnEventCoordinator.js': coordinatorModule, './GatewayAgentEventProjector.js': projectorModule, '../util/AsyncQueue.js': queueModule,
  '../dialog/errors.js': dialogErrors, '../../router/index.js': routerErrors,
  '../run/RunRegistry.js': registryModule, '../../status/agentStatus.js': statusModule,
  '../../permission/index.js': { isPermissionMode: (x: any) => ['default', 'plan', 'bypassPermissions'].includes(x),
    readPermissionSettings: () => ({}), permissionSettingsToRuleSet: () => ({ allow: [], deny: [], ask: [] }) },
  '../../agent/protocol/input.js': { parseAgentRunMode: (x: any) => ['agent', 'plan', 'ask'].includes(x) ? x : undefined },
  '../dialog/modelCatalog.js': modelCatalogModule,
});
const clone = (x: any) => JSON.parse(JSON.stringify(x, (_key, value) => value instanceof Error ? { message: value.message } : value));
const settle = async () => { for (let i = 0; i < 12; ++i) await new Promise<void>(r => setImmediate(r)); };
const cases: any[] = [];
async function run(name: string, setup: any = {}, steps = ['observe']) {
  controlledClock = steps.includes('timeout') ? new Map() : undefined;
  const root = await mkdtemp(join(tmpdir(), 'pdrs-submit-admission-'));
  const file = new registryModule.FileRunRegistry(join(root, 'registry.json'));
  const hostCalls: any[] = [], durableCalls: any[] = [], registryCalls: any[] = [], native: any[] = [], visible: any[] = [], results: any[] = [];
  const submits: any[] = [], storageCalls: any[] = [], compactCalls: any[] = [], compactAborts: any[] = [], authorizerCalls: any[] = [], sdkSetCalls: any[] = [];
  const runRef = { sessionKey: 's', projectKey: 'p', runId: 'r' };
  const input = { ...runRef, message: 'hello', channelKey: 'test', ...setup.input };
  const pendingRegistry: Promise<any>[] = [];
  const track = (promise: Promise<any>) => { pendingRegistry.push(promise.catch(() => {})); return promise; };
  const port = {
    async accept(value: any) { registryCalls.push({ op: 'accept', requestMaterial: value.requestMaterial }); return track(file.accept(value)); },
    async events(value: any) { registryCalls.push({ op: 'events' }); return track(file.events(value)); },
    async append(value: any) { registryCalls.push({ op: 'append', event: clone(value.event) }); return track(file.append(value)); },
  };
  const coordinator = new coordinatorModule.GatewayTurnEventCoordinator();
  const sessionEvents = [
    { type: 'input_accepted', sessionId: 's', turnId: 'r', messages: [{ role: 'user', content: [{ type: 'text', text: 'hello' }] }] },
    { type: 'model_event', event: { type: 'text_delta', text: 'hello back' } },
    { type: 'turn_completed', result: { type: 'success', stopReason: 'completed', usage: {} } },
  ];
  const session = {
    sessionId: 's', pendingTurnCount: () => 0,
    async recordAgentStatusMessage() { return false; },
    snapshotForRuntimeReload: () => ({ messages: [], cwd: '/unused' }),
    abort() { hostCalls.push('session_abort'); },
    async dispose() { hostCalls.push('session_dispose'); },
    async *submit(agentInput: any, options: any) {
      hostCalls.push('submit');
      submits.push({ agentInput: clone(agentInput), options: clone(options) });
      for (const event of sessionEvents) { native.push(clone(event)); yield clone(event); }
      coordinator.emit('s', { type: 'agent_status', event: 'SessionEnd' });
    },
  };
  const router = new routerModule.SessionRouter({ createSession: () => session, idleSweepIntervalMs: 0, now: () => new Date(0) });
  const hostHandle = await router.getOrCreate({ sessionKey: 's', projectKey: 'p', channelKey: 'test' });
  const replacement = new replacementModule.GatewayTurnReplacementCoordinator({
    storage: {
      async replaceLastTurn(value: any) { storageCalls.push({ op: 'replace', input: clone(value) }); return { transactionId: 'tx-1' }; },
      async finalizeLastTurnReplacement(value: any) { storageCalls.push({ op: 'finalize', input: clone(value) }); return null; },
    },
    session: { activeTurnRunId: () => undefined, hasActiveTurn: () => false, abortExpectedTurn: async () => {}, closeSession: async () => {} },
    timeoutMs: 60_000,
  });
  if (setup.pendingReplacement !== undefined) {
    await replacement.replaceLastTurn({ sessionKey: 's', projectKey: 'p', expectedTurnId: 'prev', replacementTurnId: setup.pendingReplacement });
  }
  if (setup.writeReservation) replacement.reserveTranscriptWrite('s', 'session_update');
  const interactions = new interactionModule.GatewayInteractionCoordinator({
    reconnectPort: reconnectModule.createNativeInteractionReconnectPort(),
    elicitationBus: new RecordingElicitationBus(),
    permissionBus: new RecordingPermissionBus(),
    permissionGrants: new RecordingGrants(),
  });
  if (setup.seed !== undefined) {
    const seeded = interactions.getReconnectPort().reconnect('s', setup.seed, undefined);
    assert.ok(['initial', 'no_pending'].includes(seeded.outcome), 'seeding must create the current binding');
  }
  const manualCompaction = new compactionModule.GatewayManualCompactionCoordinator({
    router: {
      async compact(sessionKey: string, options: { turnId?: string; abortSignal?: AbortSignal }) {
        compactCalls.push({ sessionKey, turnId: options.turnId });
        if (setup.compactResult) return clone(setup.compactResult);
        if (setup.compactHang) {
          return await new Promise<any>((resolveBack) => {
            options.abortSignal!.addEventListener('abort', () => {
              compactAborts.push(options.abortSignal!.reason);
              resolveBack({ type: 'aborted', turnId: options.turnId, error: options.abortSignal!.reason, usage: {} });
            });
          });
        }
        if (setup.compactError) throw Error(setup.compactError);
        return { type: 'skipped', turnId: options.turnId, reason: 'no_compactable_history', usage: {} };
      },
    },
  });
  const gateway = Object.create(InProcessGateway.prototype);
  Object.assign(gateway, {
    options: {
      runRegistry: port,
      // fix-r1 P1: with the setSdkSessionConfig port wired, where does an
      // invalid sdkSessionConfig get rejected, and does set ever run?
      ...(setup.wireSdkConfig ? { setSdkSessionConfig: async (sessionKey: string, config: any, projectKey: any, signal: any) => {
        sdkSetCalls.push({ sessionKey, config: clone(config), projectKey, aborted: signal.aborted });
        return { changed: true };
      } } : {}),
      ...(setup.authorizer ? { trustedContextAuthorizer: {
        authorize: async (request: any) => { authorizerCalls.push(clone(request)); return setup.authorizer(request); },
      } } : {}),
      resolveRunPolicy: () => undefined,
      refreshConfigBeforeTurn: async () => { hostCalls.push('refresh'); },
      afterTurnCompleted: () => { hostCalls.push('after'); },
    },
    now: () => new Date(0), abortTurnTimeoutMs: 20, uuid: () => 'generated-uuid',
    dialogProjectKeys: new Map(), workspaceAdmissions: new Map(), activeFailureGuards: new Map(), abortedTurnReasons: new Map(),
    turnEventCoordinator: coordinator, agentEventProjector: new projectorModule.GatewayAgentEventProjector(),
    turnReplacementCoordinator: replacement, manualCompactionCoordinator: manualCompaction,
    turnCompletionFence: new fenceModule.GatewayTurnCompletionFence(), router,
    interactionCoordinator: interactions,
    permissionModes: { get: () => undefined }, turnTelemetryContextResolver: { resolve: () => ({}) }, agentEventTelemetryObserver: { observe() {} },
    resolveUploadedAttachments: async () => ({ attachments: [], release: async () => {} }),
    prepareAttachmentTurn: async (message: any) => {
      hostCalls.push('prepare');
      return { agentInput: { type: 'text', text: message }, allowedReadFiles: [] };
    },
  });
  let ended = false, thrown: any = null;
  const consumer = (async () => { try { for await (const event of gateway.submitTurn(input)) visible.push(clone(event)); }
    catch (error: any) { thrown = error.message; } finally { ended = true; } })();
  async function snapshot() {
    await file.ready;
    for (let i = 0; i < 3; ++i) { await settle(); await Promise.all(pendingRegistry); await file.mutationChain; }
    results.push({ visible: clone(visible), wire: visible.map(e => JSON.stringify(e)), hostCalls: clone(hostCalls),
      submits: clone(submits), registryCalls: clone(registryCalls), storageCalls: clone(storageCalls), sdkSetCalls: clone(sdkSetCalls),
      interactionCalls: clone(interactionCalls), compactCalls: clone(compactCalls), compactAborts: clone(compactAborts),
      authorizerCalls: clone(authorizerCalls), active: router.hasActiveTurn('s'), ended, thrown });
  }
  try {
    for (const step of steps) {
      if (step === 'timeout') { const callbacks = [...controlledClock!.values()]; controlledClock!.clear(); for (const callback of callbacks) callback(); }
      else assert.equal(step, 'observe');
      await snapshot();
    }
    assert.ok(ended, `${name}: consumer must settle`);
    await consumer;
    let registryEvents: any[] = [], record: any = null;
    if (setup.reachesRegistry) {
      registryEvents = await file.events(runRef);
      const current = await file.get(runRef);
      record = current ? { state: current.state, lastSeq: current.lastSeq, revision: current.revision } : null;
    }
    cases.push({ name, setup, steps, native, results, ...(setup.reachesRegistry ? { registryEvents, record } : {}) });
  } catch (error) { console.error(name, error); throw error; } finally {
    await consumer; await settle(); await file.mutationChain;
    // Do not count post-scenario disposal as submit output.
    coordinator.dispose(); await router.shutdown(); await rm(root, { recursive: true, force: true });
    void hostHandle;
    controlledClock = undefined;
  }
}
const trusted = (text: string) => ({ text, source: 'channel', purpose: 'material_context', scope: 'turn' });
await run('reconnect-stale-connection', { seed: { connectionId: 'c1', generation: 1 }, input: { interactionBinding: { connectionId: 'c2', generation: 2 } } });
await run('reconnect-stale-generation', { seed: { connectionId: 'c1', generation: 2 }, input: { interactionBinding: { connectionId: 'c1', generation: 1 } } });
await run('reconnect-invalid-binding', { input: { interactionBinding: { connectionId: '', generation: 1 } } });
await run('reconnect-fresh-turn', { seed: { connectionId: 'c1', generation: 1 }, input: { interactionBinding: { connectionId: 'c1', generation: 2 } }, reachesRegistry: true });
await run('replacement-claim-conflict', { pendingReplacement: 'rx', reachesRegistry: true });
await run('replacement-claim-committed', { pendingReplacement: 'r', reachesRegistry: true });
await run('replacement-write-reservation', { writeReservation: true, reachesRegistry: true });
await run('compact-usage', { input: { message: '/compact extra' } });
await run('compact-compacted', { input: { message: '/compact' }, compactResult: { type: 'compacted', turnId: 'r', compactionId: 'c1', preTokens: 100, postTokens: 10, messagesSummarized: 4, usage: { inputTokens: 1, outputTokens: 2 } } });
await run('compact-busy', { input: { message: '/compact' }, compactError: 'session has active turn' });
await run('compact-failed-error', { input: { message: '/compact' }, compactError: 'disk exploded' });
await run('compact-timeout-abort', { input: { message: '/compact', timeoutMs: 100 }, compactHang: true }, ['observe', 'timeout']);
await run('plan-usage', { input: { message: '/plan' } });
await run('plan-rewritten', { input: { message: '/plan  设计 x ' }, reachesRegistry: true });
await run('trusted-unavailable', { input: { trustedContext: [trusted('t')] } });
await run('trusted-invalid-decision', { authorizer: async () => ({ source: 'src' }), input: { trustedContext: [trusted('t')] } });
await run('trusted-throws', { authorizer: async () => { throw Error('no way'); }, input: { trustedContext: [trusted('t')] } });
await run('trusted-authorized', { authorizer: async () => ({ principal: 'p1', source: 'authorized' }), input: { trustedContext: [trusted('t1'), trusted('t2')] }, reachesRegistry: true });
await run('model-selection-normalized', { input: { modelSelection: { mode: 'model', provider: 'pv', model: 'mv', reasoning: 0.8, speed: 0.5, temperature: 0.7, retired: 'drop-me' }, modelOverride: { mode: 'model', provider: 'ov', model: 'om', retired: 'drop-me-too' } }, reachesRegistry: true });
// fix-r1 P2: equal connectionId with a non-numeric generation must be a
// reconnect-port validation throw, not an interaction_reconnect_required.
await run('reconnect-equal-id-bad-generation', { seed: { connectionId: 'c1', generation: 2 }, input: { interactionBinding: { connectionId: 'c1', generation: 'x' } } });
// fix-r1 P1: the sdkSessionConfig validation window with the port wired.
await run('sdk-config-valid-wired', { wireSdkConfig: true, input: { sdkSessionConfig: { systemPrompt: 'p1' } }, reachesRegistry: true });
await run('sdk-config-invalid-wired', { wireSdkConfig: true, input: { sdkSessionConfig: { systemPrompt: 42 } }, reachesRegistry: true });
await run('sdk-config-managed-invalid-wired', { wireSdkConfig: true, input: { sdkSessionConfig: { managedPermissions: { deny: [''], ask: [] } } }, reachesRegistry: true });
await run('sdk-config-invalid-unwired', { input: { sdkSessionConfig: { systemPrompt: 42 } }, reachesRegistry: true });
assert.deepEqual(unexpectedImports, [], 'every exercised import must be real or explicitly controlled');
const out = resolve(process.argv[2]); await mkdir(out, { recursive: true });
await writeFile(join(out, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n');
console.log(`${cases.length} admission scenarios, ${cases.reduce((n, c) => n + c.steps.length, 0)} snapshots`);
