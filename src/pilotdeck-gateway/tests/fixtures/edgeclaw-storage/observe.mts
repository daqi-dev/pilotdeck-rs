// Shared observation surface for the EdgeClaw sqlite storage fixture.
//
// Both the real-TS generator and the Rust->TS verifier open a database through
// the unmodified `MemoryRepository` and call exactly the methods ported by
// A6.6-pr10a. The Rust suite mirrors this function against `EdgeClawStorage`;
// keys are compared after canonical sorting, arrays keep their order.
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { DatabaseSync } from 'node:sqlite';

export const DEFAULTS = {
  reasoningMode: 'answer_first',
  autoIndexIntervalMinutes: 5,
  autoDreamIntervalMinutes: 10,
} as const;
export const OTHER_DEFAULTS = {
  reasoningMode: 'accuracy_first',
  autoIndexIntervalMinutes: 1,
  autoDreamIntervalMinutes: 2,
} as const;

/** The same calls the Rust port exposes, with `undefined` mapped to null. */
export function observe(repo: any): any {
  const caseRecent = repo.listRecentCaseTraces(30);
  const indexRecent = repo.listRecentIndexTraces(30);
  const dreamRecent = repo.listRecentDreamTraces(30);
  assertDescendingLocale(caseRecent.map((record: any) => record.startedAt));
  assertDescendingLocale(indexRecent.map((record: any) => record.startedAt));
  assertDescendingLocale(dreamRecent.map((record: any) => record.startedAt));
  const firstCase = caseRecent[0]?.caseId;
  const firstIndex = indexRecent[0]?.indexTraceId;
  const firstDream = dreamRecent[0]?.dreamTraceId;
  return {
    pipeline: {
      workspaceDir: repo.getPipelineState('workspaceDir') ?? null,
      lastDreamAt: repo.getPipelineState('lastDreamAt') ?? null,
      missing: repo.getPipelineState('missing') ?? null,
      brokenJson: repo.getPipelineState('brokenJson') ?? null,
      nullState: repo.getPipelineState('nullState') ?? null,
      afterDelete: repo.getPipelineState('afterDelete') ?? null,
      deleted: repo.getPipelineState('temp') ?? null,
      loneSurrogate: repo.getPipelineState('loneSurrogate') ?? null,
      objectState: repo.getPipelineState('objectState') ?? null,
      arrayState: repo.getPipelineState('arrayState') ?? null,
      nulSuffix: repo.getPipelineState('nulSuffix') ?? null,
    },
    sessions: {
      pendingAll: repo.listPendingSessionKeys(),
      pendingLimit1: repo.listPendingSessionKeys(1),
      pendingLimit0: repo.listPendingSessionKeys(0),
      pendingPreferred: repo.listPendingSessionKeys(50, ['s2', 's4', ' ']),
      pendingPreferredLimit1: repo.listPendingSessionKeys(1, ['s2', 's4']),
      pendingPreferredEmpty: repo.listPendingSessionKeys(50, []),
      pendingPreferredNone: repo.listPendingSessionKeys(50, ['none']),
      countAll: repo.countPendingDialogueTurns(),
      countPreferred: repo.countPendingDialogueTurns(['s1']),
      countPreferredBlank: repo.countPendingDialogueTurns([' ']),
      earliestAll: repo.getEarliestPendingTimestamp() ?? null,
      earliestPreferred: repo.getEarliestPendingTimestamp(['s2']) ?? null,
      earliestNone: repo.getEarliestPendingTimestamp(['none']) ?? null,
      earliestBlank: repo.getEarliestPendingTimestamp(['s-blank']) ?? null,
      earliestNul: repo.getEarliestPendingTimestamp(['s-nul']) ?? null,
    },
    settings: {
      defaults: repo.getIndexingSettings(DEFAULTS),
      otherDefaults: repo.getIndexingSettings(OTHER_DEFAULTS),
    },
    caseTraces: {
      recent30: caseRecent,
      recent2: repo.listRecentCaseTraces(2),
      recent0: repo.listRecentCaseTraces(0),
      first: firstCase === undefined ? null : repo.getCaseTrace(firstCase) ?? null,
      missing: repo.getCaseTrace('missing') ?? null,
    },
    indexTraces: {
      recent30: indexRecent,
      recent2: repo.listRecentIndexTraces(2),
      recent0: repo.listRecentIndexTraces(0),
      first: firstIndex === undefined ? null : repo.getIndexTrace(firstIndex) ?? null,
      missing: repo.getIndexTrace('missing') ?? null,
    },
    dreamTraces: {
      recent30: dreamRecent,
      recent2: repo.listRecentDreamTraces(2),
      recent0: repo.listRecentDreamTraces(0),
      first: firstDream === undefined ? null : repo.getDreamTrace(firstDream) ?? null,
      missing: repo.getDreamTrace('missing') ?? null,
    },
  };
}

