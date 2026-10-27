// Executes the unmodified fixed TS ReasoningRetriever over a real
// MemoryRepository (file-backed workspace + global root) and a loopback stub.
import assert from 'node:assert/strict';
import {mkdtempSync, readdirSync, readFileSync, rmSync, statSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, relative} from 'node:path';
import {pathToFileURL} from 'node:url';
import {stub} from '../edgeclaw-llm/stub.mts';

export const REASONING = 'src/context/memory/edgeclaw-memory-core/src/core/retrieval/reasoning-loop.ts';
export const SQLITE = 'src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
export const LLM = 'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts';
export const SERVICE = 'src/context/memory/edgeclaw-memory-core/src/service.ts';

export const CLOCK_MS = 1792972800000; // 2026-10-22T00:00:00.000Z
export const RANDOM = 0.123456789;
export const TOKEN = (RANDOM).toString(36).slice(2, 10);

export function enc(v: any): any {
  if (v === undefined) return {$undefined: true};
  if (typeof v === 'number' && (Object.is(v, -0) || !Number.isFinite(v))) {
    return {$number: Object.is(v, -0) ? '-0' : String(v)};
  }
  if (Array.isArray(v)) return v.map(enc);
  if (v && typeof v === 'object') {
    return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, enc(x)]));
  }
  return v;
}

function chat(text: string) {
  return {status: 200, body: JSON.stringify({choices: [{message: {content: text}}]})};
}

function message(role: string, content: string) {
  return {role, content};
}

function normalized(value: any, origin: string): any {
  if (typeof value === 'string') return value.split(origin).join('$ORIGIN');
  if (Array.isArray(value)) return value.map((item) => normalized(item, origin));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([k, v]) => [k, normalized(v, origin)]));
  }
  return value;
}

function requestOf(request: any) {
  return {method: request.method, url: request.url, headers: request.headers, body: request.body, at: request.at};
}

function treeFiles(root: string): Array<{path: string; content: string}> {
  const out: Array<{path: string; content: string}> = [];
  const walk = (dir: string) => {
    for (const name of readdirSync(dir).sort()) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) walk(path);
      else out.push({path: relative(root, path).replaceAll('\\', '/'), content: readFileSync(path, 'utf8')});
    }
  };
  walk(root);
  return out;
}

type Seed = (repository: any) => {[name: string]: string};

function seedSingle(repository: any) {
  const store = repository.getFileMemoryStore();
  store.upsertProjectMeta({projectName: 'Alpha 项目', description: 'Alpha 项目说明', status: 'active'});
  const project = store.upsertCandidate({
    type: 'project', scope: 'project', name: 'Alpha 计划', description: '计划说明',
    capturedAt: '2026-10-20T00:00:00.000Z', sourceSessionKey: 's-project', summary: '本周完成检索主线。',
  });
  const feedback = store.upsertCandidate({
    type: 'feedback', scope: 'project', name: '交付规则', description: '交付规则说明',
    capturedAt: '2026-10-21T00:00:00.000Z', sourceSessionKey: 's-feedback', rule: '先跑测试再交付。',
  });
  repository.getGlobalUserStore().upsertUserProfile({
    type: 'user', scope: 'global', name: '用户😀', description: '用户画像',
    capturedAt: '2026-10-19T00:00:00.000Z', sourceSessionKey: 's-user',
    profile: '工程师，住北京。', relationships: ['团队负责人'],
  });
  store.repairManifests();
  return {project: project.relativePath, feedback: feedback.relativePath,
    user: 'UserIdentity/user-profile.md'};
}

