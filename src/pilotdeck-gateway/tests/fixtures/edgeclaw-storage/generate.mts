// Real-TS generator for the EdgeClaw sqlite storage fixture.
//
// Run from the repository root with the fixed baseline's tsx loader:
//   PDRS_BASELINE=/abs/PilotDeck node --import "$PDRS_BASELINE/node_modules/tsx/dist/loader.mjs" \
//     src/pilotdeck-gateway/tests/fixtures/edgeclaw-storage/generate.mts \
//     src/pilotdeck-gateway/tests/fixtures/edgeclaw-storage
//
// Every expectation below comes from executing the unmodified baseline
// `MemoryRepository` (`core/storage/sqlite.ts`) on Node 22's `node:sqlite`.
// The generator writes two independent artifacts:
//
//   * JSON observations/schema/rows for each scenario (the Rust oracle), and
//   * the raw `.db` files base64-encoded in `oracle.json`, so the Rust suite can
//     open a database that a real TS process created without rerunning Node.
//
// `.db` files are never committed; they live in a temp directory for the run.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import { DatabaseSync } from 'node:sqlite';
import {
  bindSourceTree,
  sourceClosureSha256,
} from '../../baseline_source_guard.mjs';
import {
  DEFAULTS,
  OTHER_DEFAULTS,
  dumpRows,
  dumpSchema,
  observe,
  readDbBase64,
  tableInfo,
} from './observe.mts';

assert.ok(process.env.PDRS_BASELINE, 'set PDRS_BASELINE to the fixed TS baseline');
assert.ok(process.argv[2], 'provide the fixture output directory');

const BASELINE_COMMIT = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
const SQLITE_TS =
  'src/context/memory/edgeclaw-memory-core/src/core/storage/sqlite.ts';
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);

const gitExecutable = realpathSync(
  process.env.PDRS_GIT || execFileSync('which', ['git'], { encoding: 'utf8' }).trim(),
);
const provenance = bindSourceTree(baseline, BASELINE_COMMIT, gitExecutable);
const sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [
  SQLITE_TS,
]);

// `nowIso()` is `new Date().toISOString()`. Freezing it makes the two
// generator runs byte-identical, including `pipeline_state.updated_at` and the
// defaulted `l0_sessions.created_at`.
const FIXED_NOW = '2026-10-21T00:00:00.000Z';
const FIXED_MS = Date.parse(FIXED_NOW);
class FrozenDate extends Date {
  constructor(...args: any[]) {
    super(...((args.length === 0 ? [FIXED_MS] : args) as [number]));
  }
  static now() {
    return FIXED_MS;
  }
}
(globalThis as any).Date = FrozenDate;

const { MemoryRepository } = await import(
  pathToFileURL(join(baseline, SQLITE_TS)).href
);

assert.match(
  process.versions.sqlite ?? '',
  /^3\.51\./,
  `baseline Node must use a 3.51.x SQLite, got ${process.versions.sqlite}`,
);

const root = mkdtempSync(join(tmpdir(), 'pdrs-edgeclaw-storage-'));

function openRepo(dbPath: string): any {
  const scenarioRoot = dirname(dbPath);
  return new MemoryRepository(dbPath, {
    memoryDir: join(scenarioRoot, 'memory'),
    globalRootDir: join(scenarioRoot, 'global'),
    workspaceDir: join(scenarioRoot, 'workspace'),
  });
}

function scenarioDir(name: string): string {
  const directory = join(root, name);
  mkdirSync(directory, { recursive: true });
  return directory;
}

function readArtifacts(dbPath: string) {
  return {
    schema: dumpSchema(dbPath),
    columns: {
      l0_sessions: tableInfo(dbPath, 'l0_sessions'),
      pipeline_state: tableInfo(dbPath, 'pipeline_state'),
    },
    rows: dumpRows(dbPath),
    dbBase64: readDbBase64(dbPath),
  };
}

// -- record builders -------------------------------------------------------
//
// Field insertion order is the order the baseline's `JSON.stringify` would
// emit; the Rust port parses this same JSON text and re-emits it unchanged.

function caseTrace(caseId: string, startedAt: string, extra: Record<string, unknown> = {}) {
  return {
    caseId,
    sessionKey: 's1',
    query: `q-${caseId}`,
    startedAt,
    status: 'completed',
    toolEvents: [
      {
        eventId: `e-${caseId}`,
        phase: 'start',
        toolName: 'read',
        occurredAt: startedAt,
        status: 'success',
        summary: 'ok',
      },
    ],
    assistantReply: `reply-${caseId}`,
    ...extra,
  };
}

