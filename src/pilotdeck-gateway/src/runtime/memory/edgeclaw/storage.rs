//! SQLite-backed EdgeClaw memory storage.
//!
//! The SQL text, statement order, conflict clauses, pending-session ordering
//! and trace sanitation mirror `core/storage/sqlite.ts:541-590,916-1020,
//! 1105-1176` one-for-one; `sqlite_master.sql` is compared verbatim against
//! the real TS-created database by `tests/edgeclaw_storage.rs`.
//!
//! JSON columns are JS-owned values, so they use
//! [`pilotdeck_protocol::parse_json`] / [`pilotdeck_protocol::stringify_json`]
//! (WTF-16 preserving) rather than `serde_json`: a trace or pipeline value may
//! contain a lone surrogate, which `JSON.stringify` writes as `\ud800`.

use crate::{
    metadata::js_trim,
    project_files::{has_unpaired_surrogate, node_collator, node_locale},
    runtime::memory::{js_is_space_unit, js_trim_units},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use rusqlite::{Connection, OptionalExtension, params, params_from_iter, types::Value as SqlValue};
use std::{
    collections::HashSet,
    fmt, fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
    /// The comparison cannot be reproduced without the Node host: ICU4X
    /// folds unpaired surrogates to U+FFFD while Node's collation keeps them
    /// distinct. Mirrors the existing `workspace_snapshot` boundary.
    HostRequired(String),
}
impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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

/// `types.ts:196-200` (`IndexingSettings`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexingSettings {
    pub reasoning_mode: String,
    pub auto_index_interval_minutes: i64,
    pub auto_dream_interval_minutes: i64,
}
impl IndexingSettings {
    fn to_js(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert(
            "reasoningMode".into(),
            JsValue::String(self.reasoning_mode.as_str().into()),
        );
        object.insert(
            "autoIndexIntervalMinutes".into(),
            JsValue::Number(self.auto_index_interval_minutes as f64),
        );
        object.insert(
            "autoDreamIntervalMinutes".into(),
            JsValue::Number(self.auto_dream_interval_minutes as f64),
        );
        JsValue::Object(object)
    }
}

/// A `MemoryRepository` opened on one SQLite file (`sqlite.ts:490-600`).
pub struct EdgeClawStorage {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl EdgeClawStorage {
    /// `sqlite.ts:502-566`: create the parent directory, open, enable WAL,
    /// create the base schema, then run the `pipeline_state` migration.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(&path)?;
        // `node:sqlite`'s `DatabaseSync` leaves `PRAGMA busy_timeout` at 0
        // (measured on Node 22.22.0: a second writer fails immediately with
        // `ERR_SQLITE_ERROR` / "database is locked"). rusqlite's `open` sets
        // `sqlite3_busy_timeout(db, 5000)` instead, so overwrite it explicitly.
        conn.busy_timeout(Duration::ZERO)?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(INIT_SQL)?;
        let storage = Self {
            path,
            conn: Mutex::new(conn),
        };
        storage.migrate_pipeline_state()?;
        Ok(storage)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Closing the last connection checkpoints and removes the WAL file, as
    /// `MemoryRepository.close()` does before callers copy the `.db` file.
    pub fn close(self) {}

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .expect("edgeclaw storage connection mutex poisoned")
    }

