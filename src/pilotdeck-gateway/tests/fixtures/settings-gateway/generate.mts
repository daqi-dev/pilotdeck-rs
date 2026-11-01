// Real TS golden for the U7 settings write & reload gateway adapter (#1318).
// The five methods run through the real three-layer stack: real SessionRouter
// (real beginTurn/close, real AgentHandle sessions), real InProcessGateway
// (only the interactionCoordinator is wrapped on the prototype chain to
// record close-session:<reason>), a real ProjectRuntimeRegistry prototype
// (real applyFlagSettingsForSdk / reloadOutputStylesForSdk over real Map
// state; resolve/pluginRuntime stubbed), a real GatewayRuntimeRefreshBundle
// (recording collaborators; the configStore delegate always points at the
// current scenario tree's real createPilotConfigStoreSync store), and the
// real updatePilotLocalSettings writer over real scratch YAML trees. The
// updateSettings closure body is the verbatim createLocalGateway.ts:1384-1404
// closure (createLocalGateway itself boots the whole CLI and cannot run
// offline); createLocalGateway.ts is pinned in sourceSha256 so any edit to
// the closure changes the oracle's provenance.
//
// Run from the repo root with Node 22 and the fixed baseline's tsx loader:
//   PDRS_BASELINE=/path/to/PilotDeck LANG=en_US.UTF-8 \
//     node --disable-warning=ExperimentalWarning --import .../tsx/dist/loader.mjs \
//       src/pilotdeck-gateway/tests/fixtures/settings-gateway/generate.mts <scratch>
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import { mkdir, writeFile } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

assert.equal(process.version, 'v22.22.0', '#1318 exact Node baseline');
assert.ok(process.env.PDRS_BASELINE, '#1318 set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], '#1318 provide the fixture output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/gateway/server/GatewayWsConnection.ts',
  'src/gateway/client/InProcessGateway.ts',
  'src/gateway/client/GatewayInteractionCoordinator.ts',
  'src/gateway/SessionRouter.ts',
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/cli/GatewayRuntimeRefreshBundle.ts',
  'src/cli/createLocalGateway.ts',
  'src/pilot/config/PilotConfigStore.ts',
  'src/pilot/config/updateLocalSettings.ts',
  'src/pilot/config/loadPilotConfig.ts',
];
const sourceSha256 = await baselineSourceHashes(baseline, sources);
// The real store's contentHash orders keys with localeCompare; Rust pins en-US.
const locale = new Intl.Collator().resolvedOptions().locale;
assert.equal(locale, 'en-US', 'generate with LANG=en_US.UTF-8 (see README.md)');

await mkdir(join(output, 'home'), { recursive: true });
process.env.HOME = join(output, 'home');

const { SessionRouter } = await import(pathToFileURL(join(baseline, 'src/gateway/SessionRouter.ts')).href);
const { InProcessGateway } = await import(pathToFileURL(join(baseline, 'src/gateway/client/InProcessGateway.ts')).href);
const { GatewayWsConnection } = await import(pathToFileURL(join(baseline, 'src/gateway/server/GatewayWsConnection.ts')).href);
const { PILOTDECK_GATEWAY_PROTOCOL_VERSION } = await import(pathToFileURL(join(baseline, 'src/gateway/protocol/version.ts')).href);
const { ProjectRuntimeRegistry } = await import(pathToFileURL(join(baseline, 'src/cli/ProjectRuntimeRegistry.ts')).href);
const { GatewayRuntimeRefreshBundle } = await import(pathToFileURL(join(baseline, 'src/cli/GatewayRuntimeRefreshBundle.ts')).href);
const { GatewayInteractionCoordinator } = await import(pathToFileURL(join(baseline, 'src/gateway/client/GatewayInteractionCoordinator.ts')).href);
const { asAgentHandle } = await import(pathToFileURL(join(baseline, 'src/agent/scope/AgentHandle.ts')).href);
const { createPilotConfigStoreSync } = await import(pathToFileURL(join(baseline, 'src/pilot/config/PilotConfigStore.ts')).href);
const { updatePilotLocalSettings } = await import(pathToFileURL(join(baseline, 'src/pilot/config/updateLocalSettings.ts')).href);
const { DialogGatewayError } = await import(pathToFileURL(join(baseline, 'src/gateway/dialog/errors.ts')).href);

const events: string[] = [];
// The interaction coordinator is REPLACED on the prototype chain (the #1215
// pattern): only the reason strings the methods pass are pinned here; the
// coordinator layer itself is outside this component's parity scope.
const realCoordinator = new GatewayInteractionCoordinator({});
const coordinator = Object.create(realCoordinator);
coordinator.closeSession = (sessionKey: string, reason: string) => {
  events.push(`close-session:${sessionKey}:${reason}`);
};

