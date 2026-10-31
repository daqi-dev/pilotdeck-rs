import assert from 'node:assert/strict';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {syncBuiltinESMExports} from 'node:module';
import {assertBaseline, baselineSourceHashes} from '../../_shared/baseline.mts';

const base = path.resolve(process.env.PDRS_BASELINE!);
const output = path.resolve(process.argv[2]);
const deadline = setTimeout(() => {
  process.stderr.write('#1011 generator exceeded 120 seconds\n'); process.exit(1);
}, 120000);
deadline.unref();
assert.equal(process.version, 'v22.22.0', '#1011 Node version');
const sourceCommit = assertBaseline(base);
const sourceSha256 = await baselineSourceHashes(base, [
  'src/cli/ProjectSessionFactory.ts', 'src/gateway/SessionRouter.ts',
  'src/session/storage/ProjectSessionStorage.ts', 'src/session/events/SessionRuntime.ts',
  'src/session/projection/SessionProjectionDriver.ts',
  'src/session/projection/checkpoint/SessionProjectionCheckpointBinding.ts',
  'src/session/projection/checkpoint/JsonFileSessionProjectionCheckpointStore.ts',
]);
fs.mkdirSync(output, {recursive: true});
fs.writeFileSync(path.join(output, '.generation-owner'), '#1011\n', {flag: 'wx'});
const work = fs.mkdtempSync(path.join(output, 'work-'));
// Delay the OS write only after native JsonlSessionPersistence has registered
// its writeTail. Delaying the provider call itself would invent a restore race
// in which load cannot see a write that the real provider already owns.
const appendHooks = new Map<string, () => Promise<void>>();
const appendFile = fsp.appendFile;
fsp.appendFile = async (file: any, data: any, options: any) => {
  const hook = appendHooks.get(String(file));
  if (hook) await hook();
  return appendFile(file, data, options);
};
syncBuiltinESMExports();
const {createAgentProjectSessionStorage} = await import(`${base}/src/session/storage/ProjectSessionStorage.ts`);
const {ProjectSessionFactory} = await import(`${base}/src/cli/ProjectSessionFactory.ts`);
const {SessionRouter} = await import(`${base}/src/gateway/SessionRouter.ts`);
const {ToolRegistry} = await import(`${base}/src/tool/registry/ToolRegistry.ts`);
const {SubagentProviderRegistry} = await import(`${base}/src/agent/sub/SubagentProviderRegistry.ts`);
const {SessionProjectionCheckpointBinding} = await import(`${base}/src/session/projection/checkpoint/SessionProjectionCheckpointBinding.ts`);

const clone = (value: any) => structuredClone(value);
const error = (e: any) => ({name: e.name, message: e.message});
const sessionId = 'session-1011';
const now = () => new Date('2026-10-31T00:00:00.000Z');
const draft = (title: string) => ({type: 'session_metadata', metadata: {title, titleSource: 'user'}});
function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>(r => { resolve = r; });
  return {promise, resolve};
}
async function bounded<T>(p: Promise<T>): Promise<T> {
  let timer: ReturnType<typeof setTimeout>;
  try {
    return await Promise.race([p, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(Error('#1011 gate exceeded 10 seconds')), 10000);
    })]);
  } finally { clearTimeout(timer!); }
}
function watch(p: Promise<any>) {
  const state: any = {status: 'pending'};
  const done = p.then(() => { state.status = 'fulfilled'; }, e => {
    state.status = 'rejected'; state.error = error(e);
  });
  return {state, done};
}
function fileBytes(file: string) {
  if (!fs.existsSync(file)) return null;
  const bytes = fs.readFileSync(file);
  return {utf8: bytes.toString('utf8'), sha256: createHash('sha256').update(bytes).digest('hex')};
}

