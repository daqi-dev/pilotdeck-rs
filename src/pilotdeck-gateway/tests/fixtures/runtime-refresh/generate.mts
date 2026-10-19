// Records the fixed TypeScript baseline's GatewayRuntimeRefreshBundle call
// sequences. Run from repo root with Node 22 and the baseline's tsx loader; see
// README.md. The bundle is the real class; its collaborators are recording
// fakes, except the `real-store-*` scenarios, which drive it from a real
// createPilotConfigStoreSync store over YAML files. Every scenario with files
// gets its own directory under one mkdtemp root, and only that root is removed.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { realpathSync } from 'node:fs';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
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
const sourceCommit = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable).ref;
const trackedChanges = execFileSync(gitExecutable,
  ['--no-replace-objects', '-C', baseline, 'status', '--porcelain', '--untracked-files=no'], { env: gitEnv }).toString();
assert.equal(trackedChanges, '', `baseline has uncommitted changes to tracked files:\n${trackedChanges}`);
// The real store's contentHash orders keys with localeCompare; Rust pins en-US.
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

const BUNDLE = 'src/cli/GatewayRuntimeRefreshBundle.ts';
const STORE = 'src/pilot/config/PilotConfigStore.ts';
const sourceSha256: Record<string, string> = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [BUNDLE, STORE]);
const { GatewayRuntimeRefreshBundle } = await import(pathToFileURL(join(baseline, BUNDLE)).href);
const { createPilotConfigStoreSync } = await import(pathToFileURL(join(baseline, STORE)).href);

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
const RUN_POLICY = `${OTHER_MODEL}runPolicy: {failureGuard: {enabled: true, modelFailureLimit: 3}}\n`;
const GATEWAY = (port: number) => `${RUN_POLICY}gateway: {port: ${port}}\n`;
const GATEWAY_AND_MODEL = GATEWAY(4323).replace('parity/other', 'parity/deterministic');
const BAD_SCHEMA = BASE.replace('schemaVersion: 1', 'schemaVersion: 2');

type Step = Record<string, unknown> & { op: string };
type Options = { sdk?: boolean; router?: boolean; diagnostics?: boolean; realStore?: boolean };
const scenarios: { id: string; options: Options; steps: Step[] }[] = [];
const add = (id: string, options: Options, steps: Step[]) => scenarios.push({ id, options, steps });
const ev = (changedPaths: string[], changeClasses: string[]) => ({ changedPaths, changeClasses });
const ALL = { sdk: true, router: true, diagnostics: true };
const OTHER = ev(['agent.model'], ['next-runtime']);
const OTHER_B = ev(['model.providers.parity.url', 'agent.model'], ['next-runtime', 'next-runtime']);