// Real AgentHandle sessions; key `f` disposes with a real rejection, so the
// apply close-fault cell fails through the real asAgentHandle path.
const session = (key: string) => ({
  sessionId: `sess-${key}`,
  snapshot: () => ({ sessionId: `sess-${key}`, messages: [] }),
  pendingTurns: () => [],
  pendingTurnCount: 0,
  discardQueuedTurns: async () => {},
  abort: () => {},
  dispose: async () => {
    events.push(`dispose:${key}`);
    if (key === 'f') throw new Error('fixture dispose failure');
  },
});
const router = new SessionRouter({
  createSession: async (context: any) => {
    events.push(`create:${context.sessionKey}`);
    return asAgentHandle(session(context.sessionKey));
  },
  onSessionEvict: (sessionKey: string, reason: string) => {
    events.push(`evict:${sessionKey}:${reason}`);
  },
  idleSweepIntervalMs: 0,
  idleSessionTimeoutMs: 60 * 60 * 1000,
});

// The registry prototype: the real applyFlagSettingsForSdk and
// reloadOutputStylesForSdk bodies over real Map state; resolve returns a
// per-project fake runtime (a real project runtime boots plugin processes).
const KEYS = ['a', 'a2', 'n', 'b', 'f', 'ghost', 'sx', 'sy', 'sz', 'sw'];
let nextChanged = ['style-a'];
const registry: any = Object.create(ProjectRuntimeRegistry.prototype);
Object.assign(registry, {
  sdkThinkingOverrides: new Map(),
  _sessionOverrides: new Map(),
  sdkSessionProjects: new Map(),
  sdkSessionConfigs: new Map(),
  resolve: (projectKey: any) => {
    events.push(`resolve:${projectKey ?? 'null'}`);
    return {
      projectRoot: projectKey ?? '/pdrs-fixture/project',
      pluginRuntime: {
        reloadOutputStyles: async () => {
          events.push('styles-reload');
          return { changed: [...nextChanged] };
        },
      },
    };
  },
});

// --- the per-scenario local settings tree ----------------------------------
// One `tree:<name>` setup creates a real store over a real scratch YAML
// tree; `local` is the mutable binding the updateSettings closure and the
// bundle's configStore delegate read at call time.
type Tree = { name: string; store: any; writerPath: string; storePath: string };
const trees = new Map<string, Tree>();
let local: {
  env: Record<string, string | undefined>;
  projectRoot: string;
  configStore: any;
  organizationPolicy: any;
} = { env: {}, projectRoot: '', configStore: undefined, organizationPolicy: undefined };

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
const BAD_SCHEMA = BASE.replace('schemaVersion: 1', 'schemaVersion: 2');
const BROKEN_YAML = 'agent: {model: parity/deterministic\nmodel: [unclosed';

async function makeTree(name: string, configPathOverride?: string) {
  const root = join(output, 'scratch', name);
  const home = join(root, 'home');
  const project = join(root, 'project');
  await mkdir(home, { recursive: true });
  await mkdir(project, { recursive: true });
  const writerPath = join(home, 'pilotdeck.yaml');
  await writeFile(writerPath, BASE);
  const env: Record<string, string | undefined> = { PILOT_HOME: home, HOME: home };
  const storeEnv: Record<string, string | undefined> = configPathOverride
    ? { ...env, PILOTDECK_CONFIG_PATH: configPathOverride }
    : { ...env };
  const store = createPilotConfigStoreSync({ projectRoot: project, env: storeEnv });
  store.subscribe((event: any) => events.push(`store-event:${event.changedPaths.join(',')}`));
  const tree: Tree = { name, store, writerPath, storePath: configPathOverride ?? writerPath };
  trees.set(name, tree);
  local = { env, projectRoot: project, configStore: store, organizationPolicy: undefined };
  // The probe listener subscribes before the bundle rebinds, so the probe
  // event always precedes the bundle's onConfigChange prefix.
  rebindBundleStore();
  return tree;
}