// All filesystem writes by the baseline are confined to this case's private
// home/project. Instrumentation delegates to the original methods; snapshots
// never flush, restore or otherwise repair the state being observed.
function fixture(name: string) {
  const root = path.join(work, name), projectRoot = path.join(root, 'project'), pilotHome = path.join(root, 'home');
  fs.mkdirSync(projectRoot, {recursive: true});
  const timeline: any[] = [], storages: any[] = [], gates: any[] = [], witnesses: any[] = [];
  const log = (kind: string, data: any = {}) => {
    timeline.push({ordinal: timeline.length + 1, kind, ...clone(data)});
    for (const w of witnesses) if (w.kind === kind && w.storage === data.storage) w.done.resolve();
  };
  const witness = (kind: string, storage: number) => {
    const done = deferred(); witnesses.push({kind, storage, done}); return done.promise;
  };
  function gate(kind: string, storage: number, failure?: string) {
    const g = {kind, storage, entered: deferred(), release: deferred(), used: false, failure};
    gates.push(g); return g;
  }
  async function hook(kind: string, storage: number) {
    const g = gates.find(g => !g.used && g.kind === kind && g.storage === storage);
    if (!g) return;
    g.used = true; log('gate.enter', {kindAtGate: kind, storage}); g.entered.resolve();
    await g.release.promise; log('gate.leave', {kindAtGate: kind, storage});
    if (g.failure) throw Error(g.failure);
  }
  function makeStorage() {
    const s = createAgentProjectSessionStorage({projectRoot, pilotHome, sessionId, now});
    const id = storages.push(s);
    if (id === 1) appendHooks.set(s.transcriptPath, async () => {
      log('disk.append', {storage: id}); await hook('disk.append', id);
    });
    let uuid = 0;
    (s.events as any).uuid = () => `storage-${id}-entry-${++uuid}`;
    for (const [object, method, kind] of [
      [s.persistence, 'load', 'log.load'], [s.persistence, 'append', 'log.append'],
      [s.persistence, 'flush', 'log.flush'], [s.projectionCheckpointStore, 'load', 'cache.load'],
      [s.projectionCheckpointStore, 'save', 'cache.save'], [s, 'dispose', 'storage.dispose'],
      [s.events, 'read', 'event.read'], [s.events, 'flush', 'event.flush'],
    ] as const) {
      const original = (object as any)[method].bind(object);
      (object as any)[method] = async (...args: any[]) => {
        log(`${kind}.begin`, {storage: id, args});
        // Preserve the native synchronous append -> writeTail registration.
        let result: any;
        if (kind === 'log.append') result = await original(...args);
        else { await hook(kind, id); result = await original(...args); }
        log(`${kind}.end`, {storage: id, result: result ?? null});
        return result;
      };
    }
    const hydrate = s.projections.hydrate.bind(s.projections);
    s.projections.hydrate = (entries: any, checkpoint: any = {}) => {
      log('hydrate', {storage: id, entries, checkpoint});
      hydrate(entries, checkpoint);
    };
    return s;
  }
  const storage = makeStorage();
  const runtime: any = {
    projectRoot, projectStorage: {projectRoot, pilotHome},
    snapshot: {config: {agent: {model: {provider: 'probe', model: 'deterministic'}, maxContextTokens: 32000}, model: {providers: {}}}},
    profile: {interaction: {canPrompt: false, permissionProvider: 'deny', elicitationProvider: 'none'}},
    model: {getCapabilities: () => ({maxContextTokens: 32000, maxOutputTokens: 1024}), getMultimodal: () => undefined,
      getProviderProtocol: () => 'openai', complete: async () => { throw Error('#1011 unexpected model request'); }},
    router: {stream: () => { throw Error('#1011 unexpected model stream'); }}, tools: new ToolRegistry(),
    executionWorld: {shell: {execute: () => { throw Error('#1011 unexpected shell'); }}, planStorage: {}},
    instructionStorage: {}, toolResultSpill: {}, tokenAccounting: {estimateMessages: () => 0},
    backgroundSubagents: {start: () => { throw Error('#1011 unexpected subagent'); }},
    createSessionTools: () => new ToolRegistry(),
    createSessionExecution: async () => ({handle: {world: runtime.executionWorld}, release: async () => {}}),
    routerSessionCustomRouters: {register: () => ({release: async () => {}})},
    pluginRuntime: {refresh: async () => {}, acquireSessionContributions: () => ({
      contributions: {generation: 1, tools: [], commands: [], skills: [], prompts: [], mcpInstructions: [], routers: [], mcpServers: {}, hooks: {}},
      release: async () => {},
    })},
    mcpProvider: {acquireSharedRuntime: async () => ({tools: [], release: async () => {}}), getPerSessionServerSpecs: () => []},
  };
  const handles: any[] = [];
  let creates = 0;
  const factory = new ProjectSessionFactory({
    resolveRuntime: () => runtime, acquireRuntimeLease: () => async () => {},
    acquirePermissionRuleSet: () => ({rules: {allow: [], deny: [], ask: []}, release: async () => {}}),
    getSessionOverride: () => undefined, getGateway: () => undefined, getSdkSessionConfig: () => undefined,
    getSdkSessionPlugins: () => [], getSdkOutputStyleContent: () => undefined, getSdkMcpServers: () => ({}),
    getSdkThinking: () => undefined, registerSdkConfigChangeHandler: () => () => {}, registerSessionHookHandler: () => () => {},
    createStorage: () => ++creates === 1 ? storage : makeStorage(), permissionMode: 'default',
    preparePerSessionSpecs: ({specs}: any) => specs, getAlwaysOnToolNames: () => [], pilotHome, env: {}, now,
    shouldCollectFileArtifacts: () => false,
    continuations: {provider: 'probe', providers: new SubagentProviderRegistry(),
      host: {bindParent: ({parent}: any) => { handles.push(parent); return () => {}; }},
      manager: {drainDescendants: async () => {}}} as any,
    testAgentLoopFactory: () => ({snapshotFileState: () => ({}), async *run() { throw Error('#1011 unexpected turn'); }}) as any,
  });
  const context = {sessionKey: sessionId, projectKey: projectRoot, channelKey: 'probe'};
  const router = new SessionRouter({
    createSession: c => factory.createSession(c), recreateSession: (c, old) => factory.recreateSession(c, old),
    idleSweepIntervalMs: 0, idleSessionTimeoutMs: 1e12, now,
  });
  async function seed(activate = true) {
    await storage.events.append(sessionId, 'seed-1', draft('title-1') as any);
    await storage.flush();
    const stale = fs.readFileSync(storage.projectionCheckpointPath, 'utf8');
    await storage.events.append(sessionId, 'seed-2', draft('title-2') as any);
    await storage.flush();
    const fresh = fs.readFileSync(storage.projectionCheckpointPath, 'utf8');
    if (activate) await router.getOrCreate(context);
    else { await storage.dispose(); creates = 1; }
    return {stale, fresh};
  }
  function setCache(state: string, bytes: {stale: string; fresh: string}) {
    if (state === 'missing') fs.unlinkSync(storage.projectionCheckpointPath);
    else fs.writeFileSync(storage.projectionCheckpointPath,
      state === 'corrupt' ? '{broken\n' : state === 'stale' ? bytes.stale : bytes.fresh);
  }
  function snapshot(label: string, operations: any[]) {
    const view = {
      label, operations: operations.map(o => clone(o.state)),
      handles: handles.map(h => ({state: h.state, inFlight: h.inFlight})),
      storages: storages.map(s => {
        let projection: any;
        try { projection = s.projections.snapshot(); } catch (e) { projection = {error: error(e)}; }
        return {writer: s.events.snapshotState(), projection, checkpointActive: s.projectionCheckpointBinding.active,
          persistenceActive: s.persistenceBinding.active};
      }),
      transcript: fileBytes(storage.transcriptPath), cache: fileBytes(storage.projectionCheckpointPath),
    };
    log('snapshot', view); return view;
  }
  async function close() {
    for (const g of gates) g.release.resolve();
    await bounded(router.shutdown()); await bounded(factory.dispose());
    for (const s of storages) await bounded(s.dispose());
    appendHooks.delete(storage.transcriptPath);
  }
  return {storage, storages, router, context, timeline, log, gate, witness, seed, setCache, snapshot, close};
}

