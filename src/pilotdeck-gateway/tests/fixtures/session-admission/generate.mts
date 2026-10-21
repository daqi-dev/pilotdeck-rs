// Run from the repository root with the fixed baseline's tsx loader; see README.md.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide an output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/gateway/SessionRouter.ts',
  'src/gateway/client/GatewayTurnCompletionFence.ts',
  'src/gateway/client/InProcessGateway.ts',
  ...['AgentFactoryProvider', 'AgentRegistry', 'AgentHandle', 'ScopedServiceRegistry']
    .map(name => `src/agent/scope/${name}.ts`),
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
// Transpile entire, unchanged modules. Load the real scope dependencies of the
// Router rather than the unrelated agent barrel. Gateway constructor wiring is
// skipped; any attempt to use an unrelated imported service fails closed.
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
const scope = 'src/agent/scope/';
const services = await load(scope + 'ScopedServiceRegistry.ts');
const handles = await load(scope + 'AgentHandle.ts');
const registry = await load(scope + 'AgentRegistry.ts', {
  './AgentHandle.js': handles, './ScopedServiceRegistry.js': services,
});
const factory = await load(scope + 'AgentFactoryProvider.ts', {
  './AgentHandle.js': handles, './AgentRegistry.js': registry,
});
const { SessionRouter } = await load(sources[0], {
  '../agent/index.js': { ...handles, ...registry, ...factory },
});
const { GatewayTurnCompletionFence } = await load(sources[1]);
const { InProcessGateway } = await load(sources[2]);

type Step = { op: string; key?: string; run?: string; id?: string; reason?: string };
const scenarios: { name: string; steps: Step[]; pending?: string[] }[] = [
  { name: 'admission', steps: [
    { op: 'begin', key: 'a', run: 'old' }, { op: 'begin', key: 'a', run: 'duplicate' },
    { op: 'begin', key: 'b', run: 'parallel' }, { op: 'active', key: 'a' },
    { op: 'end', key: 'a', run: 'wrong' }, { op: 'active', key: 'a' },
    { op: 'end', key: 'a', run: 'old' }, { op: 'begin', key: 'a', run: 'new' },
    { op: 'end', key: 'a', run: 'old' }, { op: 'active', key: 'a' },
    { op: 'end', key: 'a', run: '' }, { op: 'active', key: 'a' },
    { op: 'begin', key: 'a', run: '' }, { op: 'active', key: 'a' },
    { op: 'begin', key: 'a', run: 'busy-even-empty' }, { op: 'end', key: 'a' },
    { op: 'begin', key: '会话\u0000a', run: '中文' }, { op: 'active', key: '会话\u0000a' },
    { op: 'shutdown' }, { op: 'active', key: 'b' }, { op: 'begin', key: 'c', run: 'late' },
  ] },
  { name: 'maintenance', steps: [
    { op: 'maintenance_start', key: 'a' }, { op: 'begin', key: 'a', run: 'blocked' },
    { op: 'begin', key: 'b', run: 'parallel' }, { op: 'active', key: 'a' },
    { op: 'maintenance_end' }, { op: 'begin', key: 'a', run: 'after' },
  ] },
  { name: 'completion-identity', steps: [
    { op: 'fence_begin', key: 'a', run: 'same', id: 'old' },
    { op: 'wait', key: 'a', id: 'old-wait' },
    { op: 'fence_begin', key: 'a', run: 'same', id: 'new' },
    { op: 'wait', key: 'a', id: 'new-wait' },
    { op: 'current', key: 'a', id: 'old' }, { op: 'current', key: 'a', id: 'new' },
    { op: 'complete', key: 'a', id: 'old' },
    { op: 'current', key: 'a', id: 'new' }, { op: 'complete', key: 'a', id: 'old' },
    { op: 'cancel', key: 'a', run: 'wrong', reason: 'stale' },
    { op: 'cancel', key: 'a', run: 'same', reason: 'first' },
    { op: 'cancel', key: 'a', run: 'same', reason: 'second' },
    { op: 'complete', key: 'a', id: 'new' }, { op: 'wait', key: 'a', id: 'absent' },
    { op: 'cancel', key: 'a', reason: 'no-handle' },
  ] },
  { name: 'completion-empty-run', steps: [
    { op: 'fence_begin', key: 'a', run: 'r', id: 'h' },
    { op: 'cancel', key: 'a', run: '', reason: 'wrong' },
    { op: 'cancel', key: 'a' }, { op: 'complete', key: 'a', id: 'h' },
    { op: 'fence_begin', key: 'a', run: '', id: 'empty' },
    { op: 'cancel', key: 'a', run: '', reason: '' }, { op: 'complete', key: 'a', id: 'empty' },
  ] },
  { name: 'discarded-resolver-stays-pending', pending: ['orphan'], steps: [
    { op: 'fence_begin', key: 'a', run: 'old', id: 'old' },
    { op: 'handle_wait', id: 'old' },
    { op: 'fence_begin', key: 'a', run: 'new', id: 'new' },
    { op: 'discard_handle', id: 'old' },
    { op: 'complete', key: 'a', id: 'new' },
  ] },
];
// Exercise the actual Gateway abort method with the actual Router and fence.
for (const [name, active, fenced, requested] of [
  ['stale', 'new', 'new', 'old'], ['exact', 'new', 'new', 'new'],
  ['admitted-before-fence', 'new', undefined, 'new'],
  ['stale-before-fence', 'new', undefined, 'old'],
  ['absent-run', 'new', 'new', undefined], ['empty-stale', 'new', 'new', ''],
  ['empty-exact', '', '', ''], ['missing-session', undefined, undefined, 'old'],
] as const) {
  const steps: Step[] = [];
  if (active !== undefined) steps.push({ op: 'begin', key: 'a', run: active });
  if (fenced !== undefined) steps.push({ op: 'fence_begin', key: 'a', run: fenced, id: 'h' });
  steps.push({ op: 'abort_start', key: 'a', run: requested, id: 'abort' });
  if (fenced !== undefined) steps.push({ op: 'end', key: 'a', run: active }, { op: 'complete', key: 'a', id: 'h' });
  steps.push({ op: 'active', key: 'a' });
  scenarios.push({ name: `gateway-${name}`, steps });
}
scenarios.push({ name: 'gateway-old-after-replace', steps: [
  { op: 'begin', key: 'a', run: 'old' }, { op: 'fence_begin', key: 'a', run: 'old', id: 'old' },
  { op: 'end', key: 'a', run: 'old' }, { op: 'begin', key: 'a', run: 'new' },
  { op: 'fence_begin', key: 'a', run: 'new', id: 'new' },
  { op: 'abort_start', key: 'a', run: 'old', id: 'abort' },
  { op: 'complete', key: 'a', id: 'old' }, { op: 'active', key: 'a' },
  { op: 'current', key: 'a', id: 'new' }, { op: 'complete', key: 'a', id: 'new' },
] });

