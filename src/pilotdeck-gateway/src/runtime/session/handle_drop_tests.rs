//! #1010: dropping the last handle clone must land the custom async
//! release/close whatever the runtime state — the state × event × timing
//! matrix in _tmp/20261026-issue-1010/plan.md. TS has no counterpart: an
//! AgentHandle only runs onDispose through an explicit dispose() and GC is
//! nondeterministic (tests/fixtures/session-drop-release).
use super::*;
use crate::runtime::project::{ProjectRuntime, ProjectRuntimeRegistry, ResourceBundle};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::Notify;

/// Records every session-facing cleanup call. `dispose` signals completion so
/// a test that no longer holds the handle can still await the drain.
struct DropProbe {
    aborts: Mutex<Vec<String>>,
    guards: AtomicUsize,
    dispose_count: AtomicUsize,
    disposed: Notify,
}
impl DropProbe {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            aborts: Mutex::new(vec![]),
            guards: AtomicUsize::new(0),
            dispose_count: AtomicUsize::new(0),
            disposed: Notify::new(),
        })
    }
}
impl LiveSession for DropProbe {
    fn snapshot(&self) -> JsValue {
        JsValue::Null
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        self.aborts
            .lock()
            .expect("#1010 abort observations lock")
            .push(reason.to_utf8().expect("#1010 abort reason"));
        Ok(())
    }
    fn disposal_guard(&self) -> Option<Box<dyn Send>> {
        self.guards.fetch_add(1, Ordering::SeqCst);
        None
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        self.dispose_count.fetch_add(1, Ordering::SeqCst);
        self.disposed.notify_one();
        Box::pin(async { Ok(()) })
    }
}

/// Matrix cell 1: Active handle, last clone dropped, runtime alive. The drain
/// must start on the captured executor and run session.dispose() exactly once.
#[tokio::test]
async fn dropped_handle_runs_session_dispose_once() {
    let session = DropProbe::new();
    drop(LiveSessionHandle::new(session.clone(), None));
    tokio::time::timeout(Duration::from_secs(3), session.disposed.notified())
        .await
        .expect("#1010 dropped handle started the drain");
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        1,
        "#1010 drop runs session.dispose() exactly once"
    );
    assert!(
        session
            .aborts
            .lock()
            .expect("#1010 abort observations lock")
            .is_empty(),
        "#1010 no in-flight activity: drop must not abort the session"
    );
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        1,
        "#1010 the drain takes the writer guard once"
    );
}

/// Matrix cell 2: last clone dropped on a plain thread with no runtime
/// context. The executor captured at construction still receives the drain.
#[tokio::test(flavor = "multi_thread")]
async fn dropped_handle_from_plain_thread_uses_captured_executor() {
    let session = DropProbe::new();
    let handle = LiveSessionHandle::new(session.clone(), None);
    let thread = std::thread::spawn(move || drop(handle));
    thread
        .join()
        .expect("#1010 plain-thread drop finished without a runtime");
    tokio::time::timeout(Duration::from_secs(3), session.disposed.notified())
        .await
        .expect("#1010 captured executor ran the drop drain");
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        1,
        "#1010 off-runtime drop still runs session.dispose() once"
    );
}

/// Matrix cell 6: explicit dispose() called on a plain thread with no runtime
/// context. The captured executor receives it; the returned waiter future may
/// be dropped there without cancelling the drain.
#[tokio::test(flavor = "multi_thread")]
async fn explicit_dispose_from_plain_thread_uses_captured_executor() {
    let session = DropProbe::new();
    let handle = LiveSessionHandle::new(session.clone(), None);
    let activity = handle
        .begin_activity()
        .expect("#1010 activity for abort delivery");
    let thread = std::thread::spawn(move || {
        // The waiter future is dropped, not awaited: losing it must not
        // cancel the drain the captured executor received.
        drop(handle.dispose("external-dispose".into()));
    });
    thread
        .join()
        .expect("#1010 plain-thread dispose finished without a runtime");
    drop(activity);
    tokio::time::timeout(Duration::from_secs(3), session.disposed.notified())
        .await
        .expect("#1010 captured executor ran the explicit disposal");
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        1,
        "#1010 explicit off-runtime dispose runs once"
    );
    assert_eq!(
        *session
            .aborts
            .lock()
            .expect("#1010 abort observations lock"),
        vec!["external-dispose"],
        "#1010 the dispose reason reaches session.abort on the off-runtime thread"
    );
}

/// Builds the handle with `dedicated` as its captured executor, then drops
/// that runtime on a plain thread (tokio forbids dropping one in async
/// context), returning the now-runtime-less handle.
fn handle_on_dead_runtime(
    session: &Arc<DropProbe>,
    lease: Option<RuntimeLease>,
) -> LiveSessionHandle {
    let dedicated = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("#1010 dedicated runtime built");
    let handle = {
        let _guard = dedicated.enter();
        LiveSessionHandle::new(session.clone(), lease)
    };
    std::thread::spawn(move || drop(dedicated))
        .join()
        .expect("#1010 dedicated runtime dropped off the async context");
    handle
}

