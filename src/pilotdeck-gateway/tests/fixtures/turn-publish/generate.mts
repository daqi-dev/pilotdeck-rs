// Real submitTurn prototype, real projector/coordinator/queue and FileRunRegistry.
// Only host services and the storage boundary are controlled; no product bodies copied.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
assert.ok(process.env.PDRS_BASELINE); assert.ok(process.argv[2]);
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/gateway/client/InProcessGateway.ts', 'src/gateway/run/RunRegistry.ts',
  'src/gateway/client/GatewayTurnReplayStore.ts', 'src/gateway/client/GatewayTurnEventCoordinator.ts',
  'src/gateway/client/GatewayAgentEventProjector.ts', 'src/gateway/util/AsyncQueue.ts',
  'src/gateway/server/GatewayWsConnection.ts', 'src/gateway/protocol/version.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
async function load(source: string, imports: Record<string, any> = {}) {
  const compiled = transformSync(await readFile(join(baseline, source), 'utf8'), {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
  }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', compiled)((name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    if (name in imports) return imports[name];
    return new Proxy({}, { get(_t, property) {
      if (property === '__esModule') return true;
      throw new Error(`unexercised import accessed: ${source} -> ${name}.${String(property)}`);
    } });
  }, module, module.exports);
  return module.exports;
}
const storeModule = await load(sources[2]);
const coordinatorModule = await load(sources[3], { './GatewayTurnReplayStore.js': storeModule });
const queueModule = await load(sources[5]);
const registryModule = await load(sources[1]);
const projectorModule = await load(sources[4], {
  './GatewayToolResultArtifactStore.js': { GatewayToolResultArtifactStore: class { persist() { throw Error('unexpected artifact'); } } },
});
const { InProcessGateway } = await load(sources[0], {
  './GatewayTurnReplayStore.js': storeModule,
  './GatewayTurnEventCoordinator.js': coordinatorModule,
  './GatewayAgentEventProjector.js': projectorModule,
  '../util/AsyncQueue.js': queueModule,
  '../run/RunRegistry.js': registryModule,
  '../../permission/index.js': { readPermissionSettings: () => ({}), permissionSettingsToRuleSet: () => ({ allow: [], deny: [], ask: [] }) },
  '../../agent/protocol/input.js': { parseAgentRunMode: () => undefined },
});
const version = await load(sources[7]);
const { GatewayWsConnection } = await load(sources[6], { '../protocol/version.js': version });
const clone = (x: any) => JSON.parse(JSON.stringify(x));
const defer = () => { let resolve!: () => void; const promise = new Promise<void>(r => resolve = r); return { promise, resolve }; };
// Drain Promise microtasks and I/O through a bounded event-loop barrier. No wall-time sleeps.
const settle = async () => { for (let i = 0; i < 12; ++i) await new Promise<void>(r => setImmediate(r)); };
const accepted = { type: 'input_accepted', sessionId: 's', turnId: 'r', input: { type: 'text', text: 'hello' }, sequence: 71 };
const delta = { type: 'model_event', event: { type: 'text_delta', text: 'hi' }, sequence: 72 };
const terminal = { type: 'turn_completed', result: { type: 'success', stopReason: 'completed', usage: {} }, sequence: 73 };
const cases: any[] = [];
async function run(name: string, setup: any, steps: any[]) {
  const root = await mkdtemp(join(tmpdir(), 'pdrs-turn-publish-'));
  const registry = new registryModule.FileRunRegistry(join(root, 'registry.json'));
  const persist = registry.persist.bind(registry);
  let storageFailure = false;
  registry.persist = async () => { if (storageFailure) throw Error('injected storage failure'); await persist(); };
  const run = { sessionKey: 's', runId: 'r', projectKey: 'p' };
  const input = { ...run, message: 'hello' };
  const calls: any[] = [], durable: any[] = [], visible: any[] = [], results: any[] = [];
  const requests: any[] = [], pending: any[] = [];
  const agentWaiters: any[] = []; const agentItems: any[] = [];
  let agentDone = false, ended = false;
  const requestMaterial = '{"message":"hello","projectKey":"p","sessionKey":"s"}';
  if (setup.seed) {
    await registry.accept({ ...run, requestMaterial: setup.conflict ? 'different' : requestMaterial });
    for (const event of setup.seed) await registry.append({ ...run, event });
  }
  const port = {
    async accept(value: any) {
      calls.push({ op: 'accept', requestMaterial: value.requestMaterial });
      if (setup.acceptFailure) throw Error('injected accept failure');
      storageFailure = setup.acceptStorageFailure === true;
      return registry.accept(value);
    },
    async events(value: any) { calls.push({ op: 'events' }); return registry.events(value); },
    append(value: any) {
      const index = requests.length;
      const gate = defer();
      calls.push({ op: 'append', index, event: clone(value.event) });
      requests.push({ value, gate, fail: false });
      // Serialize real storage like FileRunRegistry.mutationChain. A failed
      // write must not prevent the following queued request from running.
      const previous = pending.at(-1) ?? Promise.resolve();
      const write = previous.catch(() => {}).then(async () => {
        await gate.promise;
        if (requests[index].fail && !setup.storageFailure) throw Error('injected append failure');
        storageFailure = requests[index].fail && setup.storageFailure === true;
        await registry.append(value).finally(() => storageFailure = false);
        durable.push(clone(value.event));
      });
      pending.push(write);
      return write;
    },
  };
  const coordinator = new coordinatorModule.GatewayTurnEventCoordinator();
  const session = { async *submit() {
    for (;;) {
      if (agentItems.length) { yield agentItems.shift(); continue; }
      if (agentDone) return;
      await new Promise<void>(r => agentWaiters.push(r));
    }
  } };
  const gateway = Object.create(InProcessGateway.prototype);
  Object.assign(gateway, {
    options: { runRegistry: setup.noRegistry ? undefined : port },
    dialogProjectKeys: new Map(), workspaceAdmissions: new Map(), activeFailureGuards: new Map(), abortedTurnReasons: new Map(),
    turnEventCoordinator: coordinator,
    agentEventProjector: new projectorModule.GatewayAgentEventProjector(),
    turnReplacementCoordinator: { claimForSubmit: () => setup.preflight ? 'conflict' : 'ok', hasTranscriptWriteReservation: () => false,
      commitAcceptedInput: async () => {}, releaseSubmitClaim() {} },
    turnCompletionFence: { begin: () => ({ signal: new AbortController().signal }), isCurrent: () => true, complete() {} },
    router: { beginTurn: () => true, getOrCreate: async () => session, endTurn() {} },
    interactionCoordinator: { sessionAllowRules: () => [], rejectPendingTurn() {} },
    permissionModes: { get: () => undefined },
    turnTelemetryContextResolver: { resolve: () => ({}) },
    agentEventTelemetryObserver: { observe() {} },
    prepareAttachmentTurn: async () => ({ agentInput: { type: 'text', text: 'hello' }, allowedReadFiles: [] }),
  });
  const wsFrames: any[] = [];
  const connection = new GatewayWsConnection({ onMessage() {}, onClose() {}, close() { throw Error('unexpected close'); },
    sendText(text: string) { const frame = JSON.parse(text); if (frame.type === 'event') wsFrames.push(frame); } }, {
      token: 'fixture-token', serverVersion: 'test', gateway: {
        describeServer: async () => ({ name: 'fixture', capabilities: [] }),
        async *submitTurn() { for await (const event of gateway.submitTurn(input)) { visible.push(clone(event)); yield event; } },
      },
    });
  await connection.handleMessage(JSON.stringify({ type: 'hello', protocolVersion: '1.2', clientName: 'ui', clientVersion: 'test', token: 'fixture-token' }));
  const consumer = connection.handleMessage(JSON.stringify({ type: 'request', id: 'request-9', method: 'submit_turn', params: input }))
    .then(() => { ended = true; });
  const snapshot = async () => {
    // Await real I/O only for writes whose explicit gate has been released.
    // A blocked earlier write keeps later ones pending, exactly like TS.
    await settle();
    await registry.ready; await registry.mutationChain;
    if (setup.seed || setup.acceptFailure || setup.acceptStorageFailure) await consumer;
    await settle();
    const record = await registry.get(run);
    results.push({ wsFrames: clone(wsFrames), visible: clone(visible), calls: clone(calls), durable: clone(durable), ended,
      record: record ? { state: record.state, lastSeq: record.lastSeq, revision: record.revision,
        ...(record.result ? { result: record.result } : {}) } : null,
      registryEvents: await registry.events(run),
      replay: coordinator.snapshot({ sessionKey: 's' }) ?? null });
  };
  try {
    for (const step of steps) {
      if (step.op === 'agent') { agentItems.push(step.event); agentWaiters.splice(0).forEach(r => r()); }
      else if (step.op === 'end') { agentDone = true; agentWaiters.splice(0).forEach(r => r()); }
      else if (step.op === 'emit') coordinator.emit('s', step.event);
      else if (step.op === 'release') {
        assert.ok(requests[step.index], `${name}: missing request ${step.index}`);
        requests[step.index].fail = step.fail === true;
        requests[step.index].released = true;
        requests[step.index].gate.resolve();
        for (let i = 0; i < requests.length && requests[i].released; ++i) await pending[i].catch(() => {});
      }
      else assert.equal(step.op, 'observe');
      await snapshot();
    }
    assert.equal(ended, true, `${name}: script must finish consumer`);
    await consumer;
    cases.push({ name, setup, steps, results });
  } finally { coordinator.dispose(); await rm(root, { recursive: true, force: true }); }
}
await run('ordered-durable-terminal-after-session-end', {}, [
  { op: 'observe' }, { op: 'agent', event: accepted }, { op: 'agent', event: delta },
  { op: 'agent', event: terminal }, { op: 'emit', event: { type: 'agent_status', event: 'SessionEnd', moduleSeq: 99 } },
  { op: 'release', index: 1 }, // resolving later gate must not overtake first write
  { op: 'release', index: 0 }, { op: 'release', index: 2 },
  { op: 'end' }, { op: 'release', index: 3 }, { op: 'emit', event: { type: 'late' } },
]);
await run('append-failure-then-success-single-unknown', {}, [
  { op: 'observe' }, { op: 'agent', event: accepted }, { op: 'agent', event: delta },
  { op: 'release', index: 0, fail: true }, { op: 'release', index: 1 },
  { op: 'agent', event: terminal }, { op: 'end' }, { op: 'release', index: 2, fail: true },
]);
await run('real-storage-persist-failure', { storageFailure: true }, [
  { op: 'observe' }, { op: 'agent', event: accepted }, { op: 'release', index: 0, fail: true },
  { op: 'agent', event: terminal }, { op: 'end' }, { op: 'release', index: 1 },
]);
await run('accept-storage-failure-result-unknown', { acceptStorageFailure: true }, [{ op: 'observe' }]);
await run('all-appends-fail-only-one-unknown', {}, [
  { op: 'observe' }, { op: 'agent', event: accepted }, { op: 'agent', event: delta },
  { op: 'release', index: 0, fail: true }, { op: 'release', index: 1, fail: true },
  { op: 'agent', event: terminal }, { op: 'end' }, { op: 'release', index: 2, fail: true },
]);
await run('close-waits-for-pending-append', {}, [
  { op: 'observe' }, { op: 'agent', event: accepted }, { op: 'end' },
  { op: 'observe' }, { op: 'release', index: 0 },
]);
await run('late-event-while-terminal-append-pending', {}, [
  { op: 'observe' }, { op: 'agent', event: terminal }, { op: 'end' },
  { op: 'emit', event: { type: 'agent_status', event: 'late' } },
  { op: 'release', index: 0 }, { op: 'release', index: 1 },
]);
await run('duplicate-terminal-is-registry-failure-not-second-success', {}, [
  { op: 'observe' }, { op: 'agent', event: terminal }, { op: 'agent', event: terminal },
  { op: 'end' }, { op: 'release', index: 0 }, { op: 'release', index: 1 },
]);
await run('without-registry', { noRegistry: true }, [
  { op: 'observe' }, { op: 'agent', event: accepted }, { op: 'agent', event: delta },
  { op: 'agent', event: terminal }, { op: 'end' }, { op: 'emit', event: { type: 'late' } },
]);
await run('duplicate-replays-durable-events', { seed: [{ type: 'agent_status', runId: 'r', event: 'seed' },
  { type: 'turn_completed', runId: 'r', usage: {}, finishReason: 'completed' }] }, [{ op: 'observe' }]);
await run('conflict-does-not-start-pump', { seed: [], conflict: true }, [{ op: 'observe' }]);
await run('accept-failure-result-unknown', { acceptFailure: true }, [{ op: 'observe' }]);
await run('preflight-registry-failure', { preflight: true }, [
  { op: 'observe' }, { op: 'release', index: 0, fail: true },
]);
await run('preflight-durable-before-visible', { preflight: true }, [
  { op: 'observe' }, { op: 'release', index: 0 },
]);
assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n');
console.log(`recorded ${cases.length} real TS submitTurn scenarios`);
