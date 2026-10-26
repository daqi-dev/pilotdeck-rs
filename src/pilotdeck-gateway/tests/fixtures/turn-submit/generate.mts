// Execute real TS submitTurn, SessionRouter, AgentHandle, TurnRunner and registry.
// Controlled ports are the transcript writer, host policy and agent loop boundary.
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
  'src/gateway/client/GatewayAgentEventProjector.ts', 'src/gateway/util/AsyncQueue.ts',
  'src/gateway/run/RunRegistry.ts', 'src/status/agentStatus.ts',
  'src/agent/turn/TurnRunner.ts', 'src/agent/turn/TurnInputProcessor.ts',
  'src/agent/protocol/errors.ts', 'src/agent/session/AgentSessionEventRecorder.ts',
  'src/gateway/dialog/errors.ts', 'src/router/protocol/errors.ts',
  'src/agent/policy/FailureGuard.ts', 'src/agent/policy/classify.ts',
  'src/gateway/client/GatewayToolResultArtifactStore.ts',
  'src/tool/protocol/result.ts',
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
const artifactsModule = await load(sources[21]);
const toolResultModule = await load(sources[22]);
const projectorModule = await load(sources[9], { './GatewayToolResultArtifactStore.js': artifactsModule, '../../tool/index.js': toolResultModule });
const queueModule = await load(sources[10]);
const registryModule = await load(sources[11]);
const statusModule = await load(sources[12]);
const errorsModule = await load(sources[15]);
const processorModule = await load(sources[14]);
const recorderModule = await load(sources[16]);
const runnerModule = await load(sources[13], {
  '../protocol/errors.js': errorsModule, './TurnInputProcessor.js': processorModule,
  '../../status/agentStatus.js': statusModule, '../session/AgentSessionEventRecorder.js': recorderModule,
});
const dialogErrors = await load(sources[17]);
const routerErrors = await load(sources[18]);
const classifyModule = await load(sources[20]);
const guardModule = await load(sources[19], { './classify.js': classifyModule });
const { InProcessGateway } = await load(sources[0], {
  '../../agent/policy/FailureGuard.js': guardModule,
  './GatewayTurnEventCoordinator.js': coordinatorModule,
  './GatewayAgentEventProjector.js': projectorModule, '../util/AsyncQueue.js': queueModule,
  '../dialog/errors.js': dialogErrors, '../../router/index.js': routerErrors,
  '../run/RunRegistry.js': registryModule, '../../status/agentStatus.js': statusModule,
  '../../permission/index.js': { isPermissionMode: (x: any) => ['default', 'plan', 'bypassPermissions'].includes(x),
    readPermissionSettings: () => ({}), permissionSettingsToRuleSet: () => ({ allow: [], deny: [], ask: [] }) },
  '../../agent/protocol/input.js': { parseAgentRunMode: (x: any) => ['agent', 'plan', 'ask'].includes(x) ? x : undefined },
});
const clone = (x: any) => JSON.parse(JSON.stringify(x, (_key, value) => value instanceof Error ? { message: value.message } : value));
const defer = () => { let resolve!: () => void; const promise = new Promise<void>(r => resolve = r); return { promise, resolve }; };
const settle = async () => { for (let i = 0; i < 12; ++i) await new Promise<void>(r => setImmediate(r)); };
const cases: any[] = [];
async function run(name: string, setup: any = {}, steps = ['observe', 'accept', 'terminal', 'end']) {
  controlledClock = steps.includes('timeout') ? new Map() : undefined;
  const root = await mkdtemp(join(tmpdir(), 'pdrs-turn-submit-'));
  const file = new registryModule.FileRunRegistry(join(root, 'registry.json'));
  const hostCalls: any[] = [], durableCalls: any[] = [], registryCalls: any[] = [], native: any[] = [], visible: any[] = [], results: any[] = [];
  const acceptGate = defer(), terminalGate = defer(), endGate = defer(), prepareGate = defer(), sdkGate = defer(), recoveryGate = defer(), consumerGate = defer();
  let cancelled = false, ended = false, thrown: any = null;
  let guardConfig = setup.failureGuard;
  const runRef = { sessionKey: 's', projectKey: 'p', runId: 'r' };
  const input = { ...runRef, message: 'hello', channelKey: 'test', ...setup.input };
  const pendingRegistry: Promise<any>[] = [];
  const track = (promise: Promise<any>) => { pendingRegistry.push(promise.catch(() => {})); return promise; };
  const port = {
    async accept(value: any) { registryCalls.push({ op: 'accept', requestMaterial: value.requestMaterial });
      if (setup.registryAcceptFailure) throw Error('injected registry accept failure');
      return track(file.accept(value));
    },
    async events(value: any) { registryCalls.push({ op: 'events' }); return track(file.events(value)); },
    async append(value: any) { registryCalls.push({ op: 'append', event: clone(value.event) });
      if (setup.registryAppendFailure) throw Error('injected registry append failure');
      return track(file.append(value));
    },
  };
  const transcript = {
    async recordSessionEvent() {}, async recordTurnResult() {},
    async recordAcceptedInput(_s: any, _r: any, messages: any) {
      durableCalls.push({ op: 'accept_begin', messages: clone(messages) });
      await acceptGate.promise;
      if (setup.transcriptFailure) { durableCalls.push({ op: 'accept_failed' }); throw Error('injected transcript failure'); }
      durableCalls.push({ op: 'accept_committed' });
    },
    async recordTurnCompleted() {}, async recordAgentStatusMessage() {},
  };
  const coordinator = new coordinatorModule.GatewayTurnEventCoordinator();
  const runner = new runnerModule.TurnRunner({}, transcript, undefined, () => new Date('2026-10-13T00:00:00.000Z'), undefined,
    { cwd: '/unused', transcriptPath: '', collectFileArtifacts: false });
  const session = {
    sessionId: 's', pendingTurnCount: () => 0,
    async recordAgentStatusMessage(_turnId: string, status: any) {
      if (setup.failureGuard) durableCalls.push({ op: 'status', status: clone(status), wire: JSON.stringify(status) });
      if (setup.statusRecordFailure) throw Error('injected status record failure');
      return false;
    },
    snapshotForRuntimeReload: () => ({ messages: [], cwd: '/unused' }),
    abort(reason: string) { hostCalls.push('session_abort'); if (setup.failureGuard) durableCalls.push({ op: 'abort', reason }); cancelled = true; terminalGate.resolve(); endGate.resolve(); acceptGate.resolve(); if (setup.sessionAbortFailure) throw Error('injected abort failure'); },
    async dispose() { hostCalls.push('session_dispose'); },
    async *submit(agentInput: any, options: any) {
      hostCalls.push('submit');
      cancelled = false;
      if (setup.input?.timeoutMs > 0) durableCalls.push({ op: 'execution', execution: clone(options.execution) });
      const gen = runner.run({ sessionId: 's', turnId: 'r', input: agentInput, messages: [] });
      try {
        for await (const event of gen) {
          native.push(clone(event)); yield event;
          if (event.type === 'input_accepted') break;
        }
      } finally { await gen.return(undefined); }
      if (!setup.transcriptFailure) {
        await terminalGate.promise;
        if (setup.pumpFailure) throw Error('injected pump failure');
        const modelEvents = setup.modelEvents ? [
          { type: 'model_event', event: { type: 'request_started', provider: 'provider', model: 'configured' } },
          // fix-r2 (opus r2 concurrency #1): the second request_started frame
          // carries its own reasoning/temperature/speed, so the golden pins
          // that the request_started dedup event (InProcessGateway.ts:1661-
          // 1667) carries only provider/model/source/runId — folding the
          // selection-event keys into it is a visible wire difference.
          { type: 'model_event', event: { type: 'request_started', provider: 'provider', model: 'actual', reasoning: 0.9, temperature: 0.6, speed: 0.4 } },
        ] : [];
        for (const event of [...modelEvents, ...(setup.failureEvents ?? [])]) {
          if (cancelled) break;
          native.push(clone(event)); yield event;
        }
        const events = cancelled
          ? [{ type: 'turn_completed', result: { type: 'aborted', stopReason: 'aborted_streaming', usage: {} } }]
          : [{ type: 'model_event', event: { type: 'text_delta', text: 'hello back' } },
             { type: 'turn_completed', result: { type: 'success', stopReason: 'completed', usage: {} } }];
        for (const event of events) { native.push(clone(event)); yield event; }
      }
      await endGate.promise;
      coordinator.emit('s', { type: 'agent_status', event: 'SessionEnd' });
    },
  };
  const router = new routerModule.SessionRouter({ createSession: () => session, idleSweepIntervalMs: 0, now: () => new Date(0) });
  const hostHandle = await router.getOrCreate({ sessionKey: 's', projectKey: 'p', channelKey: 'test' });
  if (setup.busy) router.beginTurn('s', 'other');
  const markDirty = router.markSessionDirty.bind(router);
  router.markSessionDirty = (...args: any[]) => { hostCalls.push('sdk_dirty'); return markDirty(...args); };
  const gateway = Object.create(InProcessGateway.prototype);
  Object.assign(gateway, {
    options: {
      runRegistry: setup.noRegistry ? undefined : port,
      workspaceId: setup.workspace,
      ...(setup.cwdAvailable ? { setSessionCwd() { hostCalls.push('cwd'); } } : {}),
      ...(setup.snapshots ? { snapshotRecorder: {
        async capturePreUser() { hostCalls.push('snapshot_pre'); return setup.snapshotFailure ? { state: 'failed', error: 'injected snapshot failure' } : { state: 'committed' }; },
        async capturePostAgent(input: any) { hostCalls.push('snapshot_post'); durableCalls.push({ op: 'snapshot_post', input: clone(input) }); return { state: 'committed' }; },
      } } : {}),
      ...(setup.recovery ? { clearRecoveredUserDialogs: async () => { hostCalls.push('recovery'); if (setup.recoveryGate) await recoveryGate.promise; if (setup.recoveryFailure) throw Error('injected recovery failure'); } } : {}),
      ...(setup.selection ? { resolveTurnModelSelection: async () => setup.selection } : {}),
      resolveRunPolicy: () => ({ failureGuard: guardConfig }),
      refreshConfigBeforeTurn: async () => { hostCalls.push('refresh'); if (setup.guardAfterRefresh) guardConfig = setup.guardAfterRefresh; if (setup.refreshFailure) throw Error('transient config error'); },
      ...(setup.sdkAvailable ? { setSdkSessionConfig: async () => { hostCalls.push('sdk_config'); if (setup.sdkGate) await sdkGate.promise;
        if (setup.sdkFailure) throw Error('injected SDK config failure'); return { changed: setup.sdkChanged === true }; } } : {}),
      afterTurnCompleted: () => { hostCalls.push('after'); if (setup.finallyFailure) throw Error('injected finally failure'); },
    },
    now: () => new Date(0), abortTurnTimeoutMs: 20,
    dialogProjectKeys: new Map(), workspaceAdmissions: new Map(setup.workspaceBusy ? [[setup.workspace, 'other:other']] : []), activeFailureGuards: new Map(), abortedTurnReasons: new Map(),
    turnEventCoordinator: coordinator, agentEventProjector: new projectorModule.GatewayAgentEventProjector(),
    turnReplacementCoordinator: { claimForSubmit: () => setup.replacement ? 'conflict' : 'ok', hasTranscriptWriteReservation: () => false,
      commitAcceptedInput: async () => { hostCalls.push('commit'); if (setup.commitFailure) throw Error('injected commit failure'); },
      releaseSubmitClaim() { hostCalls.push('release'); } },
    turnCompletionFence: new fenceModule.GatewayTurnCompletionFence(), router,
    interactionCoordinator: { reconnectForTurn: () => ({ outcome: 'reconnected' }), sessionAllowRules: () => [], rejectPendingTurn(_s: any, reason: string) { hostCalls.push(`reject:${reason}`); } },
    permissionModes: { get: () => undefined }, turnTelemetryContextResolver: { resolve: () => ({}) }, agentEventTelemetryObserver: { observe() {} },
    resolveUploadedAttachments: async () => ({ attachments: [], release: async () => { hostCalls.push('lease_release'); if (setup.leaseFailure) throw Error('injected lease release failure'); } }),
    prepareAttachmentTurn: async () => { hostCalls.push('prepare'); if (setup.prepareGate) await prepareGate.promise;
      if (setup.epochFailure) throw Error('history epoch changed');
      return { agentInput: { type: 'text', text: input.message }, allowedReadFiles: [] }; },
  });
  let abortOperation: Promise<any> | undefined, closeOperation: Promise<any> | undefined;
  const consumer = (async () => { try { for await (const event of gateway.submitTurn(input)) {
      visible.push(clone(event));
      if (setup.holdTimeoutConsumer && event.type === 'error' && event.code === 'turn_timeout') await consumerGate.promise;
    } }
    catch (error: any) { thrown = error.message; } finally { ended = true; } })();
  async function snapshot() {
    await file.ready;
    for (let i = 0; i < 3; ++i) { await settle(); await Promise.all(pendingRegistry); await file.mutationChain; }
    results.push({ visible: clone(visible), hostCalls: clone(hostCalls), durableCalls: clone(durableCalls), registryCalls: clone(registryCalls),
      active: router.hasActiveTurn('s'), ended, thrown, ...(setup.failureGuard ? { guardWire: visible.map(e => JSON.stringify(e)) } : {}) });
  }
  try {
    for (const step of steps) {
      if (step === 'accept') acceptGate.resolve();
      else if (step === 'terminal') terminalGate.resolve();
      else if (step === 'end') endGate.resolve();
      else if (step === 'prepare') prepareGate.resolve();
      else if (step === 'sdk') sdkGate.resolve();
      else if (step === 'recovery') recoveryGate.resolve();
      else if (step === 'resume') consumerGate.resolve();
      else if (step === 'timeout') { const callbacks = [...controlledClock!.values()]; controlledClock!.clear(); for (const callback of callbacks) callback(); }
      else if (step === 'abort') abortOperation = gateway.abortTurn({ sessionKey: 's', runId: 'r', reason: 'stop' });
      else if (step === 'close') closeOperation = router.close('s');
      else if (step === 'supersede') gateway.turnCompletionFence.begin('s', 'replacement');
      else if (step === 'second') {
        for await (const event of gateway.submitTurn({ ...input, runId: 'r2' })) visible.push(clone(event));
      }
      else assert.equal(step, 'observe');
      await snapshot();
    }
    assert.ok(ended, `${name}: consumer must settle`);
    await consumer; await abortOperation; await closeOperation;
    const record = await file.get(runRef);
    cases.push({ name, setup, steps, native, results, registryEvents: await file.events(runRef),
      record: record ? { state: record.state, lastSeq: record.lastSeq, revision: record.revision, ...(record.result ? { result: record.result } : {}) } : null });
  } catch (error) { console.error(name, error); throw error; } finally {
    acceptGate.resolve(); terminalGate.resolve(); endGate.resolve(); prepareGate.resolve(); sdkGate.resolve(); recoveryGate.resolve(); consumerGate.resolve();
    await consumer; await settle(); await file.mutationChain;
    // Do not count post-scenario disposal as submit output.
    coordinator.dispose(); await router.shutdown(); await rm(root, { recursive: true, force: true });
    void hostHandle;
    controlledClock = undefined;
  }
}
await run('durable-accept-and-session-end');
await run('invalid-mode', { input: { mode: 'wrong' } }, ['observe']);
await run('invalid-base-mode', { input: { basePermissionMode: 'plan' } }, ['observe']);
await run('invalid-run-id', { input: { runId: 'bad/id' } }, ['observe']);
await run('invalid-budget', { input: { maxBudgetUsd: -1 } }, ['observe']);
await run('invalid-trusted-context', { input: { trustedContext: [{}] } }, ['observe']);
await run('sdk-unavailable', { input: { sdkSessionConfig: {} } }, ['observe']);
await run('sdk-invalid', { sdkAvailable: true, input: { sdkSessionConfig: { permissionMode: 'wrong' } } }, ['observe']);
await run('session-busy', { busy: true }, ['observe']);
await run('sdk-null-busy', { busy: true, input: { sdkSessionConfig: null } }, ['observe']);
await run('sdk-session-busy', { busy: true, sdkAvailable: true, input: { sdkSessionConfig: {} } }, ['observe']);
await run('sdk-changed', { sdkAvailable: true, sdkChanged: true, input: { sdkPermissionMode: 'dontAsk' } });
await run('sdk-unchanged', { sdkAvailable: true, input: { sdkSessionConfig: {} } });
await run('sdk-failed', { sdkAvailable: true, sdkFailure: true, input: { sdkSessionConfig: {} } }, ['observe']);
await run('registry-accept-failed', { registryAcceptFailure: true }, ['observe']);
await run('preflight-append-failed', { busy: true, registryAppendFailure: true }, ['observe']);
await run('transcript-accept-failed', { transcriptFailure: true });
await run('commit-failed', { commitFailure: true }, ['observe', 'accept']);
await run('pump-failed', { pumpFailure: true }, ['observe', 'accept', 'terminal']);
await run('refresh-failed-keeps-snapshot', { refreshFailure: true });
await run('history-epoch-failed', { epochFailure: true }, ['observe']);
await run('finally-failed', { finallyFailure: true });
await run('concurrent-second-submit', {}, ['observe', 'second', 'accept', 'terminal', 'end']);
await run('abort-before-session-submit', { prepareGate: true }, ['observe', 'abort', 'prepare']);
await run('abort-during-sdk', { sdkAvailable: true, sdkGate: true, input: { sdkSessionConfig: {} } }, ['observe', 'abort', 'sdk']);
await run('abort-during-sdk-failure', { sdkAvailable: true, sdkGate: true, sdkFailure: true, input: { sdkSessionConfig: {} } }, ['observe', 'abort', 'sdk']);
await run('abort-active-turn', {}, ['observe', 'accept', 'abort']);
await run('close-active-turn', {}, ['observe', 'accept', 'close']);
await run('superseded-completion', {}, ['observe', 'supersede', 'accept']);
await run('abort-durable-pending', {}, ['observe', 'abort']);
await run('actual-model-selection', { modelEvents: true, selection: { source: 'session', selection: { provider: 'provider', model: 'configured', temperature: 0.7 } } });
await run('lease-release', { lease: true, input: { uploadedAttachments: [{ uploadId: 'fixture' }] } });
await run('lease-release-failed', { lease: true, leaseFailure: true, input: { uploadedAttachments: [{ uploadId: 'fixture' }] } });
await run('recovery-cleanup-failed', { recovery: true, recoveryFailure: true }, ['observe']);
await run('abort-during-recovery', { recovery: true, recoveryGate: true }, ['observe', 'abort', 'recovery']);
await run('abort-and-recovery-failed', { recovery: true, recoveryGate: true, recoveryFailure: true }, ['observe', 'abort', 'recovery']);
await run('execution-fingerprint', { input: { telemetry: { ignored: true }, interactionBinding: { key: 'ignored' }, syntheticMessages: [], opaqueExecution: { z: [1, null, { q: '汉字𝄞' }], a: 'included' } } });
await run('workspace-busy-before-sdk', { workspace: 'w', workspaceBusy: true, sdkAvailable: true, sdkChanged: true, input: { sdkSessionConfig: {} } }, ['observe']);
await run('workspace-released-after-turn', { workspace: 'w' }, ['observe', 'accept', 'terminal', 'end', 'second']);
await run('workspace-cwd', { cwdAvailable: true, input: { workspaceCwd: '/work' } });
await run('snapshot-success', { workspace: 'w', snapshots: true });
await run('snapshot-pre-failed', { workspace: 'w', snapshots: true, snapshotFailure: true }, ['observe', 'accept']);
await run('snapshot-agent-failed', { workspace: 'w', snapshots: true, pumpFailure: true }, ['observe', 'accept', 'terminal']);
await run('snapshot-aborted', { workspace: 'w', snapshots: true }, ['observe', 'accept', 'abort']);
await run('timeout-active-live', { noRegistry: true, input: { timeoutMs: 100 } }, ['observe', 'accept', 'timeout']);
await run('timeout-active', { input: { timeoutMs: 100 } }, ['observe', 'accept', 'timeout']);
await run('timeout-durable-pending', { input: { timeoutMs: 100 } }, ['observe', 'timeout']);
await run('timeout-prepare-pending', { prepareGate: true, input: { timeoutMs: 100 } }, ['observe', 'timeout', 'prepare']);
await run('timeout-prepare-no-revival-live-consumer', { noRegistry: true, holdTimeoutConsumer: true, prepareGate: true, input: { timeoutMs: 100 } }, ['observe', 'timeout', 'prepare', 'resume']);
await run('timeout-status-record-failed', { statusRecordFailure: true, input: { timeoutMs: 100 } }, ['observe', 'accept', 'timeout']);
await run('timeout-snapshot', { workspace: 'w', snapshots: true, input: { timeoutMs: 100 } }, ['observe', 'accept', 'timeout']);
await run('timeout-cleared-on-success', { input: { timeoutMs: 100 } }, ['observe', 'accept', 'terminal', 'end', 'timeout']);
await run('timeout-disabled', { input: { timeoutMs: 0 } });
await run('timeout-overflow-delay', { input: { timeoutMs: 2147483648 } }, ['observe', 'accept', 'timeout']);
await run('timeout-fractional-delay', { input: { timeoutMs: 0.5 } }, ['observe', 'accept', 'timeout']);
await run('timeout-invalid-deadline', { input: { timeoutMs: 9e15 } }, ['observe']);
// FailureGuard is the real TS implementation injected into the real gateway.
const failureGuard = { enabled: true, modelFailureLimit: 2, toolFailureLimits: { read_file: 2, mcp__: 4, mcp__db__: 2 }, toolLabels: { read_file: '文件', mcp__db__: '数据库' } };
const modelFailure = { type: 'turn_continued', reason: 'model_error' };
let toolSerial = 0;
const toolFailure = (toolName: string, code = 'tool_execution_failed', type = 'tool_result', success = false) => ({
  type, sessionId: 's', turnId: 'r', ...(type === 'subagent_tool_result' ? { subagentId: 'sub', subagentType: 'test' } : {}),
  result: { type: success ? 'success' : 'error', toolName, toolCallId: `call-${++toolSerial}`, content: [{ type: 'text', text: 'result' }], ...(success ? {} : { error: { code, message: 'failed' } }) },
});
await run('guard-under-limit', { failureGuard, failureEvents: [modelFailure, toolFailure('read_file'), toolFailure('read_file', 'permission_denied'), { type: 'turn_continued', reason: 'other' }] });
await run('guard-disabled', { failureGuard: { ...failureGuard, enabled: false }, failureEvents: [modelFailure, modelFailure, modelFailure] });
await run('guard-model-trip', { failureGuard, modelEvents: true, failureEvents: [modelFailure, modelFailure, modelFailure] });
await run('guard-tool-trip', { failureGuard, failureEvents: [toolFailure('read_file'), toolFailure('read_file', '', 'tool_result', true), toolFailure('read_file', 'tool_unavailable'), toolFailure('read_file')] });
await run('guard-subagent-trip', { failureGuard, failureEvents: [toolFailure('mcp__db__a'), toolFailure('mcp__db__b', 'setup_required', 'subagent_tool_result'), modelFailure] });
await run('guard-trip-status-record-failed', { failureGuard, statusRecordFailure: true, failureEvents: [modelFailure, modelFailure] });
await run('guard-trip-abort-failed', { failureGuard, sessionAbortFailure: true, failureEvents: [modelFailure, modelFailure] });
await run('guard-reset-next-turn', { failureGuard, failureEvents: [modelFailure] }, ['observe', 'accept', 'terminal', 'end', 'second']);
await run('guard-trip-next-turn', { failureGuard, failureEvents: [modelFailure, modelFailure] }, ['observe', 'accept', 'terminal', 'end', 'second']);
await run('guard-refresh-enables', { failureGuard: { ...failureGuard, enabled: false }, guardAfterRefresh: failureGuard, failureEvents: [modelFailure, modelFailure] });
await run('guard-refresh-disables', { failureGuard, guardAfterRefresh: { ...failureGuard, enabled: false }, failureEvents: [modelFailure, modelFailure] });
await run('guard-snapshot-trip', { failureGuard, workspace: 'w', snapshots: true, failureEvents: [modelFailure, modelFailure] });
await run('guard-abort-before-prepare', { failureGuard, prepareGate: true }, ['observe', 'abort', 'prepare']);
await run('guard-timeout-cleanup', { failureGuard, input: { timeoutMs: 100 } }, ['observe', 'accept', 'timeout']);
// Keep lone UTF-16 units outside the serde_json scenario bridge. They are
// compared directly as protocol JsValue strings by a dedicated Rust replay.
const validationProbes: any[] = [];
for (const patch of [{ mode: '\ud800' }, { basePermissionMode: '\udc00' }, { sessionKey: '\ud800', busy: true }]) {
  const probe = Object.create(InProcessGateway.prototype);
  Object.assign(probe, { options: {}, dialogProjectKeys: new Map(), workspaceAdmissions: new Map(),
    turnReplacementCoordinator: { claimForSubmit: () => 'ok', hasTranscriptWriteReservation: () => false, releaseSubmitClaim() {} },
    router: { beginTurn: () => false, endTurn() {} } });
  const input = { sessionKey: 's', runId: 'r', message: 'hello', ...patch };
  const events = [];
  for await (const event of probe.submitTurn(input)) events.push(clone(event));
  validationProbes.push({ input, events });
}
assert.deepEqual(unexpectedImports, [], 'every exercised import must be real or explicitly controlled');
const out = resolve(process.argv[2]); await mkdir(out, { recursive: true });
await writeFile(join(out, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases, validationProbes }, null, 2) + '\n');
console.log(`${cases.length} scenarios, ${cases.reduce((n, c) => n + c.steps.length, 0)} snapshots`);
