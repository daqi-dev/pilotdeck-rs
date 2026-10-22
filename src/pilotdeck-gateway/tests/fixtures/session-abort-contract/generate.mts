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
const sources = ['src/agent/scope/AgentHandle.ts', 'src/cli/ProjectRuntimeRegistry.ts'];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
async function load(source: string) {
  const compiled = transformSync(await readFile(join(baseline, source), 'utf8'), {
    loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
  }).code;
  const module = { exports: {} as any };
  new Function('require', 'module', 'exports', compiled)((name: string) => {
    if (name.startsWith('node:')) return requireBaseline(name);
    return new Proxy({}, { get(_target, property) {
      if (property === '__esModule') return true;
      throw new Error(`unexercised import accessed: ${source} -> ${name}.${String(property)}`);
    } });
  }, module, module.exports);
  return module.exports;
}
const { AgentHandle } = await load(sources[0]);
const { ProjectRuntimeRegistry } = await load(sources[1]);
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
  const session = {
    pendingTurns: () => [],
    async *submit() { runEntered.resolve(); await runGate.promise; },
    abort(reason: string) {
      abortCalls++;
      events.push(`abort:${reason}:${sessionDisposed ? 'disposed' : 'live'}`);
    },
    async dispose() {
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
  // Exercise the documented AgentHandle onDispose seam with session disposal
  // followed by release, matching the Rust adapter's two cleanup futures.
  const handle = new AgentHandle(session, { onDispose: async () => {
    try { await session.dispose(); } finally { await release(); }
  } });
  const running = active ? handle.submit({}).next() : undefined;
  if (active) await runEntered.promise;
  let done = false;
  const disposing = handle.dispose('dispose').then(() => { done = true; });
  if (active) { runGate.resolve(); await running; }
  await disposeEntered.promise;
  const checkpoints: any[] = [];
  const checkpoint = (phase: string) => checkpoints.push({ phase, state: handle.state,
    done, sessionDisposed, abortCalls, leaseCount: runtime.sessionLeases,
    runtimeState: runtime.runtimeState, events: [...events] });
  checkpoint('dispose_pending');
  handle.abort('during-dispose'); handle.abort('during-dispose');
  checkpoint('dispose_aborts');
  disposeGate.resolve(); await leaseEntered.promise;
  checkpoint('lease_pending');
  handle.abort('during-lease'); handle.abort('during-lease');
  checkpoint('lease_aborts');
  leaseGate.resolve(); await disposing;
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