function indexTrace(
  indexTraceId: string,
  startedAt: string,
  extra: Record<string, unknown> = {},
) {
  return {
    indexTraceId,
    sessionKey: 's1',
    trigger: 'manual',
    startedAt,
    status: 'completed',
    batchSummary: { sessionKey: 's1', messageCount: 2, storedCount: 0 },
    steps: [{ phase: 'start', at: startedAt, detail: 'begin' }],
    storedResults: [],
    ...extra,
  };
}

function dreamOutcome(extra: Record<string, unknown> = {}) {
  return {
    rewrittenProjects: 0,
    deletedProjects: 0,
    deletedFiles: 0,
    profileUpdated: false,
    summary: 'ok',
    ...extra,
  };
}

function dreamTrace(
  dreamTraceId: string,
  startedAt: string,
  status: string,
  outcome: Record<string, unknown>,
  extra: Record<string, unknown> = {},
) {
  return {
    dreamTraceId,
    trigger: 'manual',
    startedAt,
    status,
    snapshotSummary: {
      projectMetaPresent: true,
      projectFileCount: 1,
      feedbackFileCount: 0,
      hasUserProfile: false,
    },
    steps: [{ phase: 'start', at: startedAt }],
    mutations: [],
    outcome,
    ...extra,
  };
}

function session(
  l0IndexId: string,
  sessionKey: string,
  timestamp: string,
  createdAt: string,
  extra: Record<string, unknown> = {},
) {
  return {
    l0IndexId,
    sessionKey,
    timestamp,
    messages: [{ msgId: `m-${l0IndexId}`, role: 'user', content: `content-${l0IndexId}` }],
    source: 'openclaw',
    indexed: false,
    createdAt,
    ...extra,
  };
}

// -- the portable operation script ----------------------------------------
//
// The same list is replayed by the Rust suite through `EdgeClawStorage`.
// Every API-observable behavior of A6.6-pr10a appears at least once.

const T = (minute: string) => `2026-10-20T${minute}:00.000Z`;

