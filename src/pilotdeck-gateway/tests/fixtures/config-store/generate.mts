// Records the fixed TypeScript baseline's PilotConfigStore reload/publish
// behaviour. Run from repo root with Node 22 and the baseline's tsx loader; see
// README.md. Every scenario gets its own directory under one mkdtemp root, and
// only that root is removed.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { setTimeout as sleep } from 'node:timers/promises';
import { bindSourceTree, sourceClosureSha256 } from '../../baseline_source_guard.mjs';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture output directory');
const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const baseline = resolve(process.env.PDRS_BASELINE);
const outputDir = resolve(process.argv[2]);

// Shared provenance guard (#280): explicit absolute Git, pinned HEAD, every
// tracked src byte equal to its Git blob, and a clean tracked tree.
const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const gitEnv = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('GIT_')));
const git = (...args: string[]) =>
  execFileSync(gitExecutable, ['--no-replace-objects', '-C', baseline, ...args], { env: gitEnv, maxBuffer: 32 * 1024 * 1024 });
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const trackedChanges = git('status', '--porcelain', '--untracked-files=no').toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);
// stableStringify (contentHash) orders keys with localeCompare; Rust pins en-US.
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

// sourceSha256 covers the relative-import closure of the store entry point.
const ENTRY = 'src/pilot/config/PilotConfigStore.ts';
const sourceSha256: Record<string, string> = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [ENTRY]);
const { createPilotConfigStoreSync } = await import(pathToFileURL(join(baseline, ENTRY)).href);

const BASE = `schemaVersion: 1
agent: {model: parity/deterministic}
model:
  providers:
    parity:
      protocol: openai
      url: http://127.0.0.1:28794/v1
      apiKey: mock-only
      models:
        deterministic: {}
        other: {}
`;
const OTHER_MODEL = BASE.replace('agent: {model: parity/deterministic}', 'agent: {model: parity/other}');
const RUN_POLICY = `${BASE}runPolicy: {failureGuard: {enabled: true, modelFailureLimit: 3}}\n`;
const RESTART = `${BASE}gateway: {port: 4321}\n`;
const WARN = BASE.replace('schemaVersion: 1\n', '') + 'mystery: 1\n';
const BAD_SCHEMA = BASE.replace('schemaVersion: 1', 'schemaVersion: 2');
const BAD_YAML = `${BASE}agent: [\n`;
const NO_AGENT = BASE.replace('agent: {model: parity/deterministic}\n', '');
// Not a PilotConfigError: the merge recursion throws a RangeError.
const CYCLE = BASE.replace('      apiKey: mock-only', '      apiKey: mock-only\n      extraBody: &a {self: *a}');
const PROJECT = `agent: {model: parity/other}\n`;

type Step = Record<string, unknown> & { op: string };
const scenarios: { id: string; project?: boolean; steps: Step[] }[] = [];
const add = (id: string, steps: Step[], extra: { project?: boolean } = {}) => scenarios.push({ id, ...extra, steps });

