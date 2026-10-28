//! Cross-process same-DB WAL regression (fix-r1 P1-6, issue #776).
//!
//! The migration-period boundary a single-process replay cannot see: the TS
//! UI server and this service writing one `control.sqlite` at the same time.
//! The real TS baseline probe (`_tmp/20261026-issue-776/fix-r1/probe/
//! wal-two-process.mts`, PilotDeck cc7c4e6, Node v22.22.0) held the DB's WAL
//! write lock from a second node process with `BEGIN IMMEDIATE` and observed:
//!
//!   1. the parent service's `captureTurn` fails immediately (~1 ms) with
//!      `database is locked` — `node:sqlite` keeps `busy_timeout` at 0;
//!   2. reads (`listMemoryEntries`) keep working — WAL readers are not
//!      blocked by another process's write lock;
//!   3. once the other process releases, the same write succeeds.
//!
//! This test reproduces exactly that with a second Rust process (this test
//! binary's own child) holding the WAL write lock. Every wait is bounded and
//! the child is killed at a deadline, so a regression fails with a message
//! instead of hanging the suite.

#[path = "common/scratch.rs"]
mod scratch;

use pilotdeck_gateway::runtime::memory::edgeclaw::{
    llm::config::{LlmOptions, build_llm_config_with},
    service::{CaptureTurnInput, EdgeClawMemoryService, ServiceDependencies, ServiceOptions},
};
use pilotdeck_protocol::{JsObject, JsValue};
use rusqlite::Connection;
use scratch::Scratch;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const NOW: &str = "2026-10-22T00:00:00.000Z";
const NOW_MS: i64 = 1_792_627_200_000; // Date.parse("2026-10-22T00:00:00.000Z")
const CHILD: &str = "wal_lock_child";

/// Declarative seed inputs become `JsValue` for the service call, as in the
/// replay fixture.
fn js_from(value: &Value) -> JsValue {
    match value {
        Value::Null => JsValue::Null,
        Value::Bool(value) => JsValue::Bool(*value),
        Value::Number(number) => JsValue::Number(number.as_f64().unwrap_or_default()),
        Value::String(value) => JsValue::String(value.as_str().into()),
        Value::Array(items) => JsValue::Array(items.iter().map(js_from).collect()),
        Value::Object(entries) => {
            let mut object = JsObject::new();
            for (key, item) in entries {
                object.insert(key.as_str().into(), js_from(item));
            }
            JsValue::Object(object)
        }
    }
}

fn messages() -> Vec<JsValue> {
    [
        json!({"role": "user", "content": "alpha user"}),
        json!({"role": "assistant", "content": "alpha assistant"}),
    ]
    .iter()
    .map(js_from)
    .collect()
}