    /// `sqlite.ts:567-590`: migrate only when `pipeline_state` still carries a
    /// legacy `state_value` column; an already-v2 table is untouched.
    fn migrate_pipeline_state(&self) -> Result<(), StorageError> {
        let columns = {
            let conn = self.conn();
            let mut statement = conn.prepare("PRAGMA table_info(pipeline_state)")?;
            let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let has_state_json = columns.iter().any(|column| column == "state_json");
        let has_state_value = columns.iter().any(|column| column == "state_value");
        if has_state_json && !has_state_value {
            return Ok(());
        }
        if !has_state_json && !has_state_value {
            return Ok(());
        }
        self.conn().execute_batch(MIGRATE_PIPELINE_STATE_SQL)?;
        Ok(())
    }

    // -- pipeline_state ---------------------------------------------------

    /// `sqlite.ts:913-921`. Unparseable text falls back to `undefined`, like
    /// `parseJson(row.state_json, undefined)`.
    pub fn get_pipeline_state(&self, key: &str) -> Result<Option<JsValue>, StorageError> {
        let conn = self.conn();
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
        let conn = self.conn();
        Self::write_pipeline_state(&conn, key, value)
    }

    fn write_pipeline_state(
        conn: &Connection,
        key: &str,
        value: &JsValue,
    ) -> Result<(), StorageError> {
        let json = stringify_json(value)
            .map_err(|error| StorageError::Json(error.to_string()))?
            .ok_or(StorageError::Unstringifiable("pipeline_state.state_json"))?;
        conn.execute(
            "INSERT INTO pipeline_state (state_key, state_json, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(state_key) DO UPDATE SET state_json = excluded.state_json, updated_at = excluded.updated_at",
            params![key, json, now_iso()],
        )?;
        Ok(())
    }

    /// `sqlite.ts:930-932`.
    pub fn delete_pipeline_state(&self, key: &str) -> Result<(), StorageError> {
        self.conn()
            .execute("DELETE FROM pipeline_state WHERE state_key = ?1", [key])?;
        Ok(())
    }

    // -- l0_sessions ------------------------------------------------------

    /// `sqlite.ts:934-968`.
    pub fn insert_l0_session(&self, record: &L0SessionRecord) -> Result<(), StorageError> {
        let messages = stringify_json(&record.messages)
            .map_err(|error| StorageError::Json(error.to_string()))?
            .ok_or(StorageError::Unstringifiable("l0_sessions.messages_json"))?;
        let source = if record.source.is_empty() {
            "openclaw"
        } else {
            record.source.as_str()
        };
        let created_at = if record.created_at.is_empty() {
            now_iso()
        } else {
            record.created_at.clone()
        };
        self.conn().execute(
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
            let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        let conn = self.conn();
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
        Self::write_pipeline_state(&conn, state_key, &JsValue::Array(capped))
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
        let conn = self.conn();
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
        Self::write_pipeline_state(&conn, INDEXING_SETTINGS_STATE_KEY, &next.to_js())?;
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
    // ICU4X folds unpaired surrogates to U+FFFD while V8's collation keeps
    // distinct weights. Only refuse when such a key actually takes part in a
    // comparison; a single distinct key never calls the comparator.
    let has_distinct_keys = filtered.first().is_some_and(|first| {
        let first = trace_sort_key(first, sort_key);
        filtered
            .iter()
            .skip(1)
            .any(|item| trace_sort_key(item, sort_key).units() != first.units())
    });
    if has_distinct_keys
        && filtered
            .iter()
            .any(|item| has_unpaired_surrogate(trace_sort_key(item, sort_key)))
    {
        return Err(StorageError::HostRequired(
            "trace startedAt collation requires the Node host for unpaired surrogates.".into(),
        ));
    }
    let collator = trace_collator()?;
    filtered.sort_by(|left, right| {
        collator.compare_utf16(
            trace_sort_key(right, sort_key).units(),
            trace_sort_key(left, sort_key).units(),
        )
    });
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
fn clamp_int(value: Option<&JsValue>, fallback: i64, min: i64, max: i64) -> i64 {
    let numeric = match value {
        Some(JsValue::Number(number)) => Some(*number),
        Some(JsValue::String(value)) => js_parse_int_prefix(value),
        _ => None,
    };
    match numeric {
        Some(number) if number.is_finite() => number.floor().clamp(min as f64, max as f64) as i64,
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

/// JS truthiness (`if (record.outcome.profileUpdated)`).
fn js_truthy(value: &JsValue) -> bool {
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

    fn defaults() -> IndexingSettings {
        IndexingSettings {
            reasoning_mode: "answer_first".into(),
            auto_index_interval_minutes: 5,
            auto_dream_interval_minutes: 10,
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
        assert_eq!(clamp_int(Some(&JsValue::from(" 60.9")), 5, 0, 10_080), 60);
        assert_eq!(clamp_int(Some(&JsValue::Number(-3.2)), 5, 0, 10_080), 0);
        assert_eq!(
            clamp_int(Some(&JsValue::Number(99_999.0)), 5, 0, 10_080),
            10_080
        );
        assert_eq!(clamp_int(Some(&JsValue::Bool(true)), 5, 0, 10_080), 5);
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
                auto_index_interval_minutes: 5,
                auto_dream_interval_minutes: 10,
            },
        );
        // Only "accuracy_first" is recognized; anything else takes the default.
        assert_eq!(sanitized.reasoning_mode, "accuracy_first");
        assert_eq!(sanitized.auto_index_interval_minutes, 5);
        assert_eq!(sanitized.auto_dream_interval_minutes, 10);
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
    fn trace_collation_stops_at_unpaired_surrogates_that_must_be_compared() {
        // ICU4X folds unpaired surrogates to U+FFFD while V8 keeps distinct
        // weights; the module refuses only when such a key takes part in a
        // comparison (mirrors the `workspace_snapshot` boundary).
        let record = |id: &str, started: &str| {
            parse_json(&format!(r#"{{"caseId":"{id}","startedAt":"{started}"}}"#)).unwrap()
        };
        let lone = sanitize_trace_array(
            &JsValue::Array(vec![record("a", r"\ud800a"), record("b", r"\ud800b")]),
            "caseId",
            "startedAt",
        );
        assert!(matches!(lone, Err(StorageError::HostRequired(_))));
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
}
