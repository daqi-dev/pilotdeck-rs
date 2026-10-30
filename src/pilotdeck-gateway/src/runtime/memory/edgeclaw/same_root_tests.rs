//! #1082: same-rootDir two-generation SQLite serialization (library-only).
//!
//! These cases need the deterministic in-flight writer seam
//! (`EdgeClawStorage::test_in_flight_write`, `#[cfg(test)] pub(crate)`). The
//! shared `tests/edgeclaw_native_default.rs` is also compiled as an
//! integration crate, where `pub(crate)` library items are invisible, so the
//! seam-dependent matrix lives here and the integration-compatible cells stay
//! there.
use super::EdgeClawStorage;
use crate::runtime::memory::edgeclaw::{
    factory::create_with_dependencies,
    service::{EdgeClawMemoryService, ServiceDependencies},
};
use crate::runtime::memory::{MemoryConfig, NativeMemoryProviderOptions};
use pilotdeck_protocol::{JsValue, parse_json};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    fs,
    panic::AssertUnwindSafe,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    time::{Duration, Instant},
};

const NOW: &str = "2026-10-22T00:00:00.000Z";
const MS: u64 = 1792627200000;
const SAME_ROOT_ORACLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/edgeclaw-same-root-generation/oracle.json"
));

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("pdrs-1082-{}", uuid::Uuid::new_v4())))
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).expect("#1082 parse expected JSON")
}

fn options(root: &Path) -> NativeMemoryProviderOptions {
    let mut config = MemoryConfig::new(true, "edgeclaw");
    config.root_dir = root.join("store").to_string_lossy().into_owned().into();
    config.model = Some("fixture/model".into());
    NativeMemoryProviderOptions {
        project_root: root.to_path_buf(),
        config: Some(config),
        model_config: None,
        agent_model: None,
        now: None,
        telemetry: None,
    }
}

fn dependencies(root: &Path) -> ServiceDependencies {
    ServiceDependencies {
        home: Some(root.join("home")),
        clock: Some(Arc::new(|| NOW.into())),
        now_ms: Some(Arc::new(|| MS as i64)),
        ..Default::default()
    }
}

fn stage(
    root: &Path,
    handle: &tokio::runtime::Handle,
) -> Result<Arc<EdgeClawMemoryService>, String> {
    let _entered = handle.enter();
    match create_with_dependencies(&options(root), dependencies(root)) {
        Ok(Some(created)) => Ok(created.service),
        Ok(None) => Err("#1082 same-root factory unexpectedly disabled".into()),
        Err(error) => Err(error.to_string()),
    }
}

fn service(root: &Path, handle: &tokio::runtime::Handle) -> Arc<EdgeClawMemoryService> {
    stage(root, handle).expect("#1082 same-root generation stage")
}

/// Matrix cell 2/3: the new generation's stage and first write must run after
/// the old generation's in-flight write commits, never `database is locked`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_stage_serializes_with_in_flight_write() {
    let root = Root::new();
    let handle = tokio::runtime::Handle::current();
    let old = service(&root.0, &handle);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let old = Arc::clone(&old);
        let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
        std::thread::spawn(move || {
            old.repository
                .storage
                .test_in_flight_write("blocked-key", &entered, &release, false)
        })
    };
    entered.wait();
    let staged = {
        let root = root.0.clone();
        let handle = handle.clone();
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let staged = stage(&root, &handle);
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            staged
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    let finished_early = done.load(std::sync::atomic::Ordering::SeqCst);
    release.wait();
    writer
        .join()
        .expect("#1082 in-flight writer thread")
        .expect("#1082 in-flight write commits");
    let new = staged
        .join()
        .expect("#1082 stage thread")
        .unwrap_or_else(|error| {
            panic!("#1082 new stage must not fail while the old generation writes: {error}")
        });
    assert!(
        !finished_early,
        "#1082 new stage must wait for the in-flight old-generation write, not interleave"
    );
    assert!(
        new.repository
            .storage
            .get_pipeline_state("blocked-key")
            .expect("#1082 new generation read")
            .is_some(),
        "#1082 new generation reads the old generation's committed write"
    );
}

