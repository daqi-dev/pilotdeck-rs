// Shared real-TS settings harness for the U7 goldens (#1748).
//
// `native-settings/generate.mts` (#1320) and `settings-gateway/generate.mts`
// (#1318) each carried a copy of the same machinery: the real SessionRouter
// and AgentHandle sessions, the real ProjectRuntimeRegistry prototype, the
// real GatewayRuntimeRefreshBundle wiring with its configStore delegate and
// per-scenario tree rebinding, the fake WS transport's request/response
// plumbing, the setup vocabulary and the post-state snapshot. The two copies
// had already drifted, so a baseline or bundle-contract change had to be made
// twice or the golden silently stopped being the TS truth.
//
// This module is the single source for that shared half. The fixtures still
// own what is genuinely theirs: the source lists they pin, the gateway
// assembly under test, the recorded row shapes, and their extra setup verbs.
// The extraction is a pure move — the generated oracles stay byte-identical
// (#1748 evidence: both generators regenerated from the fixed baseline before
// and after the move produce the same oracle.json bytes).
import { mkdir, writeFile } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

export const BASE = `schemaVersion: 1
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
export const OTHER_MODEL = BASE.replace('agent: {model: parity/deterministic}', 'agent: {model: parity/other}');
// A runPolicy-only change: isRunPolicyOnlyChange routes it through the
// synchronous broadcast branch (no registry reload).
export const RUN_POLICY = `${BASE}runPolicy:
  failureGuard:
    enabled: false