// --- the refresh bundle (recording collaborators, real class) --------------
// The bundle's configStore delegate re-points every live subscription (the
// bundle's own attach listener, reloadConfig's transient capture listener)
// at the current tree's store, so the bundle follows the scenario trees the
// way production's single store feeds it. The registry reload promise never
// settles, so only onConfigChange's synchronous prefix (log + sdk dispatch +
// registry.reload call) is pinned; the .then (markAllDirty + broadcast)
// ordering versus the WS response would be microtask-race noise.
const bundleListeners = new Map<any, () => void>();
const storeDelegate = {
  subscribe(listener: any) {
    const unsubscribe = local.configStore.subscribe(listener);
    bundleListeners.set(listener, unsubscribe);
    return () => {
      bundleListeners.delete(listener);
      unsubscribe();
    };
  },
  reload: (reason?: string) => local.configStore.reload(reason),
};
function rebindBundleStore() {
  for (const listener of [...bundleListeners.keys()]) {
    bundleListeners.get(listener)!();
    bundleListeners.set(listener, local.configStore.subscribe(listener));
  }
}
const bundle = new GatewayRuntimeRefreshBundle({
  configStore: storeDelegate,
  registry: {
    reload: () => {
      events.push('registry-reload');
      return new Promise(() => {});
    },
    invalidate: (projectKey?: string) => {
      events.push(`invalidate:${projectKey ?? 'null'}`);
    },
  },
  memoryMaintenance: { schedule: () => {} },
  getRouter: () => ({
    markAllDirty: (reason: string) => {
      events.push(`mark-all-dirty:${reason}`);
      return 2;
    },
    markProjectDirty: (projectKey: string, reason: string) => {
      events.push(`mark-project-dirty:${projectKey}:${reason}`);
      return 1;
    },
    cachedSessionCount: () => 0,
    snapshotSession: () => undefined,
  }),
  projectRoot: '/pdrs-fixture/project',
  memoryDiagnosticsEnabled: false,
  logMemoryDiagnostic: () => {},
  summarizeMessages: () => ({}),
  dispatchSdkConfigChange: (payload: any) =>
    events.push(`sdk-config-change:${payload.changedPaths.join(',')}|${payload.changeClasses.join(',')}`),
  log: (message: string) => events.push(`log:${message}`),
});
bundle.bindServer({
  broadcastNotification: (name: string, payload: any) =>
    events.push(`broadcast:${name}:${JSON.stringify(payload)}`),
});

// The updateSettings closure, verbatim from createLocalGateway.ts:1384-1404
// (organizationPolicy/env/projectRoot/configStore come from the tree binding;
// createLocalGateway passes its own organizationPolicy/configStore the same
// way). The STAFFDECK branch of reloadExtensions below is env-gated dead
// code under this golden's contained env; kept verbatim, see the row list.
const gateway = new InProcessGateway(router, {
  interactionCoordinator: coordinator as any,
  applyFlagSettings: (input: any) => registry.applyFlagSettingsForSdk(input),
  async updateSettings(input: any) {
    if (local.organizationPolicy?.settings?.canUpdateLocalSettings === false) {
      throw new DialogGatewayError(
        "GATEWAY_ORGANIZATION_SETTINGS_UPDATE_DENIED",
        "Gateway organization policy denies SDK localSettings updates.",
      );
    }
    const updated = await updatePilotLocalSettings({ settings: input.settings, env: local.env, projectRoot: local.projectRoot });
    if (updated.changedPaths.length === 0) return updated;
    let changedPaths = updated.changedPaths;
    const unsubscribe = local.configStore.subscribe((event: any) => { changedPaths = event.changedPaths; });
    try {
      await local.configStore.reload("sdk-update-settings");
    } finally {
      unsubscribe();
    }
    return { ...updated, changedPaths };
  },
  reloadOutputStyles: (input: any) => registry.reloadOutputStylesForSdk(input),
  reloadConfig: () => bundle.reloadConfig(),
  reloadExtensions: async (input: any) => {
    const result = await bundle.reloadExtensions(input);
    if (local.env.STAFFDECK_PROJECT_BINDING_ENABLED === 'true' && input?.projectKey) registry.resolve(input.projectKey);
    return result;
  },
});
registry.setGateway(gateway);

// --- WS layer: a fake text connection exchanging real frames ----------------
const waiters: ((message: string) => void)[] = [];
let notify: ((message: string) => void) | undefined;
const connect = (connectionGateway: any) => {
  const ws = {
    onMessage(handler: (message: string) => void) { notify = handler; },
    onClose(_handler: () => void) {},
    sendText(message: string) {
      for (const waiter of waiters.splice(0)) waiter(message);
    },
    close() {},
  };
  new GatewayWsConnection(ws as any, { gateway: connectionGateway, token: 'probe-token', serverVersion: 'probe' });
  const hello = new Promise<string>((done) => waiters.push(done));
  notify!(JSON.stringify({
    type: 'hello', protocolVersion: PILOTDECK_GATEWAY_PROTOCOL_VERSION,
    clientName: 'probe', clientVersion: '0', token: 'probe-token',
  }));
  return hello;
};
await connect(gateway as any);

