//! SQLite-backed EdgeClaw memory storage.
//!
//! The SQL text, statement order, conflict clauses, pending-session ordering,
//! L0 read/repair semantics and trace sanitation mirror
//! `core/storage/sqlite.ts:541-590,916-1020,1028-1176` one-for-one;
//! `sqlite_master.sql` is compared verbatim against the real TS-created
//! database by `tests/edgeclaw_storage.rs`.
//!
//! JSON columns are JS-owned values, so they use
//! [`pilotdeck_protocol::parse_json`] / [`pilotdeck_protocol::stringify_json`]
//! (WTF-16 preserving) rather than `serde_json`: a trace or pipeline value may
//! contain a lone surrogate, which `JSON.stringify` writes as `\ud800`.

use crate::{
    metadata::js_trim,
    project_files::{node_collator, node_compare_utf16, node_locale},
    runtime::memory::{js_is_space_unit, js_trim_units},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as SqlValue};
use std::{
    collections::HashSet,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::file_memory::FileMemoryClock;

const INDEXING_SETTINGS_STATE_KEY: &str = "indexingSettings";
const RECENT_CASE_TRACES_STATE_KEY: &str = "recentCaseTraces";
const RECENT_INDEX_TRACES_STATE_KEY: &str = "recentIndexTraces";
const RECENT_DREAM_TRACES_STATE_KEY: &str = "recentDreamTraces";

/// `sqlite.ts:543-565`, character-identical so `sqlite_master.sql` matches the
/// TS-created database byte for byte (SQLite stores the statement text).
const INIT_SQL: &str = "CREATE TABLE IF NOT EXISTS l0_sessions (
        l0_index_id TEXT PRIMARY KEY,
        session_key TEXT NOT NULL,
        timestamp TEXT NOT NULL,
        messages_json TEXT NOT NULL,
        source TEXT NOT NULL,
        indexed INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL
      );
      CREATE INDEX IF NOT EXISTS idx_l0_sessions_session ON l0_sessions(session_key);
      CREATE INDEX IF NOT EXISTS idx_l0_sessions_pending ON l0_sessions(indexed, timestamp);
      CREATE TABLE IF NOT EXISTS pipeline_state (
        state_key TEXT PRIMARY KEY,
        state_json TEXT NOT NULL,
        updated_at TEXT NOT NULL
      );";

/// `sqlite.ts:575-589`. The rename is left to SQLite exactly as in TS, so the
/// resulting `sqlite_master.sql` text is identical.
const MIGRATE_PIPELINE_STATE_SQL: &str = "CREATE TABLE IF NOT EXISTS pipeline_state_v2 (
        state_key TEXT PRIMARY KEY,
        state_json TEXT NOT NULL,
        updated_at TEXT NOT NULL
      );
      INSERT INTO pipeline_state_v2 (state_key, state_json, updated_at)
      SELECT
        state_key,
        COALESCE(state_json, state_value),
        updated_at
      FROM pipeline_state;
      DROP TABLE pipeline_state;
      ALTER TABLE pipeline_state_v2 RENAME TO pipeline_state;";

#[derive(Debug)]
pub enum StorageError {
    Sqlite(rusqlite::Error),
    Closed,
    Io(std::io::Error),
    /// JSON text that JS would accept but the shared codec rejects, or an
    /// explicit refusal to bind JS `undefined` as a JSON column value.
    Json(String),
    /// `normalizeIndexTraceRecord` / `normalizeDreamTraceRecord` would throw a
    /// JS `TypeError` for this record shape; surfaced instead of silently
    /// normalizing to a different value.
    InvalidTrace(String),
    /// `JSON.stringify(undefined)` has no text; TS hands `undefined` to
    /// `node:sqlite`, which raises a binding `TypeError`.
    Unstringifiable(&'static str),
    /// A stored pipeline value the TS `save*Trace` methods spread directly
    /// (`[record, ...readPipelineState(key, [])]`). Spreading `null`, an
    /// object, a number or a boolean is a JS `TypeError`; a string is
    /// iterable and its characters are dropped by the sanitizer.
    NotIterable(String),
    /// ICU collation data could not be loaded, so `localeCompare`-based trace
    /// ordering cannot be reproduced. (Unpaired surrogates are no longer a
    /// host boundary: the shared `project_files::node_compare_utf16` maps them
    /// onto the unassigned U+E000..U+E7FF band the way Node/ICU4C weights
    /// them.)
    HostRequired(String),
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => write!(f, "database is not open"),
            Self::Sqlite(error) => write!(f, "sqlite: {error}"),
            Self::Io(error) => write!(f, "io: {error}"),
            Self::Json(error) => write!(f, "json: {error}"),
            Self::InvalidTrace(error) => write!(f, "invalid trace record: {error}"),
            Self::Unstringifiable(what) => write!(f, "{what} cannot be represented as JSON text"),
            Self::NotIterable(key) => {
                write!(f, "pipeline state {key} is not iterable (TypeError)")
            }
            Self::HostRequired(error) => write!(f, "requires the Node host: {error}"),
        }
    }
}
impl std::error::Error for StorageError {}
impl From<rusqlite::Error> for StorageError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}
impl From<std::io::Error> for StorageError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// One `l0_sessions` row (`types.ts:175-184`). `messages` is the raw JS array;
/// `created_at` empty takes `nowIso()` like `record.createdAt || nowIso()`.
/// Reads produce `normalizeL0Row` output: TEXT stops at the first NUL and
/// `messages` is filtered/normalized by [`normalize_messages`].
#[derive(Clone, Debug, PartialEq)]
pub struct L0SessionRecord {
    pub l0_index_id: String,
    pub session_key: String,
    pub timestamp: String,
    pub messages: JsValue,
    pub source: String,
    pub indexed: bool,
    pub created_at: String,
}

/// `sqlite.ts:122-127` (`RepairMemoryResult`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RepairMemoryResult {
    pub inspected: usize,
    pub updated: usize,
    pub removed: usize,
    pub rebuilt: bool,
}

/// `types.ts:196-200` (`IndexingSettings`).
///
/// The interval fields keep JS number semantics: `mergeIndexingSettings` and
/// `sanitizeIndexingSettings` can return `NaN` and `±Infinity` (the default
/// merge only floors/clamps finite numbers), so storing `i64` would truncate
/// values the TS baseline preserves.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexingSettings {
    pub reasoning_mode: String,
    pub auto_index_interval_minutes: f64,
    pub auto_dream_interval_minutes: f64,
}
impl IndexingSettings {
    pub fn to_js(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert(
            "reasoningMode".into(),
            JsValue::String(self.reasoning_mode.as_str().into()),
        );
        object.insert(
            "autoIndexIntervalMinutes".into(),
            JsValue::Number(self.auto_index_interval_minutes),
        );
        object.insert(
            "autoDreamIntervalMinutes".into(),
            JsValue::Number(self.auto_dream_interval_minutes),
        );
        JsValue::Object(object)
    }
}

/// A `MemoryRepository` opened on one SQLite file (`sqlite.ts:490-600`).
pub struct EdgeClawStorage {
    path: PathBuf,
    conn: Mutex<Option<Connection>>,
    /// The repository's `nowIso()`, used for `pipeline_state.updated_at`
    /// (`sqlite.ts:921`, read once per `setPipelineState` call) and the L0
    /// `created_at` fallback (`sqlite.ts:941`); injected so replays observe
    /// the same clock reads as TS.
    clock: FileMemoryClock,
}

struct ConnectionGuard<'a>(std::sync::MutexGuard<'a, Option<Connection>>);
impl std::ops::Deref for ConnectionGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.0.as_ref().expect("checked open under lock")
    }
}

