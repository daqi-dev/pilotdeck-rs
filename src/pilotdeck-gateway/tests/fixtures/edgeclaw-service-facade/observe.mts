// Shared real-TS scenario driver for the EdgeClaw service facade (M18).
//
// `generate.mts` and `verify.mts` both call `collect`, which imports the fixed
// `service.ts`, freezes the clock and `Math.random`, isolates HOME, and runs
// the same declarative operation sequence that `tests/edgeclaw_service_facade.rs`
// replays on the Rust side. Every observed value is encoded so `undefined`,
// `NaN` and `Infinity` survive JSON.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
import {DatabaseSync} from 'node:sqlite';

export const ENTRY = 'src/context/memory/edgeclaw-memory-core/src/service.ts';
export const NOW = '2026-10-22T00:00:00.000Z';
export const NOW_MS = Date.parse(NOW);
export const RANDOM_VALUES = [0.25, 0.5, 0.75, 0.125, 0.875, 0.375, 0.625, 0.0625];
export const RANDOM_TOKENS = RANDOM_VALUES.map((value) => value.toString(36).slice(2, 10));

const CANDIDATE = (type: string, name: string, extra: Record<string, unknown> = {}) => ({
  type,
  scope: type === 'user' ? 'global' : 'project',
  name,
  description: `${name} description`,
  summary: `${name} summary`,
  profile: `${name} profile`,
  capturedAt: NOW,
  sourceSessionKey: 'seed-session',
  ...extra,
});

const MESSAGES = [
  {role: 'user', content: 'alpha user'},
  {role: 'assistant', content: 'alpha assistant'},
];

/// `undefined`, non-finite numbers and lone-surrogate-free JSON. Rust mirrors
/// this exact encoding in its observation writer.
export function encode(value: any): any {
  if (value === undefined) return {$type: 'undefined'};
  if (typeof value === 'number' && !Number.isFinite(value)) {
    return {$type: 'number', value: Object.is(value, -0) ? '-0' : String(value)};
  }
  if (Array.isArray(value)) return value.map(encode);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, encode(item)]));
  }
  return value;
}

function clean(value: any, root: string, workspaceHash: string): any {
  if (typeof value === 'string') {
    return value
      .split(root).join('$ROOT')
      .replace(/\/workspaces\/([0-9a-f]{10})(?=\/|$)/gu, (_match, hash: string) => {
        // A wrong seed (root, fixed string, …) used to be erased by the
        // generic $HASH replacement. Bind the observed slug to
        // sha1(resolve(workspaceDir))[:10] before normalising it.
        assert.equal(hash, workspaceHash,
          `workspace data-dir slug ${hash} is not sha1(resolve(workspaceDir))=${workspaceHash}`);
        return '/workspaces/$HASH';
      });
  }
  if (Array.isArray(value)) return value.map((item) => clean(item, root, workspaceHash));
  if (value && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([key, item]) => [key, clean(item, root, workspaceHash)]),
    );
  }
  return value;
}

function errorShape(error: any) {
  return {name: error?.name ?? null, message: error?.message ?? null};
}

function syncStep(name: string, args: any, fn: () => any, state: any) {
  try {
    const value = fn();
    return {
      op: name,
      args: clean(encode(args), state.root, state.workspaceHash),
      ok: true,
      value: clean(encode(value), state.root, state.workspaceHash),
    };
  } catch (error) {
    return {
      op: name,
      args: clean(encode(args), state.root, state.workspaceHash),
      ok: false,
      error: clean(encode(errorShape(error)), state.root, state.workspaceHash),
    };
  }
}

function tree(root: string, base = root): any[] {
  if (!statSync(root, {throwIfNoEntry: false})) return [];
  return readdirSync(root).sort().flatMap((name) => {
    const path = join(root, name);
    const relative = path.slice(base.length + 1);
    return statSync(path).isDirectory()
      ? tree(path, base)
      : [{relativePath: relative, content: readFileSync(path, 'utf8')}];
  });
}

function sqliteDump(path: string) {
  const db = new DatabaseSync(path, {readOnly: true});
  try {
    return {
      pipeline: db.prepare('SELECT state_key, state_json, updated_at FROM pipeline_state ORDER BY state_key').all(),
      l0: db.prepare('SELECT l0_index_id, session_key, timestamp, messages_json, source, indexed, created_at FROM l0_sessions ORDER BY l0_index_id').all(),
    };
  } finally {
    db.close();
  }
}

