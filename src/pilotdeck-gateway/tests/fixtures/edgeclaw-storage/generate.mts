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
import { sourceClosureSha256 } from '../../baseline_source_guard.mjs';
import { bindBaseline } from '../_shared/generator_guard.mjs';
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
const provenance = bindBaseline({ baseline: baseline, commit: BASELINE_COMMIT, gitExecutable: gitExecutable }).provenance;
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

function without(record: Record<string, unknown>, key: string) {
  const copy = { ...record };
  delete copy[key];
  return copy;
}

// The corpus must actually distinguish `localeCompare` from UTF-16 code-unit
// order, otherwise a code-unit approximation would pass the oracle.
assert.equal(Math.sign('Z'.localeCompare('a')), 1);
assert.equal('Z' < 'a', true);

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
  // Default branches: empty `source` -> "openclaw", empty `createdAt` ->
  // nowIso() (frozen above). The Rust replay masks only this row's
  // `created_at` because it cannot inject the frozen clock.
  {
    kind: 'insertL0Session',
    record: session('l0-defaults', 's-defaults', T('04:00'), '', { source: '' }),
  },
  // NUL bytes: node:sqlite binds the full bytes (`hex()` proves it) but TEXT
  // reads stop at the first NUL. These rows lock that read behavior for
  // session_key, timestamp and created_at/source.
  {
    kind: 'insertL0Session',
    record: session('l0-nul-key', 'a\0b', T('06:30'), '2026-10-20T06:29:00.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-nul-ts', 's-nul', '2025\0suffix', '2026-10-20T05:00:00.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-nul-meta', 's-meta', T('05:30'), '2026\0tail', {
      source: 'oc\0x',
    }),
  },
  // A blank MIN(timestamp) is a valid string but `getEarliestPendingTimestamp`
  // returns undefined after `.trim()`.
  {
    kind: 'insertL0Session',
    record: session('l0-blank-ts', 's-blank', ' ', '2026-10-20T04:30:00.000Z'),
  },
  // Same timestamp, three distinct `created_at` values: `getLatestL0Before`
  // must use the strict `created_at < ?` tie-break, so the exact first key
  // returns nothing and the middle key returns only the oldest tied row.
  {
    kind: 'insertL0Session',
    record: session('l0-tie-1', 's-tie', T('13:00'), '2026-10-20T13:00:00.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-tie-2', 's-tie', T('13:00'), '2026-10-20T13:00:01.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-tie-3', 's-tie', T('13:00'), '2026-10-20T13:00:00.500Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-tie-4', 's-tie', T('12:59'), '2026-10-20T23:59:59.000Z'),
  },
  // One indexed and one pending row in the same session: the unindexed read
  // must keep only the pending one.
  {
    kind: 'insertL0Session',
    record: session('l0-idx-1', 's-idx', T('15:00'), '2026-10-20T14:59:00.000Z', {
      indexed: true,
    }),
  },
  {
    kind: 'insertL0Session',
    record: session('l0-idx-2', 's-idx', T('15:01'), '2026-10-20T15:00:00.000Z'),
  },
  // Raw TEXT rows: invalid JSON, a non-array JSON value, a NUL suffix (read
  // stops at the first NUL), and `normalizeMessages` shapes (non-record items,
  // blank msgId/role, non-string content, extra keys).
  {
    kind: 'rawL0Insert',
    record: {
      l0IndexId: 'l0-raw-nul',
      sessionKey: 's-raw',
      timestamp: T('14:00'),
      messagesJson: '[{"role":"user","content":"nul"}]\0garbage',
      source: 'cli',
      indexed: 0,
      createdAt: '2026-10-20T13:59:00.000Z',
    },
  },
  {
    kind: 'rawL0Insert',
    record: {
      l0IndexId: 'l0-raw-bad',
      sessionKey: 's-raw',
      timestamp: T('14:01'),
      messagesJson: '{not json',
      source: 'cli',
      indexed: 0,
      createdAt: '2026-10-20T14:00:30.000Z',
    },
  },
  {
    kind: 'rawL0Insert',
    record: {
      l0IndexId: 'l0-raw-object',
      sessionKey: 's-raw',
      timestamp: T('14:02'),
      messagesJson: '{"role":"user","content":"object"}',
      source: 'cli',
      indexed: 0,
      createdAt: '2026-10-20T14:01:30.000Z',
    },
  },
  {
    kind: 'rawL0Insert',
    record: {
      l0IndexId: 'l0-raw-normalize',
      sessionKey: 's-raw',
      timestamp: T('14:03'),
      messagesJson:
        '[1,null,{"msgId":"  ","role":"  ","content":"x","extra":1},{"msgId":"m","role":"assistant","content":5},{"msgId":" m2 ","content":"ok"}]',
      source: 'cli',
      indexed: 0,
      createdAt: '2026-10-20T14:02:30.000Z',
    },
  },
  // `markL0Indexed`: duplicate/blank/missing ids are filtered or deduped; the
  // empty and blank-only calls are no-ops. Marking `l0-a` leaves `s1` with no
  // pending rows because `l0-d` is already indexed.
  {
    kind: 'markL0Indexed',
    ids: ['l0-a', 'l0-a', ' ', 'l0-missing', ''],
  },
  { kind: 'markL0Indexed', ids: [] },
  { kind: 'markL0Indexed', ids: [' ', ''] },

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
  // `localeCompare` vs UTF-16 code-unit order: en-US sorts "0a" before "0Z"
  // even though '0Z' < '0a' by code unit. The "0" prefix keeps these below
  // every ISO timestamp used above. For the duplicate id the locale sort
  // keeps the "0Z" record; the code-unit approximation kept "0a".
  { kind: 'saveCaseTrace', record: caseTrace('case-collate-z', '0Z') },
  { kind: 'saveCaseTrace', record: caseTrace('case-collate-a', '0a') },
  { kind: 'saveCaseTrace', record: caseTrace('case-collate-dup', '0Z') },
  { kind: 'saveCaseTrace', record: caseTrace('case-collate-dup', '0a') },

  // Unpaired surrogates in `startedAt`: Node/ICU4C collates them as distinct
  // UCA implicit weights in code-unit order (\ud800 < \ud801 < \udfff), above
  // every assigned character (and below U+FFFD, which the unit test covers).
  // TS therefore saves each one, sorts them descending, and dedupes by id; the
  // Rust port must do the same instead of failing with `HostRequired`. These
  // seven saves also cover consecutive saves of distinct surrogates, the
  // existing-state query (`getPipelineState('recentCaseTraces')`), and the
  // duplicate-id retention.
  { kind: 'saveCaseTrace', record: caseTrace('case-surrogate-a', '\ud800a') },
  { kind: 'saveCaseTrace', record: caseTrace('case-surrogate-b', '\ud800b') },
  { kind: 'saveCaseTrace', record: caseTrace('case-surrogate-c', '\ud801') },
  { kind: 'saveCaseTrace', record: caseTrace('case-surrogate-d', '\udfff') },
  { kind: 'saveCaseTrace', record: caseTrace('case-surrogate-z', 'zzz') },
  { kind: 'saveCaseTrace', record: caseTrace('case-surrogate-dup', '\ud800a') },
  { kind: 'saveCaseTrace', record: caseTrace('case-surrogate-dup', '\ud801a') },

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
  // Short-circuit and JS property/length semantics. These shapes are accepted
  // by `normalizeIndexTraceRecord`; only a missing/null `storedResults` on a
  // completed record throws (covered by the errors scenario).
  {
    kind: 'saveIndexTrace',
    record: without(indexTrace('idx-g', T('17:00'), { status: 'running' }), 'storedResults'),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-h', T('18:00'), { storedResults: '' }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-i', T('19:00'), { storedResults: { length: 0 } }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-j', T('20:00'), { storedResults: 5 }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-k', T('21:00'), { storedResults: 'x' }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-l', T('22:00'), { isNoOp: 'true', storedResults: [] }),
  },
  {
    kind: 'saveIndexTrace',
    record: indexTrace('idx-m', T('23:00'), { status: 5, storedResults: [] }),
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
  // Dream short-circuit and primitive `outcome` shapes: `status === "error"`
  // never reads outcome, and property access on a number/string/array yields
  // undefined (no throw).
  {
    kind: 'saveDreamTrace',
    record: dreamTrace('dr-g', T('15:00'), 'completed', 5 as any),
  },
  {
    kind: 'saveDreamTrace',
    record: without(dreamTrace('dr-h', T('16:00'), 'error', {}), 'outcome'),
  },
  {
    kind: 'saveDreamTrace',
    record: dreamTrace('dr-i', T('17:00'), 'completed', {
      deletedFiles: '0',
      rewrittenProjects: 0,
      profileUpdated: false,
    }),
  },
  {
    kind: 'saveDreamTrace',
    record: dreamTrace('dr-j', T('18:00'), 'completed', {
      deletedFiles: 0,
      rewrittenProjects: 0,
      profileUpdated: 0,
    }),
  },
  {
    kind: 'saveDreamTrace',
    record: dreamTrace('dr-k', T('19:00'), 'success', {
      deletedFiles: -0,
      rewrittenProjects: 0,
      profileUpdated: false,
    }),
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
  // Raw NUL in TEXT: the stored bytes keep the suffix, but node:sqlite reads
  // the value as the valid JSON prefix (the suffix is never parsed).
  {
    kind: 'rawPipelineInsert',
    key: 'nulSuffix',
    json: '{"a":1}\0garbage',
    updatedAt: T('00:00'),
  },
];

// Applied only after the first snapshot; exercises the save-side `limit`
// truncation without hiding the richer history above from the oracle.
const LIMIT_OPERATIONS: any[] = [
  // Sorts above the surrogate corpus, so the assertion below still checks that
  // the newest record survives the truncation.
  {
    kind: 'saveCaseTrace',
    record: caseTrace('case-limit', '\udfffz'),
    limit: 2,
  },
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

// -- repair corpus and transform script ------------------------------------
//
// `repairL0Sessions` compares `JSON.stringify(nextMessages)` with the row it
// read, so the corpus covers: unchanged rows, a partial filter (update), a
// filter that empties the array (remove), a rewrite, an already-indexed row
// (the update must reset `indexed`), invalid/non-array JSON, a NUL suffix, and
// `normalizeMessages` defaults inside the callback input.
const REPAIR_OPERATIONS: any[] = [
  {
    kind: 'insertL0Session',
    record: session('r-keep', 's-repair', T('10:00'), '2026-10-20T09:59:00.000Z'),
  },
  {
    kind: 'insertL0Session',
    record: session('r-drop', 's-repair', T('10:01'), '2026-10-20T10:00:30.000Z', {
      messages: [{ msgId: 'm-drop', role: 'user', content: 'drop-me' }],
    }),
  },
  {
    kind: 'insertL0Session',
    record: session('r-rewrite', 's-repair', T('10:02'), '2026-10-20T10:01:30.000Z', {
      messages: [{ msgId: 'm-rw', role: 'assistant', content: 'rewrite-me' }],
    }),
  },
  {
    kind: 'insertL0Session',
    record: session('r-partial', 's-repair', T('10:03'), '2026-10-20T10:02:30.000Z', {
      messages: [{ content: 'keep' }, { content: 'drop-me' }],
    }),
  },
  {
    kind: 'insertL0Session',
    record: session('r-other', 's-other', T('10:04'), '2026-10-20T10:03:30.000Z', {
      messages: [
        { role: 'user', content: 'alpha' },
        { role: 'assistant', content: 'beta' },
      ],
    }),
  },
  {
    kind: 'insertL0Session',
    record: session('r-empty', 's-repair', T('10:05'), '2026-10-20T10:04:30.000Z', {
      messages: [],
    }),
  },
  {
    kind: 'insertL0Session',
    record: session('r-indexed', 's-repair', T('10:06'), '2026-10-20T10:05:30.000Z', {
      messages: [{ role: 'user', content: 'rewrite-me' }],
      indexed: true,
    }),
  },
  {
    kind: 'rawL0Insert',
    record: {
      l0IndexId: 'r-raw-nul',
      sessionKey: 's-repair',
      timestamp: T('10:07'),
      messagesJson: '[{"role":"user","content":"drop-me"}]\0tail',
      source: 'cli',
      indexed: 0,
      createdAt: '2026-10-20T10:06:30.000Z',
    },
  },
  {
    kind: 'rawL0Insert',
    record: {
      l0IndexId: 'r-raw-bad',
      sessionKey: 's-repair',
      timestamp: T('10:08'),
      messagesJson: '{not json',
      source: 'cli',
      indexed: 0,
      createdAt: '2026-10-20T10:07:30.000Z',
    },
  },
  {
    kind: 'rawL0Insert',
    record: {
      l0IndexId: 'r-raw-normalize',
      sessionKey: 's-repair',
      timestamp: T('10:09'),
      messagesJson: '[1,null,{"msgId":" ","role":" ","content":"keep","x":1}]',
      source: 'cli',
      indexed: 0,
      createdAt: '2026-10-20T10:08:30.000Z',
    },
  },
];

const REPAIR_STEPS: any[] = [
  { kind: 'filterContent', content: 'drop-me' },
  { kind: 'rewriteContent', from: 'rewrite-me', to: 'rewritten' },
  { kind: 'identity' },
];

function repairTransform(step: any, record: any): any[] {
  if (step.kind === 'identity') return record.messages;
  if (step.kind === 'filterContent') {
    return record.messages.filter((message: any) => message.content !== step.content);
  }
  if (step.kind === 'rewriteContent') {
    return record.messages.map((message: any) =>
      message.content === step.from ? { ...message, content: step.to } : message,
    );
  }
  throw new Error(`unknown repair transform ${step.kind}`);
}


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
      case 'markL0Indexed':
        repo.markL0Indexed(operation.ids);
        break;
      case 'rawL0Insert': {
        const record = operation.record;
        const db = new DatabaseSync(dbPath);
        try {
          db.prepare(
            `INSERT INTO l0_sessions
               (l0_index_id, session_key, timestamp, messages_json, source, indexed, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)`,
          ).run(
            record.l0IndexId,
            record.sessionKey,
            record.timestamp,
            record.messagesJson,
            record.source,
            record.indexed,
            record.createdAt,
          );
        } finally {
          db.close();
        }
        break;
      }
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

// -- scenario: calls the baseline rejects -----------------------------------
//
// A `TypeError` from normalization or from spreading a non-iterable stored
// state must surface as an error in Rust too, and must not write a value that
// TS would have left untouched. `name`/`message` are the real Node texts.
const errorAttempts: any[] = [];
const errorsPath = join(scenarioDir('errors'), 'memory.db');
withRepo(errorsPath, (repo) => {
  const attempt = (label: string, stateKey: string, record: any, run: () => void) => {
    let threw = false;
    let name: string | null = null;
    let message: string | null = null;
    try {
      run();
    } catch (error: any) {
      threw = true;
      name = error?.name ?? null;
      message = error?.message ?? null;
    }
    errorAttempts.push({
      label,
      record,
      threw,
      name,
      message,
      stateKey,
      state: repo.getPipelineState(stateKey) ?? null,
    });
  };
  const missingStored = {
    indexTraceId: 'idx-throw',
    startedAt: T('01:00'),
    status: 'completed',
  };
  attempt('index-completed-missing-storedResults', 'recentIndexTraces', missingStored, () =>
    repo.saveIndexTrace(missingStored),
  );
  const missingOutcome = {
    dreamTraceId: 'dr-throw',
    startedAt: T('02:00'),
    status: 'completed',
  };
  attempt('dream-completed-missing-outcome', 'recentDreamTraces', missingOutcome, () =>
    repo.saveDreamTrace(missingOutcome),
  );
  repo.setPipelineState('recentCaseTraces', { a: 1 });
  const objectStateRecord = caseTrace('case-throw', T('03:00'));
  attempt('case-state-object', 'recentCaseTraces', objectStateRecord, () =>
    repo.saveCaseTrace(objectStateRecord),
  );
  repo.setPipelineState('recentIndexTraces', null);
  const nullStateRecord = indexTrace('idx-throw-null', T('04:00'));
  attempt('index-state-null', 'recentIndexTraces', nullStateRecord, () =>
    repo.saveIndexTrace(nullStateRecord),
  );
  // A string is iterable; the spread produces characters that the sanitizer
  // drops, so the save succeeds with just the new record.
  repo.setPipelineState('recentCaseTraces', 'abc');
  const stringStateRecord = caseTrace('case-string', T('05:00'));
  attempt('case-state-string', 'recentCaseTraces', stringStateRecord, () =>
    repo.saveCaseTrace(stringStateRecord),
  );
});
const errors = { attempts: errorAttempts };

// -- scenario: repair before/after ------------------------------------------
//
// The pre-repair database is exported as base64 so the Rust suite starts from
// bytes a real TS process wrote; the same insert script is also replayed by
// Rust and handed back to `verify.mts` for the reverse direction.
const repairPath = join(scenarioDir('repair'), 'memory.db');
withRepo(repairPath, (repo) => {
  applyOperations(repo, REPAIR_OPERATIONS, repairPath);
});
const repairBeforeArtifacts = readArtifacts(repairPath);
let repairBefore: any;
const repairAfterSteps: any[] = [];
withRepo(repairPath, (repo) => {
  repairBefore = observe(repo);
  for (const step of REPAIR_STEPS) {
    const result = repo.repairL0Sessions((record: any) => repairTransform(step, record));
    repairAfterSteps.push({ step, result, observations: observe(repo) });
  }
});
const repairAfterArtifacts = readArtifacts(repairPath);
const repair = {
  operations: REPAIR_OPERATIONS,
  steps: REPAIR_STEPS,
  beforeBase64: repairBeforeArtifacts.dbBase64,
  before: repairBefore,
  beforeRows: repairBeforeArtifacts.rows,
  afterSteps: repairAfterSteps,
  schema: repairAfterArtifacts.schema,
  columns: repairAfterArtifacts.columns,
  rows: repairAfterArtifacts.rows,
};

assert.deepEqual(
  repairAfterSteps.map((entry) => entry.result),
  [
    { inspected: 10, updated: 1, removed: 4, rebuilt: true },
    { inspected: 6, updated: 2, removed: 0, rebuilt: true },
    { inspected: 6, updated: 0, removed: 0, rebuilt: false },
  ],
);
assert.equal(repairBefore.l0.all.length, 10);
assert.equal(repairAfterSteps[2].observations.l0.all.length, 6);
assert.equal(
  repairAfterSteps[1].observations.l0.all.find((row: any) => row.l0IndexId === 'r-indexed')
    .indexed,
  false,
  'a repaired row must be marked unindexed again',
);

// The save-side truncation must have actually truncated, otherwise the fixture
// would pass even if `limit` were ignored.
assert.equal(rustWritesLimitedObservations.caseTraces.recent30.length, 2);
assert.equal(rustWritesLimitedObservations.caseTraces.recent30[0].caseId, 'case-limit');
assert.equal(rustWritesLimitedObservations.indexTraces.recent30.length, 1);
assert.equal(rustWritesLimitedObservations.dreamTraces.recent30.length, 1);

// L0 read corpus: the oracle must actually distinguish unindexed filtering,
// ordering, the strict tie-break, clamps, and `normalizeMessages`.
const tsL0 = tsReadObservations.l0;
assert.equal(tsL0.unindexedS1.length, 0);
assert.equal(tsL0.unindexedS2.length, 2);
assert.deepEqual(
  tsL0.unindexedS2.map((record: any) => record.l0IndexId),
  ['l0-c', 'l0-b'],
);
assert.equal(tsL0.unindexedSTie.length, 4);
assert.equal(tsL0.unindexedSRaw.length, 4);
assert.equal(tsL0.unindexedSIdx.length, 1);
assert.equal(tsL0.unindexedMissing.length, 0);
assert.equal(tsL0.latestExactKey, null);
assert.equal(tsL0.latestSTieMid.l0IndexId, 'l0-tie-1');
assert.equal(tsL0.latestSTieHigh.l0IndexId, 'l0-tie-3');
assert.equal(tsL0.latestSTieAfterAll.l0IndexId, 'l0-tie-2');
assert.equal(tsL0.byIds.length, 2);
assert.equal(tsL0.recent0.length, 1);
assert.equal(tsL0.recentNeg.length, 1);
assert.equal(tsL0.recentOffsetPastEnd.length, 0);
assert.equal(tsL0.all.length, 22);
assert.equal(tsL0.recentAll.length, 22);
assert.equal(tsL0.recentOffset1.length, 3);
assert.deepEqual(
  tsL0.all.find((record: any) => record.l0IndexId === 'l0-raw-normalize').messages,
  [
    { role: 'user', content: 'x' },
    { msgId: 'm', role: 'assistant', content: '' },
    { msgId: ' m2 ', role: 'user', content: 'ok' },
  ],
);
assert.deepEqual(
  tsL0.all.find((record: any) => record.l0IndexId === 'l0-raw-nul').messages,
  [{ role: 'user', content: 'nul' }],
);
assert.equal(
  tsL0.all.find((record: any) => record.l0IndexId === 'l0-raw-bad').messages.length,
  0,
);
assert.equal(tsL0.unindexedNulKey[0].sessionKey, 'a');
assert.equal(tsL0.unindexedTruncatedKey.length, 0);

const oracle = {
  sourceCommit: provenance.ref,
  sourceSha256,
  nodeVersion: process.version,
  sqliteVersion: process.versions.sqlite,
  tsRead,
  empty,
  migration,
  rustWrites,
  errors,
  repair,
};

mkdirSync(output, { recursive: true });
const oraclePath = join(output, 'oracle.json');
const text = `${JSON.stringify(oracle, null, 2)}\n`;
writeFileSync(oraclePath, text);
const sha256 = createHash('sha256').update(text).digest('hex');
console.log(`wrote ${oraclePath}`);
console.log(`oracle sha256 ${sha256}`);
console.log('scenarios: tsRead, empty, migration, rustWrites, errors, repair');
// Only cleaned up after every assertion passed; a failure keeps the temp
// databases around for inspection.
rmSync(root, { recursive: true, force: true });
