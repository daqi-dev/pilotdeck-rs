// #686 discard-notice TS baseline probe. Imports the real cc7c4e6 source.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { bindSourceTree } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE);
const baseline = resolve(process.env.PDRS_BASELINE!);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(baseline, commit, '/usr/bin/git');
const shas = {};
function sha(path) {
  return createHash('sha256').update(readFileSync(join(baseline, path))).digest('hex');
}
const sources = [
  'src/extension/hooks/execution/HookRuntime.ts',
  'src/extension/hooks/execution/AsyncHookRegistry.ts',
  'src/lifecycle/runtime/LifecycleRuntime.ts',
  'src/agent/session/AgentSession.ts',
  'src/agent/session/SteerMailbox.ts',
  'src/agent/turn/TurnRunner.ts',
  'src/agent/loop/AgentLoop.ts',
  'src/agent/loop/AgentTurnCapabilities.ts',
  'src/gateway/permission/createGatewayPermissionHook.ts',
  'src/gateway/permission/GatewayPermissionBus.ts',
  'src/gateway/elicitation/GatewayElicitationBus.ts',
];
const [
  hookRuntimeModule,
  ,
  lifecycleModule,
  sessionModule,
  mailboxModule,
  turnRunnerModule,
  loopModule,
  capabilitiesModule,
  permissionHookModule,
  permissionBusModule,
  elicitationBusModule,
] = await Promise.all(sources.map((p) => import(pathToFileURL(join(baseline, p)).href)));
for (const p of sources) shas[p] = sha(p);

const { HookRuntime } = hookRuntimeModule;
const { LifecycleRuntime } = lifecycleModule;
const { AgentSession } = sessionModule;
const { SteerMailbox } = mailboxModule;
const { TurnRunner } = turnRunnerModule;
const { AgentLoop } = loopModule;
const { createSidecarAgentTurnCapabilities } = capabilitiesModule;
const { createGatewayPermissionHook } = permissionHookModule;
const { GatewayPermissionBus } = permissionBusModule;
const { GatewayElicitationBus } = elicitationBusModule;

const baseInput = () => ({ sessionId: 's', transcriptPath: '', cwd: process.cwd() });
const env = { PATH: '/usr/bin:/bin' };
async function dispatch(runtime, event, payload) {
  return runtime.dispatch({ event, baseInput: baseInput(), payload, matchQuery: event, env });
}
async function tick() {
  await new Promise((r) => setImmediate(r));
}
const NOTICE_NAMES = ['interaction_pending_discarded'];
function notices(events) {
  return events.filter(
    (e) => e && (NOTICE_NAMES.includes(e.type) || e.type === 'steer_unapplied' || e.type === 'warning'),
  );
}

// 1. Hooks: TS is session-scoped, so an async pending hook survives the turn
// boundary. There is no discard notice in the dispatch events.
const hooksObservations = {};
{
  const hooks = new HookRuntime({
    PreToolUse: [{ hooks: [{ type: 'command', command: `printf '%s' '{"async":true}'` }] }],
  });
  const runtime = new LifecycleRuntime(hooks);
  const first = await dispatch(runtime, 'PreToolUse', {});
  const firstIds = first.pendingAsyncHooks.map((d) => d.id);
  const second = await dispatch(runtime, 'PreToolUse', {});
  const secondIds = second.pendingAsyncHooks.map((d) => d.id);
  const completedOldAfterNextTurn = runtime.completeAsyncHook(firstIds[0], {
    stdout: '{"continue":true}',
  });
  const responses = runtime.collectAsyncHookResponses();
  const events = [...first.events, ...second.events].map((e) => e.type);
  await runtime.dispose();
  const completedAfterDispose = runtime.completeAsyncHook(firstIds[0], { stdout: '{}' });
  hooksObservations.firstPending = firstIds;
  hooksObservations.secondPending = secondIds;
  hooksObservations.firstEvents = first.events.map((e) => e.type);
  hooksObservations.secondEvents = second.events.map((e) => e.type);
  hooksObservations.completedOldAfterNextTurn = completedOldAfterNextTurn;
  hooksObservations.collectedResponses = responses.map((r) => r.id);
  hooksObservations.completedAfterDispose = completedAfterDispose;
  hooksObservations.noticeEvents = notices([...first.events, ...second.events]);
}