/// Matrix cell 3: last clone dropped after the owning runtime is gone. The
/// spawn is never polled, so the in-future fallback must land the synchronous
/// writer guard and the lease release; the async hooks cannot run — the same
/// boundary TS hits at process exit.
#[tokio::test]
async fn dropped_handle_without_runtime_lands_writer_fallback() {
    let session = DropProbe::new();
    let handle = handle_on_dead_runtime(&session, None);
    drop(handle);
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        1,
        "#1010 dead runtime: the fallback still takes the writer guard"
    );
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        0,
        "#1010 async dispose hooks need a live runtime (TS process-exit parity)"
    );
    assert!(
        session
            .aborts
            .lock()
            .expect("#1010 abort observations lock")
            .is_empty(),
        "#1010 no runtime to run the drain abort in"
    );
}

/// Matrix cell 3, lease leg: the lease captured by the handle is released
/// synchronously when the never-polled drain task is dropped.
#[tokio::test]
async fn dropped_handle_without_runtime_releases_lease_synchronously() {
    let session = DropProbe::new();
    let registry = ProjectRuntimeRegistry::new(
        "/virtual/1010-drop-release",
        Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(())),
    );
    let lease = registry
        .acquire(None)
        .expect("#1010 project lease acquired");
    let runtime: Arc<ProjectRuntime> = lease.runtime().clone();
    let handle = handle_on_dead_runtime(&session, Some(lease));
    drop(handle);
    assert_eq!(
        runtime.lease_count(),
        0,
        "#1010 dead runtime: the lease count is still released synchronously"
    );
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        1,
        "#1010 dead runtime: the writer guard fallback ran"
    );
}

/// Matrix cell 4: the last clone drops while a disposal is already draining.
/// The drop must share the in-flight drain, not restart or re-abort it.
#[tokio::test]
async fn drop_during_inflight_dispose_does_not_restart() {
    let session = DropProbe::new();
    let handle = LiveSessionHandle::new(session.clone(), None);
    let activity = handle
        .begin_activity()
        .expect("#1010 in-flight activity admitted");
    let disposal = handle.dispose("in-flight-dispose".into());
    drop(handle);
    drop(activity);
    let result = tokio::time::timeout(Duration::from_secs(3), disposal)
        .await
        .expect("#1010 in-flight disposal completed after the drop");
    result.expect("#1010 in-flight disposal succeeded");
    // Let a wrongly restarted drain (negative control) run before asserting.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        1,
        "#1010 drop during in-flight dispose must not restart it"
    );
    assert_eq!(
        *session
            .aborts
            .lock()
            .expect("#1010 abort observations lock"),
        vec!["in-flight-dispose"],
        "#1010 the drop adds no second abort reason"
    );
}

/// Matrix cell 5: already-Disposed handle dropped afterwards is a no-op.
#[tokio::test]
async fn explicit_dispose_then_drop_runs_no_second_dispose() {
    let session = DropProbe::new();
    let handle = LiveSessionHandle::new(session.clone(), None);
    handle
        .dispose("completed".into())
        .await
        .expect("#1010 explicit disposal succeeded");
    drop(handle);
    // Let a wrongly restarted drain (negative control) run before asserting.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        1,
        "#1010 dropping a disposed handle runs no second dispose"
    );
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        1,
        "#1010 no second writer guard after a completed disposal"
    );
}

/// Matrix cell 9: an in-flight turn keeps the session alive. HandleActivity
/// holds its own clone, so dropping the caller's clone must not start the
/// drain until the turn ends.
#[tokio::test]
async fn in_flight_activity_keeps_handle_alive_until_turn_ends() {
    let session = DropProbe::new();
    let handle = LiveSessionHandle::new(session.clone(), None);
    let activity = handle
        .begin_activity()
        .expect("#1010 in-flight activity admitted");
    drop(handle);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        0,
        "#1010 the activity clone keeps the handle alive; no drop drain yet"
    );
    drop(activity);
    tokio::time::timeout(Duration::from_secs(3), session.disposed.notified())
        .await
        .expect("#1010 drain started once the turn ended");
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        1,
        "#1010 the delayed drop drain ran exactly once"
    );
}

const DROP_RELEASE_ORACLE: &str =
    include_str!("../../../tests/fixtures/session-drop-release/oracle.json");

#[test]
#[ignore = "requires the pinned Node v22.22.0 TS baseline"]
fn drop_release_regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-drop-release");
}