/**
 * The Rust sanitizer sorts with the same pinned Node/ICU collator, so every
 * adjacent pair must be non-ascending under `localeCompare`. The corpus check
 * in `generate.mts` additionally asserts that locale and code-unit order
 * actually disagree for at least one planned key ("Z" vs "a").
 */
function assertDescendingLocale(values: string[]) {
  for (let index = 1; index < values.length; index += 1) {
    const left = values[index - 1];
    const right = values[index];
    assert.ok(
      left.localeCompare(right) >= 0,
      `localeCompare order is not descending: ${JSON.stringify(left)} / ${JSON.stringify(right)}`,
    );
  }
}

export function dumpSchema(dbPath: string): any[] {
  return withDatabase(dbPath, (db) =>
    db
      .prepare('SELECT type, name, tbl_name AS tblName, sql FROM sqlite_master ORDER BY rowid')
      .all()
      .map((row: any) => ({
        type: String(row.type),
        name: String(row.name),
        tblName: String(row.tblName),
        sql: row.sql === null ? null : String(row.sql),
      })),
  );
}

export function dumpRows(dbPath: string): any {
  return withDatabase(dbPath, (db) => ({
    l0_sessions: db
      .prepare(
        `SELECT l0_index_id, session_key, timestamp, messages_json, source, indexed, created_at
         FROM l0_sessions ORDER BY l0_index_id`,
      )
      .all()
      .map((row: any) => ({
        l0_index_id: String(row.l0_index_id),
        session_key: String(row.session_key),
        timestamp: String(row.timestamp),
        messages_json: String(row.messages_json),
        source: String(row.source),
        indexed: Number(row.indexed),
        created_at: String(row.created_at),
      })),
    pipeline_state: db
      .prepare('SELECT state_key, state_json, updated_at FROM pipeline_state ORDER BY state_key')
      .all()
      .map((row: any) => ({
        state_key: String(row.state_key),
        state_json: String(row.state_json),
        updated_at: String(row.updated_at),
      })),
  }));
}

export function tableInfo(dbPath: string, table: string): any[] {
  return withDatabase(dbPath, (db) =>
    db
      .prepare(`PRAGMA table_info(${table})`)
      .all()
      .map((row: any) => ({
        cid: Number(row.cid),
        name: String(row.name),
        type: String(row.type),
        notnull: Number(row.notnull),
        dflt_value: row.dflt_value === null ? null : String(row.dflt_value),
        pk: Number(row.pk),
      })),
  );
}

export function readDbBase64(dbPath: string): string {
  if (existsSync(`${dbPath}-wal`)) {
    const db = new DatabaseSync(dbPath);
    db.exec('PRAGMA wal_checkpoint(TRUNCATE);');
    db.close();
  }
  assert.equal(existsSync(`${dbPath}-wal`), false, `WAL not checkpointed: ${dbPath}-wal`);
  assert.equal(existsSync(`${dbPath}-shm`), false, `SHM left behind: ${dbPath}-shm`);
  return readFileSync(dbPath).toString('base64');
}

function withDatabase<T>(dbPath: string, run: (db: any) => T): T {
  const db = new DatabaseSync(dbPath);
  try {
    return run(db);
  } finally {
    db.close();
  }
}