interface Spec {
  name: string;
  paths: Record<string, string>;
  options?: Record<string, any>;
  seeds: any[];
  buildOps: (service: any, ids: any) => any[];
}

function specs(): Spec[] {
  return [
    {
      name: 'paths-defaults',
      paths: {workspace: 'workspace'},
      seeds: [{store: 'workspace', method: 'upsertProjectMeta', input: {projectName: 'Alpha', description: 'a', status: 'in_progress'}}],
      buildOps: () => [{op: 'getSettings'}, {op: 'getWorkspaceMode'}, {op: 'close'}],
    },
    {
      name: 'paths-db-override',
      paths: {workspace: 'workspace', rootDir: 'root', dbPath: 'custom/db/control.sqlite'},
      seeds: [],
      buildOps: () => [{op: 'getSettings'}, {op: 'close'}],
    },
    {
      name: 'paths-memory-override',
      paths: {workspace: 'workspace', rootDir: 'root', memoryDir: 'custom/memory'},
      seeds: [],
      buildOps: () => [{op: 'getSettings'}, {op: 'close'}],
    },
    {
      name: 'capture-normalization',
      paths: {workspace: 'workspace', rootDir: 'root'},
      options: {includeAssistant: false, maxMessageChars: 5, captureStrategy: 'full_session'},
      seeds: [],
      buildOps: () => [
        {op: 'captureTurn', args: {messages: MESSAGES, input: {sessionKey: 's1', timestamp: '2026-10-22T00:00:01.000Z'}}},
        {op: 'captureTurn', args: {messages: [{role: 'assistant', content: 'only assistant'}], input: {sessionKey: 's1'}}},
        {op: 'captureTurn', args: {messages: [{role: 'tool'}], input: {sessionKey: 's1'}}},
        {op: 'close'},
      ],
    },
    {
      name: 'facade-sequence',
      paths: {workspace: 'workspace', rootDir: 'root', dbPath: 'data/control.sqlite', memoryDir: 'data/memory'},
      options: {defaultIndexingSettings: {reasoningMode: 'answer_first', autoIndexIntervalMinutes: 2}},
      seeds: [
        {store: 'workspace', method: 'upsertProjectMeta', input: {projectName: 'Alpha', description: 'alpha description', status: 'in_progress'}},
        {store: 'workspace', method: 'upsertCandidate', input: CANDIDATE('project', 'Alpha note')},
        {store: 'workspace', method: 'upsertCandidate', input: CANDIDATE('feedback', 'Beta feedback')},
        {store: 'global', method: 'upsertCandidate', input: CANDIDATE('user', 'User one')},
      ],
      buildOps: (service, ids) => [
        {op: 'getSettings'},
        {op: 'saveSettings', args: {partial: {autoIndexIntervalMinutes: 5, autoDreamIntervalMinutes: 0}}},
        {op: 'getSettings'},
        {op: 'overview'},
        {op: 'snapshot'},
        {op: 'snapshot', args: {limit: 1}},
        {op: 'captureTurn', args: {messages: MESSAGES, input: {sessionKey: 's1', timestamp: '2026-10-22T00:00:01.000Z'}}},
        {op: 'captureTurn', args: {messages: [{role: 'tool'}], input: {sessionKey: 's1'}}},
        {op: 'list'},
        {op: 'list', args: {options: {kinds: ['project'], query: 'Alpha'}}},
        {op: 'get', args: {ids: [ids.entry], maxLines: 10}},
        {op: 'getUserSummary'},
        {op: 'getProjectMeta'},
        {op: 'getWorkspaceMode'},
        {op: 'listReadableProjectCatalog'},
        {op: 'getReadableProject', args: {id: 'current_project'}},
        {op: 'listReadableProjectEntries', args: {id: 'current_project', options: {}}},
        {op: 'updateProjectMeta', args: {input: {projectName: 'Alpha renamed', description: 'renamed', status: 'done'}}},
        {op: 'getSnapshotVersion'},
        {op: 'close'},
      ],
    },
    {
      name: 'traces',
      paths: {workspace: 'workspace', rootDir: 'root'},
      seeds: [
        {store: 'workspace', method: 'upsertProjectMeta', input: {projectName: 'Alpha', description: 'alpha description', status: 'in_progress'}},
        {method: 'indexTrace', input: {indexTraceId: 'idx-1', startedAt: '2026-10-22T00:00:00.000Z', status: 'error'}},
        {method: 'dreamTrace', input: {dreamTraceId: 'dream-1', startedAt: '2026-10-22T00:00:00.000Z', status: 'error'}},
      ],
      buildOps: () => [
        {op: 'saveCaseTrace', args: {record: {
          caseId: '  case-explicit  ', sessionKey: 's1', query: 'q1',
          startedAt: '2026-10-22T00:00:03.000Z', finishedAt: '2026-10-22T00:00:04.000Z',
          status: 'ok', retrieval: {contextPreview: '### [user]\nname\n### [project]\nbody'},
          toolEvents: [], assistantReply: 'reply',
        }}},
        {op: 'saveCaseTrace', args: {record: {sessionKey: 's1', query: 'q2', status: 'ok'}}},
        {op: 'saveCaseTrace', args: {record: {caseId: '   ', sessionKey: 's2', query: 'q3', status: 'ok', retrieval: {contextPreview: 'plain preview'}}}},
        {op: 'listCaseTraces'},
        {op: 'listCaseTraces', args: {limit: 1}},
        {op: 'getCaseTrace', args: {caseId: 'case-explicit'}},
        {op: 'getCaseTrace', args: {caseId: 'missing'}},
        {op: 'listIndexTraces'},
        {op: 'getIndexTrace', args: {id: 'idx-1'}},
        {op: 'listDreamTraces'},
        {op: 'getDreamTrace', args: {id: 'dream-1'}},
        {op: 'close'},
      ],
    },
    {
      name: 'export-import',
      paths: {workspace: 'workspace', rootDir: 'root'},
      seeds: [
        {store: 'workspace', method: 'upsertProjectMeta', input: {projectName: 'Alpha', description: 'alpha description', status: 'in_progress'}},
        {store: 'workspace', method: 'upsertCandidate', input: CANDIDATE('project', 'Alpha note')},
        {store: 'global', method: 'upsertCandidate', input: CANDIDATE('user', 'User one')},
      ],
      buildOps: () => [
        {op: 'export'},
        {op: 'importLastExport'},
        {op: 'list'},
        {op: 'getUserSummary'},
        {op: 'export'},
        {op: 'close'},
      ],
    },
    {
      name: 'clear',
      paths: {workspace: 'workspace', rootDir: 'root'},
      seeds: [
        {store: 'workspace', method: 'upsertProjectMeta', input: {projectName: 'Alpha', description: 'alpha description', status: 'in_progress'}},
        {store: 'workspace', method: 'upsertCandidate', input: CANDIDATE('project', 'Alpha note')},
      ],
      buildOps: () => [
        {op: 'captureTurn', args: {messages: MESSAGES, input: {sessionKey: 'session-one', timestamp: '2026-10-22T00:00:01.000Z'}}},
        {op: 'captureTurn', args: {messages: MESSAGES, input: {sessionKey: 'session-two', timestamp: '2026-10-22T00:00:02.000Z'}}},
        {op: 'clearSession', args: {sessionKey: 'session-one'}},
        {op: 'overview'},
        {op: 'clear', args: {scope: 'current_project'}},
        {op: 'overview'},
        {op: 'captureTurn', args: {messages: MESSAGES, input: {sessionKey: 'session-three', timestamp: '2026-10-22T00:00:03.000Z'}}},
        {op: 'clear', args: {scope: 'all_memory'}},
        {op: 'overview'},
        {op: 'close'},
      ],
    },
    {
      name: 'act',
      paths: {workspace: 'workspace', rootDir: 'root'},
      seeds: [
        {store: 'workspace', method: 'upsertProjectMeta', input: {projectName: 'Alpha', description: 'alpha description', status: 'in_progress'}},
        {store: 'workspace', method: 'upsertCandidate', input: CANDIDATE('project', 'Alpha note')},
        {store: 'workspace', method: 'upsertCandidate', input: CANDIDATE('feedback', 'Beta feedback')},
      ],
      buildOps: (service, ids) => [
        {op: 'act', args: {input: {action: 'edit_project_meta', projectName: 'Alpha renamed', description: 'renamed', status: 'done'}}},
        {op: 'act', args: {input: {action: 'edit_entry', id: ids.entry, name: 'Alpha note renamed', description: 'edited description'}}},
        {op: 'act', args: {input: {action: 'deprecate_entries', ids: [ids.feedback]}}},
        {op: 'list', args: {options: {includeDeprecated: true}}},
        {op: 'act', args: {input: {action: 'restore_entries', ids: [ids.feedback]}}},
        {op: 'act', args: {input: {action: 'delete_entries', ids: [ids.entry, ids.feedback]}}},
        {op: 'act', args: {input: {action: 'unknown_action'}}},
        {op: 'list'},
        {op: 'close'},
      ],
    },
    {
      name: 'closed-matrix',
      paths: {workspace: 'workspace', rootDir: 'root'},
      seeds: [
        {store: 'workspace', method: 'upsertProjectMeta', input: {projectName: 'Alpha', description: 'alpha description', status: 'in_progress'}},
        {store: 'workspace', method: 'upsertCandidate', input: CANDIDATE('project', 'Alpha note')},
      ],
      buildOps: (service, ids) => [
        {op: 'captureTurn', args: {messages: MESSAGES, input: {sessionKey: 's1', timestamp: '2026-10-22T00:00:01.000Z'}}},
        {op: 'saveCaseTrace', args: {record: {caseId: 'closed-case', sessionKey: 's1', query: 'q', startedAt: '2026-10-22T00:00:00.000Z', status: 'ok'}}},
        {op: 'export'},
        {op: 'close'},
        {op: 'getSettings'},
        {op: 'saveSettings', args: {partial: {autoIndexIntervalMinutes: 5}}},
        {op: 'overview'},
        {op: 'snapshot'},
        {op: 'captureTurn', args: {messages: MESSAGES, input: {sessionKey: 's2'}}},
        {op: 'list'},
        {op: 'get', args: {ids: [ids.entry], maxLines: 80}},
        {op: 'getUserSummary'},
        {op: 'getProjectMeta'},
        {op: 'getWorkspaceMode'},
        {op: 'listReadableProjectCatalog'},
        {op: 'getReadableProject', args: {id: 'current_project'}},
        {op: 'listReadableProjectEntries', args: {id: 'current_project', options: {}}},
        {op: 'updateProjectMeta', args: {input: {projectName: 'After close', description: 'd', status: 's'}}},
        {op: 'getSnapshotVersion'},
        {op: 'listCaseTraces'},
        {op: 'saveCaseTrace', args: {record: {sessionKey: 's1', query: 'q2', status: 'ok'}}},
        {op: 'getCaseTrace', args: {caseId: 'closed-case'}},
        {op: 'listIndexTraces'},
        {op: 'getIndexTrace', args: {id: 'none'}},
        {op: 'listDreamTraces'},
        {op: 'getDreamTrace', args: {id: 'none'}},
        {op: 'export'},
        {op: 'importLastExport'},
        {op: 'clear'},
        {op: 'clearSession', args: {sessionKey: 's1'}},
        {op: 'act', args: {input: {action: 'edit_project_meta', projectName: 'P', description: 'd', status: 's'}}},
        {op: 'close'},
        {op: 'close'},
      ],
    },
    {
      name: 'settings-nonfinite',
      paths: {workspace: 'workspace', rootDir: 'root'},
      options: {defaultIndexingSettings: {
        autoIndexIntervalMinutes: Number.NaN,
        autoDreamIntervalMinutes: Number.POSITIVE_INFINITY,
      }},
      seeds: [],
      buildOps: () => [
        {op: 'getSettings'},
        {op: 'saveSettings', args: {partial: {reasoningMode: 'answer_first'}}},
        {op: 'getSettings'},
        {op: 'close'},
      ],
    },
    {
      name: 'closed-empty',
      paths: {workspace: 'workspace', rootDir: 'root'},
      seeds: [],
      buildOps: () => [
        {op: 'close'},
        {op: 'listReadableProjectCatalog'},
        {op: 'getReadableProject', args: {id: 'current_project'}},
        {op: 'listReadableProjectEntries', args: {id: 'current_project', options: {}}},
        {op: 'captureTurn', args: {messages: [], input: {sessionKey: 'empty'}}},
        {op: 'captureTurn', args: {messages: [{role: 'tool'}], input: {sessionKey: 'tool'}}},
        {op: 'captureTurn', args: {messages: [{role: 'assistant', content: 'only assistant'}], input: {sessionKey: 'assistant'}}},
      ],
    },
  ];
}