impl EdgeClawStorage {
    /// `sqlite.ts:502-566`: create the parent directory, open, enable WAL,
    /// create the base schema, then run the `pipeline_state` migration.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        Self::with_clock(path, Arc::new(now_iso))
    }

    /// [`Self::open`] with the repository's injectable clock.
    pub fn with_clock(
        path: impl AsRef<Path>,
        clock: FileMemoryClock,
    ) -> Result<Self, StorageError> {
        let storage = Self::connect(path, clock)?;
        storage.initialize()?;
        Ok(storage)
    }

    /// Node opens and validates DatabaseSync before file-store construction;
    /// schema initialization follows it. Keep those phases separate (#778).
    pub(super) fn connect(
        path: impl AsRef<Path>,
        clock: FileMemoryClock,
    ) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path).map_err(|error| match error {
            // rusqlite appends the filename to SQLITE_CANTOPEN; Node exposes
            // sqlite3_errmsg alone (native-default sqlite-directory probe).
            rusqlite::Error::SqliteFailure(code, _)
                if code.code == rusqlite::ErrorCode::CannotOpen =>
            {
                rusqlite::Error::SqliteFailure(code, Some("unable to open database file".into()))
            }
            other => other,
        })?;
        // Node DatabaseSync uses no busy wait. A corrupt file is rejected at
        // construction (native-default sqlite-corrupt), before memory files.
        conn.busy_timeout(Duration::ZERO)?;
        conn.query_row("PRAGMA schema_version", [], |row| row.get::<_, i64>(0))?;
        Ok(Self {
            path,
            conn: Mutex::new(Some(conn)),
            clock,
        })
    }

    pub(super) fn initialize(&self) -> Result<(), StorageError> {
        {
            let conn = self.conn()?;
            conn.execute_batch("PRAGMA journal_mode = WAL;")?;
            conn.execute_batch(INIT_SQL)?;
        }
        self.migrate_pipeline_state()
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Close the SQLite handle, retaining the repository's file stores. SQLite
    /// checkpoints the WAL only when this is the last connection. Node's
    /// DatabaseSync.close() and later SQL operations report an already closed handle.
    pub fn close(&self) -> Result<(), StorageError> {
        let mut guard = self
            .conn
            .lock()
            .expect("edgeclaw storage connection mutex poisoned");
        if guard.is_none() {
            return Err(StorageError::Closed);
        }
        if let Some(conn) = guard.take()
            && let Err((conn, error)) = conn.close()
        {
            *guard = Some(conn);
            return Err(error.into());
        }
        Ok(())
    }

    fn conn(&self) -> Result<ConnectionGuard<'_>, StorageError> {
        let guard = self
            .conn
            .lock()
            .expect("edgeclaw storage connection mutex poisoned");
        if guard.is_none() {
            return Err(StorageError::Closed);
        }
        Ok(ConnectionGuard(guard))
    }

    /// `sqlite.ts:567-590`: migrate only when `pipeline_state` still carries a
    /// legacy `state_value` column; an already-v2 table is untouched.
    ///
    /// TS runs the four statements on its single-threaded event loop, so they
    /// never interleave. `exec` still auto-commits each one, which leaves a
    /// cross-process crash window that the baseline itself has. Rust adds a
    /// second, avoidable window: two threads can open the same file at once,
    /// and a second writer's `CREATE TABLE IF NOT EXISTS pipeline_state` can
    /// land between the first writer's `DROP` and `RENAME`, orphaning rows in
    /// `pipeline_state_v2` and failing the first `open()`. Take the write lock
    /// before re-checking the columns so the migration is all-or-nothing;
    /// the common already-v2 path stays a plain read.
    fn migrate_pipeline_state(&self) -> Result<(), StorageError> {
        if !Self::pipeline_state_needs_migration(&*self.conn()?)? {
            return Ok(());
        }
        let conn = self.conn()?;
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let outcome = (|| -> Result<(), StorageError> {
            if !Self::pipeline_state_needs_migration(&conn)? {
                return Ok(());
            }
            conn.execute_batch(MIGRATE_PIPELINE_STATE_SQL)?;
            Ok(())
        })();
        match outcome {
            Ok(()) => {
                conn.execute_batch("COMMIT;")?;
                Ok(())
            }
            Err(error) => {
                // `execute_batch` autocommitted nothing inside our transaction;
                // roll it back so a failed migration leaves the legacy table
                // and its rows exactly as they were.
                let _ = conn.execute_batch("ROLLBACK;");
                Err(error)
            }
        }
    }

    fn pipeline_state_needs_migration(conn: &Connection) -> Result<bool, StorageError> {
        let mut statement = conn.prepare("PRAGMA table_info(pipeline_state)")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        let columns = rows.collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let has_state_json = columns.iter().any(|column| column == "state_json");
        let has_state_value = columns.iter().any(|column| column == "state_value");
        Ok(!((has_state_json && !has_state_value) || (!has_state_json && !has_state_value)))
    }

    // -- pipeline_state ---------------------------------------------------

    /// `sqlite.ts:913-921`. Unparseable text falls back to `undefined`, like
    /// `parseJson(row.state_json, undefined)`.
    pub fn get_pipeline_state(&self, key: &str) -> Result<Option<JsValue>, StorageError> {
        let conn = self.conn()?;
        Self::read_pipeline_state(&conn, key)
    }

    /// Lock-free read; callers already hold the connection guard. Keeping this
    /// separate lets read-modify-write methods hold one guard for the whole
    /// operation, so concurrent Rust callers cannot lose updates (the TS
    /// class gets the same atomicity from its single-threaded event loop).
    fn read_pipeline_state(conn: &Connection, key: &str) -> Result<Option<JsValue>, StorageError> {
        let raw: Option<SqlValue> = conn
            .query_row(
                "SELECT state_json FROM pipeline_state WHERE state_key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        match raw {
            Some(SqlValue::Text(raw)) => Ok(parse_json(&node_text(raw)).ok()),
            _ => Ok(None),
        }
    }

    /// `sqlite.ts:923-928`.
    pub fn set_pipeline_state(&self, key: &str, value: &JsValue) -> Result<(), StorageError> {
        let now = (self.clock)();
        let conn = self.conn()?;
        Self::write_pipeline_state(&conn, key, value, &now)
    }

    fn write_pipeline_state(
        conn: &Connection,
        key: &str,
        value: &JsValue,
        // Read before the write, like the `nowIso()` argument at
        // `sqlite.ts:921`: the clock ticks even when the insert fails.
        now: &str,
    ) -> Result<(), StorageError> {
        let json = stringify_json(value)
            .map_err(|error| StorageError::Json(error.to_string()))?
            .ok_or(StorageError::Unstringifiable("pipeline_state.state_json"))?;
        conn.execute(
            "INSERT INTO pipeline_state (state_key, state_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(state_key) DO UPDATE SET state_json = excluded.state_json, updated_at = excluded.updated_at",
            params![key, json, now],
        )?;
        Ok(())
    }

    /// `sqlite.ts:930-932`.
    pub fn delete_pipeline_state(&self, key: &str) -> Result<(), StorageError> {
        self.conn()?
            .execute("DELETE FROM pipeline_state WHERE state_key = ?1", [key])?;
        Ok(())
    }

    // -- l0_sessions ------------------------------------------------------

    /// `sqlite.ts:934-968`.
    pub fn insert_l0_session(&self, record: &L0SessionRecord) -> Result<(), StorageError> {
        let conn = self.conn()?;
        Self::insert_l0_session_on(&conn, record, &self.clock)
    }

    fn insert_l0_session_on(
        conn: &Connection,
        record: &L0SessionRecord,
        clock: &FileMemoryClock,
    ) -> Result<(), StorageError> {
        let messages = stringify_json(&record.messages)
            .map_err(|error| StorageError::Json(error.to_string()))?
            .ok_or(StorageError::Unstringifiable("l0_sessions.messages_json"))?;
        let source = if record.source.is_empty() {
            "openclaw"
        } else {
            record.source.as_str()
        };
        let created_at = if record.created_at.is_empty() {
            // `record.createdAt || nowIso()` (`sqlite.ts:941`): the clock is
            // only read when the record lacks the date.
            (clock)()
        } else {
            record.created_at.clone()
        };
        conn.execute(
            "INSERT INTO l0_sessions (
               l0_index_id,
               session_key,
               timestamp,
               messages_json,
               source,
               indexed,
               created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(l0_index_id) DO UPDATE SET
               session_key = excluded.session_key,
               timestamp = excluded.timestamp,
               messages_json = excluded.messages_json,
               source = excluded.source,
               indexed = excluded.indexed,
               created_at = excluded.created_at",
            params![
                record.l0_index_id,
                record.session_key,
                record.timestamp,
                messages,
                source,
                i64::from(record.indexed),
                created_at,
            ],
        )?;
        Ok(())
    }

    /// Atomic `captureL0Session` storage step: reads the newest L0 row,
    /// evaluates the caller's duplicate predicate and inserts, all under one
    /// connection guard. TS is safe without a transaction because
    /// `captureL0Session` is synchronous and has no `await`; the Rust indexer
    /// is shared across threads, so splitting this into `list_recent_l0` +
    /// `insert_l0_session` lets two callers both observe the same previous row
    /// and both insert. Returns `false` when the predicate suppressed the
    /// insert.
    pub fn insert_l0_session_if_new(
        &self,
        record: &L0SessionRecord,
        is_duplicate: impl FnOnce(&L0SessionRecord) -> bool,
    ) -> Result<bool, StorageError> {
        let conn = self.conn()?;
        let recent = Self::list_recent_l0_on(&conn, 1, 0)?;
        if let Some(recent) = recent.first()
            && is_duplicate(recent)
        {
            return Ok(false);
        }
        Self::insert_l0_session_on(&conn, record, &self.clock)?;
        Ok(true)
    }

    /// `sqlite.ts:970-1001`: `DISTINCT session_key, MIN(timestamp)` over
    /// pending rows, ascending by first timestamp; the preferred-key branch
    /// applies the limit after fetching, exactly like the TS `.slice`.
    pub fn list_pending_session_keys(
        &self,
        limit: usize,
        preferred_session_keys: Option<&[String]>,
    ) -> Result<Vec<String>, StorageError> {
        let limit = limit.max(1);
        let preferred = preferred_keys(preferred_session_keys);
        if preferred.is_empty() {
            let conn = self.conn()?;
            let mut statement = conn.prepare(
                "SELECT DISTINCT session_key, MIN(timestamp) AS first_timestamp
                 FROM l0_sessions
                 WHERE indexed = 0
                 GROUP BY session_key
                 ORDER BY first_timestamp ASC
                 LIMIT ?1",
            )?;
            let rows = statement
                .query_map([limit as i64], |row| row.get::<_, String>(0).map(node_text))?;
            return Ok(rows.collect::<Result<Vec<_>, _>>()?);
        }
        let placeholders = placeholders(preferred.len());
        let sql = format!(
            "SELECT DISTINCT session_key, MIN(timestamp) AS first_timestamp
             FROM l0_sessions
             WHERE indexed = 0 AND session_key IN ({placeholders})
             GROUP BY session_key
             ORDER BY first_timestamp ASC"
        );
        let conn = self.conn()?;
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(preferred.iter()), |row| {
            row.get::<_, String>(0).map(node_text)
        })?;
        let mut keys = rows.collect::<Result<Vec<_>, _>>()?;
        keys.truncate(limit);
        Ok(keys)
    }

    /// `sqlite.ts:1003-1019`.
    pub fn count_pending_dialogue_turns(
        &self,
        preferred_session_keys: Option<&[String]>,
    ) -> Result<i64, StorageError> {
        let preferred = preferred_keys(preferred_session_keys);
        let conn = self.conn()?;
        if preferred.is_empty() {
            return Ok(conn.query_row(
                "SELECT COUNT(*) AS count FROM l0_sessions WHERE indexed = 0",
                [],
                |row| row.get(0),
            )?);
        }
        let placeholders = placeholders(preferred.len());
        let sql = format!(
            "SELECT COUNT(*) AS count
             FROM l0_sessions
             WHERE indexed = 0 AND session_key IN ({placeholders})"
        );
        Ok(conn.query_row(&sql, params_from_iter(preferred.iter()), |row| row.get(0))?)
    }

    /// `sqlite.ts:1021-1043`. A blank (or absent) minimum stays `undefined`.
    pub fn get_earliest_pending_timestamp(
        &self,
        preferred_session_keys: Option<&[String]>,
    ) -> Result<Option<String>, StorageError> {
        let preferred = preferred_keys(preferred_session_keys);
        let conn = self.conn()?;
        let timestamp: Option<String> = if preferred.is_empty() {
            conn.query_row(
                "SELECT MIN(timestamp) AS first_timestamp
                 FROM l0_sessions
                 WHERE indexed = 0",
                [],
                |row| row.get(0),
            )?
        } else {
            let placeholders = placeholders(preferred.len());
            let sql = format!(
                "SELECT MIN(timestamp) AS first_timestamp
                 FROM l0_sessions
                 WHERE indexed = 0 AND session_key IN ({placeholders})"
            );
            conn.query_row(&sql, params_from_iter(preferred.iter()), |row| row.get(0))?
        };
        let timestamp = timestamp.map(node_text);
        Ok(timestamp.filter(|value| !js_trim(value).is_empty()))
    }

    /// `sqlite.ts:1028-1034`: pending rows of one session, oldest first by
    /// `(timestamp, created_at)`.
    pub fn list_unindexed_l0_by_session(
        &self,
        session_key: &str,
    ) -> Result<Vec<L0SessionRecord>, StorageError> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT * FROM l0_sessions
             WHERE session_key = ?1 AND indexed = 0
             ORDER BY timestamp ASC, created_at ASC",
        )?;
        let rows = statement.query_map([session_key], l0_record_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// `sqlite.ts:1036-1045`: the newest row strictly before
    /// `(timestamp, created_at)`, comparing both columns lexicographically.
    pub fn get_latest_l0_before(
        &self,
        session_key: &str,
        timestamp: &str,
        created_at: &str,
    ) -> Result<Option<L0SessionRecord>, StorageError> {
        let conn = self.conn()?;
        let record = conn
            .query_row(
                "SELECT * FROM l0_sessions
                 WHERE session_key = ?1
                   AND (timestamp < ?2 OR (timestamp = ?2 AND created_at < ?3))
                 ORDER BY timestamp DESC, created_at DESC
                 LIMIT 1",
                params![session_key, timestamp, created_at],
                l0_record_from_row,
            )
            .optional()?;
        Ok(record)
    }

    /// `sqlite.ts:1047-1053`: filter blank ids, dedupe keeping first
    /// occurrence, then one `IN (...)` update.
    pub fn mark_l0_indexed(&self, ids: &[String]) -> Result<(), StorageError> {
        let ids = unique_non_blank_ids(ids);
        if ids.is_empty() {
            return Ok(());
        }
        let sql = format!(
            "UPDATE l0_sessions SET indexed = 1 WHERE l0_index_id IN ({})",
            placeholders(ids.len())
        );
        self.conn()?
            .execute(&sql, params_from_iter(ids.iter().copied()))?;
        Ok(())
    }

    /// `sqlite.ts:1055-1064`: filtered/deduped ids, newest first.
    pub fn get_l0_by_ids(&self, ids: &[String]) -> Result<Vec<L0SessionRecord>, StorageError> {
        let ids = unique_non_blank_ids(ids);
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT * FROM l0_sessions
             WHERE l0_index_id IN ({})
             ORDER BY timestamp DESC, created_at DESC",
            placeholders(ids.len())
        );
        let conn = self.conn()?;
        let mut statement = conn.prepare(&sql)?;
        let rows =
            statement.query_map(params_from_iter(ids.iter().copied()), l0_record_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// `sqlite.ts:1066-1073`: newest first with the TS `Math.max` clamps
    /// (`limit >= 1`, `offset >= 0`).
    pub fn list_recent_l0(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<L0SessionRecord>, StorageError> {
        let conn = self.conn()?;
        Self::list_recent_l0_on(&conn, limit, offset)
    }

    fn list_recent_l0_on(
        conn: &Connection,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<L0SessionRecord>, StorageError> {
        let mut statement = conn.prepare(
            "SELECT * FROM l0_sessions
             ORDER BY timestamp DESC, created_at DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = statement.query_map(params![limit.max(1), offset.max(0)], l0_record_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// `sqlite.ts:1075-1078`: every row, oldest first by
    /// `(timestamp, created_at)`.
    pub fn list_all_l0(&self) -> Result<Vec<L0SessionRecord>, StorageError> {
        let conn = self.conn()?;
        Self::list_all_l0_with(&conn)
    }

    fn list_all_l0_with(conn: &Connection) -> Result<Vec<L0SessionRecord>, StorageError> {
        let mut statement =
            conn.prepare("SELECT * FROM l0_sessions ORDER BY timestamp ASC, created_at ASC")?;
        let rows = statement.query_map([], l0_record_from_row)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// `sqlite.ts:1080-1104`: walk every row in ascending order, delete rows
    /// whose transform result is empty, and reset `indexed` for rewritten
    /// messages. `rebuilt` reports whether any row was touched.
    ///
    /// The TS callback returns `MemoryMessage[]`; the Rust callback returns
    /// the JSON values directly, stringified with the same JS-semantics
    /// serializer for the `JSON.stringify` comparison and the write.
    pub fn repair_l0_sessions<F>(
        &self,
        mut transform: F,
    ) -> Result<RepairMemoryResult, StorageError>
    where
        F: FnMut(&L0SessionRecord) -> Vec<JsValue>,
    {
        // Snapshot the rows like `this.listAllL0()`, then release the lock
        // before invoking the caller's transform: a callback that reads this
        // storage must not deadlock on the connection mutex. Mutations take
        // the lock per row, the same granularity as the other write methods.
        let rows = {
            let conn = self.conn()?;
            Self::list_all_l0_with(&conn)?
        };
        let inspected = rows.len();
        let mut updated = 0;
        let mut removed = 0;
        for row in &rows {
            let next_messages = transform(row);
            if next_messages.is_empty() {
                self.conn()?.execute(
                    "DELETE FROM l0_sessions WHERE l0_index_id = ?1",
                    [&row.l0_index_id],
                )?;
                removed += 1;
                continue;
            }
            let next = JsValue::Array(next_messages);
            let next_json = stringify_json(&next)
                .map_err(|error| StorageError::Json(error.to_string()))?
                .ok_or(StorageError::Unstringifiable("l0_sessions.messages_json"))?;
            let current_json = stringify_json(&row.messages)
                .map_err(|error| StorageError::Json(error.to_string()))?
                .ok_or(StorageError::Unstringifiable("l0_sessions.messages_json"))?;
            if next_json == current_json {
                continue;
            }
            self.conn()?.execute(
                "UPDATE l0_sessions SET messages_json = ?1, indexed = 0 WHERE l0_index_id = ?2",
                params![next_json, row.l0_index_id],
            )?;
            updated += 1;
        }
        Ok(RepairMemoryResult {
            inspected,
            updated,
            removed,
            rebuilt: updated > 0 || removed > 0,
        })
    }

    // -- traces -----------------------------------------------------------

    /// `sqlite.ts:1105-1112`.
    pub fn save_case_trace(&self, record: &JsValue, limit: usize) -> Result<(), StorageError> {
        self.save_trace(
            RECENT_CASE_TRACES_STATE_KEY,
            "caseId",
            record,
            limit,
            TraceKind::Case,
        )
    }

    /// `sqlite.ts:1114-1120`.
    pub fn list_recent_case_traces(&self, limit: usize) -> Result<Vec<JsValue>, StorageError> {
        self.list_traces(
            RECENT_CASE_TRACES_STATE_KEY,
            "caseId",
            limit,
            TraceKind::Case,
        )
    }

    /// `sqlite.ts:1122-1124`: searches the 200 most recent records only.
    pub fn get_case_trace(&self, case_id: &str) -> Result<Option<JsValue>, StorageError> {
        Ok(self
            .list_traces(RECENT_CASE_TRACES_STATE_KEY, "caseId", 200, TraceKind::Case)?
            .into_iter()
            .find(|record| trace_id_is(record, "caseId", case_id)))
    }

    /// `sqlite.ts:1126-1133`.
    pub fn save_index_trace(&self, record: &JsValue, limit: usize) -> Result<(), StorageError> {
        self.save_trace(
            RECENT_INDEX_TRACES_STATE_KEY,
            "indexTraceId",
            record,
            limit,
            TraceKind::Index,
        )
    }

    /// `sqlite.ts:1135-1141`.
    pub fn list_recent_index_traces(&self, limit: usize) -> Result<Vec<JsValue>, StorageError> {
        self.list_traces(
            RECENT_INDEX_TRACES_STATE_KEY,
            "indexTraceId",
            limit,
            TraceKind::Index,
        )
    }

    /// `sqlite.ts:1143-1145`.
    pub fn get_index_trace(&self, index_trace_id: &str) -> Result<Option<JsValue>, StorageError> {
        Ok(self
            .list_traces(
                RECENT_INDEX_TRACES_STATE_KEY,
                "indexTraceId",
                200,
                TraceKind::Index,
            )?
            .into_iter()
            .find(|record| trace_id_is(record, "indexTraceId", index_trace_id)))
    }

    /// `sqlite.ts:1147-1154`.
    pub fn save_dream_trace(&self, record: &JsValue, limit: usize) -> Result<(), StorageError> {
        self.save_trace(
            RECENT_DREAM_TRACES_STATE_KEY,
            "dreamTraceId",
            record,
            limit,
            TraceKind::Dream,
        )
    }

    /// `sqlite.ts:1156-1162`.
    pub fn list_recent_dream_traces(&self, limit: usize) -> Result<Vec<JsValue>, StorageError> {
        self.list_traces(
            RECENT_DREAM_TRACES_STATE_KEY,
            "dreamTraceId",
            limit,
            TraceKind::Dream,
        )
    }

    /// `sqlite.ts:1164-1166`.
    pub fn get_dream_trace(&self, dream_trace_id: &str) -> Result<Option<JsValue>, StorageError> {
        Ok(self
            .list_traces(
                RECENT_DREAM_TRACES_STATE_KEY,
                "dreamTraceId",
                200,
                TraceKind::Dream,
            )?
            .into_iter()
            .find(|record| trace_id_is(record, "dreamTraceId", dream_trace_id)))
    }

    fn save_trace(
        &self,
        state_key: &str,
        id_key: &str,
        record: &JsValue,
        limit: usize,
        kind: TraceKind,
    ) -> Result<(), StorageError> {
        let conn = self.conn()?;
        // `[record, ...readPipelineState(key, [])]`: a missing row or bad JSON
        // falls back to `[]`; a string is iterable (its characters are dropped
        // by the sanitizer); anything else throws a JS `TypeError` before the
        // write, leaving the stored value untouched.
        let existing = Self::read_pipeline_state(&conn, state_key)?;
        let mut items = match &existing {
            None | Some(JsValue::String(_)) => Vec::new(),
            Some(JsValue::Array(items)) => items.clone(),
            Some(_) => return Err(StorageError::NotIterable(state_key.to_owned())),
        };
        items.insert(0, record.clone());
        let sanitized = sanitize_trace_array(&JsValue::Array(items), id_key, "startedAt")?;
        let normalized = normalize_traces(sanitized, kind)?;
        let capped = normalized.into_iter().take(limit.max(1)).collect();
        let now = (self.clock)();
        Self::write_pipeline_state(&conn, state_key, &JsValue::Array(capped), &now)
    }

    fn list_traces(
        &self,
        state_key: &str,
        id_key: &str,
        limit: usize,
        kind: TraceKind,
    ) -> Result<Vec<JsValue>, StorageError> {
        let value = self
            .get_pipeline_state(state_key)?
            .unwrap_or_else(|| JsValue::Array(Vec::new()));
        let sanitized = sanitize_trace_array(&value, id_key, "startedAt")?;
        let normalized = normalize_traces(sanitized, kind)?;
        Ok(normalized.into_iter().take(limit.max(1)).collect())
    }

    // -- indexing settings ------------------------------------------------

    /// `sqlite.ts:1168-1170`.
    pub fn get_indexing_settings(
        &self,
        defaults: &IndexingSettings,
    ) -> Result<IndexingSettings, StorageError> {
        let stored = self.get_pipeline_state(INDEXING_SETTINGS_STATE_KEY)?;
        Ok(sanitize_indexing_settings(stored.as_ref(), defaults))
    }

    /// `sqlite.ts:1172-1177`: shallow merge over the current settings, then
    /// sanitize and persist the sanitized object.
    pub fn save_indexing_settings(
        &self,
        partial: &JsValue,
        defaults: &IndexingSettings,
    ) -> Result<IndexingSettings, StorageError> {
        let conn = self.conn()?;
        let stored = Self::read_pipeline_state(&conn, INDEXING_SETTINGS_STATE_KEY)?;
        let current = sanitize_indexing_settings(stored.as_ref(), defaults);
        let current_json = current.to_js();
        let mut merged = match &current_json {
            JsValue::Object(object) => object.clone(),
            _ => JsObject::new(),
        };
        if let Some(patch) = partial.as_object() {
            for (key, value) in patch.entries() {
                merged.insert(key.clone(), value.clone());
            }
        }
        let next = sanitize_indexing_settings(Some(&JsValue::Object(merged)), defaults);
        let now = (self.clock)();
        Self::write_pipeline_state(&conn, INDEXING_SETTINGS_STATE_KEY, &next.to_js(), &now)?;
        Ok(next)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TraceKind {
    Case,
    Index,
    Dream,
}

fn preferred_keys(preferred: Option<&[String]>) -> Vec<&str> {
    preferred
        .unwrap_or(&[])
        .iter()
        .filter(|key| !js_trim(key).is_empty())
        .map(String::as_str)
        .collect()
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `node:sqlite` returns TEXT values with `sqlite3_column_text`, which stops at
/// the first NUL even though the column keeps the full bytes (`hex()` proves
/// it). Node 22.22.0 measured: `'a\0b'` reads back as `'a'`. Mirror that at
/// every TEXT read boundary in this module.
fn node_text(value: String) -> String {
    match value.find('\0') {
        Some(index) => value[..index].to_owned(),
        None => value,
    }
}

/// `sqlite.ts:1053,1062`: `Array.from(new Set(ids.filter(...)))` — drop
/// blank ids while keeping the first occurrence of each.
fn unique_non_blank_ids(ids: &[String]) -> Vec<&str> {
    let mut seen = HashSet::new();
    ids.iter()
        .map(String::as_str)
        .filter(|id| !js_trim(id).is_empty())
        .filter(|id| seen.insert(*id))
        .collect()
}

/// `sqlite.ts:138-154` (`normalizeMessages`): keep record items, drop a blank
/// `msgId`, default a blank/non-string `role` to `"user"` and a non-string
/// `content` to `""`. Property order matches the TS object literal.
fn normalize_messages(value: &JsValue) -> JsValue {
    let JsValue::Array(items) = value else {
        return JsValue::Array(Vec::new());
    };
    let mut messages = Vec::new();
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let mut message = JsObject::new();
        if let Some(JsValue::String(msg_id)) = object.get("msgId")
            && !js_trim_units(msg_id.units()).is_empty()
        {
            message.insert("msgId".into(), JsValue::String(msg_id.clone()));
        }
        let role = match object.get("role") {
            Some(JsValue::String(role)) if !js_trim_units(role.units()).is_empty() => role.clone(),
            _ => JsString::from("user"),
        };
        message.insert("role".into(), JsValue::String(role));
        let content = match object.get("content") {
            Some(JsValue::String(content)) => content.clone(),
            _ => JsString::from(""),
        };
        message.insert("content".into(), JsValue::String(content));
        messages.push(JsValue::Object(message));
    }
    JsValue::Array(messages)
}

/// `sqlite.ts:156-166` (`normalizeL0Row`) over the `SELECT *` column order.
fn l0_record_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<L0SessionRecord> {
    let text = |index: usize| -> rusqlite::Result<String> {
        Ok(row
            .get::<_, Option<String>>(index)?
            .map(node_text)
            .unwrap_or_else(|| "null".to_owned()))
    };
    let messages = row
        .get::<_, Option<String>>(3)?
        .map(node_text)
        .and_then(|raw| parse_json(&raw).ok())
        .map(|value| normalize_messages(&value))
        .unwrap_or_else(|| JsValue::Array(Vec::new()));
    let indexed = match row.get::<_, SqlValue>(5)? {
        SqlValue::Null => false,
        SqlValue::Integer(value) => value != 0,
        SqlValue::Real(value) => value != 0.0,
        _ => true,
    };
    Ok(L0SessionRecord {
        l0_index_id: text(0)?,
        session_key: text(1)?,
        timestamp: text(2)?,
        messages,
        source: row
            .get::<_, Option<String>>(4)?
            .map(node_text)
            .unwrap_or_default(),
        indexed,
        created_at: text(6)?,
    })
}

/// `Array#sort` on trace records uses `String#localeCompare` (default locale,
/// sort usage, variant strength), not UTF-16 code-unit order. Reuse the
/// gateway's pinned Node/ICU collator (`project_files::node_collator`), which
/// every other locale-dependent sort in this crate already uses.
fn trace_collator() -> Result<&'static icu_collator::CollatorBorrowed<'static>, StorageError> {
    static COLLATOR: OnceLock<Option<icu_collator::CollatorBorrowed<'static>>> = OnceLock::new();
    COLLATOR
        .get_or_init(|| {
            let locale = node_locale(|key| std::env::var(key).ok());
            node_collator(&locale).ok()
        })
        .as_ref()
        .ok_or_else(|| StorageError::HostRequired("Collation data unavailable.".into()))
}

/// `sanitizeTraceArray`: keep object records with string key + sort key, sort
/// by `startedAt` descending (JS stable sort), drop empty ids and duplicates.
fn sanitize_trace_array(
    value: &JsValue,
    id_key: &str,
    sort_key: &str,
) -> Result<Vec<JsValue>, StorageError> {
    let JsValue::Array(items) = value else {
        return Ok(Vec::new());
    };
    let mut filtered: Vec<&JsValue> = items
        .iter()
        .filter(|item| {
            item.get(id_key).and_then(JsValue::as_string).is_some()
                && item.get(sort_key).and_then(JsValue::as_string).is_some()
        })
        .collect();
    // `Array#sort` is stable and never invokes the comparator for zero or one
    // element, so the collator (and its ICU data) is only loaded when a
    // comparison is actually needed.
    if filtered.len() >= 2 {
        let collator = trace_collator()?;
        filtered.sort_by(|left, right| {
            node_compare_utf16(
                collator,
                trace_sort_key(right, sort_key),
                trace_sort_key(left, sort_key),
            )
        });
    }
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    for item in filtered {
        let id = item
            .get(id_key)
            .and_then(JsValue::as_string)
            .expect("filtered trace has an id");
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        records.push(item.clone());
    }
    Ok(records)
}

fn trace_sort_key<'a>(item: &'a JsValue, sort_key: &str) -> &'a JsString {
    item.get(sort_key)
        .and_then(JsValue::as_string)
        .expect("filtered trace has a sort key")
}

fn normalize_traces(records: Vec<JsValue>, kind: TraceKind) -> Result<Vec<JsValue>, StorageError> {
    match kind {
        TraceKind::Case => Ok(records),
        TraceKind::Index => records
            .into_iter()
            .map(|record| normalize_trace(record, TraceKind::Index))
            .collect(),
        TraceKind::Dream => records
            .into_iter()
            .map(|record| normalize_trace(record, TraceKind::Dream))
            .collect(),
    }
}

/// `normalizeIndexTraceRecord` / `normalizeDreamTraceRecord`
/// (`sqlite.ts:196-241`): fill `isNoOp` and `displayStatus`, keeping every
/// other field and its insertion position. The `&&` chains short-circuit
/// exactly like JS: `storedResults` is only read when `status === "completed"`
/// and `outcome` only when `status !== "error"`.
fn normalize_trace(record: JsValue, kind: TraceKind) -> Result<JsValue, StorageError> {
    let object = record
        .as_object()
        .ok_or_else(|| StorageError::InvalidTrace("record is not an object".into()))?;
    let mut normalized = object.clone();
    let status = object.get("status").and_then(JsValue::as_string);
    let is_no_op = match object.get("isNoOp") {
        Some(JsValue::Bool(value)) => *value,
        _ => match kind {
            TraceKind::Index => index_is_no_op(object, status)?,
            TraceKind::Dream => dream_is_no_op(object, status)?,
            TraceKind::Case => false,
        },
    };
    let display_status = match object.get("displayStatus") {
        Some(JsValue::String(value)) if !js_trim_units(value.units()).is_empty() => value.clone(),
        _ => {
            let fallback = if status.is_some_and(|status| status.equals_str("error")) {
                "Error"
            } else if is_no_op {
                "No-op"
            } else if status.is_some_and(|status| status.equals_str("running")) {
                "Running"
            } else {
                "Completed"
            };
            JsString::from(fallback)
        }
    };
    normalized.insert("isNoOp".into(), JsValue::Bool(is_no_op));
    normalized.insert("displayStatus".into(), JsValue::String(display_status));
    Ok(JsValue::Object(normalized))
}

/// `record.status === "completed" && record.storedResults.length === 0`.
fn index_is_no_op(object: &JsObject, status: Option<&JsString>) -> Result<bool, StorageError> {
    if !status.is_some_and(|status| status.equals_str("completed")) {
        return Ok(false);
    }
    match object.get("storedResults") {
        Some(JsValue::Undefined) | Some(JsValue::Null) | None => Err(StorageError::InvalidTrace(
            "Cannot read properties of undefined (reading 'length')".into(),
        )),
        Some(value) => Ok(js_length_is_zero(value)),
    }
}

/// `record.status !== "error" && record.outcome.deletedFiles === 0 && ...`.
fn dream_is_no_op(object: &JsObject, status: Option<&JsString>) -> Result<bool, StorageError> {
    if status.is_some_and(|status| status.equals_str("error")) {
        return Ok(false);
    }
    let outcome = match object.get("outcome") {
        Some(JsValue::Undefined) | Some(JsValue::Null) | None => {
            return Err(StorageError::InvalidTrace(
                "Cannot read properties of undefined (reading 'deletedFiles')".into(),
            ));
        }
        Some(outcome) => outcome,
    };
    // Property access on a number/string/boolean/array yields `undefined`
    // (no throw); only `null`/`undefined` throw, handled above.
    let (deleted_files, rewritten_projects, profile_updated) = match outcome {
        JsValue::Object(outcome) => (
            outcome.get("deletedFiles"),
            outcome.get("rewrittenProjects"),
            outcome.get("profileUpdated"),
        ),
        _ => (None, None, None),
    };
    Ok(deleted_files.is_some_and(is_js_zero)
        && rewritten_projects.is_some_and(is_js_zero)
        && !profile_updated.is_some_and(js_truthy))
}

/// JS `value.length === 0` for the shapes `storedResults` can hold: arrays,
/// strings (UTF-16 units) and arbitrary objects with a numeric `length`.
/// Numbers and booleans have no `length` and compare false.
fn js_length_is_zero(value: &JsValue) -> bool {
    match value {
        JsValue::Array(items) => items.is_empty(),
        JsValue::String(value) => value.units().is_empty(),
        JsValue::Object(object) => {
            matches!(object.get("length"), Some(JsValue::Number(length)) if *length == 0.0)
        }
        _ => false,
    }
}

fn is_js_zero(value: &JsValue) -> bool {
    matches!(value, JsValue::Number(number) if *number == 0.0)
}

fn trace_id_is(record: &JsValue, id_key: &str, expected: &str) -> bool {
    record
        .get(id_key)
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str(expected))
}

/// `sanitizeIndexingSettings` (`sqlite.ts:255-263`).
fn sanitize_indexing_settings(
    input: Option<&JsValue>,
    defaults: &IndexingSettings,
) -> IndexingSettings {
    let record = input.and_then(JsValue::as_object);
    let reasoning_mode = match record.and_then(|record| record.get("reasoningMode")) {
        Some(JsValue::String(value)) if value.equals_str("accuracy_first") => {
            "accuracy_first".to_owned()
        }
        _ => defaults.reasoning_mode.clone(),
    };
    IndexingSettings {
        reasoning_mode,
        auto_index_interval_minutes: clamp_int(
            record.and_then(|record| record.get("autoIndexIntervalMinutes")),
            defaults.auto_index_interval_minutes,
            0,
            10_080,
        ),
        auto_dream_interval_minutes: clamp_int(
            record.and_then(|record| record.get("autoDreamIntervalMinutes")),
            defaults.auto_dream_interval_minutes,
            0,
            10_080,
        ),
    }
}

/// `clampInt` (`sqlite.ts:244-253`): numbers are floored, strings go through
/// `parseInt(value, 10)`, non-finite values fall back, then clamp to the range.
/// The clamp is `Math.max(min, Math.min(max, …))` in that order: the final
/// `Math.max` projects `-0` to `+0`, which Rust's `f64::clamp` does not.
/// The fallback may itself be non-finite (a merged default), so the result is
/// kept as a JS number.
fn clamp_int(value: Option<&JsValue>, fallback: f64, min: i64, max: i64) -> f64 {
    let numeric = match value {
        Some(JsValue::Number(number)) => Some(*number),
        Some(JsValue::String(value)) => js_parse_int_prefix(value),
        _ => None,
    };
    match numeric {
        Some(number) if number.is_finite() => {
            let clamped = number.floor().min(max as f64).max(min as f64);
            // `f64::max` / `min` may return either zero when +0 and -0 compare
            // equal; JS `Math.max` ranks +0 above -0, so normalise explicitly.
            if clamped != 0.0 {
                clamped
            } else if min >= 0 {
                if crate::mutants::on("settings-negative-zero") {
                    number.floor()
                } else {
                    0.0 /* Math.max(+0, ±0) is +0 */
                }
            } else {
                // min < 0: only -0 survives Math.min/Math.max; an upper-bound
                // clamp to zero (e.g. max = 0, n = 3.7) is +0.
                0.0_f64.copysign(number.floor())
            }
        }
        _ => fallback,
    }
}

/// `parseInt(units, 10)`: skip JS whitespace, optional sign, then ASCII digits
/// until the first non-digit. Returns `None` for no digits (`NaN`).
fn js_parse_int_prefix(value: &JsString) -> Option<f64> {
    let units = value.units();
    let mut index = 0;
    while index < units.len() && js_is_space_unit(units[index]) {
        index += 1;
    }
    let mut negative = false;
    if index < units.len() {
        match units[index] {
            unit if unit == u16::from(b'+') => index += 1,
            unit if unit == u16::from(b'-') => {
                negative = true;
                index += 1;
            }
            _ => {}
        }
    }
    let start = index;
    let mut number = 0.0_f64;
    while index < units.len() && (u16::from(b'0')..=u16::from(b'9')).contains(&units[index]) {
        number = number * 10.0 + f64::from(units[index] - u16::from(b'0'));
        index += 1;
    }
    if index == start {
        return None;
    }
    Some(if negative { -number } else { number })
}

/// JS truthiness (`if (record.outcome.profileUpdated)`). Shared with the file
/// store for caller options such as `includeDeprecated`.
pub(super) fn js_truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

/// `new Date().toISOString()` via the crate's existing `Date#toISOString`
/// port. `pilotdeck_transport::system_clock` is already the gateway's single
/// implementation (used by config/store, tools, user_dialog, file_history), so
/// this module does not carry a fourth copy of the civil-calendar algorithm.
fn now_iso() -> String {
    let millis = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis().min(i64::MAX as u128) as i64,
        Err(error) => -(error.duration().as_millis().min(i64::MAX as u128) as i64),
    };
    pilotdeck_transport::system_clock::format_epoch_iso(millis)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_int_zero_sign_matches_js_math_max_min() {
        // Node v22.22.0: `Math.max(min, Math.min(max, Math.floor(n)))`
        // (#775 r4: min=-1, max=0 rows from the Codex raw probe output).
        let rows: [(f64, i64, i64, f64, bool); 6] = [
            (-0.0, -1, 0, 0.0, true),
            (0.0, -1, 0, 0.0, false),
            (1.0, -1, 0, 0.0, false),
            (3.7, -1, 0, 0.0, false),
            (-0.0, 0, 10080, 0.0, false),
            (3.7, -1, 10080, 3.0, false),
        ];
        for (input, min, max, value, negative) in rows {
            let got = clamp_int(Some(&JsValue::Number(input)), 5.0, min, max);
            assert_eq!(got, value, "#775 clampInt({input}, {min}, {max}) value");
            assert_eq!(
                got.is_sign_negative(),
                negative,
                "#775 clampInt({input}, {min}, {max}) zero sign"
            );
        }
    }

    fn defaults() -> IndexingSettings {
        IndexingSettings {
            reasoning_mode: "answer_first".into(),
            auto_index_interval_minutes: 5.0,
            auto_dream_interval_minutes: 10.0,
        }
    }

    #[test]
    fn now_iso_matches_js_date_to_iso_string() {
        // `format_epoch_iso` is the shared `Date#toISOString` port; assert the
        // timestamp `now_iso` produces still has the JS shape.
        let value = now_iso();
        assert_eq!(value.len(), 24, "{value}");
        assert!(value.ends_with('Z'), "{value}");
        assert_eq!(&value[4..5], "-");
        assert_eq!(&value[10..11], "T");
    }

    #[test]
    fn parse_int_prefix_and_clamp_match_js() {
        let number = |value: &str| js_parse_int_prefix(&JsString::from(value));
        assert_eq!(number("  +42abc"), Some(42.0));
        assert_eq!(number("-7.9"), Some(-7.0));
        assert_eq!(number("0x10"), Some(0.0));
        assert_eq!(number("abc"), None);
        assert_eq!(number(""), None);
        assert_eq!(
            clamp_int(Some(&JsValue::from(" 60.9")), 5.0, 0, 10_080),
            60.0
        );
        assert_eq!(clamp_int(Some(&JsValue::Number(-3.2)), 5.0, 0, 10_080), 0.0);
        // Node v22.22.0 `clampInt(-0 | "-0" | "-0.0")` projects to `+0`.
        // Compare bits: `-0.0 == 0.0` would hide the sign regression.
        assert_eq!(
            clamp_int(Some(&JsValue::Number(-0.0)), 5.0, 0, 10_080).to_bits(),
            0,
            "#775 numeric -0 flips to +0"
        );
        assert_eq!(
            clamp_int(Some(&JsValue::from("-0.0")), 5.0, 0, 10_080).to_bits(),
            0,
            "#775 string -0.0 flips to +0"
        );
        assert_eq!(
            clamp_int(Some(&JsValue::Number(99_999.0)), 5.0, 0, 10_080),
            10_080.0
        );
        assert_eq!(clamp_int(Some(&JsValue::Bool(true)), 5.0, 0, 10_080), 5.0);
    }

    #[test]
    fn indexing_settings_fall_back_like_ts() {
        let sanitized = sanitize_indexing_settings(
            Some(&JsValue::Object({
                let mut object = JsObject::new();
                object.insert("reasoningMode".into(), JsValue::from("answer_first"));
                object
            })),
            &IndexingSettings {
                reasoning_mode: "accuracy_first".into(),
                auto_index_interval_minutes: 5.0,
                auto_dream_interval_minutes: 10.0,
            },
        );
        // Only "accuracy_first" is recognized; anything else takes the default.
        assert_eq!(sanitized.reasoning_mode, "accuracy_first");
        assert_eq!(sanitized.auto_index_interval_minutes, 5.0);
        assert_eq!(sanitized.auto_dream_interval_minutes, 10.0);
        assert_eq!(defaults().reasoning_mode, "answer_first");
    }

    #[test]
    fn trace_sanitizer_sorts_desc_dedupes_and_drops_blank_ids() {
        let record = |id: &str, started: &str| {
            parse_json(&format!(r#"{{"caseId":"{id}","startedAt":"{started}"}}"#)).unwrap()
        };
        let sanitized = sanitize_trace_array(
            &JsValue::Array(vec![
                record("a", "2026-10-22T00:00:00.000Z"),
                record("b", "2026-10-22T00:00:02.000Z"),
                record("a", "2026-10-22T00:00:03.000Z"),
                JsValue::from("not an object"),
                record("", "2026-10-22T00:00:04.000Z"),
            ]),
            "caseId",
            "startedAt",
        )
        .unwrap();
        let ids: Vec<JsString> = sanitized
            .iter()
            .map(|record| {
                record
                    .get("caseId")
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .clone()
            })
            .collect();
        // "a" keeps its newest record; the blank id is dropped.
        assert_eq!(ids, [JsString::from("a"), JsString::from("b"),]);
    }

    #[test]
    fn trace_sanitizer_uses_locale_compare_not_code_units() {
        // Node 22.22.0 en-US measured: "Z".localeCompare("a") === 1 while the
        // UTF-16 unit order is "Z" < "a". Descending locale order therefore
        // keeps "Z" first and, for a duplicate id, keeps the "Z" record.
        let record = |id: &str, started: &str| {
            parse_json(&format!(r#"{{"caseId":"{id}","startedAt":"{started}"}}"#)).unwrap()
        };
        let sorted = sanitize_trace_array(
            &JsValue::Array(vec![record("z", "Z"), record("a", "a")]),
            "caseId",
            "startedAt",
        )
        .unwrap();
        assert_eq!(
            sorted[0].get("caseId").and_then(JsValue::as_string),
            Some(&JsString::from("z"))
        );
        let deduped = sanitize_trace_array(
            &JsValue::Array(vec![record("same", "Z"), record("same", "a")]),
            "caseId",
            "startedAt",
        )
        .unwrap();
        assert_eq!(
            deduped[0].get("startedAt").and_then(JsValue::as_string),
            Some(&JsString::from("Z"))
        );
    }

    #[test]
    fn trace_collation_orders_unpaired_surrogates_like_node() {
        // Measured with Node 22.22.0 (`'\ud800'.localeCompare('\ud801')` etc.):
        // unpaired surrogates collate as distinct implicit weights in code-unit
        // order, above every assigned character and below U+FFFD. This used to
        // be a Rust-only `HostRequired` error branch while TS saved and listed
        // these records.
        let record = |id: &str, started: &str| {
            parse_json(&format!(r#"{{"caseId":"{id}","startedAt":"{started}"}}"#)).unwrap()
        };
        let sorted = sanitize_trace_array(
            &JsValue::Array(vec![
                record("a", r"\ud800a"),
                record("b", r"\ud800b"),
                record("c", r"\ud801"),
                record("d", r"\udfff"),
                record("r", r"\ufffd"),
                record("z", "zzz"),
            ]),
            "caseId",
            "startedAt",
        )
        .unwrap();
        let started: Vec<Vec<u16>> = sorted
            .iter()
            .map(|record| {
                record
                    .get("startedAt")
                    .and_then(JsValue::as_string)
                    .expect("startedAt")
                    .units()
                    .to_vec()
            })
            .collect();
        let unit = |text: &str| text.encode_utf16().collect::<Vec<_>>();
        assert_eq!(
            started,
            vec![
                unit("\u{fffd}"),
                vec![0xDFFF],
                vec![0xD801],
                vec![0xD800, u16::from(b'b')],
                vec![0xD800, u16::from(b'a')],
                unit("zzz"),
            ]
        );
        // Duplicate ids keep the first record after the descending stable sort,
        // i.e. the newer (larger) startedAt, exactly like TS.
        let deduped = sanitize_trace_array(
            &JsValue::Array(vec![record("dup", r"\ud800a"), record("dup", r"\ud801a")]),
            "caseId",
            "startedAt",
        )
        .unwrap();
        assert_eq!(deduped.len(), 1);
        assert_eq!(
            deduped[0]
                .get("startedAt")
                .and_then(JsValue::as_string)
                .expect("startedAt")
                .units(),
            vec![0xD801, u16::from(b'a')]
        );
        // A single record never runs the comparator, so it still stores.
        let single = sanitize_trace_array(
            &JsValue::Array(vec![record("a", r"\ud800a")]),
            "caseId",
            "startedAt",
        )
        .unwrap();
        assert_eq!(single.len(), 1);
    }

    #[test]
    fn trace_normalization_short_circuits_like_js() {
        let normalized =
            |record: &str, kind| normalize_trace(parse_json(record).unwrap(), kind).unwrap();
        // status !== "completed" never reads storedResults.
        let running = normalized(
            r#"{"indexTraceId":"x","startedAt":"t","status":"running"}"#,
            TraceKind::Index,
        );
        assert_eq!(running.get("isNoOp"), Some(&JsValue::Bool(false)));
        // String/object lengths and non-array shapes are computed with JS
        // semantics; only null/undefined would throw.
        for (stored, no_op) in [
            ("\"\"", true),
            ("{\"length\":0}", true),
            ("5", false),
            ("\"x\"", false),
            ("{}", false),
        ] {
            let record = format!(
                r#"{{"indexTraceId":"x","startedAt":"t","status":"completed","storedResults":{stored}}}"#
            );
            assert_eq!(
                normalized(&record, TraceKind::Index).get("isNoOp"),
                Some(&JsValue::Bool(no_op)),
                "storedResults={stored}"
            );
        }
        // status === "error" never reads outcome.
        let error = normalized(
            r#"{"dreamTraceId":"d","startedAt":"t","status":"error"}"#,
            TraceKind::Dream,
        );
        assert_eq!(error.get("isNoOp"), Some(&JsValue::Bool(false)));
        // Primitive outcome: property access yields undefined, no throw.
        let primitive = normalized(
            r#"{"dreamTraceId":"d","startedAt":"t","status":"completed","outcome":5}"#,
            TraceKind::Dream,
        );
        assert_eq!(primitive.get("isNoOp"), Some(&JsValue::Bool(false)));
    }

    #[test]
    fn completed_index_trace_without_stored_results_is_a_type_error() {
        let record =
            parse_json(r#"{"indexTraceId":"x","startedAt":"t","status":"completed"}"#).unwrap();
        assert!(matches!(
            normalize_trace(record, TraceKind::Index),
            Err(StorageError::InvalidTrace(_))
        ));
        let record =
            parse_json(r#"{"dreamTraceId":"d","startedAt":"t","status":"completed"}"#).unwrap();
        assert!(matches!(
            normalize_trace(record, TraceKind::Dream),
            Err(StorageError::InvalidTrace(_))
        ));
    }

    #[test]
    fn normalize_messages_filters_and_defaults_like_ts() {
        // `sqlite.ts:138-154`: non-records are dropped, a blank `msgId` is
        // omitted, a blank/non-string `role` becomes "user", a non-string
        // `content` becomes "", and extra keys are discarded.
        let value = parse_json(
            r#"[1,null,{"msgId":"  ","role":"  ","content":"x","extra":1},{"msgId":"m","role":"assistant","content":5},{"msgId":" m2 ","content":"ok"}]"#,
        )
        .unwrap();
        assert_eq!(
            stringify_json(&normalize_messages(&value))
                .unwrap()
                .unwrap(),
            r#"[{"role":"user","content":"x"},{"msgId":"m","role":"assistant","content":""},{"msgId":" m2 ","role":"user","content":"ok"}]"#
        );
        // A non-array JSON value normalizes to an empty array.
        assert_eq!(
            stringify_json(&normalize_messages(&JsValue::from("nope")))
                .unwrap()
                .unwrap(),
            "[]"
        );
    }

    #[test]
    fn unique_ids_filter_blank_and_keep_first_occurrence() {
        // `sqlite.ts:1049-1052` / `:1057-1060`.
        let ids = ["a", " ", "", "b", "a", "b"]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(unique_non_blank_ids(&ids), ["a", "b"]);
        assert!(unique_non_blank_ids(&[" ".into(), "".into()]).is_empty());
    }
}

// Repository file accessors and readable project aggregation (sqlite.ts:592-909).
pub(crate) mod accessors;

pub mod snapshots;

pub mod entries;

// Memory bundle export/import with staged swaps and Dream snapshot reads
// (sqlite.ts:264-353,422-472,1232-1350,1495-1658).
pub(crate) mod bundle;
// Dashboard overview, UI snapshot and repository-level list/count
// (sqlite.ts:1660-1775).
pub(crate) mod overview;
