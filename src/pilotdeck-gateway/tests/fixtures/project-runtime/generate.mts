// Execute the complete, unmodified lifecycle methods in memory. Only external
// resource dependencies and unrelated constructor wiring are replaced.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE');
assert.ok(process.argv[2], 'provide a fresh output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const sourceCommit = assertBaseline(baseline);
const source = 'src/cli/ProjectRuntimeRegistry.ts';
const sourceSha256 = await baselineSourceHashes(baseline, [source]);
const requireBaseline = createRequire(join(baseline, 'package.json'));
const { transformSync } = createRequire(requireBaseline.resolve('tsx'))('esbuild');
const compiled = transformSync(await readFile(join(baseline, source), 'utf8'), {
  loader: 'ts', format: 'cjs', target: 'es2022', sourcefile: source,
}).code;
const scratch = await mkdtemp(join(tmpdir(), 'pdrs-project-runtime-'));
// Retain this per-run scratch directory; never remove a fixed path.
const output = resolve(process.argv[2]);
const events: string[] = [];
let nextId = 0;
let failAt = 0;
let failCleanup: number[] = [];
class FakeBundle {
  id = ++nextId;
  constructor(_options: unknown) {}
  stage() {
    events.push(`stage:${this.id}`);
    if (this.id === failAt) throw new Error(`stage failed:${this.id}`);
    return { fixtureId: this.id };
  }
  async dispose() {
    events.push(`dispose:${this.id}`);
    if (failCleanup.includes(this.id)) throw new Error(`dispose failed:${this.id}`);
  }
}
class FakeBackground {
  id = nextId;
  shutdown() { events.push(`shutdown:${this.id}`); }
}
const module = { exports: {} as any };
new Function('require', 'module', 'exports', compiled)((name: string) => {
  if (name.startsWith('node:')) return requireBaseline(name);
  if (name === './ProjectRuntimeResourcesBundle.js') return { ProjectRuntimeResourcesBundle: FakeBundle };
  if (name === '../agent/sub/BackgroundSubagentRuntime.js') return { BackgroundSubagentRuntime: FakeBackground };
  // Imports for session/config/storage methods never exercised in this oracle.
  // Access would fail, rather than silently running a fake lifecycle method.
  return {};
}, module, module.exports);
const Registry = module.exports.ProjectRuntimeRegistry;
assert.equal(typeof Registry.prototype.buildRuntime, 'function');

const cases: any[] = [
  { name: 'resolve-cache-and-exact-invalidate', steps: [
    { op: 'resolve', root: 'a', as: 'a1' }, { op: 'resolve', root: 'a/../a', as: 'a2' },
    { op: 'invalidate', root: 'relative-a' }, { op: 'invalidate', root: 'a/.' },
    { op: 'invalidate', root: 'a/' }, { op: 'resolve', as: 'fallback' },
    { op: 'invalidate', root: 'a' }, { op: 'resolve', root: 'a', as: 'a3' }, { op: 'dispose' },
    { op: 'resolve', root: 'a' }, { op: 'reload' }, { op: 'invalidate' }, { op: 'dispose' },
  ] },
  { name: 'lease-retire-release', steps: [
    { op: 'resolve', root: 'a', as: 'old' }, { op: 'acquire', runtime: 'old', as: 'l1' },
    { op: 'acquire', runtime: 'old', as: 'l2' }, { op: 'invalidate', root: 'a' },
    { op: 'acquire', runtime: 'old', as: 'bad' }, { op: 'resolve', root: 'a', as: 'new' },
    { op: 'release', lease: 'l1' }, { op: 'release', lease: 'l1' },
    { op: 'release', lease: 'l2' }, { op: 'dispose' },
  ] },
  { name: 'reload-with-live-lease', steps: [
    { op: 'resolve', root: 'a', as: 'old' }, { op: 'acquire', runtime: 'old', as: 'l1' },
    { op: 'resolve', root: 'b', as: 'b' }, { op: 'reload' },
    { op: 'resolve', root: 'a', as: 'new' }, { op: 'release', lease: 'l1' }, { op: 'dispose' },
  ] },
  { name: 'reload-rollback-and-retry', steps: [
    { op: 'resolve', root: 'a', as: 'a' }, { op: 'resolve', root: 'b', as: 'b' },
    { op: 'reload', failAt: 4 }, { op: 'resolve', root: 'a', as: 'same' },
    { op: 'reload' }, { op: 'resolve', root: 'a', as: 'new' }, { op: 'dispose' },
  ] },
  { name: 'reload-aggregate-cleanup-errors', steps: [
    { op: 'resolve', root: 'a', as: 'a' }, { op: 'resolve', root: 'b', as: 'b' },
    { op: 'reload', failAt: 4, failCleanup: [3, 4] }, { op: 'dispose' },
  ] },
  { name: 'initial-stage-failure-retry', steps: [
    { op: 'resolve', root: 'a', failAt: 1 }, { op: 'resolve', root: 'a', as: 'a' }, { op: 'dispose' },
  ] },
  { name: 'retired-cleanup-error-is-not-replayed', steps: [
    { op: 'resolve', root: 'a', as: 'a' }, { op: 'acquire', runtime: 'a', as: 'l1' },
    { op: 'invalidate', root: 'a' }, { op: 'release', lease: 'l1', failCleanup: [1] },
    { op: 'dispose' }, { op: 'dispose' },
  ] },
  { name: 'dispose-failure-is-memoized', steps: [
    { op: 'resolve', root: 'a', as: 'a' }, { op: 'resolve', root: 'b', as: 'b' },
    { op: 'dispose', failCleanup: [1, 2] }, { op: 'dispose' },
  ] },
  { name: 'empty-reload-and-invalidate-all', steps: [
    { op: 'reload' }, { op: 'resolve', root: 'a', as: 'a' }, { op: 'resolve', root: 'b', as: 'b' },
    { op: 'invalidate', root: '' }, { op: 'dispose' },
  ] },
];
function failure(error: any): any {
  return { message: error.message, causes: error instanceof AggregateError ? error.errors.map(failure) : [] };
}
let count = 0;
for (const item of cases) {
  events.length = 0; nextId = 0;
  // Constructor wiring is outside this slice; buildRuntime itself stays real.
  const registry = Object.create(Registry.prototype);
  Object.assign(registry, {
    runtimes: new Map(), reloadTail: Promise.resolve(), runtimeDisposals: new Set(), disposed: false,
    options: { fallbackProjectRoot: join(scratch, 'fallback'), pilotHome: join(scratch, 'home'), env: {}, builtinPlugins: [] },
    _extraTools: [], routerEvents: { create() { throw new Error('unexpected router use'); } },
    permissionModes: { dispose() {} }, ownsSharedSessionState: false,
  });
  const runtimes = new Map<string, any>();
  const leases = new Map<string, any>();
  const recorded: any[] = [];
  for (const step of item.steps) {
    failAt = step.failAt ?? 0;
    failCleanup = step.failCleanup ?? [];
    let error = null;
    let id = null;
    const root = step.root === undefined ? undefined
      : step.root === '' || step.root === 'relative-a' ? step.root : `${scratch}/${step.root}`;
    try {
      switch (step.op) {
        case 'resolve': {
          const runtime = registry.resolve(root);
          if (step.as) runtimes.set(step.as, runtime);
          id = runtime.fixtureId; break;
        }
        case 'acquire': leases.set(step.as, registry.acquireRuntimeLease(runtimes.get(step.runtime))); break;
        case 'release': await leases.get(step.lease)(); break;
        case 'invalidate': registry.invalidate(root); break;
        case 'reload': await registry.reload(); break;
        case 'dispose': await registry.disposeProjectRuntimes(); break;
        default: throw new Error(`unknown operation ${step.op}`);
      }
    } catch (caught) { error = failure(caught); }
    // Finish fire-and-forget retire/initial-failure cleanup before observation.
    await new Promise(resolve => setImmediate(resolve));
    recorded.push({ input: step, id, error, events: [...events], runtimes: Object.fromEntries(
      [...runtimes].map(([label, runtime]) => [label, { id: runtime.fixtureId, state: runtime.runtimeState, leases: runtime.sessionLeases }]),
    ) });
    count++;
  }
  item.steps = recorded;
}
assert.equal(cases.length, 9);
assert.equal(count, 59);
const text = JSON.stringify({ sourceCommit, sourceSha256, cases }, null, 2) + '\n';
assert.ok(!text.includes(scratch));
await writeFile(join(output, 'oracle.json'), text, { flag: 'wx' });
console.log(`PROJECT_RUNTIME_ORACLE_PASS cases=${cases.length} steps=${count}`);
