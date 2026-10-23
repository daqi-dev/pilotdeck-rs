// Execute real TS WS -> InProcessGateway -> TurnRunner -> FileRunRegistry.
// Agent loop/transcript are controlled ports; transport, publication and seq are real.
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
  'src/gateway/server/GatewayWsConnection.ts', 'src/gateway/protocol/version.ts',
  'src/agent/modules/transport/moduleRuntime.ts', 'src/agent/modules/protocol.ts',
  'src/gateway/permission/GatewayPermissionBus.ts', 'src/gateway/elicitation/GatewayElicitationBus.ts',
  'src/gateway/client/GatewayInteractionCoordinator.ts', 'src/interaction/InteractionReconnect.ts',
  'src/interaction/InteractionReplay.ts', 'src/gateway/permission/GatewaySessionPermissionRuleSetRegistry.ts',
  'src/permission/settings.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
const unexpectedImports: string[] = [];
let controlledNow = 0;
let controlledClock: Map<object, { due: number; callback: () => void }> | undefined;
const controlledSetTimeout = (callback: () => void, ms: number) => {
  if (!controlledClock) return setTimeout(callback, ms);
  const key = {}; controlledClock.set(key, { due: controlledNow + (ms >= 1 && ms <= 2147483647 ? Math.trunc(ms) : 1), callback }); return key;
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
const projectorModule = await load(sources[9], { './GatewayToolResultArtifactStore.js': { GatewayToolResultArtifactStore: class { persist() { throw Error('unexpected artifact'); } } } });
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
const { InProcessGateway } = await load(sources[0], {
  './GatewayTurnEventCoordinator.js': coordinatorModule,
  './GatewayAgentEventProjector.js': projectorModule, '../util/AsyncQueue.js': queueModule,
  '../dialog/errors.js': dialogErrors, '../../router/index.js': routerErrors,
  '../run/RunRegistry.js': registryModule, '../../status/agentStatus.js': statusModule,
  '../../permission/index.js': { isPermissionMode: (x: any) => ['default', 'plan', 'bypassPermissions'].includes(x),
    readPermissionSettings: () => ({}), permissionSettingsToRuleSet: () => ({ allow: [], deny: [], ask: [] }) },
  '../../agent/protocol/input.js': { parseAgentRunMode: (x: any) => ['agent', 'plan', 'ask'].includes(x) ? x : undefined },
});
const versionModule = await load(sources[20]);
const wsModule = await load(sources[19], {
  '../protocol/version.js': versionModule,
  '../../extension/skills/index.js': { SkillManagerError: class extends Error {}, SkillValidationError: class extends Error {} },
  '../dialog/errors.js': dialogErrors,
});
const moduleProtocol = await load(sources[22]);
const { ModuleOperationHost } = await load(sources[21], { '../protocol.js': moduleProtocol });
const interactionReplay = await load(sources[27]);
const reconnect = await load(sources[26], { './InteractionReplay.js': interactionReplay });
const permissions = await load(sources[23], { '../../interaction/index.js': reconnect });
const elicitations = await load(sources[24], { '../../interaction/index.js': reconnect });
const grants = await load(sources[28]);
const settings = await load(sources[29]);
const { GatewayInteractionCoordinator } = await load(sources[25], {
  '../../interaction/index.js': reconnect, '../../permission/index.js': settings,
  '../permission/GatewayPermissionBus.js': permissions, '../elicitation/GatewayElicitationBus.js': elicitations,
  '../permission/GatewaySessionPermissionRuleSetRegistry.js': grants,
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
  let cancelled = false;
  const runRef = { sessionKey: 's', projectKey: 'p', runId: 'r' };
  const input: any = { ...runRef, message: 'hello', channelKey: 'test', ...setup.input };
  if (setup.generatedRunId) delete input.runId;
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
  const replay = new replayModule.GatewayTurnReplayStore();
  const coordinator = new coordinatorModule.GatewayTurnEventCoordinator({ replayStore: replay });
  const runner = new runnerModule.TurnRunner({}, transcript, undefined, () => new Date('2026-10-13T00:00:00.000Z'), undefined,
    { cwd: '/unused', transcriptPath: '', collectFileArtifacts: false });
  const session = {
    sessionId: 's', pendingTurnCount: () => 0,
    async recordAgentStatusMessage() {
      if (setup.statusRecordFailure) throw Error('injected status record failure');
      return false;
    },
    snapshotForRuntimeReload: () => ({ messages: [], cwd: '/unused' }),
    abort() { hostCalls.push('session_abort'); cancelled = true; terminalGate.resolve(); endGate.resolve(); acceptGate.resolve(); },
    async dispose() { hostCalls.push('session_dispose'); },
    async *submit(agentInput: any, options: any) {
      hostCalls.push('submit');
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
          { type: 'model_event', event: { type: 'request_started', provider: 'provider', model: 'actual' } },
        ] : [];
        const events = cancelled
          ? [{ type: 'turn_completed', result: { type: 'aborted', stopReason: 'aborted_streaming', usage: {} } }]
          : [...modelEvents, { type: 'model_event', event: { type: 'text_delta', text: 'hello back' } },
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
      refreshConfigBeforeTurn: async () => { hostCalls.push('refresh'); if (setup.refreshFailure) throw Error('transient config error'); },
      ...(setup.sdkAvailable ? { setSdkSessionConfig: async () => { hostCalls.push('sdk_config'); if (setup.sdkGate) await sdkGate.promise;
        if (setup.sdkFailure) throw Error('injected SDK config failure'); return { changed: setup.sdkChanged === true }; } } : {}),
      afterTurnCompleted: () => { hostCalls.push('after'); if (setup.finallyFailure) throw Error('injected finally failure'); },
    },
    now: () => new Date(0), uuid: () => 'r', abortTurnTimeoutMs: 20,
    dialogProjectKeys: new Map(), workspaceAdmissions: new Map(setup.workspaceBusy ? [[setup.workspace, 'other:other']] : []), activeFailureGuards: new Map(), abortedTurnReasons: new Map(),
    turnEventCoordinator: coordinator, agentEventProjector: new projectorModule.GatewayAgentEventProjector(),
    turnReplacementCoordinator: { claimForSubmit: () => setup.replacement ? 'conflict' : 'ok', hasTranscriptWriteReservation: () => false,
      commitAcceptedInput: async () => { hostCalls.push('commit'); if (setup.commitFailure) throw Error('injected commit failure'); },
      releaseSubmitClaim() { hostCalls.push('release'); } },
    turnCompletionFence: new fenceModule.GatewayTurnCompletionFence(), router,
    interactionCoordinator: new GatewayInteractionCoordinator(),
    permissionModes: { get: () => undefined }, turnTelemetryContextResolver: { resolve: () => ({}) }, agentEventTelemetryObserver: { observe() {} },
    resolveUploadedAttachments: async () => ({ attachments: [], release: async () => { hostCalls.push('lease_release'); if (setup.leaseFailure) throw Error('injected lease release failure'); } }),
    prepareAttachmentTurn: async () => { hostCalls.push('prepare'); if (setup.prepareGate) await prepareGate.promise;
      if (setup.epochFailure) throw Error('history epoch changed');
      return { agentInput: { type: 'text', text: input.message }, allowedReadFiles: [] }; },
  });
  gateway.describeServer = async () => ({ name: 'fixture', capabilities: [] });
  const frames: string[] = [];
  const ws = new wsModule.GatewayWsConnection({ onMessage() {}, onClose() {},
    sendText(text: string) { const frame = JSON.parse(text); if (frame.type !== 'hello_ok') frames.push(text); },
    close(code: number, reason: string) { throw Error(`unexpected close ${code}:${reason}`); },
  }, { gateway, token: 'token', serverVersion: 'fixture' });
  await ws.handleMessage(JSON.stringify({ type: 'hello', protocolVersion: versionModule.PILOTDECK_GATEWAY_PROTOCOL_VERSION, token: 'token', clientName: 'web', clientVersion: 'fixture' }));
  const request = (id: string, params: any = input) => ws.handleMessage(JSON.stringify({ type: 'request', id, method: 'submit_turn', params }));
  let ended = false;
  const consumer = request('a').then(() => { ended = true; });
  let closing: Promise<any> | undefined;
  async function snapshot() {
    for (let i = 0; i < 3; ++i) { await settle(); await Promise.all(pendingRegistry); await file.mutationChain; }
    results.push({ frames: [...frames], active: router.hasActiveTurn('s'), ended });
  }
  try {
    for (const step of steps) {
      if (step === 'accept') acceptGate.resolve();
      else if (step === 'terminal') terminalGate.resolve();
      else if (step === 'end') endGate.resolve();
      else if (step === 'external_error') gateway.emitForSession('s', { type: 'error', code: 'injected', message: 'external error', recoverable: true });
      else if (step === 'permission') gateway.emitForSession('s', { type: 'permission_request', requestId: 'q', toolName: 'bash', input: { command: 'pwd' }, sequence: 41 });
      else if (step === 'second') await request('b', { ...input, runId: 'r2' });
      else if (step === 'replay') await request('b');
      else if (step === 'conflict') await request('b', { ...input, message: 'different' });
      else if (step === 'close') closing = ws.handleClose();
      else if (step === 'pending') gateway.interactionCoordinator.getPermissionBus().register('s', { requestId: 'q', toolCallId: 'tc', toolName: 'bash', resolve() {}, reject() {} });
      else if (step === 'describe') await ws.handleMessage(JSON.stringify({ type: 'request', id: 'info', method: 'describe_server', params: {} }));
      else if (step === 'abort') await ws.handleMessage(JSON.stringify({ type: 'request', id: 'stop', method: 'abort_turn', params: { sessionKey: 's', runId: 'r' } }));
      else assert.equal(step, 'observe');
      await snapshot();
    }
    assert.ok(ended, `${name}: consumer must settle`);
    await consumer; await closing;
    const events = await file.events(runRef);
    cases.push({ name, setup, steps, input, native, results,
      registryEvents: events.map(({ at, ...entry }: any) => entry),
      replay: replay.snapshot({ sessionKey: 's' }), hostCalls });
  } finally {
    acceptGate.resolve(); terminalGate.resolve(); endGate.resolve();
    await consumer; coordinator.dispose(); await router.shutdown();
    await rm(root, { recursive: true, force: true });
  }
}
await run('permission-before-event-and-final', {}, ['observe', 'accept', 'permission', 'terminal', 'end']);
await run('error-before-success', {}, ['observe', 'accept', 'external_error', 'terminal', 'end']);
await run('concurrent-submit', {}, ['observe', 'second', 'accept', 'terminal', 'end']);
await run('close-active', {}, ['observe', 'accept', 'close']);
await run('close-durable-pending', {}, ['observe', 'close']);
await run('close-preserves-pending', {}, ['observe', 'accept', 'pending', 'close', 'terminal', 'end']);
await run('close-after-busy', {}, ['observe', 'second', 'close', 'accept', 'terminal', 'end']);
await run('describe-during-submit', {}, ['observe', 'describe', 'accept', 'terminal', 'end']);
await run('close-after-end', {}, ['observe', 'accept', 'terminal', 'end', 'close']);
await run('duplicate-run-new-ws-seq', {}, ['observe', 'accept', 'terminal', 'end', 'replay']);
await run('conflict', {}, ['observe', 'accept', 'terminal', 'end', 'conflict']);
await run('finally-error-response', { finallyFailure: true });
await run('registry-append-failed', { registryAppendFailure: true });
await run('invalid-mode', { input: { mode: 'wrong' } }, ['observe']);
await run('abort-active', {}, ['observe', 'accept', 'abort']);
await run('abort-after-busy', {}, ['observe', 'second', 'abort']);
await run('abort-generated-run-id', { generatedRunId: true }, ['observe', 'accept', 'abort']);
await run('close-generated-run-id', { generatedRunId: true }, ['observe', 'accept', 'close']);

// Deterministic abort/fence interleavings. The real router and fence own all
// cancellation/wait behavior; only admission/consumer completion are stepped.
const abortFenceCases: any[] = [];
for (const [name, steps] of [
  ['admission-cleared-before-consumer', ['begin-a', 'owner-done-a', 'abort-a', 'release-router', 'observe', 'finish-a', 'observe']],
  ['replacement-during-abort', ['begin-a', 'abort-a', 'observe', 'finish-a', 'begin-b', 'release-router', 'observe', 'owner-done-b', 'observe', 'finish-b', 'observe']],
  ['latest-session-fence-only', ['begin-a', 'owner-done-a', 'begin-b', 'owner-done-b', 'abort-session', 'release-router', 'observe', 'finish-b', 'observe', 'finish-a']],
  ['replacement-after-wait-snapshot', ['begin-b', 'owner-done-b', 'abort-session', 'release-router', 'observe', 'begin-c', 'finish-b', 'observe', 'finish-c']],
  ['stale-run-does-not-wait', ['begin-a', 'owner-done-a', 'begin-b', 'abort-a', 'observe', 'finish-b', 'finish-a']],
  ['completed-slot-does-not-refill', ['begin-a', 'owner-done-a', 'begin-b', 'owner-done-b', 'finish-b', 'abort-session', 'release-router', 'observe', 'finish-a', 'observe']],
  ['completed-slot-explicit-old-run', ['begin-a', 'owner-done-a', 'begin-b', 'owner-done-b', 'finish-b', 'abort-a', 'observe', 'finish-a', 'observe']],
  ['replacement-during-session-abort', ['begin-a', 'abort-session', 'observe', 'finish-a', 'begin-b', 'release-router', 'observe', 'owner-done-b', 'observe', 'finish-b', 'observe']],
  ['absent-turn-session-abort', ['abort-session', 'release-router', 'observe']],
  ['absent-turn-explicit-abort', ['abort-a', 'observe']],
  ['other-session-does-not-replace', ['begin-a', 'owner-done-a', 'begin-b@other', 'abort-session', 'release-router', 'observe', 'finish-a', 'observe', 'finish-b@other']],
  ['other-session-does-not-refill', ['begin-a@other', 'begin-b', 'finish-b', 'abort-session', 'release-router', 'observe', 'finish-a@other']],
  ['rejected-run-does-not-replace-fence', ['begin-a', 'reject-b', 'owner-done-a', 'abort-a', 'release-router', 'observe', 'finish-a', 'observe', 'finish-b']],
  ['ungated-cleanup-replacement', ['ungated', 'begin-a', 'abort-a', 'observe', 'owner-done-a', 'begin-b', 'finish-a', 'observe', 'finish-b']],
  ['obsolete-drop-preserves-current-slot', ['begin-a', 'owner-done-a', 'begin-b', 'owner-done-b', 'finish-a', 'abort-session', 'release-router', 'observe', 'finish-b', 'observe']],

] as [string, string[]][]) {
  const router = new routerModule.SessionRouter({ createSession() { throw Error('unexpected session creation'); }, idleSweepIntervalMs: 0 });
  const routerGate = defer();
  const realAbort = router.abort.bind(router);
  router.abort = async (...args: any[]) => { await realAbort(...args); if (!steps.includes('ungated')) await routerGate.promise; };
  const fence = new fenceModule.GatewayTurnCompletionFence();
  const gateway = Object.create(InProcessGateway.prototype);
  Object.assign(gateway, { router, turnCompletionFence: fence, abortedTurnReasons: new Map() });
  const handles = new Map<string, any>();
  const results: boolean[] = [];
  let ended = false;
  let abort: Promise<void> | undefined;
  for (const rawStep of steps) {
    const [step, sessionKey = 's'] = rawStep.split('@');
    const runId = step.slice(-1);
    const handleKey = `${sessionKey}:${runId}`;
    if (step.startsWith('begin-')) {
      assert.ok(router.beginTurn(sessionKey, runId));
      handles.set(handleKey, fence.begin(sessionKey, runId));
    } else if (step.startsWith('reject-')) {
      assert.equal(router.beginTurn(sessionKey, runId), false);
    } else if (step.startsWith('owner-done-')) {
      router.endTurn(sessionKey, runId);
    } else if (step.startsWith('finish-')) {
      router.endTurn(sessionKey, runId);
      if (handles.has(handleKey)) fence.complete(sessionKey, handles.get(handleKey));
    } else if (step === 'ungated') {
      // Exercise the real Router microtask boundary without an injected gate.
    } else if (step === 'release-router') {
      routerGate.resolve();
    } else if (step.startsWith('abort-')) {
      abort = gateway.abortTurn({ sessionKey: 's', ...(step === 'abort-a' ? { runId: 'a' } : {}) }).then(() => { ended = true; });
    } else {
      assert.equal(step, 'observe');
      await settle();
      results.push(ended);
    }
  }
  routerGate.resolve();
  await abort;
  abortFenceCases.push({ name, steps, results });
  await router.shutdown();
}

// Exercise the real Module sequence owner with an independent nonzero origin.
const moduleHost = new ModuleOperationHost(() => new Date(0));
moduleHost.accept({ runId: 'r', operationId: 'op', requestId: 'module-request' });
const moduleSequences = [41, 42, 42, 44, 43].map(sequence => ({ sequence, ...moduleHost.acceptSequence('op', 'stream', sequence) }));

// Controlled setTimeout preserves Node's host boundary: advancing the clock
// marks deadlines due; only the next host timer turn invokes their callbacks.
// The replay class is real TS. No expected snapshots are authored here.
const timerCases: any[] = [];
for (const retention of [0, 5, 30000]) {
  for (const ordering of ['during-step', 'after-step', 'dispose-before-due', 'dispose-after-due', 'replace-after-due']) {
    controlledNow = 0; controlledClock = new Map();
    const replay = new replayModule.GatewayTurnReplayStore({ terminalRetentionMs: retention });
    replay.start('s', 'r'); replay.record('s', { type: 'text', text: 'retained', runId: 'r' }); replay.retainTerminal('s', 'r');
    const snapshots: any[] = [];
    const snap = () => snapshots.push(clone(replay.snapshot({ sessionKey: 's' })));
    snap();
    if (ordering === 'dispose-before-due') replay.dispose();
    if (ordering === 'after-step') await Promise.resolve();
    // Advancing a controlled clock marks expiry without invoking callbacks
    // inside the current host step; dispatch remains a separate timer turn.
    controlledNow += Math.max(1, retention);
    const due = [...controlledClock.values()].filter(entry => entry.due <= controlledNow).map(entry => entry.callback); controlledClock.clear();
    if (ordering === 'after-step') for (const callback of due) callback();
    if (ordering === 'dispose-after-due') replay.dispose();
    if (ordering === 'replace-after-due') replay.start('s', 'new');
    snap(); await Promise.resolve(); snap();
    // The end of this host step is the timer dispatch boundary.
    if (ordering !== 'after-step') for (const callback of due) callback();
    snap(); replay.dispose();
    timerCases.push({ retention, ordering, snapshots });
    controlledClock = undefined;
  }
}
assert.deepEqual(unexpectedImports, [], 'unexpected imports');
const out = resolve(process.argv[2]); await mkdir(out, { recursive: true });
await writeFile(join(out, 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases, moduleSequences, timerCases, abortFenceCases }, null, 2) + '\n');
console.log(`${cases.length} WS cases, ${timerCases.length} controlled timer cases, ${abortFenceCases.length} abort fence cases`);