const states = ['missing', 'fresh', 'stale', 'corrupt', 'concurrent'];
const events = ['load', 'append', 'recreate', 'checkpoint', 'drain', 'shutdown'];
const cases: any[] = [];
for (const state of states) for (const event of events) {
  const name = `${state}-${event}`, f = fixture(name), s = f.storage;
  const operations: any[] = [], snapshots: any[] = [];
  try {
    const bytes = await bounded(f.seed()); f.setCache(state, bytes); f.timeline.length = 0;
    snapshots.push(f.snapshot('before', operations));
    let g: ReturnType<typeof f.gate>;
    if (state === 'concurrent') {
      g = f.gate('disk.append', 1);
      operations.push(watch(s.events.append(sessionId, 'concurrent-1', draft('concurrent-1') as any)));
      await bounded(g.entered.promise);
      operations.push(watch(s.events.append(sessionId, 'concurrent-2', draft('concurrent-2') as any)));
    } else {
      const kind = {load: 'cache.load', append: 'disk.append', recreate: 'cache.load', checkpoint: 'cache.save', drain: 'log.flush', shutdown: 'cache.save'}[event]!;
      g = f.gate(kind, event === 'recreate' ? 2 : 1);
    }
    const run = () => {
      switch (event) {
        case 'load': return s.restore();
        case 'append': return s.events.append(sessionId, 'append', draft('appended') as any);
        case 'recreate': f.router.markSessionDirty(sessionId); return f.router.getOrCreate(f.context);
        case 'checkpoint': return s.projectionCheckpointBinding.flush();
        case 'drain': return s.flush();
        case 'shutdown': return f.router.shutdown();
        default: throw Error('#1011 unknown event');
      }
    };
    const boundary = {load: 'log.load.begin', recreate: 'storage.dispose.begin', checkpoint: 'event.read.begin',
      drain: 'event.flush.begin', shutdown: 'event.flush.begin'}[event];
    const disposalEntered = state === 'concurrent' && boundary
      ? f.witness(boundary, 1) : undefined;
    operations.push(watch(run()));
    // In concurrent rows wait for a real read boundary for load/recreate;
    // flushing events await the deliberately blocked commit tail instead.
    if (state !== 'concurrent') await bounded(g.entered.promise);
    else if (disposalEntered) await bounded(disposalEntered);
    snapshots.push(f.snapshot('held', operations));
    // Pin the read/second-write race too: load sees the first durable record,
    // restores it, then the second write may finish. Without this gate the
    // raw read could contain either three or four records depending on I/O.
    const secondWrite = state === 'concurrent' && event === 'load' ? f.gate('disk.append', 1) : undefined;
    g.release.resolve();
    if (secondWrite) {
      await bounded(secondWrite.entered.promise);
      await bounded(operations.at(-1).done);
      secondWrite.release.resolve();
    }
    await bounded(Promise.all(operations.map(o => o.done)));
    snapshots.push(f.snapshot('settled', operations));
    for (const o of operations) assert.equal(o.state.status, 'fulfilled', `#1011 ${name} operation`);
    const row = {name, state, event, snapshots, timeline: clone(f.timeline)};
    cases.push(row);
    console.log('OBSERVED', name, JSON.stringify({operations: snapshots[2].operations,
      writer: snapshots[2].storages.map((s: any) => s.writer.sequence),
      cacheWrites: f.timeline.filter(r => r.kind === 'cache.save.end').length}));
  } finally { await f.close(); }
}