add('initial-and-identity-reload', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'a' },
  { op: 'reload' },
  { op: 'reload' },
  { op: 'diagnostics' },
]);
add('initial-warnings-in-diagnostics', [
  { op: 'write', file: 'global', yaml: WARN },
  { op: 'create' },
  { op: 'diagnostics' },
  { op: 'subscribe', name: 'a' },
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'reload' },
  { op: 'diagnostics' },
]);
add('initial-load-failure', [
  { op: 'write', file: 'global', yaml: BAD_SCHEMA },
  { op: 'create' },
]);
add('change-classes', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'a' },
  { op: 'write', file: 'global', yaml: OTHER_MODEL },
  { op: 'reload' },
  { op: 'write', file: 'global', yaml: RUN_POLICY },
  { op: 'reload' },
  { op: 'write', file: 'global', yaml: RESTART },
  { op: 'reload' },
]);
add('concurrent-reloads-coalesce', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'a' },
  { op: 'subscribe', name: 'b' },
  { op: 'write', file: 'global', yaml: OTHER_MODEL },
  { op: 'reload', count: 3 },
  { op: 'reload', count: 2 },
  { op: 'snapshot' },
]);
add('failure-keeps-diagnostics-and-version', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'a' },
  { op: 'write', file: 'global', yaml: BAD_SCHEMA },
  { op: 'reload', count: 2 },
  { op: 'snapshot' },
  { op: 'diagnostics' },
  { op: 'write', file: 'global', yaml: NO_AGENT },
  { op: 'reload' },
  { op: 'diagnostics' },
  { op: 'write', file: 'global', yaml: BAD_YAML },
  { op: 'reload' },
  { op: 'diagnostics' },
  // A non-PilotConfigError keeps the previous failure's diagnostics.
  { op: 'write', file: 'global', yaml: CYCLE },
  { op: 'reload' },
  { op: 'diagnostics' },
  { op: 'write', file: 'global', yaml: OTHER_MODEL },
  { op: 'reload' },
  { op: 'diagnostics' },
  { op: 'snapshot' },
]);
add('missing-file-failure', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'remove', file: 'global' },
  { op: 'reload' },
  { op: 'diagnostics' },
]);
add('subscriber-isolation', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'first' },
  { op: 'subscribe', name: 'thrower', behavior: 'throw' },
  { op: 'subscribe', name: 'last' },
  { op: 'write', file: 'global', yaml: OTHER_MODEL },
  { op: 'reload' },
  { op: 'snapshot' },
]);
add('subscriber-reentrant-reload', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'reentrant', behavior: 'reentrant' },
  { op: 'subscribe', name: 'after' },
  { op: 'write', file: 'global', yaml: OTHER_MODEL },
  { op: 'reload' },
  { op: 'reload' },
]);
add('subscribe-unsubscribe-during-publish', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'adder', behavior: 'subscribe:late' },
  { op: 'subscribe', name: 'remover', behavior: 'unsubscribe:victim' },
  { op: 'subscribe', name: 'victim' },
  { op: 'reload' },
  { op: 'reload' },
]);
add('unsubscribe-and-duplicate', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'a' },
  { op: 'subscribe', name: 'a' },
  { op: 'subscribe', name: 'b' },
  { op: 'reload' },
  { op: 'unsubscribe', name: 'a' },
  { op: 'reload' },
  { op: 'unsubscribe', name: 'a' },
  { op: 'unsubscribe', name: 'b' },
  { op: 'reload' },
]);
add('project-config', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'a' },
  { op: 'write', file: 'project', yaml: PROJECT },
  { op: 'reload' },
  { op: 'remove', file: 'project' },
  { op: 'reload' },
], { project: true });
// fs.watch and setTimeout are real here; the writes are 30 ms apart and the
// waits are several debounce windows long, so each burst yields one reload.
add('watch-debounce', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'create' },
  { op: 'subscribe', name: 'a' },
  { op: 'wait', ms: 300 },
  { op: 'watch' },
  { op: 'write', file: 'global', yaml: OTHER_MODEL },
  { op: 'wait', ms: 30 },
  { op: 'write', file: 'global', yaml: RUN_POLICY },
  { op: 'wait', ms: 30 },
  { op: 'write', file: 'global', yaml: RESTART },
  { op: 'wait', ms: 1500 },
  { op: 'snapshot' },
  { op: 'write', file: 'global', yaml: BAD_SCHEMA },
  { op: 'wait', ms: 1500 },
  { op: 'snapshot' },
  { op: 'diagnostics' },
  { op: 'unwatch' },
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'wait', ms: 1000 },
  { op: 'snapshot' },
]);
// A missing file is watched through its directory; a missing directory makes
// fs.watch throw, which the store swallows, so that path is not watched.
add('watch-missing-project-file-watches-directory', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'mkdir', file: 'projectConfigDir' },
  { op: 'create' },
  { op: 'subscribe', name: 'a' },
  { op: 'wait', ms: 300 },
  { op: 'watch' },
  { op: 'write', file: 'project', yaml: PROJECT },
  { op: 'wait', ms: 1500 },
  { op: 'snapshot' },
  { op: 'unwatch' },
], { project: true });
add('watch-missing-project-directory-not-watched', [
  { op: 'write', file: 'global', yaml: BASE },
  { op: 'mkdir', file: 'projectRoot' },
  { op: 'create' },
  { op: 'subscribe', name: 'a' },
  { op: 'wait', ms: 300 },
  { op: 'watch' },
  { op: 'write', file: 'project', yaml: PROJECT },
  { op: 'wait', ms: 1500 },
  { op: 'snapshot' },
  { op: 'unwatch' },
], { project: true });

function normalize(value: unknown, root: string): unknown {
  if (typeof value === 'string') return value.split(root).join('<ROOT>');
  if (Array.isArray(value)) return value.map((v) => normalize(v, root));
  if (value !== null && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(value)) {
      const v = (value as Record<string, unknown>)[key];
      // The fixture is JSON: an undefined member is recorded as an explicit tag.
      out[normalize(key, root) as string] = v === undefined ? { $: 'undefined' } : normalize(v, root);
    }
    return out;
  }
  return value;
}
const errorRecord = (error: any, root: string) => normalize({
  name: error?.name,
  code: error?.code,
  message: error?.message,
  ...(Array.isArray(error?.diagnostics) ? { diagnostics: error.diagnostics } : {}),
}, root);

function assertInside(root: string, path: string): string {
  const rel = relative(root, path);
  assert.ok(rel !== '' && rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel), `refusing to touch ${path} outside ${root}`);
  return path;
}