const recorded = [];
for (const scenario of scenarios) {
  const aborts: any[] = [];
  let releaseMaintenance!: () => void;
  let maintenance: Promise<any> | undefined;
  // A zero idle timeout does NOT disable sweepIdle in the real Router. Freeze
  // time so a millisecond tick cannot evict our fake session before admission.
  const router = new SessionRouter({ idleSweepIntervalMs: 0, idleSessionTimeoutMs: 60_000,
    now: () => new Date(0),
    createSession: () => ({
      abort: (reason: string) => aborts.push(reason), dispose: async () => {},
      seedReadState: async () => { await new Promise<void>(r => { releaseMaintenance = r; }); return { applied: true }; },
    }),
  });
  await router.getOrCreate({ sessionKey: 'a', channelKey: 'fixture' });
  const fence = new GatewayTurnCompletionFence();
  // Constructor creates many unrelated services; none participates in abortTurn.
  const gateway = Object.assign(Object.create(InProcessGateway.prototype), {
    router, turnCompletionFence: fence, abortedTurnReasons: new Map(),
  });
  const handles = new Map<string, any>();
  const settled: Record<string, boolean> = {};
  const pending: Promise<void>[] = [];
  const results = [];
  for (const step of scenario.steps) {
    const key = step.key!;
    let value: any = null;
    switch (step.op) {
      case 'begin': value = router.beginTurn(key, step.run); break;
      case 'active': value = { active: router.hasActiveTurn(key), run: router.activeTurnRunId(key) ?? null }; break;
      case 'end': router.endTurn(key, step.run); break;
      case 'shutdown': await router.shutdown(); break;
      case 'maintenance_start': {
        maintenance = router.seedReadState({ sessionKey: key, channelKey: 'fixture' }, { path: 'x', mtime: 1 });
        // seedReadState reserves synchronously; drain its finite creation chain.
        while (!releaseMaintenance) await Promise.resolve();
        break;
      }
      case 'maintenance_end': releaseMaintenance(); await maintenance; break;
      case 'fence_begin': {
        const handle = fence.begin(key, step.run!);
        handles.set(step.id!, handle); break;
      }
      case 'current': value = fence.isCurrent(key, handles.get(step.id!)); break;
      case 'cancel': value = fence.cancel(key, step.reason, step.run); break;
      case 'complete': fence.complete(key, handles.get(step.id!)); break;
      case 'discard_handle': handles.delete(step.id!); break;
      case 'handle_wait': {
        settled.orphan = false;
        pending.push(handles.get(step.id!).done.then(() => { settled.orphan = true; }));
        break;
      }
      case 'wait':
      case 'abort_start': {
        const id = step.id!;
        settled[id] = false;
        const operation = step.op === 'wait' ? fence.waitForCompletion(key)
          : gateway.abortTurn({ sessionKey: key, runId: step.run, reason: step.reason });
        pending.push(operation.then(() => { settled[id] = true; }));
        break;
      }
      default: throw new Error(step.op);
    }
    // No timers: these methods have a bounded promise chain (abort -> wait -> done).
    for (let i = 0; i < 12; i++) await Promise.resolve();
    results.push({ value, handles: Object.fromEntries([...handles].map(([id, h]) => [id, {
      cancelled: h.signal.aborted,
      reason: !h.signal.aborted ? null : h.signal.reason instanceof Error
        ? { name: h.signal.reason.name, message: h.signal.reason.message } : h.signal.reason,
    }])), settled: { ...settled }, aborts: [...aborts],
    reasons: [...gateway.abortedTurnReasons.entries()],
    });
  }
  assert.deepEqual(Object.keys(settled).filter(id => !settled[id]), scenario.pending ?? [],
    `unexpected undrained wait in ${scenario.name}`);
  if (!scenario.pending) await Promise.all(pending);
  recorded.push({ ...scenario, results });
  await router.shutdown();
}
assertBaseline(baseline);
await baselineSourceHashes(baseline, sources);
await mkdir(resolve(process.argv[2]), { recursive: true });
await writeFile(join(resolve(process.argv[2]), 'oracle.json'), JSON.stringify({ sourceCommit, sourceSha256, scenarios: recorded }, null, 2) + '\n');
console.log(`recorded ${recorded.length} scenarios from ${sourceCommit}`);