let next = 0;
async function rawRequest(method: string, params: unknown): Promise<Record<string, unknown>> {
  const id = `r${next++}`;
  const frame: Record<string, unknown> = { type: 'request', id, method };
  if (params !== undefined) frame.params = params;
  events.length = 0;
  const received = new Promise<string>((done) => waiters.push(done));
  notify!(JSON.stringify(frame));
  const response: Record<string, unknown> = JSON.parse(await received);
  delete response.id;
  return response;
}

function postState() {
  const read = (path: string) => {
    try { return readFileSync(path, 'utf8'); } catch { return null; }
  };
  const post: {
    turns: string[];
    sessions: Record<string, boolean>;
    overrides: Record<string, { effort: string | null; mode: string | null }>;
    trees: Record<string, { version: number; storeYaml: string | null; writerYaml: string | null }>;
  } = { turns: [], sessions: {}, overrides: {}, trees: {} };
  post.turns = KEYS.filter((key) => router.hasActiveTurn(key));
  for (const key of KEYS) {
    post.sessions[key] = router.hasSession(key);
    const thinking = registry.sdkThinkingOverrides.get(key);
    const overrides = registry._sessionOverrides.get(key) ?? {};
    post.overrides[key] = { effort: thinking ? thinking.mode : null, mode: overrides.permissionMode ?? null };
  }
  for (const [name, tree] of trees) {
    post.trees[name] = {
      version: tree.store.getSnapshot().version,
      storeYaml: read(tree.storePath),
      writerYaml: read(tree.writerPath),
    };
  }
  return post;
}

async function runSetup(step: string) {
  const parts = step.split(':');
  const [action, arg] = parts;
  switch (action) {
    case 'ensure-session':
      assert.ok(KEYS.includes(arg), `setup key ${arg} must be a scenario key`);
      await router.getOrCreate({ sessionKey: arg, projectKey: '/p' } as any);
      break;
    case 'begin-turn':
      assert.ok(KEYS.includes(parts[1]), `setup key ${parts[1]} must be a scenario key`);
      router.beginTurn(parts[1], parts[2]);
      break;
    case 'styles-session':
      assert.ok(KEYS.includes(parts[1]), `setup key ${parts[1]} must be a scenario key`);
      registry.sdkSessionProjects.set(parts[1], parts[2]);
      await router.getOrCreate({ sessionKey: parts[1], projectKey: '/p' } as any);
      break;
    case 'styles-config':
      registry.sdkSessionConfigs.set(arg, { outputStyle: 'default' });
      break;
    case 'styles-changed':
      nextChanged = arg.split(',');
      break;
    case 'tree':
      await makeTree(arg);
      break;
    case 'tree-split': {
      // The store reads a dedicated config file (PILOTDECK_CONFIG_PATH) the
      // writer never touches: the only single-threaded composition where
      // the write succeeds and the reload still fails.
      const root = join(output, 'scratch', arg);
      await mkdir(join(root, 'home'), { recursive: true });
      await mkdir(join(root, 'project'), { recursive: true });
      const alt = join(root, 'alt-config.yaml');
      await writeFile(alt, BASE);
      await makeTree(arg, alt);
      break;
    }
    case 'break-yaml': {
      const tree = trees.get(arg)!;
      await writeFile(tree.storePath, BROKEN_YAML);
      break;
    }
    case 'break-store-yaml': {
      const tree = trees.get(arg)!;
      await writeFile(tree.storePath, BAD_SCHEMA);
      break;
    }
    case 'write-yaml': {
      const tree = trees.get(parts[1])!;
      await writeFile(tree.storePath, parts[2] === 'other' ? OTHER_MODEL : BASE);
      break;
    }
    case 'deny-org':
      local.organizationPolicy = { settings: { canUpdateLocalSettings: false } };
      break;
    case 'allow-org':
      local.organizationPolicy = undefined;
      break;
    case 'attach-bundle':
      bundle.attach();
      break;
    default:
      assert.fail(`unknown setup action ${action}`);
  }
}

const rows: any[] = [];
async function run(id: string, method: string, params: unknown, setup: string[] = []) {
  for (const step of setup) await runSetup(step);
  const response = await rawRequest(method, params);
  // `undefined` params must stay distinct from `params: null` (fix-r1 P2-2
  // of #1215): the key is simply omitted from the row.
  rows.push({ id, method, ...(params === undefined ? {} : { params }), setup, events: [...events], response, post: postState() });
}

// --- apply_flag_settings ----------------------------------------------------
await run('apply-effort-low', 'apply_flag_settings', { sessionKey: 'a', settings: { effortLevel: 'low' } },
  ['ensure-session:a']);