/// Matrix cell 3: the new generation's first write and the old generation's
/// in-flight write are serialized, never `database is locked`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_first_write_serializes_with_old_generation_write() {
    let root = Root::new();
    let handle = tokio::runtime::Handle::current();
    let old = service(&root.0, &handle);
    let new = service(&root.0, &handle);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let old = Arc::clone(&old);
        let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
        std::thread::spawn(move || {
            old.repository
                .storage
                .test_in_flight_write("old-key", &entered, &release, false)
        })
    };
    entered.wait();
    let first = {
        let new = Arc::clone(&new);
        let done = Arc::clone(&done);
        std::thread::spawn(move || {
            let result = new
                .repository
                .storage
                .set_pipeline_state("new-key", &JsValue::from("new"));
            done.store(true, std::sync::atomic::Ordering::SeqCst);
            result
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    let finished_early = done.load(std::sync::atomic::Ordering::SeqCst);
    release.wait();
    writer
        .join()
        .expect("#1082 in-flight writer thread")
        .expect("#1082 in-flight write commits");
    first
        .join()
        .expect("#1082 first-write thread")
        .unwrap_or_else(|error| {
            panic!("#1082 new first write must not fail while the old generation writes: {error}")
        });
    assert!(
        !finished_early,
        "#1082 new first write must wait for the in-flight old-generation write"
    );
    assert_eq!(
        new.repository
            .storage
            .get_pipeline_state("new-key")
            .expect("#1082 read new key"),
        Some(JsValue::from("new")),
        "#1082 new generation's first write is persisted"
    );
    assert_eq!(
        old.repository
            .storage
            .get_pipeline_state("old-key")
            .expect("#1082 read old key"),
        Some(js(&json!({"in_flight":true}))),
        "#1082 old generation reads its committed in-flight row"
    );
}

/// Matrix cell 4: a WAL read either runs before the writer commits (sees no
/// row) or after it (sees the row), but never fails with a lock error.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_read_during_in_flight_write_does_not_error() {
    let root = Root::new();
    let handle = tokio::runtime::Handle::current();
    let old = service(&root.0, &handle);
    let new = service(&root.0, &handle);
    new.repository
        .storage
        .set_pipeline_state("visible-key", &JsValue::from("visible"))
        .expect("#1082 seed visible row");
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let writer = {
        let old = Arc::clone(&old);
        let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
        std::thread::spawn(move || {
            old.repository
                .storage
                .test_in_flight_write("blocked-key", &entered, &release, false)
        })
    };
    entered.wait();
    let reader = {
        let new = Arc::clone(&new);
        std::thread::spawn(move || new.repository.storage.get_pipeline_state("blocked-key"))
    };
    std::thread::sleep(Duration::from_millis(100));
    release.wait();
    writer
        .join()
        .expect("#1082 in-flight writer thread")
        .expect("#1082 in-flight write commits");
    let read = reader
        .join()
        .expect("#1082 reader thread")
        .unwrap_or_else(|error| panic!("#1082 read must not fail with a lock error: {error}"));
    assert!(
        read.is_none() || read == Some(js(&json!({"in_flight":true}))),
        "#1082 WAL read sees a committed value or no row, got {read:?}"
    );
    assert_eq!(
        new.repository
            .storage
            .get_pipeline_state("visible-key")
            .expect("#1082 read visible key"),
        Some(JsValue::from("visible")),
        "#1082 reads keep returning committed data while another generation writes"
    );
}

/// Matrix cell 9: a writer that panics while holding the in-process lock
/// releases it; a later generation's write must still succeed (TS has no
/// poisoning analogue, but the lock must never become permanently fatal).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_lock_survives_writer_panic() {
    let root = Root::new();
    let handle = tokio::runtime::Handle::current();
    let old = service(&root.0, &handle);
    let new = service(&root.0, &handle);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let writer = {
        let old = Arc::clone(&old);
        let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
        std::thread::spawn(move || {
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _ = old.repository.storage.test_in_flight_write(
                    "panic-key",
                    &entered,
                    &release,
                    true,
                );
            }))
        })
    };
    entered.wait();
    release.wait();
    assert!(
        writer.join().expect("#1082 panic thread").is_err(),
        "#1082 in-flight writer panics after taking the lock"
    );
    new.repository
        .storage
        .set_pipeline_state("after-panic", &JsValue::from("ok"))
        .unwrap_or_else(|error| panic!("#1082 write after writer panic: {error}"));
    assert_eq!(
        new.repository
            .storage
            .get_pipeline_state("after-panic")
            .expect("#1082 read after-panic row"),
        Some(JsValue::from("ok")),
        "#1082 a panicked writer does not poison the shared lock for later generations"
    );
}