function seedGeneral(repository: any) {
  const store = repository.getFileMemoryStore();
  store.upsertGeneralProjectMeta({projectId: 'id1', projectName: '项目 一', description: '第一个项目', status: 'active'});
  store.upsertGeneralProjectMeta({projectId: 'id2', projectName: 'Alpha', description: 'Alpha 交付项目', status: 'active'});
  const p1 = store.upsertCandidate({
    type: 'project', scope: 'project', projectId: 'id1', name: '项目一计划', description: '项目一说明',
    capturedAt: '2026-10-18T00:00:00.000Z', sourceSessionKey: 's1', summary: '项目一的进展。',
  });
  const p2 = store.upsertCandidate({
    type: 'project', scope: 'project', projectId: 'id2', name: 'Alpha 计划', description: 'Alpha 说明',
    capturedAt: '2026-10-20T00:00:00.000Z', sourceSessionKey: 's2', summary: 'Alpha 的进展。',
  });
  const f2 = store.upsertCandidate({
    type: 'feedback', scope: 'project', projectId: 'id2', name: 'Alpha 规则', description: 'Alpha 规则说明',
    capturedAt: '2026-10-21T00:00:00.000Z', sourceSessionKey: 's3', rule: 'Alpha 先评审。',
  });
  repository.getGlobalUserStore().upsertUserProfile({
    type: 'user', scope: 'global', name: '用户', description: '用户画像',
    capturedAt: '2026-10-19T00:00:00.000Z', sourceSessionKey: 's-user',
    profile: '工程师。', relationships: ['负责人'],
  });
  store.repairManifests();
  return {p1: p1.relativePath, p2: p2.relativePath, f2: f2.relativePath,
    user: 'UserIdentity/user-profile.md'};
}