await run('apply-permissions-plan', 'apply_flag_settings', { sessionKey: 'a2', settings: { permissions: { defaultMode: 'plan' } } },
  ['ensure-session:a2']);
await run('apply-clear-both', 'apply_flag_settings', { sessionKey: 'a', settings: { effortLevel: null, permissions: null } });
await run('apply-permissions-empty-clears', 'apply_flag_settings', { sessionKey: 'n', settings: { permissions: {} } },
  ['ensure-session:n']);
await run('apply-empty-settings', 'apply_flag_settings', { sessionKey: 'n', settings: {} });
await run('apply-applied-no-session', 'apply_flag_settings', { sessionKey: 'ghost', settings: { effortLevel: 'high' } });
await run('apply-busy', 'apply_flag_settings', { sessionKey: 'b', settings: { effortLevel: 'low' } },
  ['ensure-session:b', 'begin-turn:b:run-1']);
await run('apply-order-settings-before-busy', 'apply_flag_settings', { sessionKey: 'b', settings: [] });
await run('apply-order-key-first', 'apply_flag_settings', { sessionKey: '  ', settings: [] });
await run('apply-close-fault', 'apply_flag_settings', { sessionKey: 'f', settings: { effortLevel: 'low' } },
  ['ensure-session:f']);
await run('apply-invalid-effort', 'apply_flag_settings', { sessionKey: 'a', settings: { effortLevel: 'turbo' } });
await run('apply-unknown-key', 'apply_flag_settings', { sessionKey: 'a', settings: { answer: 42 } });
await run('apply-permissions-bad-shape', 'apply_flag_settings', { sessionKey: 'a', settings: { permissions: [] } });
await run('apply-permissions-subkey', 'apply_flag_settings', { sessionKey: 'a', settings: { permissions: { mode: 'plan' } } });
await run('apply-permissions-bad-mode', 'apply_flag_settings', { sessionKey: 'a', settings: { permissions: { defaultMode: 'yolo' } } });
await run('apply-permissions-number-mode', 'apply_flag_settings', { sessionKey: 'a', settings: { permissions: { defaultMode: 42 } } });
await run('apply-settings-missing', 'apply_flag_settings', { sessionKey: 'a' });
await run('apply-settings-null', 'apply_flag_settings', { sessionKey: 'a', settings: null });
await run('apply-blank-session-key', 'apply_flag_settings', { sessionKey: '   ', settings: {} });
await run('apply-nonstring-session-key', 'apply_flag_settings', { sessionKey: 5, settings: {} });
await run('apply-null-params', 'apply_flag_settings', null);
await run('apply-params-omitted', 'apply_flag_settings', undefined);
// The `input.sessionKey?.trim()` guard short-circuits on a nullish key value
// and on a primitive params (its property read yields undefined): both take
// INVALID_SESSION_KEY, distinct from the non-string key TypeError above.
await run('apply-null-session-key', 'apply_flag_settings', { sessionKey: null, settings: {} });
await run('apply-scalar-params', 'apply_flag_settings', 5);

// --- reload_output_styles ----------------------------------------------------
await run('styles-closes-matching', 'reload_output_styles', { projectKey: '/proj-A' },
  ['styles-session:sx:/proj-A', 'styles-config:sx', 'styles-session:sy:/proj-B', 'styles-config:sy', 'styles-session:sz:/proj-A']);
await run('styles-skips-other-project', 'reload_output_styles', { projectKey: '/proj-B' });
await run('styles-no-config-no-close', 'reload_output_styles', { projectKey: '/proj-A' });
await run('styles-omitted-params', 'reload_output_styles', undefined);
await run('styles-null-params', 'reload_output_styles', null);
await run('styles-changed-list', 'reload_output_styles', {},
  ['styles-changed:style-x,style-y', 'styles-session:sw:/proj-A', 'styles-config:sw']);

// --- update_settings --------------------------------------------------------
await run('update-merges-store-paths', 'update_settings',
  { source: 'localSettings', settings: { agent: { maxContextTokens: 100000 } } }, ['tree:t1']);
await run('update-rewrite-same-value', 'update_settings',
  { source: 'localSettings', settings: { agent: { maxContextTokens: 100000 } } });
await run('update-clear-value', 'update_settings',
  { source: 'localSettings', settings: { agent: { maxContextTokens: null } } });
await run('update-noop-early-return', 'update_settings',
  { source: 'localSettings', settings: { agent: {} } });
await run('update-org-denied', 'update_settings',
  { source: 'localSettings', settings: { agent: { maxContextTokens: 100000 } } }, ['tree:t2', 'deny-org']);
