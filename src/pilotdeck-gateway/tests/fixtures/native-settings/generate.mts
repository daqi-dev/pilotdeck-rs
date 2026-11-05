// Real TS golden for the U7 settings routing fixture (#1320). The six
// methods run through the real three-layer stack exactly like the #1318
// settings-gateway golden (real SessionRouter / real InProcessGateway /
// real GatewayWsConnection over a fake text transport / real
// ProjectRuntimeRegistry prototype / real GatewayRuntimeRefreshBundle /
// real updatePilotLocalSettings writer), extended for the routing
// acceptance of #1320:
//
//   - `resolve_settings` is wired (createLocalGateway.ts:1402 binds
//     `toGatewayResolvedSettings(configStore.getSnapshot())`; the function
//     body is the verbatim createLocalGateway.ts:1580-1610 copy, and
//     createLocalGateway.ts is pinned in sourceSha256);
//   - every row records 调用 → 通知 → 再读: the WS response, the
//     notification frames the connection received before and after that
//     response, and a follow-up `resolve_settings` re-read (version /
//     contentHash / config) proving reads and writes share one store
//     generation;
//   - the bundle's registry delegate settles on a setImmediate macrotask
//     (the #1318 golden pinned only the synchronous prefix of the reload
//     chain because its registry never settled), so the golden pins the
//     full chain: log → sdk dispatch → registry.reload() → (settle) →
//     markAllDirty → broadcast, with the broadcast frame landing AFTER
//     the reload_config response and BEFORE it for the synchronous
//     branches (reloadExtensions, runPolicy-only changes);
//   - `bindServer` mirrors GatewayServer.broadcastNotification
//     (server/GatewayServer.ts:80-84: iterate the connection set calling
//     sendNotification on each);
//   - the `matrix` section assembles the gateway per the #1320 native
//     matrix (only resolve_settings / reload_config / reload_extensions
//     configured — the isolated native process owns exactly those three
//     delegates) and records the answers the native production route must
//     reproduce cell by cell;
//   - the `divergenceCases` section records the two non-string
//     reload_extensions cells deferred to #1320 by
//     specs/gateway-parity/diffs/U7-EXTENSIONS-RELOAD-STRING-COERCION-001
//     (changedPaths:[1,true,null] / projectKey:5); they document the TS
//     truth (original values pass through) and are NOT replayed in Rust
//     while the divergence stays registered.
//
// Run from the repo root with Node 22 and the fixed baseline's tsx loader:
//   PDRS_BASELINE=/path/to/PilotDeck LANG=en_US.UTF-8 \
//     node --disable-warning=ExperimentalWarning --import .../tsx/dist/loader.mjs \
//       src/pilotdeck-gateway/tests/fixtures/native-settings/generate.mts <scratch>
import { assertBaseline, baselineSourceHashes } from '../_shared/baseline.mts';
import { mkdir, writeFile } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';