function serviceOptions(spec: Spec, scenarioRoot: string) {
  const path = (key: string) => resolve(scenarioRoot, spec.paths[key]);
  const options: any = {
    workspaceDir: path('workspace'),
    llm: {provider: 'p', model: 'm', modelRef: 'p/m', baseUrl: 'http://127.0.0.1:1/v1', apiKey: 'fixture-key', apiType: 'openai-responses'},
    ...spec.options,
  };
  if (spec.paths.rootDir !== undefined) options.rootDir = path('rootDir');
  if (spec.paths.dbPath !== undefined) options.dbPath = path('dbPath');
  if (spec.paths.memoryDir !== undefined) options.memoryDir = path('memoryDir');
  return options;
}

function seed(service: any, item: any) {
  if (item.method === 'indexTrace') return service.repository.saveIndexTrace(item.input);
  if (item.method === 'dreamTrace') return service.repository.saveDreamTrace(item.input);
  const store = item.store === 'global'
    ? service.repository.getGlobalUserStore()
    : service.repository.getFileMemoryStore();
  return store[item.method](item.input);
}

function idsFor(service: any): any {
  const entries = service.list({includeDeprecated: true});
  const pick = (name: string) => entries.find((item: any) => item.name === name)?.relativePath;
  return {entry: pick('Alpha note'), feedback: pick('Beta feedback')};
}