await run('update-bad-source', 'update_settings', { source: 'remote', settings: {} }, ['allow-org']);
await run('update-source-missing', 'update_settings', { settings: {} });
await run('update-settings-missing', 'update_settings', { source: 'localSettings' });
await run('update-settings-array', 'update_settings', { source: 'localSettings', settings: [] });
await run('update-writer-unsupported', 'update_settings',
  { source: 'localSettings', settings: { nope: 1 } }, ['tree:t3']);
await run('update-writer-invalid', 'update_settings',
  { source: 'localSettings', settings: { agent: { maxContextTokens: -5 } } });
await run('update-writer-yaml-invalid', 'update_settings',
  { source: 'localSettings', settings: { agent: { maxContextTokens: 100000 } } }, ['tree:t4', 'break-yaml:t4']);
await run('update-reload-error', 'update_settings',
  { source: 'localSettings', settings: { agent: { maxContextTokens: 100000 } } }, ['tree-split:t5', 'break-store-yaml:t5']);
await run('update-runs-through-bundle', 'update_settings',
  { source: 'localSettings', settings: { extension: { includeHookEvents: true } } }, ['tree:t6', 'attach-bundle']);
// `input.source` throws only on nullish params; a primitive params reads
// `undefined` and takes the INVALID_SETTINGS_SOURCE gate.
await run('update-null-params', 'update_settings', null);
await run('update-params-omitted', 'update_settings', undefined);
await run('update-scalar-params', 'update_settings', 5);

// --- reload_config ----------------------------------------------------------
await run('reload-config-unchanged', 'reload_config', undefined, ['tree:t7']);
await run('reload-config-with-change', 'reload_config', undefined, ['write-yaml:t7:other']);
await run('reload-config-error', 'reload_config', undefined, ['tree:t8', 'break-store-yaml:t8']);

// --- reload_extensions ------------------------------------------------------
await run('extensions-manual', 'reload_extensions', undefined);
await run('extensions-project', 'reload_extensions', { projectKey: '/p' });
await run('extensions-changed-paths', 'reload_extensions', { changedPaths: ['ext/a.ts', 'ext/b.ts'] });
await run('extensions-bad-changed-paths', 'reload_extensions', { changedPaths: 'x' });

// --- assembly-layer cases (recorded, not component-replayable) ---------------
// A gateway assembled without the five options answers CAPABILITY_UNAVAILABLE
// for apply/update/reload_output_styles and the `{reloaded:false}` projection
// for the two reloads; the Rust component is constructed with the capability,
// so these document the wiring-layer contract for the future routing PR.
const bare = new InProcessGateway(new SessionRouter({
  createSession: async () => asAgentHandle(session('bare')),
  idleSweepIntervalMs: 0,
}), {});
await connect(bare as any);
const capabilityCases: any[] = [];
for (const method of ['apply_flag_settings', 'update_settings', 'reload_output_styles', 'reload_config', 'reload_extensions']) {
  const response = await rawRequest(method, { sessionKey: 's' });
  capabilityCases.push({ method, response });
}
await connect(gateway as any);
const unknownMethod = await rawRequest('settings_probe', { sessionKey: 's' });

// --- coverage guards: pin the matrix expectations the Rust replay asserts ----
const byId = (id: string) => rows.find((row) => row.id === id);
assert.equal(byId('apply-effort-low').response.ok, true, 'apply ok');
assert.deepEqual(byId('apply-effort-low').response.result, { applied: ['effortLevel'], cleared: [] }, 'apply result');
assert.deepEqual(byId('apply-effort-low').events, ['dispose:a', 'evict:a:closed'], 'apply closes through the router only');
assert.equal(byId('apply-effort-low').post.overrides.a.effort, 'low', 'apply commits the override');
assert.equal(byId('apply-effort-low').post.sessions.a, false, 'apply evicts');
assert.equal(byId('apply-permissions-plan').post.overrides.a2.mode, 'plan', 'permissions override commits');
assert.deepEqual(byId('apply-clear-both').response.result, { applied: [], cleared: ['effortLevel', 'permissions.defaultMode'] }, 'clear both');
assert.equal(byId('apply-clear-both').post.overrides.a.effort, null, 'clear removes the override');
assert.deepEqual(byId('apply-clear-both').events, [], 'close of an evicted key is a no-op');
assert.deepEqual(byId('apply-permissions-empty-clears').response.result,
  { applied: [], cleared: ['permissions.defaultMode'] }, 'an empty permissions object still clears');
