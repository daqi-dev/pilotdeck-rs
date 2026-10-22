// Real InProcessGateway read face (getActiveTurnSnapshot, runGet, runEvents,
// runReattach, readSessionMessages) with the real FileRunRegistry, coordinator
// and replay store. Only the durable history reader and one injected registry
// failure are controlled; no product bodies are copied. The sameTurnEpoch
// retry loop is the product implementation. Its exhaustive epoch matrix is
// golden-tested by tests/fixtures/turn-replay; this fixture pins the read
// face: the history/stream merge shape, error propagation, the registry
// projection and cursor paging. Every result also records its top-level key
// order, so a port with the right fields in the wrong order fails.
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
  'src/gateway/dialog/errors.ts'];
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
const registryModule = await load(sources[1]);
const errorsModule = await load(sources[4]);
const { InProcessGateway } = await load(sources[0], {
  './GatewayTurnReplayStore.js': storeModule,
  './GatewayTurnEventCoordinator.js': coordinatorModule,
  '../run/RunRegistry.js': registryModule,
  '../dialog/errors.js': errorsModule,
});
const clone = (x: any) => JSON.parse(JSON.stringify(x));
// Registry records carry this process's accept/append timestamps; both the
// generator and the Rust replay mask them before comparison.
const maskRecord = (record: any) => {
  if (record === undefined) return null;
  const masked = clone(record);
  for (const key of ['acceptedAt', 'updatedAt']) if (typeof masked[key] === 'string') masked[key] = '<date>';
  return masked;
};
const keys = (value: any) => (value !== null && typeof value === 'object' && !Array.isArray(value))
  ? Object.keys(value) : null;
const errorOf = (error: any) => ({ name: error.name, ...(error.code !== undefined ? { code: error.code } : {}),
  message: error.message });

const readCases: any[] = [];
async function readCase(name: string, setup: any, historyPlan: any[]) {
  const coordinator = new coordinatorModule.GatewayTurnEventCoordinator();
  let currentRun: string | null = null;
  if (setup.startRun !== undefined) {
    coordinator.start('s', setup.startRun, () => {});
    currentRun = setup.startRun;
    for (const event of setup.events ?? []) coordinator.record('s', event);
  }
  const seenInputs: any[] = [];
  let calls = 0;
  const options: any = {};
  if (setup.configured !== false) {
    options.readSessionMessages = async (input: any) => {
      const plan = historyPlan[calls];
      assert.ok(plan, `${name}: history call ${calls} was not planned`);
      seenInputs.push(clone(input));
      calls += 1;
      // Mutations run inside the host history read, after the epoch's `before`
      // snapshot and before the `stream` snapshot observes them.
      for (const mutation of plan.mutate ?? []) {
        if (mutation === 'retain') coordinator.retainTerminal('s', currentRun);
        else if (mutation.start !== undefined) {
          coordinator.start('s', mutation.start, () => {});
          currentRun = mutation.start;
        } else if (mutation.record !== undefined) coordinator.record('s', mutation.record);
        else assert.fail(`unknown mutation ${JSON.stringify(mutation)}`);
      }
      if (plan.error) { const error = new Error(plan.error.message); error.name = plan.error.name; throw error; }
      return clone(plan.history);
    };
  }
  const gateway = Object.create(InProcessGateway.prototype);
  gateway.turnEventCoordinator = coordinator;
  gateway.options = options;
  let outcome: any;
  try {
    const result = await gateway.readSessionMessages(setup.input);
    outcome = { historyCalls: calls, seenInputs: clone(seenInputs), resultKeys: keys(result), result: clone(result) };
  } catch (error: any) {
    outcome = { historyCalls: calls, seenInputs: clone(seenInputs), error: errorOf(error) };
  }
  readCases.push({ name, setup, history: historyPlan, outcome });
  coordinator.dispose();
}
const historyBody = {
  messages: [{ id: 'm1', role: 'user', text: 'hello' }, { id: 'm2', role: 'assistant', text: 'hi' }],
  total: 2,
  session: { sessionKey: 's', projectKey: 'p', title: 'fixture' },
};
const accepted = { type: 'input_accepted', runId: 'r1' };
const delta = { type: 'model_event', runId: 'r1', text: 'hi' };
await readCase('read-stable-active-merges-history-and-stream',
  { input: { sessionKey: 's', limit: 50 }, startRun: 'r1', events: [accepted, delta] },
  [{ history: historyBody }]);