const OPERATIONS: any[] = [
  // pipeline_state: insert, overwrite (upsert), delete, explicit null,
  // nested JSON, arrays, and a lone surrogate through JSON text.
  { kind: 'setPipelineState', key: 'workspaceDir', value: '/pdrs-fixture/workspace-a' },
  { kind: 'setPipelineState', key: 'workspaceDir', value: '/pdrs-fixture/workspace-b' },
  {
    kind: 'setPipelineState',
    key: 'objectState',
    value: { nested: { a: [1, 2, 3] }, flag: true, none: null },
  },
  { kind: 'setPipelineState', key: 'arrayState', value: [1, 'two', { three: 3 }] },
  { kind: 'setPipelineState', key: 'loneSurrogate', value: 'a\ud800b' },
  { kind: 'setPipelineState', key: 'lastDreamAt', value: T('01:00') },
  { kind: 'setPipelineState', key: 'temp', value: 'gone' },
  { kind: 'deletePipelineState', key: 'temp' },
  { kind: 'setPipelineState', key: 'afterDelete', value: [1, 2, 3] },
  { kind: 'setPipelineState', key: 'nullState', value: null },

  // l0_sessions: pending ordering is by MIN(timestamp) per session, the
  // upsert updates an existing row, and an indexed row is not pending.
  {
    kind: 'insertL0Session',
    record: session('l0-a', 's1', T('10:00'), '2026-10-20T09:59:00.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-b', 's2', T('10:05'), '2026-10-20T10:04:00.000Z', {
      source: 'cli',
    }),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-c', 's2', T('09:55'), '2026-10-20T09:54:00.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-d', 's1', T('11:00'), '2026-10-20T10:59:00.000Z', {
      indexed: true,
    }),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-e', 's3', T('12:00'), '2026-10-20T11:59:00.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-f', 's4', T('08:00'), '2026-10-20T07:59:00.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-g', 's4', T('07:00'), '2026-10-20T06:59:00.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-a', 's1', T('10:30'), '2026-10-20T10:29:00.000Z', {
      messages: [
        { msgId: 'm-l0-a', role: 'user', content: 'content-l0-a' },
        { msgId: 'm-l0-a-2', role: 'assistant', content: 'reply' },
      ],
      source: 'cli',
    }),
  },

  // Cases: stable descending sort, duplicate id keeps the newest, entries that
  // are not objects / lack a string id or startedAt / have an empty id are
  // dropped, and a blank id is a valid identity.
  { kind: 'saveCaseTrace', record: caseTrace('case-a', T('10:00')) },
  { kind: 'saveCaseTrace', record: caseTrace('case-b', T('12:00')) },
  {
    kind: 'saveCaseTrace',
    record: caseTrace('case-a', T('13:00'), { assistantReply: 'newer' }),
  },
  { kind: 'saveCaseTrace', record: 'oops' },
  {
    kind: 'saveCaseTrace',
    record: { caseId: 5, startedAt: T('12:30'), status: 'completed' },
  },
  {
    kind: 'saveCaseTrace',
    record: { caseId: 'case-c', status: 'completed' },
  },
  { kind: 'saveCaseTrace', record: caseTrace('', T('15:00')) },
  { kind: 'saveCaseTrace', record: caseTrace(' ', T('16:00')) },
  { kind: 'saveCaseTrace', record: caseTrace('case-g', T('12:00')) },

  // Index traces: `isNoOp` and `displayStatus` normalization.
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-a', T('10:00')),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-b', T('11:00'), {
      storedResults: [{ kind: 'x', count: 2 }],
    }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-a', T('12:00'), { storedResults: [] }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-c', T('13:00'), {
      status: 'running',
      displayStatus: '   ',
    }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-d', T('14:00'), {
      status: 'error',
      displayStatus: 'Custom',
    }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-e', T('15:00'), {
      isNoOp: true,
      displayStatus: 'Done',
    }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-f', T('16:00'), {
      storedResults: [{ kind: 'y', count: 1 }],
      displayStatus: '',
    }),
  },

  // Dream traces: the same normalization over `outcome`, plus truthiness of
  // `profileUpdated`.
  {
    kind: 'saveDreamTrace',
    record: dreamTrace('dr-a', T('09:00'), 'completed', dreamOutcome()),
  },
  {
    kind: 'saveDreamTrace',
    record: dreamTrace('dr-b', T('10:00'), 'completed', dreamOutcome({ deletedFiles: 1 })),
  },
  {
    kind: 'saveDreamTrace',
    record: dreamTrace(
      'dr-c',
      T('11:00'),
      'error',
      dreamOutcome({ profileUpdated: true }),
    ),
  },
  {
    kind: 'saveDreamTrace',
    record: dreamTrace('dr-d', T('12:00'), 'completed', dreamOutcome(), {
      isNoOp: false,
      displayStatus: '',
    }),
  },
  {
    kind: 'saveDreamTrace',
    record: dreamTrace('dr-e', T('13:00'), 'running', dreamOutcome()),
  },
  {
    kind: 'saveDreamTrace',
    record: dreamTrace(
      'dr-f',
      T('14:00'),
      'completed',
      dreamOutcome({ profileUpdated: true }),
    ),
  },

  // Indexing settings: floor, clamp, parseInt strings, and the only accepted
  // reasoning mode. The second save merges over the first.
  {
    kind: 'saveIndexingSettings',
    defaults: DEFAULTS,
    partial: {
      reasoningMode: 'accuracy_first',
      autoIndexIntervalMinutes: 45.5,
      autoDreamIntervalMinutes: 99999,
    },
  },
  {
    kind: 'saveIndexingSettings',
    defaults: DEFAULTS,
    partial: {
      reasoningMode: 'bogus',
      autoIndexIntervalMinutes: '60.9',
      autoDreamIntervalMinutes: '-3',
    },
  },

  // Corrupt JSON text that JS `getPipelineState` parses to `undefined`.
  {
    kind: 'rawPipelineInsert',
    key: 'brokenJson',
    json: '{not json',
    updatedAt: T('00:00'),
  },
];

// Applied only after the first snapshot; exercises the save-side `limit`
// truncation without hiding the richer history above from the oracle.
const LIMIT_OPERATIONS: any[] = [
  { kind: 'saveCaseTrace', record: caseTrace('case-limit', T('20:00')), limit: 2 },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-limit', T('20:00')),
    limit: 1,
  },
  {
    kind: 'saveDreamTrace',
    record: dreamTrace('dr-limit', T('20:00'), 'completed', dreamOutcome()),
    limit: 0,
  },
];

function applyOperations(repo: any, operations: any[], dbPath: string): void {
  for (const operation of operations) {
    switch (operation.kind) {
      case 'setPipelineState':
        repo.setPipelineState(operation.key, operation.value);
        break;
      case 'deletePipelineState':
        repo.deletePipelineState(operation.key);
        break;
      case 'insertL0Session':
        repo.insertL0Session(operation.record);
        break;
      case 'saveCaseTrace':
        repo.saveCaseTrace(operation.record, operation.limit);
        break;
      case 'saveIndexTrace':
        repo.saveIndexTrace(operation.record, operation.limit);
        break;
      case 'saveDreamTrace':
        repo.saveDreamTrace(operation.record, operation.limit);
        break;
      case 'saveIndexingSettings':
        repo.saveIndexingSettings(operation.partial, operation.defaults);
        break;
      case 'rawPipelineInsert': {
        const db = new DatabaseSync(dbPath);
        try {
          db.prepare(
            'INSERT INTO pipeline_state (state_key, state_json, updated_at) VALUES (?, ?, ?)',
          ).run(operation.key, operation.json, operation.updatedAt);
        } finally {
          db.close();
        }
        break;
      }
      default:
        throw new Error(`unknown operation ${operation.kind}`);
    }
  }
}