const scratch = await mkdtemp(join(tmpdir(), 'pdrs-config-store-'));
const recorded: unknown[] = [];
try {
  for (const scenario of scenarios) {
    const root = join(scratch, scenario.id);
    const home = join(root, 'home');
    const projectRoot = join(root, 'project');
    await mkdir(home, { recursive: true });
    const files: Record<string, string> = {
      global: join(home, 'pilotdeck.yaml'),
      project: join(projectRoot, '.pilotdeck', 'pilotdeck.yaml'),
      projectConfigDir: join(projectRoot, '.pilotdeck'),
      projectRoot,
    };
    const options = {
      env: { HOME: home, PILOT_HOME: home },
      ...(scenario.project ? { projectRoot } : {}),
    };
    let store: any;
    let stopWatching: (() => void) | undefined;
    const listeners = new Map<string, (event: any) => void>();
    const unsubscribers = new Map<string, () => void>();
    const events: unknown[] = [];
    const pending: Promise<unknown>[] = [];
    const makeListener = (name: string, behavior?: string) => (event: any) => {
      events.push({
        listener: name,
        previousVersion: event.previousSnapshot.version,
        nextVersion: event.nextSnapshot.version,
        changedPaths: event.changedPaths,
        changeClasses: event.changeClasses,
        storeVersion: store.getSnapshot().version,
        storeIsNext: store.getSnapshot() === event.nextSnapshot,
        diagnosticsCount: store.getDiagnostics().length,
      });
      if (behavior === 'throw') throw new Error(`listener ${name} failed`);
      if (behavior === 'reentrant') {
        const next = event.nextSnapshot;
        pending.push(store.reload('reentrant').then(
          (snapshot: any) => events.push({ listener: name, reentrantResult: snapshot === next ? 'same-snapshot' : `version ${snapshot.version}` }),
          (error: any) => events.push({ listener: name, reentrantResult: `error ${error?.name}` }),
        ));
      }
      if (behavior?.startsWith('subscribe:')) {
        const late = behavior.slice('subscribe:'.length);
        if (!listeners.has(late)) {
          const listener = makeListener(late);
          listeners.set(late, listener);
          unsubscribers.set(late, store.subscribe(listener));
        }
      }
      if (behavior?.startsWith('unsubscribe:')) unsubscribers.get(behavior.slice('unsubscribe:'.length))?.();
    };
    const results: unknown[] = [];
    for (const step of scenario.steps) {
      const out: Record<string, unknown> = {};
      const file = step.file ? assertInside(root, files[step.file as string]) : undefined;
      switch (step.op) {
        case 'write':
          await mkdir(dirname(file!), { recursive: true });
          await writeFile(file!, step.yaml as string);
          break;
        case 'remove':
          await rm(file!, { force: true });
          break;
        case 'mkdir':
          await mkdir(file!, { recursive: true });
          break;
        case 'create':
          try {
            store = createPilotConfigStoreSync(options);
            out.version = store.getSnapshot().version;
            out.diagnostics = normalize(store.getDiagnostics(), root);
          } catch (error) {
            out.error = errorRecord(error, root);
          }
          break;
        case 'subscribe': {
          const name = step.name as string;
          const listener = listeners.get(name) ?? makeListener(name, step.behavior as string | undefined);
          listeners.set(name, listener);
          unsubscribers.set(name, store.subscribe(listener));
          break;
        }
        case 'unsubscribe':
          unsubscribers.get(step.name as string)!();
          break;
        case 'reload': {
          const count = (step.count as number | undefined) ?? 1;
          const calls = Array.from({ length: count }, () => store.reload('manual'));
          const settled = await Promise.allSettled(calls);
          await Promise.allSettled(pending.splice(0));
          out.results = settled.map((r) => r.status === 'fulfilled'
            ? { version: (r.value as any).version }
            : { error: errorRecord(r.reason, root) });
          const values = settled.map((r) => (r.status === 'fulfilled' ? r.value : r.reason));
          out.sameResult = values.every((v) => v === values[0]);
          break;
        }
        case 'snapshot':
          out.version = store.getSnapshot().version;
          break;
        case 'diagnostics':
          out.diagnostics = normalize(store.getDiagnostics(), root);
          break;
        case 'watch':
          stopWatching = store.startWatching();
          break;
        case 'unwatch':
          stopWatching!();
          stopWatching = undefined;
          break;
        case 'wait':
          await sleep(step.ms as number);
          await Promise.allSettled(pending.splice(0));
          break;
        default:
          throw new Error(`unknown op ${step.op}`);
      }
      out.events = events.splice(0);
      results.push(out);
    }
    stopWatching?.();
    recorded.push({ id: scenario.id, ...(scenario.project ? { project: true } : {}), steps: scenario.steps, results });
  }
} finally {
  await rm(scratch, { recursive: true, force: true });
}
const oracle = {
  sourceCommit,
  node: process.version,
  sourceSha256,
  count: recorded.length,
  scenarios: recorded,
};
await writeFile(join(outputDir, 'oracle.json'), JSON.stringify(oracle, null, 2) + '\n');
console.log(JSON.stringify({ scenarios: recorded.length, sources: Object.keys(sourceSha256).length, output: join(outputDir, 'oracle.json') }));