await readCase('read-stable-missing-turn',
  { input: { sessionKey: 's' } },
  [{ history: { messages: [], total: 0, session: { sessionKey: 's' } } }]);
await readCase('read-terminal-during-read-retries-then-merges-terminal-stream',
  { input: { sessionKey: 's' }, startRun: 'r1', events: [accepted] },
  [{ mutate: ['retain'], history: historyBody }, { history: historyBody }]);
await readCase('read-replaced-run-during-read-retries',
  { input: { sessionKey: 's' }, startRun: 'r1', events: [accepted] },
  [{ mutate: [{ start: 'r2' }, { record: { type: 'agent_status', runId: 'r2', event: 'restarted' } }], history: historyBody },
    { history: historyBody }]);
await readCase('read-epoch-never-settles-error',
  { input: { sessionKey: 's' }, startRun: 'r1', events: [accepted] },
  [{ mutate: [{ start: 'r2' }], history: historyBody },
    { mutate: [{ start: 'r3' }], history: historyBody },
    { mutate: [{ start: 'r4' }], history: historyBody }]);
await readCase('read-history-error-propagates-without-retry',
  { input: { sessionKey: 's' }, startRun: 'r1', events: [accepted] },
  [{ mutate: [{ start: 'r2' }], error: { name: 'Error', message: 'injected history failure' } }]);
await readCase('read-not-configured',
  { input: { sessionKey: 's' }, configured: false }, []);
await readCase('read-history-stream-key-replaced-in-place',
  { input: { sessionKey: 's' }, startRun: 'r1', events: [accepted] },
  [{ history: { stream: 'stale', messages: [], total: 0 } }]);

const snapshotCases: any[] = [];
async function snapshotCase(name: string, setup: any, calls: any[]) {
  const coordinator = new coordinatorModule.GatewayTurnEventCoordinator();
  if (setup.startRun !== undefined) {
    coordinator.start('s', setup.startRun, () => {});
    for (const event of setup.events ?? []) coordinator.record('s', event);
  }
  const gateway = Object.create(InProcessGateway.prototype);
  gateway.turnEventCoordinator = coordinator;
  gateway.options = {};
  const results: any[] = [];
  for (const call of calls) {
    const result = await gateway.getActiveTurnSnapshot(call);
    results.push({ keys: keys(result), result: clone(result) });
  }
  snapshotCases.push({ name, setup, calls, results });
  coordinator.dispose();
}
await snapshotCase('snapshot-delegation-events-and-states',
  { startRun: 'r1', events: [accepted, delta] }, [
    { sessionKey: 's' },
    { sessionKey: 's', includeEvents: false },
    { sessionKey: 'missing' },
  ]);