function runOp(service: any, op: any, state: any) {
  if (op.op === 'getSettings') return syncStep(op.op, op.args, () => service.getSettings(), state);
  if (op.op === 'saveSettings') return syncStep(op.op, op.args, () => service.saveSettings(op.args.partial), state);
  if (op.op === 'overview') return syncStep(op.op, op.args, () => service.overview(), state);
  if (op.op === 'snapshot') return syncStep(op.op, op.args, () => service.snapshot(op.args?.limit), state);
  if (op.op === 'captureTurn') return syncStep(op.op, op.args, () => service.captureTurn(op.args.messages, op.args.input), state);
  if (op.op === 'list') return syncStep(op.op, op.args, () => service.list(op.args?.options), state);
  if (op.op === 'get') return syncStep(op.op, op.args, () => service.get(op.args.ids, op.args.maxLines), state);
  if (op.op === 'getUserSummary') return syncStep(op.op, op.args, () => service.getUserSummary(), state);
  if (op.op === 'getProjectMeta') return syncStep(op.op, op.args, () => service.getProjectMeta(), state);
  if (op.op === 'getWorkspaceMode') return syncStep(op.op, op.args, () => service.getWorkspaceMode(), state);
  if (op.op === 'listReadableProjectCatalog') return syncStep(op.op, op.args, () => service.listReadableProjectCatalog(), state);
  if (op.op === 'getReadableProject') return syncStep(op.op, op.args, () => service.getReadableProject(op.args.id), state);
  if (op.op === 'listReadableProjectEntries') return syncStep(op.op, op.args, () => service.listReadableProjectEntries(op.args.id, op.args.options), state);
  if (op.op === 'updateProjectMeta') return syncStep(op.op, op.args, () => service.updateProjectMeta(op.args.input), state);
  if (op.op === 'getSnapshotVersion') return syncStep(op.op, op.args, () => service.getSnapshotVersion(), state);
  if (op.op === 'saveCaseTrace') return syncStep(op.op, op.args, () => service.saveCaseTrace(op.args.record), state);
  if (op.op === 'listCaseTraces') return syncStep(op.op, op.args, () => service.listCaseTraces(op.args?.limit), state);
  if (op.op === 'getCaseTrace') return syncStep(op.op, op.args, () => service.getCaseTrace(op.args.caseId), state);
  if (op.op === 'listIndexTraces') return syncStep(op.op, op.args, () => service.listIndexTraces(op.args?.limit), state);
  if (op.op === 'getIndexTrace') return syncStep(op.op, op.args, () => service.getIndexTrace(op.args.id), state);
  if (op.op === 'listDreamTraces') return syncStep(op.op, op.args, () => service.listDreamTraces(op.args?.limit), state);
  if (op.op === 'getDreamTrace') return syncStep(op.op, op.args, () => service.getDreamTrace(op.args.id), state);
  if (op.op === 'export') {
    const result = syncStep(op.op, op.args, () => service.exportBundle(), state);
    if (result.ok) state.lastExport = result.value;
    return result;
  }
  if (op.op === 'importLastExport') {
    assert.ok(state.lastExport, 'importLastExport needs a prior export');
    return syncStep(op.op, op.args, () => service.importBundle(state.lastExport), state);
  }
  if (op.op === 'clear') return syncStep(op.op, op.args, () => service.clear(op.args?.scope), state);
  if (op.op === 'clearSession') return syncStep(op.op, op.args, () => service.clearSession(op.args.sessionKey), state);
  if (op.op === 'act') return syncStep(op.op, op.args, () => service.act(op.args.input), state);
  if (op.op === 'close') return syncStep(op.op, op.args, () => service.close(), state);
  throw new Error(`unknown op ${op.op}`);
}