assert.equal(byId('apply-permissions-empty-clears').post.sessions.n, false, 'a cleared flag closes the session');
assert.deepEqual(byId('apply-empty-settings').events, [], 'a no-op change never closes');
assert.equal(byId('apply-busy').response.error.code, 'SESSION_BUSY', 'busy code');
assert.equal(byId('apply-busy').response.error.message, 'Cannot apply flag settings during an active turn.', 'busy message');
assert.deepEqual(byId('apply-busy').events, [], 'busy runs no delegate');
assert.equal(byId('apply-order-settings-before-busy').response.error.code, 'INVALID_FLAG_SETTINGS', 'settings check precedes busy');
assert.equal(byId('apply-order-key-first').response.error.code, 'INVALID_SESSION_KEY', 'key check precedes settings');
assert.equal(byId('apply-invalid-effort').response.error.code, 'INVALID_FLAG_SETTINGS', 'invalid effort code');
assert.equal(byId('apply-invalid-effort').response.error.message, 'effortLevel must be low, medium, high, or null.', 'invalid effort message');
assert.equal(byId('apply-unknown-key').response.error.code, 'UNSUPPORTED_FLAG_SETTING', 'unknown key code');
assert.equal(byId('apply-unknown-key').response.error.message, 'answer has no PilotDeck session equivalent.', 'unknown key message');
assert.equal(byId('apply-permissions-subkey').response.error.message,
  'permissions.mode has no PilotDeck session equivalent.', 'permission subkey message');
assert.equal(byId('apply-permissions-bad-mode').response.error.message,
  'permissions.defaultMode=yolo is not supported.', 'bad mode message');
assert.equal(byId('apply-permissions-number-mode').response.error.message,
  'permissions.defaultMode=42 is not supported.', 'number mode message');
assert.equal(byId('apply-permissions-bad-shape').response.error.code, 'INVALID_FLAG_SETTINGS', 'permissions shape code');
assert.equal(byId('apply-nonstring-session-key').response.error.code, 'gateway_request_failed', 'non-string key envelope');
assert.equal(byId('apply-nonstring-session-key').response.error.message,
  'input.sessionKey?.trim is not a function', 'non-string key message');
assert.equal(byId('apply-null-params').response.error.message,
  "Cannot read properties of null (reading 'sessionKey')", 'null params message');
assert.equal(byId('apply-params-omitted').response.error.message,
  "Cannot read properties of undefined (reading 'sessionKey')", 'omitted params message');
assert.equal(byId('apply-null-session-key').response.error.code, 'INVALID_SESSION_KEY', 'null session key code');
assert.equal(byId('apply-null-session-key').response.error.message, 'sessionKey is required.', 'null session key message');
assert.equal(byId('apply-scalar-params').response.error.code, 'INVALID_SESSION_KEY', 'scalar params takes the key gate');
assert.equal(byId('apply-close-fault').response.error.code, 'gateway_request_failed', 'close fault envelope');
assert.equal(byId('apply-close-fault').response.error.message, 'Failed to dispose agent handle.', 'close fault message');
assert.equal(byId('apply-close-fault').post.overrides.f.effort, 'low', 'close fault keeps the commit');
assert.equal(byId('styles-closes-matching').response.result.reloaded, true, 'styles reloaded');
assert.deepEqual(byId('styles-closes-matching').response.result.changed, ['style-a'], 'styles changed');
assert.ok(byId('styles-closes-matching').events.includes('close-session:sx:sdk_output_styles_reloaded'), 'styles closes matching');
assert.ok(!byId('styles-closes-matching').events.includes('close-session:sy:sdk_output_styles_reloaded'), 'styles skips other project');
assert.equal(byId('styles-closes-matching').post.sessions.sx, false, 'styles evicts matching');
assert.equal(byId('styles-closes-matching').post.sessions.sy, true, 'styles keeps other project');
assert.equal(byId('styles-no-config-no-close').events.includes('dispose:sz'), false, 'no config no close');
assert.equal(byId('styles-null-params').response.error.code, 'gateway_request_failed', 'styles null params envelope');
assert.equal(byId('styles-null-params').response.error.message,
  "Cannot read properties of null (reading 'projectKey')", 'styles null params message');