/// Matrix cell 8 (legacy schema needing a migration): a writer in another
/// process still gets Node's immediate `database is locked`; the rejected
/// short-`busy_timeout` fix would wait here instead.
#[test]
fn same_root_stage_needing_migration_fails_immediately_against_external_writer() {
    let root = Root::new();
    let path = root.0.join("legacy").join("memory.sqlite");
    fs::create_dir_all(path.parent().expect("#1082 legacy parent")).expect("#1082 legacy mkdir");
    let legacy = Connection::open(&path).expect("#1082 create legacy database");
    legacy
        .busy_timeout(Duration::ZERO)
        .expect("#1082 legacy busy timeout");
    legacy
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE l0_sessions (
               l0_index_id TEXT PRIMARY KEY,
               session_key TEXT NOT NULL,
               timestamp TEXT NOT NULL,
               messages_json TEXT NOT NULL,
               source TEXT NOT NULL,
               indexed INTEGER NOT NULL DEFAULT 0,
               created_at TEXT NOT NULL
             );
             CREATE INDEX idx_l0_sessions_session ON l0_sessions(session_key);
             CREATE INDEX idx_l0_sessions_pending ON l0_sessions(indexed, timestamp);
             CREATE TABLE pipeline_state (
               state_key TEXT PRIMARY KEY,
               state_json TEXT,
               state_value TEXT,
               updated_at TEXT NOT NULL
             );
             INSERT INTO pipeline_state (state_key, state_json, state_value, updated_at)
             VALUES ('workspaceDir', NULL, '\"legacy\"', '2026-10-22T00:00:00.000Z');",
        )
        .expect("#1082 legacy schema");
    let blocker = Connection::open(&path).expect("#1082 open blocker");
    blocker
        .busy_timeout(Duration::ZERO)
        .expect("#1082 blocker busy timeout");
    blocker
        .execute_batch("BEGIN IMMEDIATE;")
        .expect("#1082 blocker write lock");
    let started = Instant::now();
    let error = EdgeClawStorage::open(&path)
        .err()
        .unwrap_or_else(|| panic!("#1082 cross-process stage must fail immediately"));
    let elapsed = started.elapsed();
    blocker
        .execute_batch("ROLLBACK;")
        .expect("#1082 release blocker");
    assert!(
        elapsed < Duration::from_millis(500),
        "#1082 cross-process stage still fails immediately (elapsed {elapsed:?})"
    );
    assert!(
        error.to_string().contains("database is locked"),
        "#1082 cross-process stage reports database is locked, got {error}"
    );
    let oracle: Value = serde_json::from_str(SAME_ROOT_ORACLE).expect("#1082 same-root TS oracle");
    let ts_message = oracle["threaded"]["stage-legacy-while-write"]["error"]["message"]
        .as_str()
        .expect("#1082 TS legacy stage error message");
    assert!(
        error.to_string().ends_with(ts_message),
        "#1082 Rust cross-process lock error matches the real TS message, got {error}"
    );
    let migrated = EdgeClawStorage::open(&path).expect("#1082 migrate once the blocker is gone");
    assert_eq!(
        migrated
            .get_pipeline_state("workspaceDir")
            .expect("#1082 migrated row"),
        Some(JsValue::from("legacy")),
        "#1082 the migration still runs after the lock is released"
    );
}