export async function collect(baseline: string) {
  assert.equal(process.version, 'v22.22.0');
  const root = mkdtempSync(join(tmpdir(), 'pdrs-service-facade-'));
  const home = join(root, 'home');
  mkdirSync(home, {recursive: true});
  const oldHome = process.env.HOME;
  const oldDate = globalThis.Date;
  const oldDateNow = Date.now;
  const oldRandom = Math.random;
  let randomIndex = 0;
  class FrozenDate extends oldDate {
    constructor(...args: any[]) {
      super(...((args.length ? args : [NOW_MS]) as [number]));
    }
    static now() {
      return NOW_MS;
    }
  }
  (globalThis as any).Date = FrozenDate;
  Date.now = () => NOW_MS;
  Math.random = () => RANDOM_VALUES[randomIndex++ % RANDOM_VALUES.length];
  process.env.HOME = home;
  const {EdgeClawMemoryService} = await import(pathToFileURL(join(baseline, ENTRY)).href);
  const observed: any[] = [];
  try {
    for (const spec of specs()) {
      const scenarioRoot = join(root, spec.name);
      mkdirSync(scenarioRoot, {recursive: true});
      const options = serviceOptions(spec, scenarioRoot);
      const service = new EdgeClawMemoryService(options);
      for (const item of spec.seeds) seed(service, item);
      const ids = idsFor(service);
      const ops = spec.buildOps(service, ids);
      const steps = [];
      const workspaceHash = createHash('sha1')
        .update(resolve(service.workspaceDir)).digest('hex').slice(0, 10);
      const state: any = {root, workspaceHash};
      for (const op of ops) steps.push(runOp(service, op, state));
      // Every scenario closes in its ops. A second close is harmless here and
      // guarantees the SQLite handle is released for the dump.
      try { service.close(); } catch {}
      const cleaned = (value: any) => clean(encode(value), root, workspaceHash);
      // `resolveDefaultRootDir` treats an empty `rootDir` as absent, so the
      // global store lives under HOME rather than under cwd.
      const effectiveRoot = options.rootDir ? options.rootDir : join(home, '.edgeclaw', 'memory');
      observed.push({
        name: spec.name,
        options: clean(encode(spec.options ?? null), root, workspaceHash),
        seeds: clean(encode(spec.seeds), root, workspaceHash),
        ops: clean(encode(ops), root, workspaceHash),
        inputPaths: clean(encode(spec.paths), root, workspaceHash),
        paths: {
          workspace: clean(options.workspaceDir, root, workspaceHash),
          rootDir: options.rootDir === undefined ? null : clean(options.rootDir, root, workspaceHash),
          dataDir: clean(service.dataDir, root, workspaceHash),
          dbPath: clean(service.dbPath, root, workspaceHash),
          memoryDir: clean(service.memoryDir, root, workspaceHash),
        },
        steps,
        sqlite: cleaned(sqliteDump(service.dbPath)),
        files: {
          memory: cleaned(tree(service.memoryDir)),
          global: cleaned(tree(join(effectiveRoot, 'global'))),
        },
      });
    }
  } finally {
    if (oldHome === undefined) delete process.env.HOME;
    else process.env.HOME = oldHome;
    (globalThis as any).Date = oldDate;
    Date.now = oldDateNow;
    Math.random = oldRandom;
    rmSync(root, {recursive: true, force: true});
  }
  return {
    sourceCommit: 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',
    nodeVersion: process.version,
    randomValues: RANDOM_VALUES,
    randomTokens: RANDOM_TOKENS,
    scenarios: observed,
  };
}
