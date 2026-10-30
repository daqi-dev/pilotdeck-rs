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
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

const NOW: &str = "2026-10-22T00:00:00.000Z";
const MS: u64 = 1792627200000;
pub(super) const SAME_ROOT_ORACLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/edgeclaw-same-root-generation/oracle.json"
));

pub(super) struct Root(pub(super) PathBuf);

impl Root {
    pub(super) fn new() -> Self {
        Self(std::env::temp_dir().join(format!("pdrs-1082-{}", uuid::Uuid::new_v4())))
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).expect("#1082 parse expected JSON")
}

pub(super) fn options(root: &Path) -> NativeMemoryProviderOptions {
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

pub(super) fn dependencies(root: &Path) -> ServiceDependencies {
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

// Per-thread observation cannot leak into parallel tests or other storages.
thread_local! {
    static LOCK_OBSERVER: std::cell::RefCell<Option<mpsc::Sender<LockEvent>>> = const { std::cell::RefCell::new(None) };
}
#[derive(Debug, PartialEq)]
pub(super) enum LockEvent {
    Contended,
    CloseContended,
    DropContended,
    DropClosed,
    Completed,
}
pub(super) fn lock_observer() -> Option<mpsc::Sender<LockEvent>> {
    LOCK_OBSERVER.with(|observer| observer.borrow().clone())
}
fn join_bounded<T>(task: std::thread::JoinHandle<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !task.is_finished() {
        assert!(
            Instant::now() < deadline,
            "#1082 INVALID worker completion timeout"
        );
        std::thread::yield_now();
    }
    task.join().expect("#1082 INVALID worker panic")
}

struct Writer {
    release: Option<mpsc::Sender<()>>,
    task: Option<std::thread::JoinHandle<Result<(), super::StorageError>>>,
}
impl Writer {
    fn start(old: Arc<EdgeClawMemoryService>, key: &'static str) -> Self {
        let (entered, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let task = std::thread::spawn(move || {
            old.repository
                .storage
                .test_in_flight_write(key, &entered, &wait, false)
        });
        let writer = Self {
            release: Some(release),
            task: Some(task),
        };
        ready
            .recv_timeout(Duration::from_secs(5))
            .expect("#1082 INVALID writer never entered");
        writer
    }
    fn finish(mut self) {
        self.release
            .take()
            .expect("#1082 INVALID release owner")
            .send(())
            .expect("#1082 INVALID writer gone");
        join_bounded(self.task.take().expect("#1082 INVALID writer owner"))
            .expect("#1082 INVALID writer commit");
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(release) = self.release.take() {
            let _ = release.send(());
        }
        // Error cleanup must not turn a bounded failure into an unbounded join.
        // The owned worker receives release; dropping its handle detaches it.
        drop(self.task.take());
    }
}
fn competing_events<T: Send + 'static>(
    writer: Writer,
    action: impl FnOnce() -> T + Send + 'static,
) -> (Vec<LockEvent>, T) {
    let (sender, events) = mpsc::channel();
    let task = std::thread::spawn(move || {
        LOCK_OBSERVER.with(|observer| *observer.borrow_mut() = Some(sender.clone()));
        let result = action();
        LOCK_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        let _ = sender.send(LockEvent::Completed);
        result
    });
    let first = events.recv_timeout(Duration::from_secs(5));
    writer.finish();
    let mut observed = vec![first.expect("#1082 INVALID no acquisition/completion evidence")];
    while observed.last() != Some(&LockEvent::Completed) {
        observed.push(
            events
                .recv_timeout(Duration::from_secs(5))
                .expect("#1082 INVALID competing completion evidence"),
        );
    }
    (observed, join_bounded(task))
}
fn competing<T: Send + 'static>(
    writer: Writer,
    action: impl FnOnce() -> T + Send + 'static,
) -> (bool, T) {
    let (events, result) = competing_events(writer, action);
    (events.contains(&LockEvent::Contended), result)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_stage_serializes_with_in_flight_write() {
    let root = Root::new();
    let handle = tokio::runtime::Handle::current();
    let old = service(&root.0, &handle);
    let writer = Writer::start(old, "blocked-key");
    let path = root.0.clone();
    let (contended, result) = competing(writer, move || stage(&path, &handle));
    let new = result.expect("#1082 new stage must serialize with old writer");
    assert!(
        contended,
        "#1082 new stage must observe actual lock contention"
    );
    assert_eq!(
        new.repository
            .storage
            .get_pipeline_state("blocked-key")
            .expect("#1082 read staged old row"),
        Some(js(&json!({"in_flight":true}))),
        "#1082 new stage sees committed old row"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_first_write_serializes_with_old_generation_write() {
    let root = Root::new();
    let handle = tokio::runtime::Handle::current();
    let old = service(&root.0, &handle);
    let new = service(&root.0, &handle);
    let writer = Writer::start(old.clone(), "old-key");
    let next = new.clone();
    let (contended, result) = competing(writer, move || {
        next.repository
            .storage
            .set_pipeline_state("new-key", &JsValue::from("new"))
    });
    result.expect("#1082 new first write must serialize with old writer");
    assert!(
        contended,
        "#1082 new first write must observe actual lock contention"
    );
    assert_eq!(
        new.repository
            .storage
            .get_pipeline_state("new-key")
            .expect("#1082 read new key"),
        Some(JsValue::from("new")),
        "#1082 new first write persisted"
    );
    assert_eq!(
        old.repository
            .storage
            .get_pipeline_state("old-key")
            .expect("#1082 read old key"),
        Some(js(&json!({"in_flight":true}))),
        "#1082 old row persisted"
    );
}

/// In-process reads serialize too: they must see the committed row, not the
/// uncommitted WAL snapshot from the separate worker-thread TS control.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_read_during_in_flight_write_does_not_error() {
    let root = Root::new();
    let handle = tokio::runtime::Handle::current();
    let old = service(&root.0, &handle);
    let new = service(&root.0, &handle);
    let writer = Writer::start(old, "blocked-key");
    let (contended, read) = competing(writer, move || {
        new.repository.storage.get_pipeline_state("blocked-key")
    });
    let read = read.expect("#1082 serialized read succeeds");
    assert_eq!(
        read,
        Some(js(&json!({"in_flight":true}))),
        "#1082 serialized read must see committed row"
    );
    assert!(
        contended,
        "#1082 serialized read must observe actual lock contention"
    );
}

#[test]
fn same_root_registry_reclaims_dead_paths_and_preserves_live_locks() {
    let root = Root::new();
    let live_key = root.0.join("live");
    let live = super::shared_process_lock(&live_key);
    for n in 0..32 {
        drop(super::shared_process_lock(
            &root.0.join(format!("stage-{n}")),
        ));
    }
    let again = super::shared_process_lock(&live_key);
    assert!(
        Arc::ptr_eq(&live, &again),
        "#1082 registry must preserve live lock identity"
    );
    let table = super::process_locks().lock().expect("#1082 registry table");
    let owned = table.keys().filter(|key| key.starts_with(&root.0)).count();
    assert_eq!(owned, 1, "#1082 registry must reclaim dead stage paths");
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
    let (entered, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let writer = {
        let old = Arc::clone(&old);
        std::thread::spawn(move || {
            std::panic::catch_unwind(AssertUnwindSafe(|| {
                let _ =
                    old.repository
                        .storage
                        .test_in_flight_write("panic-key", &entered, &wait, true);
            }))
        })
    };
    ready
        .recv_timeout(Duration::from_secs(5))
        .expect("#1082 INVALID panic writer entry");
    release
        .send(())
        .expect("#1082 INVALID panic writer release");
    assert!(
        join_bounded(writer).is_err(),
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

/// Execute the serial cells; oracle-only shape checks cannot prove Rust parity.
#[test]
fn same_root_generation_matches_ts_matrix() {
    fn cell(value: Value) -> Value {
        json!({"ok":true,"value":value})
    }
    fn value(storage: &EdgeClawStorage, key: &str) -> Value {
        let value = storage
            .get_pipeline_state(key)
            .expect("#1082 serial read")
            .expect("#1082 serial row exists");
        let text = pilotdeck_protocol::stringify_json(&value)
            .expect("#1082 serial JSON")
            .expect("#1082 serial defined");
        serde_json::from_str(&text).expect("#1082 serial JSON parse")
    }
    let root = Root::new();
    let path = root.0.join("memory.sqlite");
    let mut serial = json!({});
    let initial = EdgeClawStorage::open(&path).expect("#1082 serial initial open");
    initial.close().expect("#1082 serial initial close");
    serial["old-stage"] = cell(json!(true));
    let a = EdgeClawStorage::open(&path).expect("#1082 serial old open");
    let b = EdgeClawStorage::open(&path).expect("#1082 serial new open");
    serial["two-generations-open"] = cell(json!(true));
    a.set_pipeline_state("old-key", &js(&json!({"from":"old"})))
        .expect("#1082 serial old write");
    serial["new-reads-old-write"] = cell(value(&b, "old-key"));
    b.set_pipeline_state("new-key", &js(&json!({"from":"new"})))
        .expect("#1082 serial new write");
    serial["old-reads-new-write"] = cell(value(&a, "new-key"));
    a.close().expect("#1082 serial old close");
    serial["old-close"] = cell(json!(true));
    b.set_pipeline_state("after-close", &js(&json!({"ok":true})))
        .expect("#1082 serial write after close");
    serial["new-write-after-old-close"] = cell(json!(true));
    b.close().expect("#1082 serial new close");
    serial["new-close"] = cell(json!(true));
    let c = EdgeClawStorage::open(&path).expect("#1082 serial reopen");
    serial["reopen-reads-all"] = cell(json!([
        value(&c, "old-key"),
        value(&c, "new-key"),
        value(&c, "after-close")
    ]));
    c.close().expect("#1082 serial reopened close");
    let oracle: Value = serde_json::from_str(SAME_ROOT_ORACLE).expect("#1082 serial oracle");
    assert_eq!(
        serial, oracle["serial"],
        "#1082 serial Rust outputs match TS cells"
    );
}

/// The final owner can disappear without an explicit service.close(). Its
/// SQLite close/checkpoint must participate in the same serialization.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_drop_serializes_with_in_flight_write() {
    let root = Root::new();
    let handle = tokio::runtime::Handle::current();
    let old = service(&root.0, &handle);
    let new = service(&root.0, &handle);
    let writer = Writer::start(old.clone(), "drop-key");
    let weak = Arc::downgrade(&new.repository);
    let (events, ()) = competing_events(writer, move || drop(new));
    assert_eq!(
        events.first(),
        Some(&LockEvent::DropContended),
        "#1082 implicit close must observe Drop-specific lock contention"
    );
    assert!(
        events.contains(&LockEvent::DropClosed),
        "#1082 implicit close must finish SQLite close"
    );
    assert!(
        weak.upgrade().is_none(),
        "#1082 implicit close releases final storage owner"
    );
    assert!(
        old.repository
            .storage
            .get_pipeline_state("drop-key")
            .expect("#1082 read after implicit close")
            .is_some(),
        "#1082 implicit close preserves writer row"
    );
}

// Exercise the public callback boundary, not a fabricated private lock holder.
// A self-contention event is a deterministic regression signal; a missing
// event is INVALID. A regressed worker may be parked, but is never joined.
fn reentrant_drop(mode: &str) {
    let root = Root::new();
    let path = root.0.join("memory.sqlite");
    let mode = mode.to_owned();
    let (sender, events) = mpsc::channel();
    let task = std::thread::spawn(move || {
        let old = EdgeClawStorage::open(&path).expect("#1082 callback old open");
        let peer_path = if mode == "other-root" {
            path.with_file_name("peer.sqlite")
        } else {
            path.clone()
        };
        let new = Arc::new(EdgeClawStorage::open(&peer_path).expect("#1082 callback peer open"));
        let second =
            Arc::new(EdgeClawStorage::open(&peer_path).expect("#1082 callback second peer open"));
        let weak = Arc::downgrade(&new);
        let second_weak = Arc::downgrade(&second);
        let record = |id: &str| super::L0SessionRecord {
            l0_index_id: id.into(),
            session_key: "callback".into(),
            timestamp: NOW.into(),
            messages: js(&json!([])),
            source: "openclaw".into(),
            indexed: false,
            created_at: NOW.into(),
        };
        old.insert_l0_session(&record("seed"))
            .expect("#1082 callback seed row");
        LOCK_OBSERVER.with(|observer| *observer.borrow_mut() = Some(sender.clone()));
        let mut entered = 0;
        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            old.insert_l0_session_if_new(&record("next"), |_| {
                entered += 1;
                if mode == "explicit" {
                    new.close()
                        .expect("#1082 callback explicit close completes");
                    assert!(
                        matches!(new.close(), Err(super::StorageError::Closed)),
                        "#1082 callback explicit close is synchronous"
                    );
                    second
                        .get_pipeline_state("unused")
                        .expect("#1082 callback peer read reenters");
                }
                drop(new);
                drop(second);
                assert!(
                    second_weak.upgrade().is_none(),
                    "#1082 callback second owner released"
                );
                assert!(
                    weak.upgrade().is_none(),
                    "#1082 callback peer final owner released"
                );
                if mode == "panic" {
                    panic!("#1082 intentional callback unwind");
                }
                mode == "early"
            })
        }));
        let key = std::ptr::from_ref(old.process.as_ref()) as usize;
        assert!(
            !super::process_lock_held(key),
            "#1082 callback outer guard releases ownership"
        );
        LOCK_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        let threw = result.is_err();
        if mode == "panic" {
            let panic = result.expect_err("#1082 callback panic caught");
            assert_eq!(
                panic.downcast_ref::<&str>(),
                Some(&"#1082 intentional callback unwind"),
                "#1082 callback catches only the intended panic"
            );
        } else {
            assert_eq!(
                result
                    .expect("#1082 callback unexpectedly panicked")
                    .expect("#1082 callback storage operation"),
                mode != "early",
                "#1082 callback insertion outcome"
            );
        }
        let next = EdgeClawStorage::open(&path).expect("#1082 callback later generation opens");
        let mut ids: Vec<_> = next
            .list_recent_l0(10, 0)
            .expect("#1082 callback later generation reads")
            .into_iter()
            .map(|row| row.l0_index_id)
            .collect();
        ids.sort();
        next.set_pipeline_state("after-callback", &JsValue::from("ok"))
            .expect("#1082 callback later generation writes");
        let actual = json!({"entered":entered, "threw":threw, "rows":ids,
            "after":next.get_pipeline_state("after-callback").expect("#1082 callback persisted later write")
                .expect("#1082 callback later value exists").as_string().expect("#1082 callback later value string")
                .to_utf8().expect("#1082 callback later value UTF8")});
        let oracle: Value = serde_json::from_str(SAME_ROOT_ORACLE).expect("#1082 callback oracle");
        let oracle_mode = if mode == "explicit" {
            "normal"
        } else {
            mode.as_str()
        };
        if mode == "early" {
            // TS has no equivalent public predicate; this is a Rust guard
            // cleanup regression, not a claim of a TS callback probe.
            assert_eq!(
                actual,
                json!({"entered":1,"threw":false,"rows":["seed"],"after":"ok"}),
                "#1082 early callback preserves the existing row and releases ownership"
            );
        } else {
            assert_eq!(
                actual, oracle["closeCallbacks"][oracle_mode],
                "#1082 callback matches TS visible outcomes"
            );
        }
        drop(next);
        drop(old);
        sender
            .send(LockEvent::Completed)
            .expect("#1082 INVALID callback receiver");
    });
    let mut closed = 0;
    loop {
        let event = events
            .recv_timeout(Duration::from_secs(5))
            .expect("#1082 INVALID callback observation timeout");
        assert!(
            !matches!(
                event,
                LockEvent::Contended | LockEvent::DropContended | LockEvent::CloseContended
            ),
            "#1082 callback Drop must not reacquire its own process lock: {event:?}"
        );
        if event == LockEvent::DropClosed {
            closed += 1;
        }
        if event == LockEvent::Completed {
            break;
        }
    }
    join_bounded(task);
    assert_eq!(
        closed, 2,
        "#1082 callback closes both peer connections synchronously"
    );
}

#[test]
fn same_root_drop_from_locked_callback_completes() {
    reentrant_drop("normal");
}
#[test]
fn same_root_drop_from_locked_callback_early_return() {
    reentrant_drop("early");
}
#[test]
fn same_root_drop_from_locked_callback_unwinds() {
    reentrant_drop("panic");
}

#[test]
fn same_root_close_from_locked_callback_is_synchronous() {
    reentrant_drop("explicit");
}
#[test]
fn other_root_drop_from_locked_callback_completes() {
    reentrant_drop("other-root");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_root_close_serializes_with_in_flight_write() {
    let root = Root::new();
    let handle = tokio::runtime::Handle::current();
    let old = service(&root.0, &handle);
    let new = service(&root.0, &handle);
    let writer = Writer::start(old.clone(), "close-key");
    let peer = new.clone();
    let (events, result) = competing_events(writer, move || peer.repository.storage.close());
    result.expect("#1082 explicit close succeeds after writer");
    assert_eq!(
        events.first(),
        Some(&LockEvent::CloseContended),
        "#1082 explicit close must observe Close-specific lock contention"
    );
    assert!(
        matches!(
            new.repository.storage.get_pipeline_state("close-key"),
            Err(super::StorageError::Closed)
        ),
        "#1082 explicit close leaves peer connection closed"
    );
    assert_eq!(
        old.repository
            .storage
            .get_pipeline_state("close-key")
            .expect("#1082 read after explicit close"),
        Some(js(&json!({"in_flight":true}))),
        "#1082 explicit close preserves writer row"
    );
}

#[test]
fn same_root_reentrant_guards_preserve_outer_owner() {
    let root = Root::new();
    let path = root.0.join("memory.sqlite");
    let (sender, events) = mpsc::channel();
    let task = std::thread::spawn(move || {
        let old = EdgeClawStorage::open(&path).expect("#1082 guard old open");
        let peer = EdgeClawStorage::open(&path).expect("#1082 guard peer open");
        let key = std::ptr::from_ref(old.process.as_ref()) as usize;
        LOCK_OBSERVER.with(|observer| *observer.borrow_mut() = Some(sender.clone()));
        let outer = old.conn().expect("#1082 guard outer acquire");
        let inner = peer.conn().expect("#1082 guard inner acquire");
        drop(inner);
        let inner_preserved = super::process_lock_held(key);
        drop(outer);
        let outer_released = !super::process_lock_held(key);
        LOCK_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        sender
            .send(LockEvent::Completed)
            .expect("#1082 INVALID guard event receiver");
        (inner_preserved, outer_released)
    });
    let event = events
        .recv_timeout(Duration::from_secs(5))
        .expect("#1082 INVALID guard observation timeout");
    assert_eq!(
        event,
        LockEvent::Completed,
        "#1082 nested guards must reuse current ownership"
    );
    let (inner_preserved, outer_released) = join_bounded(task);
    assert!(
        inner_preserved,
        "#1082 inner guard preserves outer ownership"
    );
    assert!(
        outer_released,
        "#1082 outer guard releases thread ownership"
    );
}