const supplemental: any[] = [];
for (const state of states.filter(s => s !== 'concurrent')) {
  const f = fixture(`cold-${state}`);
  try {
    const bytes = await bounded(f.seed(false)); f.setCache(state, bytes); f.timeline.length = 0;
    const before = f.snapshot('before', []), gate = f.gate('cache.load', 2);
    const op = watch(f.router.getOrCreate(f.context));
    await bounded(gate.entered.promise); const held = f.snapshot('held', [op]);
    gate.release.resolve(); await bounded(op.done);
    const settled = f.snapshot('settled', [op]);
    assert.equal(op.state.status, 'fulfilled', `#1011 cold-${state} factory load`);
    supplemental.push({name: `cold-${state}`, before, held, settled, timeline: clone(f.timeline)});
    console.log('OBSERVED', `cold-${state}`, JSON.stringify(settled.operations));
  } finally { await f.close(); }
}
// Corrupt bytes, invalid envelope, codec failure and an anchor mismatch are
// distinct fallbacks. Keep the exact bytes consumed and hydrate arguments.
for (const variant of ['anchor', 'future', 'wrong-session', 'row-watermark', 'codec']) {
  const f = fixture(`invalid-${variant}`), s = f.storage;
  try {
    const {fresh} = await bounded(f.seed()), cache = JSON.parse(fresh);
    if (variant === 'anchor') cache.anchor.entryId = 'unrelated';
    if (variant === 'future') { cache.asOfSequence = 20; cache.anchor.sequence = 20; }
    if (variant === 'wrong-session') cache.sessionId = 'other';
    const key = Object.keys(cache.projections).find(k => k.includes('conversation'))!;
    assert.ok(key, '#1011 conversation checkpoint exists');
    if (variant === 'row-watermark') cache.projections[key].asOfSequence = 20;
    if (variant === 'codec') cache.projections[key].state = null;
    fs.writeFileSync(s.projectionCheckpointPath, JSON.stringify(cache) + '\n');
    f.timeline.length = 0;
    const before = f.snapshot('before', []), op = watch(s.restore());
    await bounded(op.done); const settled = f.snapshot('settled', [op]);
    assert.equal(op.state.status, 'fulfilled', `#1011 ${variant} disposable cache`);
    supplemental.push({name: `invalid-${variant}`, before, settled, timeline: clone(f.timeline)});
  } finally { await f.close(); }
}