// 2. Steer: an undrained item is discarded when the turn closes, and
// TurnRunner yields one per-item steer_unapplied event. No aggregate count.
const steerObservations = {};
{
  const item = {
    itemId: 'a',
    message: { role: 'user', content: [], metadata: { queueItemId: 'a' } },
    allowedReadFiles: ['/safe'],
  };
  const mailbox = new SteerMailbox({ recordMutation: async () => {} });
  mailbox.start('t');
  await mailbox.enqueue('t', item);
  const controller = new AbortController();
  controller.abort('probe');
  let modelCalls = 0;
  const model = new Proxy(
    {},
    {
      get(_t, key) {
        if (key === 'then') return undefined;
        return () => {
          modelCalls++;
          throw new Error(`unexpected model call: ${String(key)}`);
        };
      },
    },
  );
  const config = { cwd: process.cwd(), runMode: 'agent' };
  const capabilities = createSidecarAgentTurnCapabilities(config, {
    ports: { model, toolExecution: { list: () => [], executeAll: async () => [] } },
    now: () => new Date('2026-10-25T00:00:00Z'),
  });
  const loop = new AgentLoop(config, capabilities);
  const transcript = {
    recordSessionEvent() {},
    recordAcceptedInput() {},
    recordTurnResult() {},
    async recordDurableMessage() {},
  };
  const runner = new TurnRunner(
    loop,
    transcript,
    undefined,
    () => new Date('2026-10-25T00:00:00Z'),
    undefined,
    { cwd: process.cwd(), transcriptPath: '', collectFileArtifacts: false },
  );
  const events = [];
  for await (const event of runner.run({
    sessionId: 's',
    turnId: 't',
    messages: [],
    input: { type: 'text', text: 'start' },
    abortSignal: controller.signal,
    drainSteerMessages: () => [],
    drainOrCloseSteerMailbox: () => mailbox.drainOrClose('t'),
    claimSteerMessage: (id) => mailbox.claim('t', id),
    ackSteerMessage: (id) => mailbox.ack('t', id),
    closeSteerMailbox: () => mailbox.close('t'),
  })) {
    events.push({
      type: event.type,
      ...(event.type === 'steer_unapplied'
        ? { itemId: event.itemId, reason: event.reason, sessionId: event.sessionId, turnId: event.turnId }
        : {}),
    });
  }
  steerObservations.events = events;
  steerObservations.noticeEvents = notices(events);
  steerObservations.modelCalls = modelCalls;
}

// 3. Permission: a pending Web prompt is denied when the turn/session closes.
// The only user-visible traffic is the original permission_request; there is
// no aggregate discard notice.
const permissionObservations = {};
{
  const hooks = new HookRuntime({
    PermissionRequest: [{ hooks: [{ type: 'callback', name: 'pilotdeck.gateway.permission' }] }],
  });
  const bus = new GatewayPermissionBus();
  const emitted = [];
  hooks.getCallbackExecutor().register(
    'pilotdeck.gateway.permission',
    createGatewayPermissionHook({
      sessionKey: 's',
      permissionRules: [],
      bus,
      uuid: () => 'req-1',
      emit: (event) => {
        emitted.push(event.type);
        return true;
      },
    }),
  );
  const runtime = new LifecycleRuntime(hooks);
  const pending = dispatch(runtime, 'PermissionRequest', { toolName: 'danger', toolUseId: 'call' });
  for (let i = 0; i < 100 && bus.pendingCount('s') === 0; i++) await tick();
  const pendingBeforeClose = bus.pendingCount('s');
  bus.denySession('s', 'turn_ended');
  const result = await pending;
  const pendingAfterClose = bus.pendingCount('s');
  await runtime.dispose();
  permissionObservations.emitted = emitted;
  permissionObservations.pendingBeforeClose = pendingBeforeClose;
  permissionObservations.pendingAfterClose = pendingAfterClose;
  permissionObservations.resultEvents = result.events.map((e) => e.type);
  permissionObservations.resultBlockingErrors = result.blockingErrors;
  permissionObservations.noticeEvents = notices(result.events);
}

// 4. Elicitation/user dialog: pending entries are rejected at turn end, but the
// bus has no event/notice channel at all.
const elicitationObservations = {};
{
  const bus = new GatewayElicitationBus();
  let rejected = null;
  bus.register('s', {
    requestId: 'q',
    toolCallId: 'call',
    toolName: 'ask',
    resolve: () => {},
    reject: (error) => {
      rejected = error.message;
    },
  });
  const pendingBeforeClose = bus.pendingCount('s');
  bus.rejectSession('s', 'turn_ended');
  elicitationObservations.pendingBeforeClose = pendingBeforeClose;
  elicitationObservations.pendingAfterClose = bus.pendingCount('s');
  elicitationObservations.rejected = rejected;
  elicitationObservations.noticeEvents = [];
}

console.log(
  JSON.stringify(
    {
      commit,
      sourceSha256: shas,
      hooks: hooksObservations,
      steer: steerObservations,
      permission: permissionObservations,
      elicitation: elicitationObservations,
      discardNoticeNames: NOTICE_NAMES,
    },
    null,
    2,
  ),
);