add('attach-is-idempotent-and-dispose-unsubscribes', ALL, [
  { op: 'attach' },
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: ev(['runPolicy'], ['next-request']) },
  { op: 'dispose' },
  { op: 'dispose' },
  { op: 'emit', event: ev(['runPolicy'], ['next-request']) },
  { op: 'attach' },
  { op: 'emit', event: ev(['runPolicy'], ['next-request']) },
  { op: 'bind' },
  { op: 'emit', event: ev(['runPolicy'], ['next-request']) },
]);
add('empty-change-classes-are-ignored', ALL, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: ev([], []) },
  { op: 'emit', event: ev(['agent.model'], []) },
  { op: 'flush' },
]);
add('restart-required-only-warns', ALL, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: ev(['gateway.port'], ['restart-required']) },
  { op: 'emit', event: ev(['gateway.port', 'gateway.host'], ['restart-required', 'restart-required']) },
  // Not every class is restart-required: the runtime refresh path runs.
  { op: 'emit', event: ev(['gateway.port', 'agent.model'], ['restart-required', 'next-runtime']) },
  { op: 'resolve', index: 0 },
  { op: 'flush' },
]);
add('run-policy-only-skips-runtime-reload', ALL, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: ev(['runPolicy.failureGuard.enabled'], ['next-request']) },
  { op: 'emit', event: ev(['runPolicy', 'runPolicy.failureGuard.modelFailureLimit'], ['next-request', 'next-request']) },
  // Prefix only, not a runPolicy path; no changed paths is not run-policy-only.
  { op: 'emit', event: ev(['runPolicyX'], ['next-runtime']) },
  { op: 'emit', event: ev([], ['next-runtime']) },
  { op: 'emit', event: ev(['runPolicy', 'agent.model'], ['next-request', 'next-runtime']) },
  { op: 'resolve', index: 0 },
  { op: 'resolve', index: 1 },
  { op: 'resolve', index: 2 },
  { op: 'flush' },
]);
add('run-policy-only-without-sdk-or-server', { router: true }, [
  { op: 'attach' },
  { op: 'emit', event: ev(['runPolicy'], ['next-request']) },
  { op: 'flush' },
]);
add('runtime-refresh-success-order', ALL, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: OTHER },
  { op: 'flush' },
  { op: 'resolve', index: 0 },
  { op: 'flush' },
]);
add('runtime-refresh-failure-keeps-runtimes', ALL, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: OTHER },
  { op: 'reject', index: 0, message: 'stage failed' },
  { op: 'flush' },
]);
add('concurrent-changes-settle-in-resolution-order', ALL, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: OTHER },
  { op: 'emit', event: OTHER_B },
  { op: 'resolve', index: 1 },
  { op: 'flush' },
  { op: 'reject', index: 0, message: 'first reload failed' },
  { op: 'flush' },
]);
add('concurrent-changes-failure-then-success', ALL, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: OTHER },
  { op: 'emit', event: OTHER_B },
  { op: 'reject', index: 0, message: 'first reload failed' },
  { op: 'resolve', index: 1 },
  { op: 'flush' },
]);
add('no-router-no-diagnostics', { sdk: true }, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: OTHER },
  { op: 'resolve', index: 0 },
  { op: 'flush' },
]);
add('diagnostics-without-router', { sdk: true, diagnostics: true }, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: OTHER },
  { op: 'resolve', index: 0 },
  { op: 'flush' },
]);
add('dispose-before-reload-settles-skips-broadcast', ALL, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'emit', event: OTHER },
  { op: 'dispose' },
  { op: 'resolve', index: 0 },
  { op: 'flush' },
]);
add('registry-sync-throw-is-swallowed-by-store', ALL, [
  { op: 'attach' },
  { op: 'bind' },
  { op: 'registryThrows', message: 'Project runtime registry is disposed.' },
  { op: 'emit', event: OTHER },
  { op: 'flush' },
  { op: 'emit', event: ev(['runPolicy'], ['next-request']) },
]);
add('bind-server-later', ALL, [
  { op: 'attach' },
  { op: 'emit', event: ev(['runPolicy'], ['next-request']) },
  { op: 'emit', event: OTHER },
  { op: 'bind' },
  { op: 'resolve', index: 0 },
  { op: 'flush' },
]);
add('reload-config-captures-last-event', ALL, [
  { op: 'reloadConfig', emits: [] },
  { op: 'reloadConfig', emits: [OTHER] },
  { op: 'attach' },
  { op: 'bind' },
  { op: 'reloadConfig', emits: [ev(['runPolicy'], ['next-request']), OTHER_B] },
  { op: 'resolve', index: 0 },
  { op: 'flush' },
  { op: 'reloadConfig', emits: [OTHER], fail: 'reload failed' },
  { op: 'reloadConfig', emits: [] },
]);
add('refresh-config-before-turn', ALL, [
  { op: 'refreshBeforeTurn', emits: [] },
  { op: 'refreshBeforeTurn', emits: [], fail: 'turn reload failed' },
]);
add('reload-extensions', ALL, [
  { op: 'reloadExtensions' },
  { op: 'bind' },
  { op: 'reloadExtensions', input: {} },
  { op: 'reloadExtensions', input: { projectKey: '/work/a', changedPaths: ['skills/x.md', 'agents/y.md'] } },
  { op: 'reloadExtensions', input: { projectKey: '', changedPaths: ['a'] } },
  { op: 'reloadExtensions', input: { projectKey: '/work/\ud800lone', changedPaths: [] } },
  { op: 'reloadExtensions', input: { changedPaths: ['\udfff'] } },
]);
add('reload-extensions-without-router', { sdk: true }, [
  { op: 'reloadExtensions', input: { projectKey: '/work/a' } },
  { op: 'reloadExtensions', input: { changedPaths: ['x'] } },
]);
add('after-turn-without-diagnostics', { router: true }, [
  { op: 'afterTurn', input: { sessionKey: 's1', projectKey: '/work/a', runId: 'r1' } },
  { op: 'afterTurn', input: { sessionKey: 's2' } },
  { op: 'afterTurn', input: { sessionKey: 's3', projectKey: '' } },
]);
add('after-turn-with-diagnostics', ALL, [
  { op: 'afterTurn', input: { sessionKey: 's1', projectKey: '/work/a', runId: 'r1' } },
  { op: 'afterTurn', input: { sessionKey: 'missing' } },
  { op: 'afterTurn', input: { sessionKey: 's3', projectKey: '\ud800' } },
]);
add('after-turn-diagnostics-without-router', { diagnostics: true }, [
  { op: 'afterTurn', input: { sessionKey: 's1', runId: 'r1' } },
]);
add('real-store-change-classes', { ...ALL, realStore: true }, [
  { op: 'write', yaml: BASE },
  { op: 'createStore' },
  { op: 'attach' },
  { op: 'bind' },
  { op: 'reloadConfig' },
  { op: 'write', yaml: OTHER_MODEL },
  { op: 'reloadConfig' },
  { op: 'resolve', index: 0 },
  { op: 'flush' },
  { op: 'write', yaml: RUN_POLICY },
  { op: 'reloadConfig' },
  { op: 'write', yaml: GATEWAY(4321) },
  { op: 'reloadConfig' },
  { op: 'write', yaml: `${GATEWAY(4321)}modules: {a: 1}\n` },
  { op: 'reloadConfig' },
  { op: 'write', yaml: `${GATEWAY(4321)}modules: {a: 2, b: 3}\n` },
  { op: 'reloadConfig' },
  { op: 'write', yaml: GATEWAY_AND_MODEL },
  { op: 'refreshBeforeTurn' },
  { op: 'reject', index: 1, message: 'stage failed' },
  { op: 'flush' },
  { op: 'write', yaml: BAD_SCHEMA },
  { op: 'reloadConfig' },
  { op: 'refreshBeforeTurn' },
  { op: 'write', yaml: BASE },
  { op: 'reloadConfig' },
  { op: 'resolve', index: 3 },
  { op: 'resolve', index: 2 },
  { op: 'flush' },
]);
add('real-store-dispose', { ...ALL, realStore: true }, [
  { op: 'write', yaml: BASE },
  { op: 'createStore' },
  { op: 'attach' },
  { op: 'bind' },
  { op: 'dispose' },
  { op: 'write', yaml: OTHER_MODEL },
  { op: 'reloadConfig' },
  { op: 'flush' },
]);