for (const variant of ['turn-result', 'save-reject', 'capture-race', 'predecessor-reject', 'predecessor-pending', 'cancel-waiter']) {
  const f = fixture(variant), s = f.storage;
  try {
    await bounded(f.seed()); f.timeline.length = 0;
    const before = f.snapshot('before', []), operations: any[] = [], details: any = {};
    if (variant === 'turn-result') {
      // Binding.dispose waits for the already scheduled write without adding
      // another one, so this observes the automatic turn-result trigger.
      await bounded(s.events.append(sessionId, 'completed-turn', {type: 'turn_result', result: {
        type: 'success', sessionId, turnId: 'completed-turn', usage: {inputTokens: 1, outputTokens: 2},
        permissionDenials: [], turns: 1, stopReason: 'completed', startedAt: now().toISOString(), completedAt: now().toISOString(),
      }} as any));
      await bounded(s.projectionCheckpointBinding.dispose());
    } else if (variant === 'save-reject') {
      s.projectionCheckpointStore.save = async () => { f.log('injected.save.reject'); throw Error('SAVE_1011'); };
      operations.push(watch(s.flush())); await bounded(operations[0].done);
    } else if (variant === 'capture-race') {
      const read = s.events.read.bind(s.events); let attempts = 0;
      s.events.read = async () => {
        const r = await read(); attempts++;
        await s.events.append(sessionId, `race-${attempts}`, draft(`race-${attempts}`) as any);
        return r;
      };
      const diagnostics: any[] = [];
      const binding = new SessionProjectionCheckpointBinding({sessionId, runtime: s.events, projections: s.projections,
        store: s.projectionCheckpointStore, onError: e => diagnostics.push(error(e))});
      operations.push(watch(binding.flush())); await bounded(operations[0].done);
      await bounded(binding.dispose()); s.events.read = read;
      details.attempts = attempts; details.diagnostics = diagnostics;
      assert.equal(attempts, 3, '#1011 capture retry budget');
    } else {
      const gate = f.gate('disk.append', 1, variant === 'predecessor-reject' ? 'APPEND_1011' : undefined);
      operations.push(watch(s.events.append(sessionId, 'first', draft('first') as any)));
      await bounded(gate.entered.promise);
      operations.push(watch(s.events.append(sessionId, 'second', draft('second') as any)));
      operations.push(watch(s.flush()));
      await bounded(s.projectionCheckpointStore.load(sessionId));
      details.held = f.snapshot('predecessor-held', operations);
      if (variant === 'cancel-waiter') {
        const controller = new AbortController();
        const cancelled = watch(Promise.race([operations[0].done, new Promise((_, reject) => {
          controller.signal.addEventListener('abort', () => reject(Error('WAITER_CANCEL_1011')), {once: true});
        })]));
        controller.abort(); await bounded(cancelled.done); details.waiter = cancelled.state;
      }
      // Explicit cleanup is outside the pending observation; never infer that
      // an unreleased predecessor will finish or that cancelling a waiter stops it.
      gate.release.resolve(); await bounded(Promise.all(operations.map(o => o.done)));
    }
    const settled = f.snapshot('settled', operations);
    supplemental.push({name: variant, before, details, settled, timeline: clone(f.timeline)});
    console.log('OBSERVED', variant, JSON.stringify({operations: settled.operations,
      attempts: details.attempts, diagnostics: details.diagnostics, waiter: details.waiter}));
  } finally { await f.close(); }
}

assert.equal(cases.length, 30, '#1011 matrix count');
assert.equal(supplemental.length, 15, '#1011 supplemental count');
// Raw UTF-8 cache/transcript bytes, their hashes and every ordered observation
// live inside the oracle so the existing byte-for-byte guard covers originals.
fs.writeFileSync(path.join(output, 'oracle.json'), JSON.stringify({
  sourceCommit, sourceSha256, node: process.version, states, events,
  timings: ['before', 'held', 'settled'], cases, supplemental,
}, null, 2) + '\n', {flag: 'wx'});
clearTimeout(deadline);
console.log('PASS: #1011 real TS cache matrix: 30 scenarios / 90 snapshots + 15 boundaries');