export async function collect(baseline: string) {
  assert.equal(process.version, 'v22.22.0');
  const home = mkdtempSync(join(tmpdir(), 'pdrs-retriever-home-'));
  const oldHome = process.env.HOME;
  const RealDate = Date;
  const oldRandom = Math.random;
  process.env.HOME = home;
  let now = CLOCK_MS;
  class FakeDate extends RealDate {
    constructor(...args: any[]) {
      if (args.length === 0) super(now);
      else super(...(args as []));
    }
    static now() { return now; }
  }
  (globalThis as any).Date = FakeDate;
  Math.random = () => RANDOM;
  const {ReasoningRetriever} = await import(pathToFileURL(join(baseline, REASONING)).href);
  const {MemoryRepository} = await import(pathToFileURL(join(baseline, SQLITE)).href);
  const {LlmMemoryExtractor} = await import(pathToFileURL(join(baseline, LLM)).href);
  const {buildMemoryRecallSystemContext} = await import(pathToFileURL(join(baseline, SERVICE)).href);
  const scenarios: any[] = [];
  const scratch = mkdtempSync(join(tmpdir(), 'pdrs-retriever-scratch-'));
  try {
    const definitions = [
      {name: 'single-routes', mode: 'single', seed: seedSingle, steps: [
        chat('{"route":"user"}'), chat('{"selected_ids":["@user"]}'),
        chat('{"route":"project"}'), chat('{"selected_ids":["@project","@feedback"]}'),
        chat('{"route":"mix"}'), chat('{"selected_ids":["@feedback"]}'),
        chat('{"route":"none"}'),
        chat('{"route":"none"}'),
      ], operations: [
        {kind: 'retrieve', query: 'Who am I?', recentMessages: [message('user', 'my name is Ada'), message('assistant', 'ok')]},
        {kind: 'retrieve', query: 'What is Alpha status?', workspaceHint: '/ws/alpha'},
        {kind: 'retrieve', query: 'mix both', retrievalMode: 'auto'},
        {kind: 'retrieve', query: 'hello'},
        {kind: 'retrieve', query: 'hello', retrievalMode: 'explicit'},
      ]},
      {name: 'single-fallback', mode: 'single', seed: seedSingle, steps: [
        chat('{"route":"project"}'), chat('{"selected_ids":[]}'),
        chat('{"route":"project"}'), chat('{bad'),
      ], operations: [
        {kind: 'retrieve', query: 'Alpha plan'},
        {kind: 'retrieve', query: 'Alpha plan again'},
      ]},
      {name: 'general-project', mode: 'general', seed: seedGeneral, steps: [
        chat('{"route":"mix"}'), chat('{"selected_project_id":"id2","reason":"Alpha 精确命中"}'),
        chat('{"selected_ids":["@f2"]}'),
        chat('{"route":"project"}'), chat('{"selected_project_id":"","reason":""}'),
      ], operations: [
        {kind: 'retrieve', query: 'Alpha 进展', recentMessages: [message('user', '继续 Alpha')]},
        {kind: 'retrieve', query: '未知项目'},
      ]},
      {name: 'service-wrappers', mode: 'single', seed: seedSingle, steps: [
        chat('{"route":"project"}'), chat('{"selected_ids":["@project"]}'),
        chat('{"route":"none"}'),
      ], operations: [
        {kind: 'retrieveContext', query: 'context please'},
        {kind: 'search', query: 'search please'},
      ]},
      {name: 'cache', mode: 'single', seed: seedSingle, steps: [
        chat('{"route":"project"}'), chat('{"selected_ids":["@project"]}'),
        chat('{"route":"project"}'), chat('{"selected_ids":["@project"]}'),
        chat('{"route":"project"}'), chat('{"selected_ids":["@project"]}'),
        chat('{"route":"project"}'), chat('{"selected_ids":["@project"]}'),
      ], operations: [
        {kind: 'retrieve', query: 'Cache  Query'},
        {kind: 'retrieve', query: 'cache query'},
        {kind: 'advance', ms: 29_999},
        {kind: 'retrieve', query: 'cache query'},
        {kind: 'advance', ms: 1},
        {kind: 'retrieve', query: 'cache query'},
        {kind: 'reset'},
        {kind: 'retrieve', query: 'cache query'},
        {kind: 'retrieve', query: 'other key'},
      ]},
    ];
    for (const definition of definitions) {
      now = CLOCK_MS;
      const root = join(scratch, definition.name);
      const memory = join(root, 'data', 'memory');
      const globalRoot = join(root, 'global');
      const workspaceDir = definition.mode === 'general' ? join(home, '.pilotdeck') : join(root, 'work');
      const repository = new MemoryRepository(join(root, 'data', 'memory.db'), {memoryDir: memory, globalRootDir: globalRoot, workspaceDir});
      const paths = definition.seed(repository);
      const steps = definition.steps.map((step) => {
        if (step.body !== undefined && typeof step.body === 'string' && step.body.includes('@')) {
          const body = step.body.replace(/@(\w+)/g, (_m: string, key: string) => paths[key] ?? _m);
          return {...step, body};
        }
        return step;
      });
      const server = await stub(steps, () => now);
      const config = {
        agents: {defaults: {model: {primary: 'p/m'}}},
        models: {providers: {p: {api: 'chat', baseUrl: server.origin, apiKey: 'fixture-key', models: [{id: 'm'}]}}},
      };
      const extractor = new LlmMemoryExtractor(config, {telemetry: {}}, undefined);
      const retriever = new ReasoningRetriever(repository, extractor);
      const operations: any[] = [];
      try {
        for (const operation of definition.operations) {
          if (operation.kind === 'advance') {
            now += operation.ms;
            operations.push(enc({kind: 'advance', ms: operation.ms}));
            continue;
          }
          if (operation.kind === 'reset') {
            retriever.resetTransientState();
            operations.push(enc({kind: 'reset'}));
            continue;
          }
          const before = server.requests.length;
          const options = {
            ...(operation.recentMessages ? {recentMessages: operation.recentMessages} : {}),
            ...(operation.workspaceHint ? {workspaceHint: operation.workspaceHint} : {}),
            ...(operation.retrievalMode ? {retrievalMode: operation.retrievalMode} : {}),
          };
          if (operation.kind === 'search') options.retrievalMode = 'explicit';
          let result = await retriever.retrieve(operation.query, options);
          if (operation.kind === 'retrieveContext') {
            result = {...result, systemContext: result.context ? buildMemoryRecallSystemContext(result.context) : ''};
          }
          operations.push(enc({
            kind: operation.kind,
            query: operation.query,
            input: {
              recentMessages: operation.recentMessages,
              workspaceHint: operation.workspaceHint,
              retrievalMode: operation.retrievalMode,
            },
            result,
            stats: retriever.getRuntimeStats(),
            requests: server.requests.slice(before).map(requestOf),
          }));
        }
      } finally {
        await server.close();
      }
      repository.close();
      scenarios.push(enc({
        name: definition.name,
        mode: definition.mode,
        steps,
        storage: {
          memoryFiles: treeFiles(memory),
          globalFiles: treeFiles(globalRoot),
        },
        operations: normalized(operations, server.origin),
      }));
    }
    const matrix = await collectMatrix(ReasoningRetriever, home);
    return {scenarios, matrix, clockMs: CLOCK_MS, randomToken: TOKEN};
  } finally {
    (globalThis as any).Date = RealDate;
    Math.random = oldRandom;
    if (oldHome === undefined) delete process.env.HOME;
    else process.env.HOME = oldHome;
    rmSync(home, {recursive: true, force: true});
    rmSync(scratch, {recursive: true, force: true});
  }
}