/// The TS side of the #1010 matrix (_tmp/20261026-issue-1010/plan.md). Every
/// cell where the Rust fix intentionally diverges is pinned here by what TS
/// does NOT provide, so the Rust drop-drain stays a documented difference
/// rather than an unobserved drift.
#[test]
fn drop_release_matches_ts() {
    crate::test_baseline::oracle(DROP_RELEASE_ORACLE);
    let oracle: serde_json::Value =
        serde_json::from_str(DROP_RELEASE_ORACLE).expect("#1010 drop-release fixture");
    let cases = oracle["cases"]
        .as_array()
        .expect("#1010 drop-release case rows");
    assert_eq!(cases.len(), 5, "#1010 drop-release denominator");
    for row in cases {
        let name = row["name"].as_str().expect("#1010 case name");
        assert_eq!(row["node"], "v22.22.0", "#1010 {name} node oracle");
    }
    let case = |name: &str| {
        cases
            .iter()
            .find(|row| row["name"] == name)
            .unwrap_or_else(|| panic!("#1010 {name} case missing from oracle"))
    };
    // cells 1/10: dropping every reference collects the TS handle without
    // running onDispose — no deterministic destructor exists there.
    let gc = case("handle-drop-gc")["observations"].clone();
    assert_eq!(gc["stateBefore"], "active", "#1010 handle-drop-gc state");
    assert_eq!(gc["collected"], true, "#1010 handle-drop-gc collected");
    assert_eq!(gc["onDisposeCalls"], 0, "#1010 no drop-side cleanup in TS");
    // cell 4: losing the dispose waiter does not cancel the in-flight
    // finishDispose — the TS expectation the Rust drop-drain must meet too.
    let waiter = case("abandoned-waiter")["observations"].clone();
    assert_eq!(
        waiter["stateAfter"], "disposed",
        "#1010 abandoned-waiter state"
    );
    assert_eq!(
        waiter["onDisposeCalls"], 1,
        "#1010 abandoned-waiter cleanup still ran"
    );
    for (name, release_calls) in [
        ("process-exit-release-pending", 0),
        ("process-exit-close-pending", 1),
    ] {
        let observation = &case(name)["observations"];
        assert_eq!(
            observation["childExit"], 7,
            "#1010 cleanup stage child exit"
        );
        assert_eq!(
            observation["stateAtExit"], "draining",
            "#1010 cleanup remains draining at exit"
        );
        assert_eq!(
            observation["onDisposeRan"], true,
            "#1010 cleanup reached the stage gate"
        );
        assert_eq!(
            observation["releaseCalls"], release_calls,
            "#1010 TS release completion count"
        );
        assert_eq!(
            observation["closeCalls"], 0,
            "#1010 TS parked close never completes"
        );
    }
    // cells 3/8: process.exit() is the TS equivalent of a vanished runtime;
    // the sync cutoff crossed but the async cleanup did not land.
    let exit = case("process-exit")["observations"].clone();
    assert_eq!(exit["childExit"], 7, "#1010 process-exit child code");
    assert_eq!(exit["stateAtExit"], "draining", "#1010 process-exit cutoff");
    assert_eq!(
        exit["onDisposeRan"], false,
        "#1010 process-exit skipped the pending async cleanup"
    );
}

#[test]
#[ignore = "requires the pinned Node v22.22.0 TS baseline"]
fn drop_release_preserves_unrelated_out_files() {
    let baseline = std::path::PathBuf::from(
        std::env::var_os("PDRS_BASELINE").expect("#1010 preservation requires PDRS_BASELINE"),
    );
    let output = std::env::temp_dir().join(format!("pdrs-1010-preserve-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(output.join("work-process-exit"))
        .expect("#1010 preservation seed directory");
    let keep = output.join("work-process-exit/keep.txt");
    std::fs::write(&keep, b"#1010 original evidence").expect("#1010 preservation seed file");
    for oracle_exists in [false, true] {
        let original_oracle = std::fs::read(output.join("oracle.json")).ok();
        let mut command = crate::test_baseline::node_command();
        command
            .args([
                "--disable-warning=ExperimentalWarning",
                "--expose-gc",
                "--import",
            ])
            .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
            .arg(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/session-drop-release/generate.mts"),
            )
            .arg(&output)
            .env("PDRS_BASELINE", &baseline);
        let (status, stdout, stderr) = crate::test_baseline::run_generator_with_deadline(
            &mut command,
            "session-drop-release",
            Duration::from_secs(90),
        )
        .expect("#1010 preservation generator completed within its bound");
        println!(
            "#1010 preservation existing_oracle={oracle_exists} status={status}\n{}{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        assert_eq!(
            std::fs::read(&keep).ok().as_deref(),
            Some(b"#1010 original evidence".as_slice()),
            "#1010 generator preserves existing work-process-exit bytes"
        );
        assert_eq!(
            status.success(),
            !oracle_exists,
            "#1010 generator respects exclusive oracle creation"
        );
        if let Some(original) = original_oracle {
            assert_eq!(
                std::fs::read(output.join("oracle.json")).expect("#1010 existing oracle readable"),
                original,
                "#1010 generator preserves existing oracle bytes"
            );
        }
    }
    std::fs::remove_dir_all(output).expect("#1010 remove owned preservation scratch");
}