fn capture(
    service: &EdgeClawMemoryService,
    messages: &[JsValue],
    session_key: &str,
    timestamp: &str,
) -> Result<(), String> {
    service
        .capture_turn(
            messages,
            CaptureTurnInput {
                session_key: session_key.into(),
                timestamp: Some(timestamp.into()),
                source: None,
            },
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[test]
fn cross_process_wal_writer_blocks_service_writes_like_ts() {
    let scratch = Scratch::new("edgeclaw-wal-two-process");
    let dir = scratch.path();
    fs::create_dir_all(dir.join("workspace")).expect("#776 wal-two-process workspace dir");
    fs::create_dir_all(dir.join("home")).expect("#776 wal-two-process home dir");
    // Fully explicit LLM options, as in the replay fixture: no env and no
    // openclaw store is read. No request is ever sent — `captureTurn` never
    // reaches the LLM — so no transport seam is needed.
    let llm = LlmOptions {
        provider: Some("p".into()),
        model: Some("m".into()),
        model_ref: Some("p/m".into()),
        api_type: Some("openai-completions".into()),
        base_url: Some("http://127.0.0.1/v1".into()),
        api_key: Some("fixture-key".into()),
        headers: None,
    };
    let config = build_llm_config_with(Some(&llm), None, &|_| None);
    let mut options = ServiceOptions::new(dir.join("workspace"));
    options.root_dir = Some(dir.join("root"));
    let service = EdgeClawMemoryService::with_dependencies(
        options,
        ServiceDependencies {
            home: Some(dir.join("home")),
            clock: Some(Arc::new(|| NOW.to_owned())),
            now_ms: Some(Arc::new(|| NOW_MS)),
            llm_config: Some(config),
            ..ServiceDependencies::default()
        },
    )
    .expect("#776 wal-two-process construct test service");

    // Baseline: the write path works before another process holds the lock.
    let messages = messages();
    capture(&service, &messages, "s0", "2026-10-22T00:00:00.000Z")
        .expect("#776 wal-two-process baseline captureTurn");

    // The child holds the WAL write lock (BEGIN IMMEDIATE) and publishes its
    // marker atomically (tmp + rename), so a poll can never observe a partial
    // signal file.
    let locked_marker = dir.join("wal2p-locked.marker");
    let mut child =
        Command::new(std::env::current_exe().expect("#776 wal-two-process child executable"))
            .args(["--exact", CHILD, "--ignored", "--nocapture"])
            .env("PDRS_WAL2P_DB", &service.db_path)
            .env("PDRS_WAL2P_LOCKED", &locked_marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("#776 wal-two-process spawn lock child");
    let deadline = Instant::now() + Duration::from_secs(20);
    while !locked_marker.exists() {
        assert!(
            Instant::now() < deadline,
            "#776 wal-two-process: the child never locked the database"
        );
        thread::sleep(Duration::from_millis(10));
    }

    // 1) While the write lock is held, the service's write must fail —
    // immediately, with node:sqlite's `busy_timeout` 0 semantics.
    let started = Instant::now();
    let locked = capture(&service, &messages, "s1", "2026-10-22T00:00:01.000Z");
    let elapsed = started.elapsed();
    let error = locked.expect_err(
        "#776 wal-two-process: captureTurn must fail while another process \
         holds the WAL write lock",
    );
    assert!(
        error.to_lowercase().contains("database is locked"),
        "#776 wal-two-process: unexpected locked-write error shape: {error}"
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "#776 wal-two-process: the locked write must fail immediately \
         (busy_timeout 0), took {elapsed:?}"
    );

    // 2) WAL readers are not blocked by the writer's lock.
    service
        .repository
        .list_memory_entries(&JsObject::new())
        .expect("#776 wal-two-process: reads must keep working under the write lock");

    // 3) Release the child's lock; the same write must then succeed.
    let mut stdin = child
        .stdin
        .take()
        .expect("#776 wal-two-process child stdin");
    stdin
        .write_all(b"release\n")
        .expect("#776 wal-two-process release write");
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child
            .try_wait()
            .expect("#776 wal-two-process child try_wait")
        {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "#776 wal-two-process: the child never exited after release"
        );
        thread::sleep(Duration::from_millis(10));
    };
    let output = child
        .wait_with_output()
        .expect("#776 wal-two-process child output");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        status.success(),
        "#776 wal-two-process: the lock child failed: {text}"
    );
    // #273: only the libtest receipt proves the child ran its test.
    assert!(
        text.contains("test result: ok. 1 passed"),
        "#776 wal-two-process: the lock child did not run: {text}"
    );
    capture(&service, &messages, "s1", "2026-10-22T00:00:01.000Z")
        .expect("#776 wal-two-process: the retry after release must succeed");

    service.close().expect("#776 wal-two-process close service");
}

/// Spawned by [`cross_process_wal_writer_blocks_service_writes_like_ts`]:
/// hold the WAL write lock of the DB in `PDRS_WAL2P_DB` with `BEGIN
/// IMMEDIATE`, publish the marker in `PDRS_WAL2P_LOCKED`, then roll back and
/// exit once stdin releases us.
#[test]
#[ignore = "spawned by cross_process_wal_writer_blocks_service_writes_like_ts"]
fn wal_lock_child() {
    let (Ok(db), Ok(locked)) = (
        std::env::var("PDRS_WAL2P_DB"),
        std::env::var("PDRS_WAL2P_LOCKED"),
    ) else {
        return;
    };
    let conn = Connection::open(&db).expect("#776 wal-two-process child open control.sqlite");
    conn.busy_timeout(Duration::ZERO)
        .expect("#776 wal-two-process child busy_timeout");
    conn.execute_batch("BEGIN IMMEDIATE")
        .expect("#776 wal-two-process child BEGIN IMMEDIATE");
    let tmp = format!("{}.tmp", locked);
    fs::write(&tmp, "locked\n").expect("#776 wal-two-process child marker tmp");
    fs::rename(&tmp, &locked).expect("#776 wal-two-process child marker publish");
    let mut release = String::new();
    std::io::stdin()
        .read_line(&mut release)
        .expect("#776 wal-two-process child release read");
    conn.execute_batch("ROLLBACK")
        .expect("#776 wal-two-process child ROLLBACK");
}
