//! Replay of the real-TS EdgeClaw sqlite storage fixture.
//!
//! `tests/fixtures/edgeclaw-storage/generate.mts` ran the unmodified baseline
//! `MemoryRepository` on Node 22's `node:sqlite` and recorded:
//!
//! * `tsRead` — a TS-written database (base64) plus TS's observations, schema
//!   and row text; Rust must open that exact file and read the same values.
//! * `empty` — TS's fresh schema; a Rust-created database must match
//!   `sqlite_master` and `PRAGMA table_info` byte for byte.
//! * `migration` — a legacy `pipeline_state` database (base64) plus TS's
//!   post-migration schema/rows; Rust must migrate it identically.
//! * `rustWrites` — an operation script replayed through `EdgeClawStorage`,
//!   with TS's observations of the same script and of the save-side `limit`
//!   truncation applied afterwards.
//!
//! The `#[ignore]` case closes the loop the other way: it hands the Rust-written
//! file to `verify.mts`, which reopens it through the real `MemoryRepository`.

use pilotdeck_gateway::runtime::memory::edgeclaw::{
    EdgeClawStorage, IndexingSettings, L0SessionRecord, StorageError,
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use rusqlite::{Connection, params};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

#[path = "common/baseline.rs"]
mod baseline;

const ORACLE: &str = include_str!("fixtures/edgeclaw-storage/oracle.json");
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

// -- oracle accessors ------------------------------------------------------

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value
        .get(key)
        .unwrap_or_else(|| panic!("oracle is missing field {key}"))
}

fn as_array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("expected an array, got {other:?}"),
    }
}

fn as_text(value: &JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| panic!("expected a string, got {value:?}"))
        .to_utf8()
        .expect("fixture strings are valid UTF-8")
}

fn as_usize(value: &JsValue) -> usize {
    value.as_number().expect("expected a number") as usize
}

fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    JsValue::Object(object)
}

fn strings(values: Vec<String>) -> JsValue {
    JsValue::Array(
        values
            .iter()
            .map(|value| JsValue::from(value.as_str()))
            .collect(),
    )
}

fn number(value: i64) -> JsValue {
    JsValue::Number(value as f64)
}

fn optional(value: Option<JsValue>) -> JsValue {
    value.unwrap_or(JsValue::Null)
}

// -- JSON comparison (object key order is irrelevant) ----------------------

fn json_eq(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Object(left), JsValue::Object(right)) => {
            left.len() == right.len()
                && left.entries().iter().all(|(key, value)| {
                    right
                        .get_key(key)
                        .is_some_and(|candidate| json_eq(value, candidate))
                })
        }
        (JsValue::Array(left), JsValue::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right.iter())
                    .all(|(left, right)| json_eq(left, right))
        }
        _ => left == right,
    }
}