// JSON fixture: undefined is recorded as an explicit tag; the scratch root is
// replaced so paths are machine-independent.
function normalize(value: unknown, root: string): unknown {
  if (value === undefined) return { $: 'undefined' };
  if (typeof value === 'string') return root ? value.split(root).join('<ROOT>') : value;
  if (Array.isArray(value)) return value.map((v) => normalize(v, root));
  if (value !== null && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const key of Object.keys(value)) out[normalize(key, root) as string] = normalize((value as any)[key], root);
    return out;
  }
  return value;
}
const errorRecord = (error: any) => ({
  name: error?.name,
  message: error?.message,
  ...(error?.code !== undefined ? { code: error.code } : {}),
});
function assertInside(root: string, path: string): string {
  const rel = relative(root, path);
  assert.ok(rel !== '' && rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel), `refusing to touch ${path} outside ${root}`);
  return path;
}
const flush = async () => {
  for (let i = 0; i < 5; i++) await new Promise((r) => setImmediate(r));
};

const scratch = await mkdtemp(join(tmpdir(), 'pdrs-runtime-refresh-'));
const recorded: unknown[] = [];
try {
  for (const scenario of scenarios) {
    const root = join(scratch, scenario.id);
    const home = join(root, 'home');
    await mkdir(home, { recursive: true });
    const configFile = assertInside(root, join(home, 'pilotdeck.yaml'));
    const calls: unknown[] = [];
    const call = (name: string, args: Record<string, unknown> = {}) => calls.push(normalize({ call: name, ...args }, root));

    // Config store: scripted fake, or the real store behind a recording shim.
    const listeners = new Map<number, (event: any) => void>();
    let nextListener = 0;
    let script: { emits: unknown[]; fail?: string } = { emits: [] };
    let realStore: any;
    const configStore = {
      subscribe(listener: (event: any) => void) {
        const id = ++nextListener;
        call('store.subscribe', { id });
        const unsubscribeReal = realStore?.subscribe(listener);
        if (!realStore) listeners.set(id, listener);
        return () => {
          call('store.unsubscribe', { id });
          unsubscribeReal?.();
          listeners.delete(id);
        };
      },
      reload(reason: string) {
        call('store.reload', { reason });
        if (realStore) return realStore.reload(reason);
        for (const event of script.emits) emit(event);
        return script.fail ? Promise.reject(new Error(script.fail)) : Promise.resolve({ version: 0 });
      },
    };
    const emit = (event: any) => {
      // Same listener isolation as DefaultPilotConfigStore.publish.
      for (const listener of [...listeners.values()]) {
        try {
          listener(event);
        } catch {
          // swallowed
        }
      }
    };
    const pending: { resolve: () => void; reject: (e: unknown) => void }[] = [];
    let registryThrows: string | undefined;
    const registry = {
      reload() {
        call('registry.reload');
        if (registryThrows) throw new Error(registryThrows);
        return new Promise<void>((resolve, reject) => pending.push({ resolve, reject }));
      },
      invalidate(...args: unknown[]) {
        call('registry.invalidate', { args });
      },
    };
    const router = {
      markAllDirty(reason: string) {
        call('router.markAllDirty', { reason });
        return 2;
      },
      markProjectDirty(projectKey: string, reason: string) {
        call('router.markProjectDirty', { projectKey, reason });
        return 1;
      },
      cachedSessionCount() {
        call('router.cachedSessionCount');
        return 3;
      },
      snapshotSession(sessionKey: string) {
        call('router.snapshotSession', { sessionKey });
        return sessionKey === 'missing' ? undefined : { messages: [{ role: 'user', key: sessionKey }] };
      },
    };
    const server = {
      broadcastNotification(name: string, payload?: unknown) {
        call('server.broadcast', { name, payload });
      },
    };
    const bundle = new GatewayRuntimeRefreshBundle({
      configStore,
      registry,
      memoryMaintenance: {
        schedule: (...args: unknown[]) => call('memory.schedule', { args }),
      },
      getRouter: () => {
        call('getRouter');
        return scenario.options.router ? router : undefined;
      },
      projectRoot: '/work/root',
      memoryDiagnosticsEnabled: !!scenario.options.diagnostics,
      logMemoryDiagnostic: (input: Record<string, unknown>) => call('memory.diagnostic', { input }),
      // Integer-like keys enumerate first; a later `sessionKey` overrides the
      // literal's own; undefined members survive the spread.
      summarizeMessages: (messages: unknown) => {
        call('memory.summarize', { messages });
        return { messageCount: (messages as unknown[]).length, 7: 'seven', sessionKey: 'from-summary', absent: undefined };
      },
      ...(scenario.options.sdk ? { dispatchSdkConfigChange: (payload: unknown) => call('sdk.configChange', { payload }) } : {}),
      warn: (message: string, error?: unknown) =>
        call('warn', { message, detail: error instanceof Error ? errorRecord(error) : error }),
      log: (message: string) => call('log', { message }),
    });

    const results: unknown[] = [];
    for (const step of scenario.steps) {
      const out: Record<string, unknown> = {};
      script = { emits: (step.emits as unknown[] | undefined) ?? [], fail: step.fail as string | undefined };
      switch (step.op) {
        case 'attach': bundle.attach(); break;
        case 'dispose': bundle.dispose(); break;
        case 'bind': bundle.bindServer(server); break;
        case 'emit': emit(step.event); break;
        case 'flush': await flush(); break;
        case 'resolve': pending[step.index as number].resolve(); break;
        case 'reject': pending[step.index as number].reject(new Error(step.message as string)); break;
        case 'registryThrows': registryThrows = step.message as string; break;
        case 'write':
          await mkdir(dirname(configFile), { recursive: true });
          await writeFile(configFile, step.yaml as string);
          break;
        case 'createStore':
          realStore = createPilotConfigStoreSync({ env: { HOME: home, PILOT_HOME: home } });
          out.version = realStore.getSnapshot().version;
          break;
        case 'reloadConfig':
        case 'refreshBeforeTurn': {
          const promise = step.op === 'reloadConfig' ? bundle.reloadConfig() : bundle.refreshConfigBeforeTurn();
          try {
            out.result = normalize(await promise, root);
          } catch (error: any) {
            out.error = normalize(errorRecord(error), root);
          }
          break;
        }
        case 'reloadExtensions':
          out.result = normalize(await ('input' in step ? bundle.reloadExtensions(step.input) : bundle.reloadExtensions()), root);
          break;
        case 'afterTurn':
          out.result = normalize(await bundle.afterTurnCompleted(step.input), root);
          break;
        default:
          throw new Error(`unknown op ${step.op}`);
      }
      out.calls = calls.splice(0);
      results.push(out);
    }
    assert.equal(calls.length, 0);
    bundle.dispose();
    recorded.push({ id: scenario.id, options: scenario.options, steps: scenario.steps, results });
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
