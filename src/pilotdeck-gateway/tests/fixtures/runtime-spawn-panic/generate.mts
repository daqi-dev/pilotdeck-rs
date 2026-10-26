// Execute real TS bodies; only runtime/storage ports and host services are injected.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
const baseline = resolve(process.env.PDRS_BASELINE!);
const sourceCommit = assertBaseline(baseline);
const sources = ['src/mcp/runtime/SessionMcpRuntimeRegistry.ts', 'src/gateway/client/InProcessGateway.ts',
  'src/gateway/client/GatewayTurnReplayStore.ts', 'src/gateway/client/GatewayTurnEventCoordinator.ts',
  'src/gateway/util/AsyncQueue.ts', 'src/gateway/client/GatewayAgentEventProjector.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const require = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(require.resolve('tsx'))('esbuild');
async function load(source: string, imports: Record<string, any> = {}) {
  const code = transformSync(await readFile(join(baseline, source), 'utf8'), { loader: 'ts', format: 'cjs' }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', code)((name: string) => {
    if (name.startsWith('node:')) return require(name);
    if (name in imports) return imports[name];
    return new Proxy({}, { get: (_, key) => {
      if (key === '__esModule') return true;
      throw Error(`unexpected import ${source}: ${name}.${String(key)}`);
    } });
  }, module, module.exports);
  return module.exports;
}
const { SessionMcpRuntimeRegistry } = await load(sources[0]);
const store = await load(sources[2]);
const coordinator = await load(sources[3], { './GatewayTurnReplayStore.js': store });
const queue = await load(sources[4]);
const projector = await load(sources[5], { './GatewayToolResultArtifactStore.js': { GatewayToolResultArtifactStore: class { persist() { throw Error('unexpected artifact'); } } } });
const { InProcessGateway } = await load(sources[1], {
  './GatewayTurnReplayStore.js': store, './GatewayTurnEventCoordinator.js': coordinator,
  './GatewayAgentEventProjector.js': projector, '../util/AsyncQueue.js': queue,
  '../../permission/index.js': { readPermissionSettings: () => ({}), permissionSettingsToRuleSet: () => ({ allow: [], deny: [], ask: [] }) },
  '../../agent/protocol/input.js': { parseAgentRunMode: () => undefined },
});
const defer = () => { let resolve!: () => void, reject!: (e: Error) => void; const promise = new Promise<void>((r, j) => { resolve = r; reject = j; }); return { promise, resolve, reject }; };
const tick = () => new Promise<void>(r => setImmediate(r));
const deadline = setTimeout(() => { throw Error('spawn panic probe exceeded 10 seconds'); }, 10000);
// Abort only the observer, exactly like dropping a Rust waiting future.
function observe(work: Promise<any>, signal: AbortSignal) {
  return Promise.race([work, new Promise(r => signal.addEventListener('abort', () => r('cancelled'), { once: true }))]);
}
const cases: any[] = [];
const matrix = ['shared', 'durable'].flatMap(scope => [1, 3].flatMap(n => ['P0', 'P1'].flatMap(phase => ['W', 'C'].map(state => ({ scope, n, phase, state, id: `${scope}-${n}-${phase}-${state}` })))));
for (const scope of ['shared', 'durable']) matrix.push({ scope, n: 1, phase: 'cancelled', state: 'W', id: `predecessor-${scope}` });
for (const { scope, n, phase, state, id } of matrix) {
  const gate = defer(), started = defer(), begin = defer();
  const visible: any[] = []; let calls = 0, cleaned = false;
  let previous = Promise.resolve(), successorPolled = false;
  const fail = () => {
    const index = calls++;
    const operation = async () => {
      if (phase === 'cancelled' && index > 0) { successorPolled = true; return; }
      started.resolve(); if (phase !== 'P0') await gate.promise; throw Error('injected panic');
    };
    const write = phase === 'cancelled' ? previous.catch(() => {}).then(operation) : operation();
    previous = write; return write;
  };
  let work: Promise<any>, owner: any;
  if (scope === 'shared') {
    owner = new SessionMcpRuntimeRegistry(); owner.register('s', { stop: fail });
    work = owner.dispose().catch((e: Error) => e.message);
    for (let i = 1; i < n; i++) assert.equal(owner.dispose(), owner.dispose());
  } else {
    owner = new coordinator.GatewayTurnEventCoordinator();
    const session = { async *submit() { await begin.promise;
      yield { type: 'model_event', event: { type: 'text_delta', text: 'hi' } };
      yield { type: 'model_event', event: { type: 'text_delta', text: 'again' } };
    } };
    const gateway = Object.create(InProcessGateway.prototype);
    Object.assign(gateway, {
      options: { runRegistry: { accept: async () => ({ duplicate: false }), append: fail } },
      dialogProjectKeys: new Map(), workspaceAdmissions: new Map(), activeFailureGuards: new Map(), abortedTurnReasons: new Map(),
      turnEventCoordinator: owner, agentEventProjector: new projector.GatewayAgentEventProjector(),
      turnReplacementCoordinator: { claimForSubmit: () => 'ok', hasTranscriptWriteReservation: () => false,
        commitAcceptedInput: async () => {}, releaseSubmitClaim() {} },
      turnCompletionFence: { begin: () => ({ signal: new AbortController().signal }), isCurrent: () => true, complete() {} },
      router: { beginTurn: () => true, getOrCreate: async () => session, endTurn() { cleaned = true; } },
      interactionCoordinator: { sessionAllowRules: () => [], rejectPendingTurn() {} }, permissionModes: { get: () => undefined },
      turnTelemetryContextResolver: { resolve: () => ({}) }, agentEventTelemetryObserver: { observe() {} },
      prepareAttachmentTurn: async () => ({ agentInput: { type: 'text', text: 'hello' }, allowedReadFiles: [] }),
    });
    work = (async () => { for await (const event of gateway.submitTurn({ sessionKey: 's', runId: 'r', projectKey: 'p', message: 'hello' })) visible.push(event); return 'drained'; })();
  }
  const abort = new AbortController();
  let waiting = Array.from({ length: n }, () => observe(work, abort.signal));
  if (state === 'C') {
    abort.abort(); assert.deepEqual(await Promise.all(waiting), Array(n).fill('cancelled'));
    waiting = Array.from({ length: n }, () => work);
  }
  begin.resolve(); await started.promise;
  if (phase !== 'P0') {
    let done = false; void work.then(() => done = true); await tick();
    assert.equal(done, false, 'unreleased predecessor must remain pending');
    assert.equal(successorPolled, false);
    if (phase === 'cancelled') gate.reject(Error('cancelled gate')); else gate.resolve();
  }
  const outcomes = await Promise.all(waiting);
  if (scope === 'shared') { assert.equal(owner.state, 'disposed'); assert.equal(owner.size, 0); }
  else { assert.equal(cleaned, true); assert.equal(visible.length, phase === 'cancelled' ? 2 : 1); owner.dispose(); }
  cases.push({ id, outcomes, calls, visible });
}
clearTimeout(deadline);
assertBaseline(baseline); assert.deepEqual(await baselineSourceHashes(baseline, sources), sourceSha256);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, cases }) + '\n', { flag: 'wx' });
console.log(JSON.stringify(cases, null, 2));