`;
export const BAD_SCHEMA = BASE.replace('schemaVersion: 1', 'schemaVersion: 2');
// A YAML document the real loader rejects while the writer still accepts it.
export const BROKEN_YAML = 'agent: {model: parity/deterministic\nmodel: [unclosed';

export type Tree = { name: string; store: any; writerPath: string; storePath: string };
export type Local = {
  env: Record<string, string | undefined>;
  projectRoot: string;
  configStore: any;
  organizationPolicy: any;
};

export type SettingsHarnessOptions = {
  output: string;
  events: string[];
  keys: string[];
  SessionRouter: any;
  asAgentHandle: any;
  ProjectRuntimeRegistry: any;
  GatewayInteractionCoordinator: any;
  GatewayRuntimeRefreshBundle: any;
  createPilotConfigStoreSync: any;
  // `settle`: the registry reload resolves on a setImmediate macrotask (the
  // #1320 golden pins the full settlement chain). `never`: the promise never
  // settles (the #1318 golden pins only the synchronous prefix).
  registryReload: 'settle' | 'never';
  // Session keys whose `dispose()` rejects (the #1318 close-fault cell).
  rejectDisposeKeys?: string[];
  // Attach the bundle on the first scenario tree (#1320) or leave it to the
  // fixture's `attach-bundle` setup verb (#1318).
  autoAttachBundle?: boolean;
  // Extra side effect for every broadcast, run before the event is recorded
  // (the #1320 golden delivers the frame to its live connections).
  broadcast?: (name: string, payload: unknown) => void;
  // The bundle's optional `warn` collaborator; omitted when not supplied so
  // the #1318 golden keeps its collaborator set unchanged.
  warn?: (message: string, error?: unknown) => void;
  // Pass a copy of the tree env to createPilotConfigStoreSync (#1318 did;
  // #1320 passed the binding itself).
  copyTreeEnv?: boolean;
};

export function createSettingsHarness(options: SettingsHarnessOptions) {
  const {
    output,
    events,
    keys,
    SessionRouter,
    asAgentHandle,
    ProjectRuntimeRegistry,
    GatewayInteractionCoordinator,
    GatewayRuntimeRefreshBundle,
    createPilotConfigStoreSync,
    registryReload,
    rejectDisposeKeys = [],
    autoAttachBundle = false,
    broadcast = () => {},
    warn,
    copyTreeEnv = false,
  } = options;

  // The interaction coordinator is REPLACED on the prototype chain (the #1215
  // pattern): only the reason strings the methods pass are pinned here; the
  // coordinator layer itself is outside this component's parity scope.
  const realCoordinator = new GatewayInteractionCoordinator({});
  const coordinator = Object.create(realCoordinator);
  coordinator.closeSession = (sessionKey: string, reason: string) => {
    events.push(`close-session:${sessionKey}:${reason}`);
  };

  // Real AgentHandle sessions over the real router. A key in
  // `rejectDisposeKeys` disposes with a real rejection.
  const session = (key: string) => ({
    sessionId: `sess-${key}`,
    snapshot: () => ({ sessionId: `sess-${key}`, messages: [] }),
    pendingTurns: () => [],
    pendingTurnCount: 0,
    discardQueuedTurns: async () => {},
    abort: () => {},
    dispose: async () => {
      events.push(`dispose:${key}`);
      if (rejectDisposeKeys.includes(key)) throw new Error('fixture dispose failure');
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
  const trees = new Map<string, Tree>();
  const local: Local = { env: {}, projectRoot: '', configStore: undefined, organizationPolicy: undefined };

  // --- the refresh bundle ----------------------------------------------------
  // The bundle's configStore delegate re-points every live subscription at the
  // current tree's store, so the bundle follows the scenario trees the way
  // production's single store feeds it.
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
        return registryReload === 'settle'
          ? new Promise<void>((settle) => setImmediate(settle))
          : new Promise(() => {});
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
    ...(warn ? { warn } : {}),
  });
  bundle.bindServer({
    broadcastNotification(name: string, payload: unknown) {
      broadcast(name, payload);
      events.push(`broadcast:${name}:${JSON.stringify(payload)}`);
    },
  });

  let bundleAttached = false;
  async function makeTree(name: string, configPathOverride?: string) {
    const root = join(output, 'scratch', name);
    const home = join(root, 'home');
    const project = join(root, 'project');
    await mkdir(home, { recursive: true });
    await mkdir(project, { recursive: true });
    const writerPath = join(home, 'pilotdeck.yaml');
    await writeFile(writerPath, BASE);
    const env: Record<string, string | undefined> = { PILOT_HOME: home, HOME: home };
    let storeEnv: Record<string, string | undefined> = copyTreeEnv ? { ...env } : env;
    if (configPathOverride) storeEnv = { ...env, PILOTDECK_CONFIG_PATH: configPathOverride };
    const store = createPilotConfigStoreSync({ projectRoot: project, env: storeEnv });
    store.subscribe((event: any) => events.push(`store-event:${event.changedPaths.join(',')}`));
    const tree: Tree = { name, store, writerPath, storePath: configPathOverride ?? writerPath };
    trees.set(name, tree);
    local.env = env;
    local.projectRoot = project;
    local.configStore = store;
    local.organizationPolicy = undefined;
    // The probe listener subscribes before the bundle rebinds, so the probe
    // event always precedes the bundle's onConfigChange prefix.
    rebindBundleStore();
    if (autoAttachBundle && !bundleAttached) {
      bundle.attach();
      bundleAttached = true;
    }
    return tree;
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
    post.turns = keys.filter((key) => router.hasActiveTurn(key));
    for (const key of keys) {
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
        if (!keys.includes(arg)) throw new Error(`setup key ${arg} must be a scenario key`);
        await router.getOrCreate({ sessionKey: arg, projectKey: '/p' } as any);
        break;
      case 'begin-turn':
        if (!keys.includes(parts[1])) throw new Error(`setup key ${parts[1]} must be a scenario key`);
        router.beginTurn(parts[1], parts[2]);
        break;
      case 'styles-session':
        if (!keys.includes(parts[1])) throw new Error(`setup key ${parts[1]} must be a scenario key`);
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
        const variant = parts[2] === 'other' ? OTHER_MODEL : parts[2] === 'runpolicy' ? RUN_POLICY : BASE;
        await writeFile(tree.storePath, variant);
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
        throw new Error(`unknown setup action ${action}`);
    }
  }

  return {
    KEYS: keys,
    coordinator,
    session,
    router,
    registry,
    trees,
    local,
    bundle,
    makeTree,
    postState,
    runSetup,
    BASE,
    OTHER_MODEL,
    RUN_POLICY,
    BAD_SCHEMA,
  };
}