const runCases: any[] = [];
async function runCase(name: string, setup: any, calls: any[]) {
  const root = await mkdtemp(join(tmpdir(), 'pdrs-turn-read-'));
  const registry = new registryModule.FileRunRegistry(join(root, 'registry.json'));
  for (const seed of setup.seed ?? []) {
    const ref = { sessionKey: seed.sessionKey, runId: seed.runId,
      ...(seed.projectKey === undefined ? {} : { projectKey: seed.projectKey }) };
    await registry.accept({ ...ref, requestMaterial: seed.material });
    for (const event of seed.events ?? []) await registry.append({ ...ref, event });
  }
  let port: any = registry;
  if (setup.registryFailure) {
    const failure = new registryModule.RunRegistryError(setup.registryFailure.code, setup.registryFailure.message);
    port = { get: async () => { throw failure; }, events: async () => { throw failure; } };
  }
  const gateway = Object.create(InProcessGateway.prototype);
  gateway.turnEventCoordinator = new coordinatorModule.GatewayTurnEventCoordinator();
  gateway.options = setup.noRegistry ? {} : { runRegistry: port };
  const outcomes: any[] = [];
  for (const call of calls) {
    try {
      let result: any;
      if (call.method === 'runGet') result = await gateway.runGet(call.input);
      else if (call.method === 'runEvents') result = await gateway.runEvents(call.input);
      else if (call.method === 'runReattach') result = await gateway.runReattach(call.input);
      else assert.fail(`unknown method ${call.method}`);
      const mapped = call.method === 'runEvents' ? clone(result) : maskRecord(result);
      outcomes.push({ keys: keys(mapped), ...(mapped === null ? {} : { result: mapped }) });
    } catch (error: any) {
      outcomes.push({ error: errorOf(error) });
    }
  }
  runCases.push({ name, setup, calls, outcomes });
  await rm(root, { recursive: true, force: true });
}
const ref = (runId: string) => ({ sessionKey: 's', projectKey: 'p', runId });
const status = (index: number) => ({ type: 'agent_status', runId: 'r1', event: `e${index}` });
const completed = { type: 'turn_completed', runId: 'r1', usage: {}, finishReason: 'completed' };
await runCase('run-get-maps-durable-records', { seed: [
  { sessionKey: 's', projectKey: 'p', runId: 'r1', material: 'm',
    events: [{ type: 'input_accepted', runId: 'r1' }, { type: 'model_event', runId: 'r1', text: 'hi' }, completed] },
  { sessionKey: 's', runId: 'r2', material: 'm2', events: [] },
] }, [
  { method: 'runGet', input: ref('r1') },
  { method: 'runGet', input: { sessionKey: 's', runId: 'r2' } },
  { method: 'runReattach', input: ref('r1') },
  { method: 'runEvents', input: ref('r1') },
]);
await runCase('run-get-missing-cross-session-and-cross-project', { seed: [
  { sessionKey: 's', projectKey: 'p', runId: 'r1', material: 'm', events: [] },
] }, [
  { method: 'runGet', input: ref('missing') },
  { method: 'runGet', input: { sessionKey: 's2', projectKey: 'p', runId: 'r1' } },
  { method: 'runGet', input: { sessionKey: 's', projectKey: 'p2', runId: 'r1' } },
  { method: 'runGet', input: { sessionKey: 's', runId: 'r1' } },
  { method: 'runReattach', input: { sessionKey: 's2', runId: 'r1' } },
]);
await runCase('run-events-paging', { seed: [
  { sessionKey: 's', projectKey: 'p', runId: 'r1', material: 'm',
    events: [status(1), status(2), status(3), status(4), status(5)] },
] }, [
  { method: 'runEvents', input: ref('r1') },
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: 2, limit: 2 } },
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: 4 } },
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: 5 } },
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: 6 } },
  { method: 'runEvents', input: { ...ref('r1'), limit: 1 } },
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: 0, limit: 500 } },
]);
await runCase('run-events-validation-and-not-found', { seed: [
  { sessionKey: 's', projectKey: 'p', runId: 'r1', material: 'm',
    events: [{ type: 'input_accepted', runId: 'r1' }] },
  { sessionKey: 's', projectKey: 'p', runId: 'empty', material: 'm2', events: [] },
] }, [
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: -1 } },
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: 1.5 } },
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: '3' } },
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: 9007199254740992 } },
  { method: 'runEvents', input: { ...ref('r1'), limit: 0 } },
  { method: 'runEvents', input: { ...ref('r1'), limit: 501 } },
  { method: 'runEvents', input: { ...ref('r1'), limit: 2.5 } },
  { method: 'runEvents', input: { ...ref('r1'), limit: '10' } },
  { method: 'runEvents', input: { ...ref('ghost'), afterSeq: -1 } },
  { method: 'runEvents', input: ref('ghost') },
  { method: 'runEvents', input: ref('empty') },
  { method: 'runEvents', input: { ...ref('r1'), afterSeq: 1, limit: 1 } },
  { method: 'runEvents', input: { ...ref('r1'), limit: 1 } },
]);
await runCase('run-capability-unavailable', { noRegistry: true }, [
  { method: 'runGet', input: ref('r1') },
  { method: 'runEvents', input: ref('r1') },
  { method: 'runReattach', input: ref('r1') },
]);
await runCase('run-registry-failure-propagates',
  { registryFailure: { code: 'terminal', message: 'injected registry failure' } }, [
    { method: 'runGet', input: ref('r1') },
    { method: 'runEvents', input: ref('r1') },
  ]);

assertBaseline(baseline);
assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({
  sourceCommit, sourceSha256, readCases, snapshotCases, runCases,
}, null, 2) + '\n');
console.log(`recorded ${readCases.length} read, ${snapshotCases.length} snapshot, ${runCases.length} run cases from real TS`);