// M10 cache matrix with a fake repository and a gated extractor: the cache
// layer is independent of storage and HTTP, and the gate makes the await
// interleavings deterministic.
async function collectMatrix(ReasoningRetriever: any, home: string) {
  let now = CLOCK_MS;
  const oldNow = Date.now;
  Date.now = () => now;
  const repo = {
    getSnapshotVersion: () => 'v1',
    getWorkspaceMode: () => 'single',
    getUserSummary: () => ({identityBackground: ['背景'], files: [{relativePath: 'global/UserIdentity/user-profile.md', updatedAt: '2026-10-21T00:00:00.000Z'}]}),
    getFileMemoryStore: () => ({getProjectMeta: () => null}),
    listMemoryEntries: () => [],
    getMemoryRecordsByIds: () => [],
    listReadableProjectCatalog: () => [],
    getReadableProject: () => undefined,
    listReadableProjectEntries: () => [],
  };
  let route = 'none';
  let calls = 0;
  let gate: Promise<void> | undefined;
  const extractor: any = {
    async decideFileMemoryRoute() { calls += 1; if (gate) await gate; return route; },
    async selectRecallProject() { return {}; },
    async selectFileManifestEntries() { return []; },
  };
  const retriever = new ReasoningRetriever(repo, extractor);
  const out: any[] = [];
  const record = (name: string, extra: any = {}) => out.push({name, stats: retriever.getRuntimeStats(), ...extra});
  try {
    route = 'none';
    const r1 = await retriever.retrieve('Hello  World');
    record('empty-miss', {context: r1.context, cacheHit: r1.debug?.cacheHit, calls});
    const r2 = await retriever.retrieve('hello world');
    record('normalized-hit', {context: r2.context, cacheHit: r2.debug?.cacheHit, elapsedMs: r2.debug?.elapsedMs, calls});
    const r3 = await retriever.retrieve('other');
    record('different-key-miss', {cacheHit: r3.debug?.cacheHit, calls});
    retriever.resetTransientState();
    const r4 = await retriever.retrieve('hello world');
    record('reset-clears', {cacheHit: r4.debug?.cacheHit, calls});

    now += 29_999;
    const t1 = await retriever.retrieve('ttl');
    record('ttl-setup', {cacheHit: t1.debug?.cacheHit, calls});
    now += 29_999;
    const t2 = await retriever.retrieve('ttl');
    record('ttl-29999-hit', {cacheHit: t2.debug?.cacheHit, calls});
    now += 1;
    const t3 = await retriever.retrieve('ttl');
    record('ttl-boundary-miss', {cacheHit: t3.debug?.cacheHit, calls});

    retriever.resetTransientState();
    calls = 0;
    let fire!: () => void;
    gate = new Promise<void>((resolve) => { fire = resolve; });
    route = 'user';
    const pending = retriever.retrieve('inflight');
    await new Promise<void>((resolve) => setImmediate(resolve));
    retriever.resetTransientState();
    fire(); gate = undefined;
    const p = await pending;
    const afterReset = await retriever.retrieve('inflight');
    record('reset-during-await', {firstContext: p.context, secondCacheHit: afterReset.debug?.cacheHit, secondContext: afterReset.context, calls});

    retriever.resetTransientState();
    calls = 0;
    let g1!: () => void, g2!: () => void;
    const gate1 = new Promise<void>((resolve) => { g1 = resolve; });
    const gate2 = new Promise<void>((resolve) => { g2 = resolve; });
    let n = 0;
    const realDecide = extractor.decideFileMemoryRoute;
    extractor.decideFileMemoryRoute = async () => { n += 1; if (n === 1) await gate1; else await gate2; return n === 1 ? 'user' : 'project'; };
    const c1 = retriever.retrieve('concurrent');
    const c2 = retriever.retrieve('concurrent');
    await new Promise<void>((resolve) => setImmediate(resolve));
    route = 'user';
    g1(); await c1; g2(); await c2;
    extractor.decideFileMemoryRoute = realDecide;
    const c3 = await retriever.retrieve('concurrent');
    record('same-key-concurrent', {decides: n, calls, cachedRoute: c3.debug?.route, cachedCacheHit: c3.debug?.cacheHit, cachedIntent: c3.intent});
  } finally {
    Date.now = oldNow;
  }
  return out;
}