assert.equal(process.version, 'v22.22.0', '#1320 exact Node baseline');
assert.ok(process.env.PDRS_BASELINE, '#1320 set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], '#1320 provide the fixture output directory');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
const sourceCommit = assertBaseline(baseline);
const sources = [
  'src/gateway/server/GatewayWsConnection.ts',
  'src/gateway/server/GatewayServer.ts',
  'src/gateway/client/InProcessGateway.ts',
  'src/gateway/client/GatewayInteractionCoordinator.ts',
  'src/gateway/SessionRouter.ts',
  'src/cli/ProjectRuntimeRegistry.ts',
  'src/cli/GatewayRuntimeRefreshBundle.ts',
  'src/cli/createLocalGateway.ts',
  'src/pilot/config/PilotConfigStore.ts',
  'src/pilot/config/updateLocalSettings.ts',
  'src/pilot/config/loadPilotConfig.ts',
  'src/pilot/config/classifyChanges.ts',
  'src/pilot/config/redact.ts',
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
const { redactConfig } = await import(pathToFileURL(join(baseline, 'src/pilot/config/redact.ts')).href);
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

// Real AgentHandle sessions over the real router.
const KEYS = ['a', 'b', 'sx'];
const session = (key: string) => ({
  sessionId: `sess-${key}`,
  snapshot: () => ({ sessionId: `sess-${key}`, messages: [] }),
  pendingTurns: () => [],
  pendingTurnCount: 0,
  discardQueuedTurns: async () => {},
  abort: () => {},
  dispose: async () => {
    events.push(`dispose:${key}`);
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
// tree; `local` is the mutable binding the delegates read at call time.
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
// A runPolicy-only change: isRunPolicyOnlyChange routes it through the
// synchronous broadcast branch (no registry reload).
const RUN_POLICY = `${BASE}runPolicy:
  failureGuard:
    enabled: false
`;
const BAD_SCHEMA = BASE.replace('schemaVersion: 1', 'schemaVersion: 2');

// The bundle is attached once (createLocalGateway.ts:831 `attach()`), on the
// first scenario tree's store; later trees rebind the subscription.
let bundleAttached = false;

async function makeTree(name: string) {
  const root = join(output, 'scratch', name);
  const home = join(root, 'home');
  const project = join(root, 'project');
  await mkdir(home, { recursive: true });
  await mkdir(project, { recursive: true });
  const writerPath = join(home, 'pilotdeck.yaml');
  await writeFile(writerPath, BASE);
  const env: Record<string, string | undefined> = { PILOT_HOME: home, HOME: home };
  const store = createPilotConfigStoreSync({ projectRoot: project, env });
  store.subscribe((event: any) => events.push(`store-event:${event.changedPaths.join(',')}`));
  trees.set(name, { name, store, writerPath, storePath: writerPath });
  local = { env, projectRoot: project, configStore: store, organizationPolicy: undefined };
  // The probe listener subscribes before the bundle rebinds, so the probe
  // event always precedes the bundle's onConfigChange prefix.
  rebindBundleStore();
  if (!bundleAttached) {
    bundle.attach();
    bundleAttached = true;
  }
  return trees.get(name)!;
}

// --- the refresh bundle ------------------------------------------------------
// The bundle's configStore delegate re-points every live subscription at the
// current tree's store, so the bundle follows the scenario trees the way
// production's single store feeds it. The registry delegate settles on a
// setImmediate macrotask: the reload_config / update_settings response
// resolves before the settlement (markAllDirty + broadcast), which is the
// production truth (a real registry reload outlives the store reload) made
// deterministic — the #1318 golden pinned only the prefix because its
// registry never settled.
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
      return new Promise<void>((settle) => setImmediate(settle));
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
  warn: (message: string, error?: unknown) =>
    events.push(`warn:${message}${error === undefined ? '' : `:${String(error)}`}`),
});

// --- the notification server: GatewayServer.broadcastNotification mirror ---
// server/GatewayServer.ts:80-84: `for (const conn of connections)
// conn.sendNotification(name, payload)`. The set holds the real
// GatewayWsConnection instances; sendNotification drops frames until the
// connection completed its hello (this.authed), exactly like the real
// server with a mid-handshake connection.
const connections: any[] = [];
bundle.bindServer({
  broadcastNotification(name: string, payload: unknown) {
    for (const conn of connections) conn.sendNotification(name, payload);
    events.push(`broadcast:${name}:${JSON.stringify(payload)}`);
  },
});

// `toGatewayResolvedSettings`, verbatim from createLocalGateway.ts:1580-1610
// (createLocalGateway boots the whole CLI and cannot run offline; the file
// is pinned in sourceSha256 so any edit to the projection changes the
// oracle's provenance).
function toGatewayResolvedSettings(snapshot: any) {
  return {
    schemaVersion: snapshot.schemaVersion,
    version: snapshot.version,
    loadedAt: snapshot.loadedAt.toISOString(),
    contentHash: snapshot.contentHash,
    config: redactConfig(snapshot.config) as Record<string, unknown>,
    sources: snapshot.sources.map((source: any) => ({
      kind: source.kind,
      priority: source.priority,
      loadedAt: source.loadedAt.toISOString(),
      ...(source.path ? { path: source.path } : {}),
      ...(source.contentHash ? { contentHash: source.contentHash } : {}),
      ...(source.phase ? { phase: source.phase } : {}),
    })),
    diagnostics: snapshot.diagnostics.map((diagnostic: any) => ({
      code: diagnostic.code,
      severity: diagnostic.severity,
      message: diagnostic.message,
      ...(diagnostic.path ? { path: diagnostic.path } : {}),
      ...(diagnostic.source ? { source: diagnostic.source } : {}),
      ...(diagnostic.hint ? { hint: diagnostic.hint } : {}),
      ...(diagnostic.redactedValue ? { redactedValue: diagnostic.redactedValue } : {}),
      ...(diagnostic.recoverable !== undefined ? { recoverable: diagnostic.recoverable } : {}),
    })),
  };
}

// The updateSettings closure, verbatim from createLocalGateway.ts:1384-1404;
// the reloadExtensions closure keeps the STAFFDECK branch verbatim (dead
// under this golden's contained env, see the row list).
const resolveSettingsDelegate = async () => toGatewayResolvedSettings(local.configStore.getSnapshot());
const updateSettingsDelegate = async (input: any) => {
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
};
const reloadExtensionsDelegate = async (input: any) => {
  const result = await bundle.reloadExtensions(input);
  if (local.env.STAFFDECK_PROJECT_BINDING_ENABLED === 'true' && input?.projectKey) registry.resolve(input.projectKey);
  return result;
};

// Phase A: the fully configured assembly (all six delegates).
const gateway = new InProcessGateway(router, {
  interactionCoordinator: coordinator as any,
  applyFlagSettings: (input: any) => registry.applyFlagSettingsForSdk(input),
  updateSettings: updateSettingsDelegate,
  resolveSettings: resolveSettingsDelegate,
  reloadOutputStyles: (input: any) => registry.reloadOutputStylesForSdk(input),
  reloadConfig: () => bundle.reloadConfig(),
  reloadExtensions: reloadExtensionsDelegate,
});
registry.setGateway(gateway);

// --- WS layer: fake text transports exchanging real frames ------------------
// Every frame a connection sends is recorded in arrival order; a row splits
// the stream at its own response frame (before / after) so the
// notification-vs-response interleaving is pinned.
type FakeWs = {
  incoming: any[];
  wakeups: (() => void)[];
  onMessageHandler?: (message: string) => void;
  onMessage(handler: (message: string) => void): void;
  onClose(_handler: () => void): void;
  sendText(message: string): void;
  close(): void;
};
function fakeWs(): FakeWs {
  const ws: FakeWs = {
    incoming: [],
    wakeups: [],
    onMessage(handler) {
      ws.onMessageHandler = handler;
    },
    onClose() {},
    sendText(message: string) {
      ws.incoming.push(JSON.parse(message));
      for (const wake of ws.wakeups.splice(0)) wake();
    },
    close() {},
  };
  return ws;
}
async function awaitFrame(ws: FakeWs, pred: (frame: any) => boolean): Promise<any> {
  for (;;) {
    const found = ws.incoming.find(pred);
    if (found) return found;
    await new Promise<void>((wake) => ws.wakeups.push(wake));
  }
}
async function connect(connectionGateway: any): Promise<FakeWs> {
  const ws = fakeWs();
  const connection = new GatewayWsConnection(ws as any, { gateway: connectionGateway, token: 'probe-token', serverVersion: 'probe' });
  connections.push(connection);
  ws.onMessageHandler!(JSON.stringify({
    type: 'hello', protocolVersion: PILOTDECK_GATEWAY_PROTOCOL_VERSION,
    clientName: 'probe', clientVersion: '0', token: 'probe-token',
  }));
  await awaitFrame(ws, (frame) => frame.type === 'hello_ok');
  return ws;
}
const primary = await connect(gateway as any);

let next = 0;
function send(ws: FakeWs, method: string, params: unknown): string {
  const id = `r${next++}`;
  const frame: Record<string, unknown> = { type: 'request', id, method };
  if (params !== undefined) frame.params = params;
  ws.onMessageHandler!(JSON.stringify(frame));
  return id;
}
async function request(ws: FakeWs, method: string, params: unknown): Promise<any> {
  const id = send(ws, method, params);
  return awaitFrame(ws, (frame) => frame.type === 'response' && frame.id === id);
}
// The drain protocol: one macrotask flush lets the registry settlement
// (scheduled during the store reload, before the response send) run its
// .then chain (markAllDirty + broadcast) so the post-response buffer is
// complete; nothing after it can still be pending.
const drain = () => new Promise<void>((resolve) => setImmediate(resolve));

// --- normalization ------------------------------------------------------------
const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/;
function normalize(value: any): any {
  if (typeof value === 'string') {
    if (ISO.test(value)) return '<ts>';
    if (value.startsWith(output)) return `<root>${value.slice(output.length)}`;
    return value;
  }
  if (Array.isArray(value)) return value.map(normalize);
  if (value && typeof value === 'object') {
    const out: Record<string, unknown> = {};
    for (const [key, entry] of Object.entries(value)) out[key] = normalize(entry);
    return out;
  }
  return value;
}
// Every recorded frame passes through normalize: the resolve snapshot embeds
// loadedAt and sources[].path, and any error message may embed a scratch path.
const responseFrame = (frame: any) => ({ ok: frame.ok, ...(frame.ok ? { result: normalize(frame.result) } : { error: normalize(frame.error) }) });
const notificationFrame = (frame: any) => ({ type: frame.type, name: frame.name, payload: normalize(frame.payload) });
const splitFrames = (ws: FakeWs, response: any) => {
  const at = ws.incoming.indexOf(response);
  assert.ok(at >= 0, '#1320 the response frame must be in the connection buffer');
  return {
    before: ws.incoming.slice(0, at).map(notificationFrame),
    after: ws.incoming.slice(at + 1).map(notificationFrame),
  };
};

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
    case 'write-yaml': {
      const tree = trees.get(parts[1])!;
      const variant = parts[2] === 'other' ? OTHER_MODEL : parts[2] === 'runpolicy' ? RUN_POLICY : BASE;
      await writeFile(tree.storePath, variant);
      break;
    }
    case 'break-store-yaml': {
      const tree = trees.get(arg)!;
      await writeFile(tree.storePath, BAD_SCHEMA);
      break;
    }
    case 'deny-org':
      local.organizationPolicy = { settings: { canUpdateLocalSettings: false } };
      break;
    case 'allow-org':
      local.organizationPolicy = undefined;
      break;
    default:
      assert.fail(`unknown setup action ${action}`);
  }
}

const rows: any[] = [];
async function run(id: string, method: string, params: unknown, setup: string[] = [], into: any[] = rows) {
  for (const step of setup) await runSetup(step);
  assert.ok(local.configStore, `row ${id} must run against a scenario tree`);
  events.length = 0;
  primary.incoming.length = 0;
  const response = await request(primary, method, params);
  await drain();
  const { before, after } = splitFrames(primary, response);
  const reRead = await request(primary, 'resolve_settings', undefined);
  into.push({
    id, method, ...(params === undefined ? {} : { params }), setup,
    events: [...events], before, response: responseFrame(response), after,
    reRead: normalize(reRead.result), post: postState(),
  });
}

// --- resolve_settings ---------------------------------------------------------
await run('resolve-baseline', 'resolve_settings', undefined, ['tree:r0']);
await run('resolve-null-params', 'resolve_settings', null, ['tree:r1']);
await run('resolve-scalar-params', 'resolve_settings', 5, ['tree:r2']);

// --- apply_flag_settings ------------------------------------------------------
await run('apply-effort-low', 'apply_flag_settings', { sessionKey: 'a', settings: { effortLevel: 'low' } },
  ['tree:a1', 'ensure-session:a']);
await run('apply-busy', 'apply_flag_settings', { sessionKey: 'b', settings: { effortLevel: 'low' } },
  ['tree:a2', 'ensure-session:b', 'begin-turn:b:run-1']);
await run('apply-invalid-effort', 'apply_flag_settings', { sessionKey: 'a', settings: { effortLevel: 'turbo' } },
  ['tree:a3']);

// --- update_settings ----------------------------------------------------------
await run('update-merges-and-notifies', 'update_settings',
  { source: 'localSettings', settings: { agent: { maxContextTokens: 100000 } } }, ['tree:u1']);
await run('update-org-denied', 'update_settings',
  { source: 'localSettings', settings: { agent: { maxContextTokens: 100000 } } }, ['tree:u2', 'deny-org']);
await run('update-noop', 'update_settings',
  { source: 'localSettings', settings: { agent: {} } }, ['allow-org']);

// --- reload_output_styles -----------------------------------------------------
await run('styles-closes-matching', 'reload_output_styles', { projectKey: '/proj-A' },
  ['tree:s1', 'styles-session:sx:/proj-A', 'styles-config:sx']);
await run('styles-null-params', 'reload_output_styles', null, ['tree:s2']);
// #1397: a scalar params object reads `input.projectKey` as undefined, so
// the registry resolves the all-runtimes runtime and the reload succeeds.
await run('styles-scalar-params', 'reload_output_styles', 5, ['tree:s3']);

// --- reload_config ------------------------------------------------------------
await run('reload-config-unchanged', 'reload_config', undefined, ['tree:rc1']);
await run('reload-config-with-change', 'reload_config', undefined, ['tree:rc2', 'write-yaml:rc2:other']);
await run('reload-config-runpolicy-only', 'reload_config', undefined, ['tree:rc3', 'write-yaml:rc3:runpolicy']);
await run('reload-config-error', 'reload_config', undefined, ['tree:rc4', 'break-store-yaml:rc4']);

// --- reload_extensions --------------------------------------------------------
await run('extensions-manual', 'reload_extensions', undefined, ['tree:x1']);
await run('extensions-project', 'reload_extensions', { projectKey: '/p' }, ['tree:x2']);
await run('extensions-changed-paths', 'reload_extensions', { changedPaths: ['ext/a.ts', 'ext/b.ts'] }, ['tree:x3']);
// #1397: `input?.projectKey` short-circuits on nullish and on a primitive
// params alike (its property read yields undefined): both take the
// all-runtimes branch and succeed.
await run('extensions-null-params', 'reload_extensions', null, ['tree:x4']);
await run('extensions-scalar-params', 'reload_extensions', 5, ['tree:x5']);

// --- probes: in-flight merge, reconnect, multi-connection broadcast ------------
const probes: Record<string, any> = {};

// Two concurrent reload_config calls share the store's single in-flight
// reload: both responses report the same changedPaths, the version bumps
// once and the settlement chain runs once.
{
  await runSetup('tree:c1');
  await runSetup('write-yaml:c1:other');
  events.length = 0;
  primary.incoming.length = 0;
  const first = send(primary, 'reload_config', undefined);
  const second = send(primary, 'reload_config', undefined);
  const response1 = await awaitFrame(primary, (frame) => frame.type === 'response' && frame.id === first);
  const response2 = await awaitFrame(primary, (frame) => frame.type === 'response' && frame.id === second);
  await drain();
  const { before, after } = splitFrames(primary, response2);
  const reRead = await request(primary, 'resolve_settings', undefined);
  probes.concurrency = {
    id: 'reload-config-concurrent', setup: ['tree:c1', 'write-yaml:c1:other'],
    events: [...events], before,
    responses: [responseFrame(response1), responseFrame(response2)],
    after, reRead: normalize(reRead.result), post: postState(),
  };
}

// While a reload_config with a pending change is in flight (before the
// store's reload microtask runs), a resolve_settings reads the OLD
// generation; after the settlement, a brand-new connection reads the NEW
// generation and the store survives the reconnect (the next reload bumps
// the version monotonically instead of restarting it).
{
  await runSetup('tree:g1');
  await runSetup('write-yaml:g1:other');
  events.length = 0;
  primary.incoming.length = 0;
  const reloadId = send(primary, 'reload_config', undefined);
  const midId = send(primary, 'resolve_settings', undefined);
  const reloadResponse = await awaitFrame(primary, (frame) => frame.type === 'response' && frame.id === reloadId);
  const midResponse = await awaitFrame(primary, (frame) => frame.type === 'response' && frame.id === midId);
  await drain();
  // The wire order on the first connection: the mid-flight read answers
  // first (old generation), then the reload response, then the settled
  // broadcast.
  assert.deepEqual(
    primary.incoming.map((frame) => frame.type === 'response' ? `response:${frame.id}` : `notification:${frame.name}`),
    [`response:${midId}`, `response:${reloadId}`, 'notification:config_changed'],
    'wire order: the mid-flight read answers before the reload response',
  );
  const at = primary.incoming.indexOf(reloadResponse);
  const before = primary.incoming.slice(0, at).filter((frame) => frame.type === 'notification').map(notificationFrame);
  const after = primary.incoming.slice(at + 1).filter((frame) => frame.type === 'notification').map(notificationFrame);
  const second = await connect(gateway as any);
  const secondRead = await request(second, 'resolve_settings', undefined);
  const continuityReload = await request(primary, 'reload_config', undefined);
  const continuityRead = await request(second, 'resolve_settings', undefined);
  probes.reconnect = {
    id: 'reconnect-during-reload', setup: ['tree:g1', 'write-yaml:g1:other'],
    events: [...events], before, after,
    // The wire order on the first connection: the mid-flight read answers
    // first (old generation), then the reload response, then the settled
    // broadcast, then the post-reconnect continuity reload.
    wire: primary.incoming.map((frame) => frame.type === 'response' ? `response:${frame.id}` : `notification:${frame.name}`),
    midFlightRead: { version: midResponse.result.version, contentHash: midResponse.result.contentHash },
    reloadResponse: responseFrame(reloadResponse),
    secondConnectionRead: { version: secondRead.result.version, contentHash: secondRead.result.contentHash },
    continuityReload: responseFrame(continuityReload),
    continuityRead: { version: continuityRead.result.version, contentHash: continuityRead.result.contentHash },
    post: postState(),
  };
}

// The broadcast reaches every live connection (the GatewayServer mirror
// iterates the connection set), not only the one that made the call.
{
  await runSetup('tree:x6');
  const second = await connect(gateway as any);
  events.length = 0;
  primary.incoming.length = 0;
  second.incoming.length = 0;
  const response = await request(primary, 'reload_extensions', undefined);
  await drain();
  const { before, after } = splitFrames(primary, response);
  const reRead = await request(primary, 'resolve_settings', undefined);
  probes.secondConnection = {
    id: 'extensions-broadcast-reaches-every-connection', setup: ['tree:x6'],
    events: [...events], before, response: responseFrame(response), after,
    secondBefore: second.incoming.map(notificationFrame),
    reRead: normalize(reRead.result), post: postState(),
  };
}

// --- Phase B: the #1320 native matrix assembly --------------------------------
// Only the three delegates the isolated native process owns; the other
// three methods answer the TS unconfigured shapes through the WS dispatch
// (CAPABILITY_UNAVAILABLE), which the native route must reproduce cell by
// cell through the component's own option handling.
const matrixGateway = new InProcessGateway(router, {
  interactionCoordinator: coordinator as any,
  resolveSettings: resolveSettingsDelegate,
  reloadConfig: () => bundle.reloadConfig(),
  reloadExtensions: reloadExtensionsDelegate,
});
registry.setGateway(matrixGateway);
const matrix = await connect(matrixGateway as any);
const matrixRows: any[] = [];
async function runMatrix(id: string, method: string, params: unknown, setup: string[]) {
  for (const step of setup) await runSetup(step);
  assert.ok(local.configStore, `matrix row ${id} must run against a scenario tree`);
  events.length = 0;
  matrix.incoming.length = 0;
  const response = await request(matrix, method, params);
  await drain();
  const { before, after } = splitFrames(matrix, response);
  const reRead = await request(matrix, 'resolve_settings', undefined);
  matrixRows.push({
    id, method, ...(params === undefined ? {} : { params }), setup,
    events: [...events], before, response: responseFrame(response), after,
    reRead: normalize(reRead.result), post: postState(),
  });
}

await runMatrix('matrix-apply-unavailable', 'apply_flag_settings', { sessionKey: 'a', settings: { effortLevel: 'low' } }, ['tree:pb1']);
await runMatrix('matrix-update-unavailable', 'update_settings', { source: 'localSettings', settings: {} }, []);
await runMatrix('matrix-styles-unavailable', 'reload_output_styles', { projectKey: '/proj-A' }, []);
await runMatrix('matrix-resolve', 'resolve_settings', undefined, []);
await runMatrix('matrix-reload-config-unchanged', 'reload_config', undefined, []);
await runMatrix('matrix-reload-extensions-manual', 'reload_extensions', undefined, []);

// --- divergenceCases: registered divergence, recorded but not replayed --------
// specs/gateway-parity/diffs/U7-EXTENSIONS-RELOAD-STRING-COERCION-001: TS
// passes the original non-string values through; the Rust component coerces
// them with String(value). Recorded so the TS truth is pinned as evidence;
// the Rust replay skips this section while the entry stands.
const divergenceCases: any[] = [];
await run('extensions-changed-paths-numbers', 'reload_extensions', { changedPaths: [1, true, null] },
  ['tree:dv1'], divergenceCases);
await run('extensions-project-key-number', 'reload_extensions', { projectKey: 5 },
  ['tree:dv2'], divergenceCases);

// --- coverage guards: pin the matrix expectations the Rust replay asserts -----
const byId = (id: string) => rows.find((row) => row.id === id);
// resolve_settings
assert.equal(byId('resolve-baseline').response.ok, true, 'resolve ok');
assert.equal(byId('resolve-baseline').response.result.version, 1, 'resolve initial version');
assert.equal(byId('resolve-baseline').response.result.config.model.providers.parity.apiKey, '<redacted>', 'resolve redacts');
assert.equal(byId('resolve-baseline').response.result.sources.length, 2, 'resolve sources');
assert.equal(byId('resolve-baseline').before.length, 0, 'resolve emits no notification');
assert.deepEqual(byId('resolve-null-params').response.result.version, 1, 'null params are not read');
assert.deepEqual(byId('resolve-scalar-params').response.result.contentHash, byId('resolve-baseline').response.result.contentHash, 'scalar params are not read');
assert.equal(byId('resolve-baseline').reRead.version, 1, 'a read-only row keeps the generation');
// apply_flag_settings
assert.deepEqual(byId('apply-effort-low').response.result, { applied: ['effortLevel'], cleared: [] }, 'apply result');
assert.deepEqual(byId('apply-effort-low').events, ['dispose:a', 'evict:a:closed'], 'apply closes through the router only');
assert.deepEqual(byId('apply-effort-low').before, [], 'apply emits no notification');
assert.equal(byId('apply-effort-low').post.overrides.a.effort, 'low', 'apply commits the override');
assert.equal(byId('apply-effort-low').post.sessions.a, false, 'apply evicts');
assert.equal(byId('apply-busy').response.error.code, 'SESSION_BUSY', 'busy code');
assert.deepEqual(byId('apply-busy').events, [], 'busy runs no delegate');
assert.equal(byId('apply-invalid-effort').response.error.code, 'INVALID_FLAG_SETTINGS', 'invalid effort code');
// update_settings
assert.equal(byId('update-merges-and-notifies').response.ok, true, 'update ok');
assert.deepEqual(byId('update-merges-and-notifies').response.result,
  { applied: ['agent.maxContextTokens'], cleared: [], changedPaths: ['agent.maxContextTokens'] }, 'update result');
assert.deepEqual(byId('update-merges-and-notifies').before, [], 'update broadcast settles after the response');
assert.deepEqual(byId('update-merges-and-notifies').after, [
  { type: 'notification', name: 'config_changed', payload: { changedPaths: ['agent.maxContextTokens'], changeClasses: ['next-request'] } },
], 'update broadcasts the change');
assert.equal(byId('update-merges-and-notifies').reRead.version, 2, 'update bumps the generation');
// `maxContextTokens` matches SECRET_KEY_PATTERN's /token/i, so the gateway
// projection redacts it on the way out; the raw write is proven by the tree
// YAML below, the same-store read by the version bump above.
assert.equal(byId('update-merges-and-notifies').reRead.config.agent.maxContextTokens, '<redacted>', 'update reads back redacted');
assert.ok(String(byId('update-merges-and-notifies').post.trees.u1.writerYaml).includes('maxContextTokens: 100000'),
  'update writes the raw value through the local writer');
assert.equal(byId('update-org-denied').response.error.code, 'GATEWAY_ORGANIZATION_SETTINGS_UPDATE_DENIED', 'org denied code');
assert.deepEqual(byId('update-org-denied').after, [], 'a denied update broadcasts nothing');
assert.deepEqual(byId('update-noop').response.result,
  { applied: [], cleared: [], changedPaths: [] }, 'noop result');
assert.deepEqual(byId('update-noop').events, [], 'noop reloads nothing');
// reload_output_styles
assert.deepEqual(byId('styles-closes-matching').response.result, { reloaded: true, changed: ['style-a'] }, 'styles result');
assert.ok(byId('styles-closes-matching').events.includes('close-session:sx:sdk_output_styles_reloaded'), 'styles closes matching');
assert.equal(byId('styles-closes-matching').post.sessions.sx, false, 'styles evicts matching');
assert.deepEqual(byId('styles-closes-matching').before, [], 'styles emits no config_changed');
assert.equal(byId('styles-null-params').response.error.code, 'gateway_request_failed', 'styles null params envelope');
assert.equal(byId('styles-null-params').response.error.message,
  "Cannot read properties of null (reading 'projectKey')", 'styles null params message');
assert.deepEqual(byId('styles-scalar-params').response.result, { reloaded: true, changed: ['style-a'] }, 'styles scalar params succeed');
assert.ok(byId('styles-scalar-params').events.includes('resolve:null'), 'styles scalar params resolve the all-runtimes runtime');
// reload_config
assert.deepEqual(byId('reload-config-unchanged').response.result, { reloaded: true, changedPaths: [] }, 'reload unchanged result');
assert.deepEqual(byId('reload-config-unchanged').before, [], 'unchanged broadcasts nothing');
assert.deepEqual(byId('reload-config-unchanged').after, [], 'unchanged broadcasts nothing after settle either');
assert.equal(byId('reload-config-unchanged').reRead.version, 2, 'an unchanged reload still bumps the generation');
assert.deepEqual(byId('reload-config-with-change').response.result,
  { reloaded: true, changedPaths: ['agent.model.id', 'agent.model.model'] }, 'reload change result');
assert.deepEqual(byId('reload-config-with-change').before, [], 'the registry-reload broadcast settles after the response');
assert.deepEqual(byId('reload-config-with-change').after, [
  { type: 'notification', name: 'config_changed', payload: { changedPaths: ['agent.model.id', 'agent.model.model'], changeClasses: ['next-request'] } },
], 'the settled chain broadcasts once');
assert.deepEqual(byId('reload-config-with-change').events, [
  'store-event:agent.model.id,agent.model.model',
  'log:[pilotdeck] Config reloaded, refreshing runtimes: agent.model.id, agent.model.model',
  'sdk-config-change:agent.model.id,agent.model.model|next-request',
  'registry-reload',
  'mark-all-dirty:config_changed',
  'broadcast:config_changed:{"changedPaths":["agent.model.id","agent.model.model"],"changeClasses":["next-request"]}',
], 'the log → sdk → registry.reload → settle → markAllDirty → broadcast order');
assert.equal(byId('reload-config-with-change').reRead.version, 2, 'the reload bumps the generation');
assert.equal(byId('reload-config-with-change').reRead.config.agent.model.id, 'parity/other', 'the reload reads back the new config');
// The runPolicy section is absent from BASE, so the diff reports the new
// section path itself, not the leaf.
assert.deepEqual(byId('reload-config-runpolicy-only').response.result,
  { reloaded: true, changedPaths: ['runPolicy'] }, 'runPolicy result');
assert.deepEqual(byId('reload-config-runpolicy-only').before, [
  { type: 'notification', name: 'config_changed', payload: { changedPaths: ['runPolicy'], changeClasses: ['next-request'] } },
], 'a runPolicy-only change broadcasts synchronously, before the response');
assert.deepEqual(byId('reload-config-runpolicy-only').after, [], 'no settlement follows the runPolicy branch');
assert.deepEqual(byId('reload-config-runpolicy-only').events, [
  'store-event:runPolicy',
  'log:[pilotdeck] Config reloaded (runPolicy applies next turn): runPolicy',
  'sdk-config-change:runPolicy|next-request',
  'broadcast:config_changed:{"changedPaths":["runPolicy"],"changeClasses":["next-request"]}',
], 'the runPolicy branch runs no registry reload');
assert.equal(byId('reload-config-error').response.error.code, 'CONFIG_SCHEMA_VERSION_UNSUPPORTED', 'reload error code');
assert.equal(byId('reload-config-error').response.error.message, 'Unsupported schemaVersion 2.', 'reload error message');
assert.deepEqual(byId('reload-config-error').events, [], 'a failed reload publishes no event');
assert.equal(byId('reload-config-error').reRead.version, 1, 'a failed reload keeps the generation');
// reload_extensions
assert.deepEqual(byId('extensions-manual').response.result, { reloaded: true, changedPaths: [] }, 'extensions manual result');
assert.deepEqual(byId('extensions-manual').before, [
  { type: 'notification', name: 'config_changed', payload: { changedPaths: [], changeClasses: ['extension-changed'] } },
], 'extensions broadcasts synchronously, before the response');
assert.deepEqual(byId('extensions-manual').after, [], 'nothing follows the extensions broadcast');
assert.deepEqual(byId('extensions-manual').events, [
  'log:[pilotdeck] Extensions reload requested for all runtimes: (manual)',
  'invalidate:null',
  'mark-all-dirty:extension_changed',
  'broadcast:config_changed:{"changedPaths":[],"changeClasses":["extension-changed"]}',
], 'extensions manual order');
assert.ok(byId('extensions-project').events.includes('invalidate:/p'), 'extensions project invalidates');
assert.ok(byId('extensions-project').events.includes('mark-project-dirty:/p:extension_changed'), 'extensions project dirties');
assert.deepEqual(byId('extensions-changed-paths').response.result,
  { reloaded: true, changedPaths: ['ext/a.ts', 'ext/b.ts'] }, 'extensions changed paths result');
assert.deepEqual(byId('extensions-changed-paths').before, [
  { type: 'notification', name: 'config_changed', payload: { changedPaths: ['ext/a.ts', 'ext/b.ts'], changeClasses: ['extension-changed'] } },
], 'extensions broadcasts the changed paths');
// #1397
assert.deepEqual(byId('extensions-null-params').response.result, { reloaded: true, changedPaths: [] }, '#1397 extensions null params result');
assert.ok(byId('extensions-null-params').events.includes('invalidate:null'), '#1397 extensions null params take the all-runtimes branch');
assert.deepEqual(byId('extensions-scalar-params').response.result, { reloaded: true, changedPaths: [] }, '#1397 extensions scalar params result');
assert.ok(byId('extensions-scalar-params').events.includes('invalidate:null'), '#1397 extensions scalar params take the all-runtimes branch');
assert.equal(rows.length, 21, 'phase A row count');
// probes
assert.equal(probes.concurrency.responses[0].result.reloaded, true, 'concurrent first ok');
assert.deepEqual(probes.concurrency.responses[0].result, probes.concurrency.responses[1].result, 'concurrent calls share the in-flight reload');
assert.equal(probes.concurrency.events.filter((event: string) => event === 'registry-reload').length, 1, 'one settlement for both calls');
assert.equal(probes.concurrency.reRead.version, 2, 'the shared reload bumps the generation once');
assert.deepEqual(probes.concurrency.after, [
  { type: 'notification', name: 'config_changed', payload: { changedPaths: ['agent.model.id', 'agent.model.model'], changeClasses: ['next-request'] } },
], 'the shared settlement broadcasts once');
assert.equal(probes.reconnect.midFlightRead.version, 1, 'the mid-flight read sees the old generation');
assert.deepEqual(probes.reconnect.reloadResponse.result.changedPaths, ['agent.model.id', 'agent.model.model'], 'the in-flight reload reports its change');
assert.equal(probes.reconnect.secondConnectionRead.version, 2, 'a reconnected client reads the new generation');
assert.deepEqual(probes.reconnect.continuityReload.result, { reloaded: true, changedPaths: [] }, 'the post-reconnect reload works');
assert.equal(probes.reconnect.continuityRead.version, 3, 'the store survives the reconnect (monotonic version)');
assert.equal(probes.reconnect.post.trees.g1.version, 3, 'the tree version confirms no new store');
assert.deepEqual(probes.secondConnection.before, probes.secondConnection.secondBefore, 'the broadcast reaches every connection');
// matrix
const matrixById = (id: string) => matrixRows.find((row) => row.id === id);
assert.equal(matrixById('matrix-apply-unavailable').response.ok, false, 'matrix apply rejects');
assert.equal(matrixById('matrix-apply-unavailable').response.error.code, 'CAPABILITY_UNAVAILABLE', 'matrix apply code');
assert.equal(matrixById('matrix-apply-unavailable').response.error.message, 'apply_flag_settings is unavailable.', 'matrix apply message');
assert.equal(matrixById('matrix-update-unavailable').response.error.code, 'CAPABILITY_UNAVAILABLE', 'matrix update code');
assert.equal(matrixById('matrix-update-unavailable').response.error.message, 'update_settings is unavailable.', 'matrix update message');
assert.equal(matrixById('matrix-styles-unavailable').response.error.code, 'CAPABILITY_UNAVAILABLE', 'matrix styles code');
assert.equal(matrixById('matrix-styles-unavailable').response.error.message, 'reload_output_styles is unavailable.', 'matrix styles message');
assert.deepEqual(matrixById('matrix-apply-unavailable').events, [], 'an unconfigured method runs no delegate');
assert.equal(matrixById('matrix-resolve').response.ok, true, 'matrix resolve ok');
assert.equal(matrixById('matrix-resolve').reRead.version, 1, 'matrix resolve generation');
assert.deepEqual(matrixById('matrix-reload-config-unchanged').response.result, { reloaded: true, changedPaths: [] }, 'matrix reload config ok');
assert.deepEqual(matrixById('matrix-reload-extensions-manual').response.result, { reloaded: true, changedPaths: [] }, 'matrix extensions ok');
assert.deepEqual(matrixById('matrix-reload-extensions-manual').before, [
  { type: 'notification', name: 'config_changed', payload: { changedPaths: [], changeClasses: ['extension-changed'] } },
], 'matrix extensions broadcasts before the response');
assert.equal(matrixRows.length, 6, 'matrix row count');
// divergence cases (registered divergence; recorded, not replayed)
assert.deepEqual(divergenceCases[0].response.result.changedPaths, [1, true, null], 'TS passes the original values through');
assert.deepEqual(divergenceCases[0].before, [
  { type: 'notification', name: 'config_changed', payload: { changedPaths: [1, true, null], changeClasses: ['extension-changed'] } },
], 'the broadcast payload carries the original values');
assert.ok(divergenceCases[1].events.includes('mark-project-dirty:5:extension_changed'), 'the project branch runs on the original key');
assert.equal(divergenceCases[1].events.includes('registry-reload'), false, 'the project branch runs no registry reload');

await writeFile(
  join(output, 'oracle.json'),
  JSON.stringify({
    sourceCommit, sourceSha256, node: process.version,
    setupKeys: KEYS, cases: rows,
    probes, matrix: matrixRows, divergenceCases,
  }, null, 2) + '\n',
  { flag: 'wx' },
);
console.log(
  `PASS: ${rows.length} real TS native settings cases + ${matrixRows.length} matrix cases + ` +
  `${Object.keys(probes).length} probes + ${divergenceCases.length} divergence cases`,
);