function withRepo(dbPath: string, run: (repo: any) => void): void {
  const repo = openRepo(dbPath);
  try {
    run(repo);
  } finally {
    repo.close();
  }
}

// -- scenario: TS writes, TS reads -----------------------------------------

const tsReadPath = join(scenarioDir('ts-read'), 'memory.db');
let tsReadObservations: any;
withRepo(tsReadPath, (repo) => {
  applyOperations(repo, OPERATIONS, tsReadPath);
  tsReadObservations = observe(repo);
});
const tsRead = { observations: tsReadObservations, ...readArtifacts(tsReadPath) };

// -- scenario: empty database ----------------------------------------------

const emptyPath = join(scenarioDir('empty'), 'memory.db');
let emptyObservations: any;
withRepo(emptyPath, (repo) => {
  emptyObservations = observe(repo);
});
const empty = { observations: emptyObservations, ...readArtifacts(emptyPath) };

// -- scenario: legacy schema -> migration ----------------------------------

const migrationPath = join(scenarioDir('migration'), 'memory.db');
{
  const legacy = new DatabaseSync(migrationPath);
  try {
    legacy.exec(`
      CREATE TABLE pipeline_state (
        state_key TEXT PRIMARY KEY,
        state_json TEXT,
        state_value TEXT,
        updated_at TEXT NOT NULL
      );
    `);
    const insert = legacy.prepare(
      'INSERT INTO pipeline_state (state_key, state_json, state_value, updated_at) VALUES (?, ?, ?, ?)',
    );
    insert.run(
      'workspaceDir',
      JSON.stringify('/legacy/workspace'),
      null,
      '2026-01-01T00:00:00.000Z',
    );
    insert.run(
      'legacyOnly',
      null,
      JSON.stringify({ from: 'state_value' }),
      '2026-01-02T00:00:00.000Z',
    );
    insert.run(
      'bothColumns',
      JSON.stringify('state_json wins'),
      JSON.stringify('state_value loses'),
      '2026-01-03T00:00:00.000Z',
    );
    insert.run(
      'recentCaseTraces',
      JSON.stringify([caseTrace('legacy-case', '2025-01-01T00:00:00.000Z')]),
      null,
      '2026-01-04T00:00:00.000Z',
    );
  } finally {
    legacy.close();
  }
}
const legacyBase64 = readDbBase64(migrationPath);
let migrationObservations: any;
withRepo(migrationPath, (repo) => {
  migrationObservations = observe(repo);
});
const migration = {
  legacyBase64,
  observations: migrationObservations,
  ...readArtifacts(migrationPath),
};

// -- scenario: the portable script, both snapshots -------------------------

const rustWritesPath = join(scenarioDir('rust-writes'), 'memory.db');
let rustWritesObservations: any;
let rustWritesLimitedObservations: any;
withRepo(rustWritesPath, (repo) => {
  applyOperations(repo, OPERATIONS, rustWritesPath);
  rustWritesObservations = observe(repo);
  applyOperations(repo, LIMIT_OPERATIONS, rustWritesPath);
  rustWritesLimitedObservations = observe(repo);
});
const rustWrites = {
  operations: OPERATIONS,
  limitOperations: LIMIT_OPERATIONS,
  observations: rustWritesObservations,
  limitedObservations: rustWritesLimitedObservations,
  ...readArtifacts(rustWritesPath),
};

// The save-side truncation must have actually truncated, otherwise the fixture
// would pass even if `limit` were ignored.
assert.equal(rustWritesLimitedObservations.caseTraces.recent30.length, 2);
assert.equal(rustWritesLimitedObservations.caseTraces.recent30[0].caseId, 'case-limit');
assert.equal(rustWritesLimitedObservations.indexTraces.recent30.length, 1);
assert.equal(rustWritesLimitedObservations.dreamTraces.recent30.length, 1);

const oracle = {
  sourceCommit: provenance.ref,
  sourceSha256,
  nodeVersion: process.version,
  sqliteVersion: process.versions.sqlite,
  tsRead,
  empty,
  migration,
  rustWrites,
};

mkdirSync(output, { recursive: true });
const oraclePath = join(output, 'oracle.json');
const text = `${JSON.stringify(oracle, null, 2)}\n`;
writeFileSync(oraclePath, text);
const sha256 = createHash('sha256').update(text).digest('hex');
console.log(`wrote ${oraclePath}`);
console.log(`oracle sha256 ${sha256}`);
console.log('scenarios: tsRead, empty, migration, rustWrites');
// Only cleaned up after every assertion passed; a failure keeps the temp
// databases around for inspection.
rmSync(root, { recursive: true, force: true });