fn canonical(value: &JsValue) -> String {
    match value {
        JsValue::Object(object) => {
            let mut entries: Vec<_> = object.entries().iter().collect();
            entries.sort_by(|left, right| left.0.units().cmp(right.0.units()));
            let body = entries
                .iter()
                .map(|(key, value)| {
                    let quoted = stringify_json(&JsValue::String((*key).clone()))
                        .expect("key serializes")
                        .expect("key is not undefined");
                    format!("{quoted}:{}", canonical(value))
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        JsValue::Array(items) => format!(
            "[{}]",
            items.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        JsValue::Undefined => "undefined".to_owned(),
        JsValue::Null => "null".to_owned(),
        other => stringify_json(other)
            .expect("value serializes")
            .expect("value is not undefined"),
    }
}

fn assert_json_eq(actual: &JsValue, expected: &JsValue, context: &str) {
    if !json_eq(actual, expected) {
        panic!(
            "{context}\n  actual:   {}\n  expected: {}",
            canonical(actual),
            canonical(expected)
        );
    }
}

// -- temp directories ------------------------------------------------------

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "pdrs-edgeclaw-storage-test-{}-{label}-{}",
            std::process::id(),
            TEMP_COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

// -- base64 (the oracle stores the raw .db bytes) --------------------------

fn base64_decode(text: &str) -> Vec<u8> {
    fn value(byte: u8) -> Option<u32> {
        Some(match byte {
            b'A'..=b'Z' => u32::from(byte - b'A'),
            b'a'..=b'z' => u32::from(byte - b'a') + 26,
            b'0'..=b'9' => u32::from(byte - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }
    let mut output = Vec::with_capacity(text.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in text.bytes() {
        if byte == b'=' {
            break;
        }
        if byte.is_ascii_whitespace() {
            continue;
        }
        let value = value(byte).unwrap_or_else(|| panic!("invalid base64 byte {byte}"));
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    output
}

fn write_encoded_db(directory: &TempDir, name: &str, base64: &str) -> PathBuf {
    let path = directory.path().join(name);
    fs::write(&path, base64_decode(base64)).expect("write decoded database");
    path
}

// -- database dumps mirrored from observe.mts ------------------------------

fn dump_schema(conn: &Connection) -> JsValue {
    let mut statement = conn
        .prepare("SELECT type, name, tbl_name, sql FROM sqlite_master ORDER BY rowid")
        .expect("prepare sqlite_master");
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })
        .expect("query sqlite_master");
    let mut items = Vec::new();
    for row in rows {
        let (kind, name, table, sql) = row.expect("sqlite_master row");
        items.push(object(vec![
            ("type", JsValue::from(kind.as_str())),
            ("name", JsValue::from(name.as_str())),
            ("tblName", JsValue::from(table.as_str())),
            (
                "sql",
                sql.map(|sql| JsValue::from(sql.as_str()))
                    .unwrap_or(JsValue::Null),
            ),
        ]));
    }
    JsValue::Array(items)
}

fn dump_table_info(conn: &Connection, table: &str) -> JsValue {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .expect("prepare table_info");
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .expect("query table_info");
    let mut items = Vec::new();
    for row in rows {
        let (cid, name, kind, notnull, default, pk) = row.expect("table_info row");
        items.push(object(vec![
            ("cid", number(cid)),
            ("name", JsValue::from(name.as_str())),
            ("type", JsValue::from(kind.as_str())),
            ("notnull", number(notnull)),
            (
                "dflt_value",
                default
                    .map(|value| JsValue::from(value.as_str()))
                    .unwrap_or(JsValue::Null),
            ),
            ("pk", number(pk)),
        ]));
    }
    JsValue::Array(items)
}

fn dump_columns(conn: &Connection) -> JsValue {
    object(vec![
        ("l0_sessions", dump_table_info(conn, "l0_sessions")),
        ("pipeline_state", dump_table_info(conn, "pipeline_state")),
    ])
}

/// `node:sqlite` reads TEXT up to the first NUL (`sqlite3_column_text`), so
/// the TS dump/observation surface never sees the suffix. Mirror it here.
fn node_text(value: String) -> String {
    match value.find('\0') {
        Some(index) => value[..index].to_owned(),
        None => value,
    }
}

fn dump_rows(conn: &Connection) -> JsValue {
    let mut l0_statement = conn
        .prepare(
            "SELECT l0_index_id, session_key, timestamp, messages_json, source, indexed, created_at
             FROM l0_sessions ORDER BY l0_index_id",
        )
        .expect("prepare l0 dump");
    let l0_rows = l0_statement
        .query_map([], |row| {
            let text = |index: usize| row.get::<_, String>(index).map(node_text);
            Ok(object(vec![
                ("l0_index_id", JsValue::from(text(0)?.as_str())),
                ("session_key", JsValue::from(text(1)?.as_str())),
                ("timestamp", JsValue::from(text(2)?.as_str())),
                ("messages_json", JsValue::from(text(3)?.as_str())),
                ("source", JsValue::from(text(4)?.as_str())),
                ("indexed", number(row.get::<_, i64>(5)?)),
                ("created_at", JsValue::from(text(6)?.as_str())),
            ]))
        })
        .expect("query l0 dump");
    let l0_sessions = l0_rows
        .collect::<Result<Vec<_>, _>>()
        .expect("collect l0 rows");

    let mut pipeline_statement = conn
        .prepare("SELECT state_key, state_json, updated_at FROM pipeline_state ORDER BY state_key")
        .expect("prepare pipeline dump");
    let pipeline_rows = pipeline_statement
        .query_map([], |row| {
            let text = |index: usize| row.get::<_, String>(index).map(node_text);
            Ok(object(vec![
                ("state_key", JsValue::from(text(0)?.as_str())),
                ("state_json", JsValue::from(text(1)?.as_str())),
                ("updated_at", JsValue::from(text(2)?.as_str())),
            ]))
        })
        .expect("query pipeline dump");
    let pipeline_state = pipeline_rows
        .collect::<Result<Vec<_>, _>>()
        .expect("collect pipeline rows");

    object(vec![
        ("l0_sessions", JsValue::Array(l0_sessions)),
        ("pipeline_state", JsValue::Array(pipeline_state)),
    ])
}

/// `updated_at` is `nowIso()`; Rust writes real wall-clock time while the TS
/// oracle froze the clock, so the two sides can only be compared with that
/// column masked. The same holds for `l0_sessions.created_at` on the one row
/// whose `createdAt` is empty (`insertL0Session` then calls `nowIso()`).
fn mask_volatile_columns(rows: &JsValue) -> JsValue {
    let sessions = as_array(field(rows, "l0_sessions"))
        .iter()
        .map(|row| {
            let mut object = row.as_object().expect("row is an object").clone();
            let is_defaulted = object
                .get("l0_index_id")
                .and_then(JsValue::as_string)
                .is_some_and(|id| id.equals_str("l0-defaults"));
            if is_defaulted {
                object.insert("created_at".into(), JsValue::from(""));
            }
            JsValue::Object(object)
        })
        .collect::<Vec<_>>();
    let pipeline = as_array(field(rows, "pipeline_state"))
        .iter()
        .map(|row| {
            let mut object = row.as_object().expect("row is an object").clone();
            object.insert("updated_at".into(), JsValue::from(""));
            JsValue::Object(object)
        })
        .collect::<Vec<_>>();
    object(vec![
        ("l0_sessions", JsValue::Array(sessions)),
        ("pipeline_state", JsValue::Array(pipeline)),
    ])
}

fn journal_mode(conn: &Connection) -> String {
    conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("read journal_mode")
}

// -- observation mirrored from observe.mts --------------------------------

fn settings_json(settings: &IndexingSettings) -> JsValue {
    object(vec![
        (
            "reasoningMode",
            JsValue::from(settings.reasoning_mode.as_str()),
        ),
        (
            "autoIndexIntervalMinutes",
            JsValue::Number(settings.auto_index_interval_minutes as f64),
        ),
        (
            "autoDreamIntervalMinutes",
            JsValue::Number(settings.auto_dream_interval_minutes as f64),
        ),
    ])
}

fn first_id(records: &[JsValue], key: &str) -> Option<String> {
    records
        .first()
        .and_then(|record| record.get(key))
        .and_then(JsValue::as_string)
        .map(|id| id.to_utf8().expect("fixture trace ids are UTF-8"))
}

fn observe(storage: &EdgeClawStorage) -> JsValue {
    let pipeline = object(vec![
        (
            "workspaceDir",
            optional(storage.get_pipeline_state("workspaceDir").unwrap()),
        ),
        (
            "lastDreamAt",
            optional(storage.get_pipeline_state("lastDreamAt").unwrap()),
        ),
        (
            "missing",
            optional(storage.get_pipeline_state("missing").unwrap()),
        ),
        (
            "brokenJson",
            optional(storage.get_pipeline_state("brokenJson").unwrap()),
        ),
        (
            "nullState",
            optional(storage.get_pipeline_state("nullState").unwrap()),
        ),
        (
            "afterDelete",
            optional(storage.get_pipeline_state("afterDelete").unwrap()),
        ),
        (
            "deleted",
            optional(storage.get_pipeline_state("temp").unwrap()),
        ),
        (
            "loneSurrogate",
            optional(storage.get_pipeline_state("loneSurrogate").unwrap()),
        ),
        (
            "objectState",
            optional(storage.get_pipeline_state("objectState").unwrap()),
        ),
        (
            "arrayState",
            optional(storage.get_pipeline_state("arrayState").unwrap()),
        ),
        (
            "nulSuffix",
            optional(storage.get_pipeline_state("nulSuffix").unwrap()),
        ),
    ]);

    let preferred = vec!["s2".to_owned(), "s4".to_owned(), " ".to_owned()];
    let preferred_no_blank = vec!["s2".to_owned(), "s4".to_owned()];
    let none = vec!["none".to_owned()];
    let blank = vec![" ".to_owned()];
    let s1 = vec!["s1".to_owned()];
    let s2 = vec!["s2".to_owned()];
    let s_blank = vec!["s-blank".to_owned()];
    let s_nul = vec!["s-nul".to_owned()];

    let sessions = object(vec![
        (
            "pendingAll",
            strings(storage.list_pending_session_keys(50, None).unwrap()),
        ),
        (
            "pendingLimit1",
            strings(storage.list_pending_session_keys(1, None).unwrap()),
        ),
        (
            "pendingLimit0",
            strings(storage.list_pending_session_keys(0, None).unwrap()),
        ),
        (
            "pendingPreferred",
            strings(
                storage
                    .list_pending_session_keys(50, Some(&preferred))
                    .unwrap(),
            ),
        ),
        (
            "pendingPreferredLimit1",
            strings(
                storage
                    .list_pending_session_keys(1, Some(&preferred_no_blank))
                    .unwrap(),
            ),
        ),
        (
            "pendingPreferredEmpty",
            strings(storage.list_pending_session_keys(50, Some(&[])).unwrap()),
        ),
        (
            "pendingPreferredNone",
            strings(storage.list_pending_session_keys(50, Some(&none)).unwrap()),
        ),
        (
            "countAll",
            number(storage.count_pending_dialogue_turns(None).unwrap()),
        ),
        (
            "countPreferred",
            number(storage.count_pending_dialogue_turns(Some(&s1)).unwrap()),
        ),
        (
            "countPreferredBlank",
            number(storage.count_pending_dialogue_turns(Some(&blank)).unwrap()),
        ),
        (
            "earliestAll",
            optional(
                storage
                    .get_earliest_pending_timestamp(None)
                    .unwrap()
                    .map(|value| JsValue::from(value.as_str())),
            ),
        ),
        (
            "earliestPreferred",
            optional(
                storage
                    .get_earliest_pending_timestamp(Some(&s2))
                    .unwrap()
                    .map(|value| JsValue::from(value.as_str())),
            ),
        ),
        (
            "earliestNone",
            optional(
                storage
                    .get_earliest_pending_timestamp(Some(&none))
                    .unwrap()
                    .map(|value| JsValue::from(value.as_str())),
            ),
        ),
        (
            "earliestBlank",
            optional(
                storage
                    .get_earliest_pending_timestamp(Some(&s_blank))
                    .unwrap()
                    .map(|value| JsValue::from(value.as_str())),
            ),
        ),
        (
            "earliestNul",
            optional(
                storage
                    .get_earliest_pending_timestamp(Some(&s_nul))
                    .unwrap()
                    .map(|value| JsValue::from(value.as_str())),
            ),
        ),
    ]);

    let default_settings = IndexingSettings {
        reasoning_mode: "answer_first".into(),
        auto_index_interval_minutes: 5,
        auto_dream_interval_minutes: 10,
    };
    let other_defaults = IndexingSettings {
        reasoning_mode: "accuracy_first".into(),
        auto_index_interval_minutes: 1,
        auto_dream_interval_minutes: 2,
    };
    let settings = object(vec![
        (
            "defaults",
            settings_json(&storage.get_indexing_settings(&default_settings).unwrap()),
        ),
        (
            "otherDefaults",
            settings_json(&storage.get_indexing_settings(&other_defaults).unwrap()),
        ),
    ]);

    let case_recent = storage.list_recent_case_traces(30).unwrap();
    let index_recent = storage.list_recent_index_traces(30).unwrap();
    let dream_recent = storage.list_recent_dream_traces(30).unwrap();

    let case_traces = object(vec![
        ("recent30", JsValue::Array(case_recent.clone())),
        (
            "recent2",
            JsValue::Array(storage.list_recent_case_traces(2).unwrap()),
        ),
        (
            "recent0",
            JsValue::Array(storage.list_recent_case_traces(0).unwrap()),
        ),
        (
            "first",
            optional(
                first_id(&case_recent, "caseId")
                    .map(|id| storage.get_case_trace(&id).unwrap())
                    .unwrap_or(None),
            ),
        ),
        (
            "missing",
            optional(storage.get_case_trace("missing").unwrap()),
        ),
    ]);
    let index_traces = object(vec![
        ("recent30", JsValue::Array(index_recent.clone())),
        (
            "recent2",
            JsValue::Array(storage.list_recent_index_traces(2).unwrap()),
        ),
        (
            "recent0",
            JsValue::Array(storage.list_recent_index_traces(0).unwrap()),
        ),
        (
            "first",
            optional(
                first_id(&index_recent, "indexTraceId")
                    .map(|id| storage.get_index_trace(&id).unwrap())
                    .unwrap_or(None),
            ),
        ),
        (
            "missing",
            optional(storage.get_index_trace("missing").unwrap()),
        ),
    ]);
    let dream_traces = object(vec![
        ("recent30", JsValue::Array(dream_recent.clone())),
        (
            "recent2",
            JsValue::Array(storage.list_recent_dream_traces(2).unwrap()),
        ),
        (
            "recent0",
            JsValue::Array(storage.list_recent_dream_traces(0).unwrap()),
        ),
        (
            "first",
            optional(
                first_id(&dream_recent, "dreamTraceId")
                    .map(|id| storage.get_dream_trace(&id).unwrap())
                    .unwrap_or(None),
            ),
        ),
        (
            "missing",
            optional(storage.get_dream_trace("missing").unwrap()),
        ),
    ]);

    object(vec![
        ("pipeline", pipeline),
        ("sessions", sessions),
        ("settings", settings),
        ("caseTraces", case_traces),
        ("indexTraces", index_traces),
        ("dreamTraces", dream_traces),
    ])
}

// -- operation script replay ----------------------------------------------

fn parse_settings(value: &JsValue) -> IndexingSettings {
    IndexingSettings {
        reasoning_mode: as_text(field(value, "reasoningMode")),
        auto_index_interval_minutes: field(value, "autoIndexIntervalMinutes")
            .as_number()
            .expect("default number") as i64,
        auto_dream_interval_minutes: field(value, "autoDreamIntervalMinutes")
            .as_number()
            .expect("default number") as i64,
    }
}

fn apply_operations(storage: &EdgeClawStorage, operations: &JsValue) {
    for operation in as_array(operations) {
        let kind = as_text(field(operation, "kind"));
        match kind.as_str() {
            "setPipelineState" => storage
                .set_pipeline_state(&as_text(field(operation, "key")), field(operation, "value"))
                .expect("set pipeline state"),
            "deletePipelineState" => storage
                .delete_pipeline_state(&as_text(field(operation, "key")))
                .expect("delete pipeline state"),
            "insertL0Session" => {
                let record = field(operation, "record");
                storage
                    .insert_l0_session(&L0SessionRecord {
                        l0_index_id: as_text(field(record, "l0IndexId")),
                        session_key: as_text(field(record, "sessionKey")),
                        timestamp: as_text(field(record, "timestamp")),
                        messages: field(record, "messages").clone(),
                        source: record.get("source").map(as_text).unwrap_or_default(),
                        indexed: record
                            .get("indexed")
                            .and_then(JsValue::as_bool)
                            .unwrap_or(false),
                        created_at: record.get("createdAt").map(as_text).unwrap_or_default(),
                    })
                    .expect("insert l0 session");
            }
            "saveCaseTrace" => {
                let limit = operation.get("limit").map(as_usize).unwrap_or(30);
                storage
                    .save_case_trace(field(operation, "record"), limit)
                    .expect("save case trace");
            }
            "saveIndexTrace" => {
                let limit = operation.get("limit").map(as_usize).unwrap_or(30);
                storage
                    .save_index_trace(field(operation, "record"), limit)
                    .expect("save index trace");
            }
            "saveDreamTrace" => {
                let limit = operation.get("limit").map(as_usize).unwrap_or(30);
                storage
                    .save_dream_trace(field(operation, "record"), limit)
                    .expect("save dream trace");
            }
            "saveIndexingSettings" => {
                let defaults = parse_settings(field(operation, "defaults"));
                storage
                    .save_indexing_settings(field(operation, "partial"), &defaults)
                    .expect("save indexing settings");
            }
            "rawPipelineInsert" => {
                let connection = Connection::open(storage.path()).expect("open second connection");
                connection
                    .execute(
                        "INSERT INTO pipeline_state (state_key, state_json, updated_at)
                         VALUES (?1, ?2, ?3)",
                        params![
                            as_text(field(operation, "key")),
                            as_text(field(operation, "json")),
                            as_text(field(operation, "updatedAt")),
                        ],
                    )
                    .expect("insert raw pipeline row");
            }
            other => panic!("unknown operation {other}"),
        }
    }
}

// -- tests ----------------------------------------------------------------

#[test]
fn bundled_sqlite_is_the_node_series() {
    let version = rusqlite::version();
    assert!(
        version.starts_with("3.51."),
        "bundled SQLite must stay on Node 22's 3.51 series, got {version}"
    );
}

#[test]
fn rust_reads_the_ts_written_database() {
    let oracle = baseline::oracle(ORACLE);
    let ts_read = field(&oracle, "tsRead");
    let directory = TempDir::new("ts-read");
    let path = write_encoded_db(
        &directory,
        "ts-read.db",
        &as_text(field(ts_read, "dbBase64")),
    );

    let storage = EdgeClawStorage::open(&path).expect("open TS-written database");
    let observations = observe(&storage);
    drop(storage);

    assert_json_eq(
        &observations,
        field(ts_read, "observations"),
        "Rust observations of the TS-written database",
    );

    let connection = Connection::open(&path).expect("open for dump");
    assert_eq!(journal_mode(&connection), "wal");
    assert_json_eq(
        &dump_schema(&connection),
        field(ts_read, "schema"),
        "sqlite_master of the TS-written database",
    );
    assert_json_eq(
        &dump_columns(&connection),
        field(ts_read, "columns"),
        "table_info of the TS-written database",
    );
    assert_json_eq(
        &dump_rows(&connection),
        field(ts_read, "rows"),
        "stored row text of the TS-written database",
    );
}

#[test]
fn rust_creates_the_same_empty_schema_as_ts() {
    let oracle = baseline::oracle(ORACLE);
    let empty = field(&oracle, "empty");
    let directory = TempDir::new("empty");
    let path = directory.path().join("empty.db");

    let storage = EdgeClawStorage::open(&path).expect("create empty database");
    let observations = observe(&storage);
    drop(storage);

    assert_json_eq(
        &observations,
        field(empty, "observations"),
        "Rust observations of a fresh database",
    );

    let connection = Connection::open(&path).expect("open for dump");
    assert_eq!(journal_mode(&connection), "wal");
    assert_json_eq(
        &dump_schema(&connection),
        field(empty, "schema"),
        "sqlite_master must be character-identical to the TS schema",
    );
    assert_json_eq(
        &dump_columns(&connection),
        field(empty, "columns"),
        "table_info must match the TS schema",
    );
    assert_json_eq(
        &dump_rows(&connection),
        field(empty, "rows"),
        "a fresh database has no rows",
    );
}

#[test]
fn rust_migrates_the_legacy_schema_like_ts() {
    let oracle = baseline::oracle(ORACLE);
    let migration = field(&oracle, "migration");
    let directory = TempDir::new("migration");
    let path = write_encoded_db(
        &directory,
        "legacy.db",
        &as_text(field(migration, "legacyBase64")),
    );

    let storage = EdgeClawStorage::open(&path).expect("migrate legacy database");
    let observations = observe(&storage);
    drop(storage);

    assert_json_eq(
        &observations,
        field(migration, "observations"),
        "Rust observations after migrating the legacy database",
    );

    let connection = Connection::open(&path).expect("open for dump");
    assert_eq!(journal_mode(&connection), "wal");
    assert_json_eq(
        &dump_schema(&connection),
        field(migration, "schema"),
        "migrated sqlite_master must match TS",
    );
    assert_json_eq(
        &dump_columns(&connection),
        field(migration, "columns"),
        "migrated table_info must match TS",
    );
    assert_json_eq(
        &dump_rows(&connection),
        field(migration, "rows"),
        "migrated rows (COALESCE state_json/state_value) must match TS",
    );
}

#[test]
fn rust_writes_and_reads_the_portable_script() {
    let oracle = baseline::oracle(ORACLE);
    let rust_writes = field(&oracle, "rustWrites");
    let directory = TempDir::new("rust-writes");
    let path = directory.path().join("rust.db");

    let storage = EdgeClawStorage::open(&path).expect("create Rust database");
    apply_operations(&storage, field(rust_writes, "operations"));
    assert_json_eq(
        &observe(&storage),
        field(rust_writes, "observations"),
        "TS observations of the portable operation script",
    );
    apply_operations(&storage, field(rust_writes, "limitOperations"));
    assert_json_eq(
        &observe(&storage),
        field(rust_writes, "limitedObservations"),
        "save-side limit truncation",
    );
    drop(storage);

    let connection = Connection::open(&path).expect("open for dump");
    assert_eq!(journal_mode(&connection), "wal");
    assert_json_eq(
        &dump_schema(&connection),
        field(rust_writes, "schema"),
        "Rust-created sqlite_master must match the TS-created schema",
    );
    assert_json_eq(
        &dump_columns(&connection),
        field(rust_writes, "columns"),
        "Rust-created table_info must match TS",
    );
    assert_json_eq(
        &mask_volatile_columns(&dump_rows(&connection)),
        &mask_volatile_columns(field(rust_writes, "rows")),
        "Rust-written row text (JSON column text included) must match TS",
    );
}

/// Every replay goes through `common/baseline.rs::oracle`, which pins the
/// commit and source hashes. Prove the guard actually rejects drift instead of
/// trusting that the fixture happens to be correct today.
#[test]
fn provenance_guard_rejects_a_drifted_oracle() {
    let drifted = ORACLE.replace(
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",
        "0000000000000000000000000000000000000000",
    );
    assert_ne!(drifted, ORACLE, "fixture must record the baseline commit");
    // The pre-fix wiring (`parse_json(ORACLE)`) accepted this text happily;
    // keep that as the in-test negative control so the guard cannot regress
    // into a no-op.
    assert!(
        parse_json(&drifted).is_ok(),
        "the unguarded parser must accept the drift for this control to mean anything"
    );
    let drifted: &str = &drifted;
    let result = std::panic::catch_unwind(|| baseline::oracle(drifted));
    assert!(
        result.is_err(),
        "provenance guard accepted an oracle with a drifted sourceCommit"
    );
}

/// Real TS `save*Trace` calls that throw (recorded in `errors.attempts`) must
/// fail in Rust too and must not overwrite the stored pipeline value. The
/// string-state case is iterable in JS and therefore succeeds.
#[test]
fn trace_writes_that_throw_in_ts_fail_without_writing() {
    let oracle = baseline::oracle(ORACLE);
    let errors = field(&oracle, "errors");
    let directory = TempDir::new("errors");
    let path = directory.path().join("errors.db");
    let storage = EdgeClawStorage::open(&path).expect("create Rust database");
    for attempt in as_array(field(errors, "attempts")) {
        let label = as_text(field(attempt, "label"));
        let record = field(attempt, "record");
        let result = match label.as_str() {
            "index-completed-missing-storedResults" => storage.save_index_trace(record, 30),
            "dream-completed-missing-outcome" => storage.save_dream_trace(record, 30),
            "case-state-object" => {
                storage
                    .set_pipeline_state("recentCaseTraces", &object(vec![("a", number(1))]))
                    .unwrap();
                storage.save_case_trace(record, 30)
            }
            "index-state-null" => {
                storage
                    .set_pipeline_state("recentIndexTraces", &JsValue::Null)
                    .unwrap();
                storage.save_index_trace(record, 30)
            }
            "case-state-string" => {
                storage
                    .set_pipeline_state("recentCaseTraces", &JsValue::from("abc"))
                    .unwrap();
                storage.save_case_trace(record, 30)
            }
            other => panic!("unknown error scenario {other}"),
        };
        let threw = field(attempt, "threw")
            .as_bool()
            .expect("threw is a boolean");
        assert_eq!(result.is_err(), threw, "outcome for {label}: {result:?}");
        let state_key = as_text(field(attempt, "stateKey"));
        let state = optional(storage.get_pipeline_state(&state_key).unwrap());
        assert_json_eq(
            &state,
            field(attempt, "state"),
            &format!("state after {label}"),
        );
    }
}

/// Node 22.22.0 `DatabaseSync` leaves `PRAGMA busy_timeout` at 0: a second
/// writer fails immediately with `ERR_SQLITE_ERROR` / "database is locked".
/// rusqlite would otherwise wait its default 5 s, so assert the elapsed time
/// as well as the error, not just that a write eventually fails.
#[test]
fn write_lock_contention_fails_immediately_like_node() {
    let directory = TempDir::new("busy");
    let path = directory.path().join("busy.db");
    let storage = EdgeClawStorage::open(&path).expect("create Rust database");
    let blocker = Connection::open(storage.path()).expect("open blocking connection");
    blocker
        .busy_timeout(Duration::ZERO)
        .expect("disable blocker timeout");
    blocker
        .execute_batch("BEGIN IMMEDIATE;")
        .expect("take the write lock");
    blocker
        .execute(
            "INSERT INTO pipeline_state (state_key, state_json, updated_at)
             VALUES (?1, ?2, ?3)",
            params!["blocker", "{}", "2026-10-20T00:00:00.000Z"],
        )
        .expect("hold a write");

    let defaults = IndexingSettings {
        reasoning_mode: "answer_first".into(),
        auto_index_interval_minutes: 5,
        auto_dream_interval_minutes: 10,
    };
    let started = Instant::now();
    let error = storage
        .save_indexing_settings(&object(vec![]), &defaults)
        .expect_err("the write lock must reject the second writer");
    let elapsed = started.elapsed();
    blocker
        .execute_batch("ROLLBACK;")
        .expect("release the write lock");

    assert!(
        elapsed < Duration::from_millis(500),
        "busy timeout waited {elapsed:?}; Node fails immediately (timeout 0)"
    );
    match &error {
        StorageError::Sqlite(rusqlite::Error::SqliteFailure(inner, _)) => {
            assert_eq!(
                inner.code,
                rusqlite::ErrorCode::DatabaseBusy,
                "unexpected sqlite error: {error}"
            );
        }
        other => panic!("expected a SQLITE_BUSY error, got {other:?}"),
    }
    assert!(
        error.to_string().contains("database is locked"),
        "Node's message is \"database is locked\", got {error}"
    );
}

/// `insertL0Session` defaults (`sqlite.ts:929,952`): empty `source` becomes
/// `"openclaw"`, empty `createdAt` becomes `nowIso()`. The TS side is frozen in
/// the oracle, so the portable replay masks this row's `created_at`; this test
/// pins the Rust write-side defaults themselves.
#[test]
fn empty_source_and_created_at_use_the_ts_defaults() {
    let directory = TempDir::new("defaults");
    let path = directory.path().join("defaults.db");
    let storage = EdgeClawStorage::open(&path).expect("create Rust database");
    storage
        .insert_l0_session(&L0SessionRecord {
            l0_index_id: "l0-defaults".into(),
            session_key: "s-defaults".into(),
            timestamp: "2026-10-20T04:00:00.000Z".into(),
            messages: JsValue::Array(vec![]),
            source: String::new(),
            indexed: false,
            created_at: String::new(),
        })
        .expect("insert with empty defaults");

    let connection = Connection::open(storage.path()).expect("open for dump");
    let (source, created_at): (String, String) = connection
        .query_row(
            "SELECT source, created_at FROM l0_sessions WHERE l0_index_id = ?1",
            ["l0-defaults"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("read defaults");
    assert_eq!(source, "openclaw");
    assert_eq!(created_at.len(), 24, "{created_at}");
    assert_eq!(&created_at[4..5], "-");
    assert_eq!(&created_at[10..11], "T");
    assert_eq!(&created_at[19..20], ".");
    assert!(created_at.ends_with('Z'), "{created_at}");
}

/// Rust -> TS: a real baseline process reopens the Rust-written database. This
/// needs the checkout and its node_modules, so it is ignored by default.
#[test]
#[ignore = "requires PDRS_BASELINE (PilotDeck cc7c4e6 with node_modules)"]
fn real_ts_reads_the_rust_written_database() {
    let baseline = std::env::var("PDRS_BASELINE").expect("set PDRS_BASELINE");
    let oracle = baseline::oracle(ORACLE);
    let rust_writes = field(&oracle, "rustWrites");
    let directory = TempDir::new("rust-to-ts");
    let path = directory.path().join("rust.db");

    let storage = EdgeClawStorage::open(&path).expect("create Rust database");
    apply_operations(&storage, field(rust_writes, "operations"));
    apply_operations(&storage, field(rust_writes, "limitOperations"));
    drop(storage);

    let verify =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/edgeclaw-storage/verify.mts");
    let loader = Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs");
    let output = directory.path().join("verify.json");
    let status = Command::new("node")
        .arg("--import")
        .arg(&loader)
        .arg(&verify)
        .arg(&path)
        .arg(&output)
        .env("PDRS_BASELINE", &baseline)
        .status()
        .expect("run node verifier");
    assert!(status.success(), "verify.mts failed with {status}");

    let verified = parse_json(&fs::read_to_string(&output).expect("read verify.json"))
        .expect("verify.json parses");
    assert_json_eq(
        field(&verified, "observations"),
        field(rust_writes, "limitedObservations"),
        "real TS observations of the Rust-written database",
    );
    assert_json_eq(
        field(&verified, "schema"),
        field(rust_writes, "schema"),
        "real TS sees the Rust-created sqlite_master",
    );
    assert_json_eq(
        field(&verified, "columns"),
        field(rust_writes, "columns"),
        "real TS sees the Rust-created table_info",
    );
    assert_json_eq(
        &mask_volatile_columns(field(&verified, "rows")),
        &mask_volatile_columns(field(rust_writes, "rows")),
        "real TS reads the Rust-written row text",
    );
}