assert.equal(byId('update-merges-store-paths').response.ok, true, 'update ok');
assert.deepEqual(byId('update-merges-store-paths').response.result.applied, ['agent.maxContextTokens'], 'update applied');
assert.equal(byId('update-noop-early-return').response.result.changedPaths.length, 0, 'noop changedPaths');
assert.equal(byId('update-noop-early-return').events.length, 0, 'noop does not reload');
assert.deepEqual(byId('update-rewrite-same-value').response.result.changedPaths, [], 'rewrite same value merges the empty event');
assert.equal(byId('update-rewrite-same-value').events.includes('store-event:'), true, 'rewrite still reloads');
assert.equal(byId('update-org-denied').response.error.code, 'GATEWAY_ORGANIZATION_SETTINGS_UPDATE_DENIED', 'org denied code');
assert.equal(byId('update-bad-source').response.error.code, 'INVALID_SETTINGS_SOURCE', 'bad source code');
assert.equal(byId('update-settings-missing').response.error.code, 'INVALID_LOCAL_SETTINGS', 'missing settings code');
assert.equal(byId('update-null-params').response.error.message,
  "Cannot read properties of null (reading 'source')", 'update null params message');
assert.equal(byId('update-params-omitted').response.error.message,
  "Cannot read properties of undefined (reading 'source')", 'update omitted params message');
assert.equal(byId('update-scalar-params').response.error.code, 'INVALID_SETTINGS_SOURCE', 'update scalar params code');
assert.equal(byId('update-writer-unsupported').response.error.code, 'UNSUPPORTED_LOCAL_SETTING', 'writer unsupported code');
assert.equal(byId('update-writer-invalid').response.error.code, 'INVALID_LOCAL_SETTINGS', 'writer invalid code');
assert.equal(byId('update-writer-yaml-invalid').response.error.code, 'LOCAL_SETTINGS_YAML_INVALID', 'writer yaml code');
assert.equal(byId('update-reload-error').response.error.code, 'CONFIG_SCHEMA_VERSION_UNSUPPORTED', 'reload error code');
assert.equal(byId('update-reload-error').response.error.message, 'Unsupported schemaVersion 2.', 'reload error message');
assert.ok(byId('update-reload-error').events.length === 0, 'failed reload publishes no event');
assert.ok(byId('update-runs-through-bundle').events.includes('registry-reload'), 'bundle sees the update chain');
assert.ok(byId('update-runs-through-bundle').events.some((event: string) => event.startsWith('log:[pilotdeck] Config reloaded')),
  'bundle logs the reload');
assert.deepEqual(byId('reload-config-unchanged').response.result, { reloaded: true, changedPaths: [] }, 'reload config unchanged');
assert.equal(byId('reload-config-with-change').response.result.reloaded, true, 'reload config change');
assert.ok(byId('reload-config-with-change').events.includes('registry-reload'),
  'the still-attached bundle follows the store the reload runs on');
assert.equal(byId('reload-config-error').response.error.message, 'Unsupported schemaVersion 2.', 'reload config error message');
assert.deepEqual(byId('extensions-manual').response.result, { reloaded: true, changedPaths: [] }, 'extensions manual result');
assert.ok(byId('extensions-manual').events.includes('invalidate:null'), 'extensions manual invalidates all');
assert.ok(byId('extensions-manual').events.includes(
  'broadcast:config_changed:{"changedPaths":[],"changeClasses":["extension-changed"]}'), 'extensions broadcasts');
assert.ok(byId('extensions-project').events.includes('invalidate:/p'), 'extensions project invalidates');
assert.ok(byId('extensions-project').events.includes('mark-project-dirty:/p:extension_changed'), 'extensions project dirties');
assert.equal(byId('extensions-bad-changed-paths').response.error.code, 'gateway_request_failed', 'bad changedPaths envelope');
assert.equal(byId('extensions-bad-changed-paths').response.error.message,
  'changedPaths.join is not a function', 'bad changedPaths message');
assert.equal(rows.length, 54, 'matrix row count');
for (const row of capabilityCases) {
  if (row.method === 'reload_config' || row.method === 'reload_extensions') {
    assert.deepEqual(row.response.result, { reloaded: false, reason: 'unsupported' }, `${row.method} unsupported projection`);
  } else {
    assert.equal(row.response.error.code, 'CAPABILITY_UNAVAILABLE', `${row.method} capability code`);
    assert.equal(row.response.error.message, `${row.method} is unavailable.`, `${row.method} capability message`);
  }
}
assert.equal(unknownMethod.ok, false, 'unknown method fails');
assert.equal(unknownMethod.error.code, 'gateway_request_failed', 'unknown method envelope');
assert.equal(unknownMethod.error.message, 'Unknown gateway method settings_probe.', 'unknown method message');

await writeFile(
  join(output, 'oracle.json'),
  JSON.stringify({
    sourceCommit, sourceSha256, node: process.version,
    setupKeys: KEYS, cases: rows,
    assemblyCases: { capability: capabilityCases, unknownMethod },
  }, null, 2) + '\n',
  { flag: 'wx' },
);
console.log(`PASS: ${rows.length} real TS settings gateway cases + ${capabilityCases.length + 1} assembly cases`);
