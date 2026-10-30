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
    assert_eq!(cases.len(), 7, "#1010 drop-release denominator");
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

struct GeneratorScratch(std::path::PathBuf);
impl GeneratorScratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("pdrs-1010-preserve-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).expect("#1010 claim generator test scratch");
        Self(path)
    }
}
impl Drop for GeneratorScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_drop_generator(
    output: &Path,
    preload: Option<&Path>,
) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    let baseline = std::path::PathBuf::from(
        std::env::var_os("PDRS_BASELINE").expect("#1010 preservation requires PDRS_BASELINE"),
    );
    let mut command = crate::test_baseline::node_command();
    command
        .args([
            "--disable-warning=ExperimentalWarning",
            "--expose-gc",
            "--import",
        ])
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"));
    if let Some(preload) = preload {
        command.arg("--import").arg(preload);
    }
    command
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/session-drop-release/generate.mts"),
        )
        .arg(output)
        .env("PDRS_BASELINE", &baseline);
    crate::test_baseline::run_generator_with_deadline(
        &mut command,
        "session-drop-release",
        Duration::from_secs(90),
    )
    .expect("#1010 preservation generator completed within its bound")
}

#[test]
#[ignore = "requires the pinned Node v22.22.0 TS baseline"]
fn drop_release_preserves_unrelated_out_files() {
    let scratch = GeneratorScratch::new();
    // Empty, pre-existing work root, and pre-existing oracle all reject at
    // admission. A directory's existence is never permission to modify it.
    for seed in ["work", "oracle", "empty"] {
        let output = scratch.0.join(seed);
        std::fs::create_dir(&output).expect("#1010 pre-existing output");
        let keep = output.join("work-process-exit/keep.txt");
        if seed != "empty" {
            std::fs::create_dir(output.join("work-process-exit"))
                .expect("#1010 preservation seed directory");
            std::fs::write(&keep, b"#1010 original evidence")
                .expect("#1010 preservation seed file");
        }
        if seed == "oracle" {
            std::fs::write(output.join("oracle.json"), b"#1010 original oracle")
                .expect("#1010 seed existing oracle");
        }
        let entries = || {
            let mut names: Vec<_> = std::fs::read_dir(&output)
                .expect("#1010 read output entries")
                .map(|entry| entry.expect("#1010 read output entry").file_name())
                .collect();
            names.sort();
            names
        };
        let before = entries();
        let (status, stdout, stderr) = run_drop_generator(&output, None);
        println!(
            "#1010 preservation seed={seed} status={status}\n{}{}",
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr)
        );
        if seed != "empty" {
            assert_eq!(
                std::fs::read(&keep).ok().as_deref(),
                Some(b"#1010 original evidence".as_slice()),
                "#1010 generator preserves existing work-process-exit bytes"
            );
        }
        if seed == "oracle" {
            assert_eq!(
                std::fs::read(output.join("oracle.json")).expect("#1010 existing oracle readable"),
                b"#1010 original oracle",
                "#1010 generator preserves existing oracle bytes"
            );
        }
        assert!(
            !status.success(),
            "#1010 generator rejects existing output at admission"
        );
        let error = String::from_utf8_lossy(&stderr);
        assert!(
            error.contains("EEXIST") && error.contains(output.to_string_lossy().as_ref()),
            "#1010 rejection identifies existing output"
        );
        assert_eq!(
            entries(),
            before,
            "#1010 admission refusal leaves directory entries unchanged"
        );
    }
}

#[test]
#[ignore = "requires the pinned Node v22.22.0 TS baseline"]
fn drop_release_preserves_late_oracle() {
    let scratch = GeneratorScratch::new();
    let output = scratch.0.join("out");
    let preload = scratch.0.join("late-oracle.mjs");
    // Intercept only the actual final write. This deterministic competitor
    // arrives after exclusive directory admission, so wx is tested directly.
    std::fs::write(
        &preload,
        r#"import fs from 'node:fs';
const write = fs.writeFileSync;
fs.writeFileSync = function(file, ...args) {
 if (String(file).endsWith('/oracle.json')) {
  fs.writeFileSync = write;
  write(file, '#1010 competing oracle', {flag:'wx'});
 }
 return write(file, ...args);
};
"#,
    )
    .expect("#1010 write late oracle hook");
    let (status, stdout, stderr) = run_drop_generator(&output, Some(&preload));
    println!(
        "#1010 late oracle status={status}\n{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    assert_eq!(
        std::fs::read(output.join("oracle.json")).expect("#1010 late oracle exists"),
        b"#1010 competing oracle",
        "#1010 generator preserves late oracle bytes"
    );
    assert!(!status.success(), "#1010 late oracle write is refused");
    let error = String::from_utf8_lossy(&stderr);
    assert!(
        error.contains("EEXIST") && error.contains("oracle.json"),
        "#1010 late oracle rejection identifies file: {error}"
    );
    assert_eq!(
        std::fs::read_dir(&output)
            .expect("#1010 late oracle output entries")
            .count(),
        1,
        "#1010 generator cleans owned work roots on failure"
    );
}

async fn assert_shutdown_result(handle: &LiveSessionHandle, waiter: DisposeFuture<'static>) {
    let error = tokio::time::timeout(Duration::from_secs(3), waiter)
        .await
        .expect("#1010 shutdown disposal returns within its bound")
        .expect_err("#1010 shutdown disposal cannot claim success");
    assert_eq!(
        error.message, "Session lifecycle executor stopped.",
        "#1010 unpolled drain reports stopped executor"
    );
    assert_eq!(
        handle.state(),
        HandleState::Disposed,
        "#1010 unpolled drain publishes Disposed"
    );
    assert!(
        !handle
            .0
            .progress
            .lock()
            .expect("#1010 shutdown progress")
            .disposal_completed,
        "#1010 unpolled drain does not claim completed hooks"
    );
    assert!(
        handle
            .0
            .progress
            .lock()
            .expect("#1010 shutdown completion lock")
            .disposal
            .as_ref()
            .expect("#1010 shutdown completion receiver")
            .borrow()
            .is_some(),
        "#1010 fallback explicitly publishes completion"
    );
    let repeated = tokio::time::timeout(Duration::from_secs(3), handle.dispose("again".into()))
        .await
        .expect("#1010 repeated shutdown disposal returns")
        .expect_err("#1010 repeated shutdown disposal cannot succeed");
    assert_eq!(
        repeated.message, error.message,
        "#1010 shutdown result remains stable"
    );
}

#[tokio::test]
async fn explicit_dispose_on_dead_executor_finishes() {
    let session = DropProbe::new();
    let handle = handle_on_dead_runtime(&session, None);
    let waiter = handle.dispose("dead".into());
    assert_shutdown_result(&handle, waiter).await;
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        1,
        "#1010 dead explicit disposal takes writer fallback once"
    );
}

#[tokio::test]
async fn queued_dispose_shutdown_finishes() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("#1010 queued executor");
    let session = DropProbe::new();
    let handle = {
        let _entered = executor.enter();
        LiveSessionHandle::new(session.clone(), None)
    };
    let waiter = handle.dispose("queued".into());
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        0,
        "#1010 queued drain has never been polled"
    );
    std::thread::spawn(move || drop(executor))
        .join()
        .expect("#1010 queued shutdown");
    assert_shutdown_result(&handle, waiter).await;
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        1,
        "#1010 queued disposal takes writer fallback once"
    );
}

/// A closed completion sender already returns an error: the review's
/// hypothetical infinite wait was not present, independently of fallback.
#[tokio::test]
async fn closed_lifecycle_channel_returns_error() {
    let (sender, receiver) = watch::channel(None::<RuntimeResult>);
    drop(sender);
    let error = tokio::time::timeout(Duration::from_secs(3), wait_result(receiver))
        .await
        .expect("#1010 closed completion channel returns boundedly")
        .expect_err("#1010 closed completion channel returns error");
    assert_eq!(
        error.message, "Session lifecycle executor stopped.",
        "#1010 closed completion channel error"
    );
}

struct ParkedDispose {
    entered: watch::Sender<bool>,
    panic: bool,
}
impl LiveSession for ParkedDispose {
    fn snapshot(&self) -> JsValue {
        JsValue::Null
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.entered.send_replace(true);
            assert!(!self.panic, "#1010 intentional dispose panic");
            std::future::pending::<RuntimeResult>().await
        })
    }
}

fn poison<T: Send>(mutex: &Mutex<T>) {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _guard = mutex
                    .lock()
                    .expect("#1010 poison starts from healthy mutex");
                panic!("#1010 intentional mutex poison");
            })
            .join()
            .expect_err("#1010 poison worker panicked as intended");
    });
}

async fn polled_shutdown(poison_progress: bool) {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#1010 polled executor");
    let (entered, mut entered_rx) = watch::channel(false);
    let handle = {
        let _enter = executor.enter();
        LiveSessionHandle::new(
            Arc::new(ParkedDispose {
                entered,
                panic: false,
            }),
            None,
        )
    };
    let waiter = handle.dispose("polled".into());
    // Drive the owning executor only until the real disposer reaches its
    // never-completing hook. No wall-clock sleep decides the cancellation point.
    let executor = std::thread::spawn(move || {
        executor.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), entered_rx.wait_for(|v| *v))
                .await
                .expect("#1010 disposer reached permanent gate")
                .expect("#1010 disposer gate sender alive");
        });
        executor
    })
    .join()
    .expect("#1010 drive disposer thread");
    if poison_progress {
        poison(&handle.0.progress);
    }
    std::thread::spawn(move || drop(executor))
        .join()
        .expect("#1010 polled shutdown");
    let error = tokio::time::timeout(Duration::from_secs(3), waiter)
        .await
        .expect("#1010 cancelled disposal completes boundedly")
        .expect_err("#1010 cancelled disposal cannot claim success");
    assert_eq!(
        error.message, "Agent session dispose task cancelled.",
        "#1010 shutdown cancellation is not a panic"
    );
    let progress = handle
        .0
        .progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        progress.state,
        HandleState::Disposed,
        "#1010 cancelled drain publishes Disposed"
    );
    assert!(
        !progress.disposal_completed,
        "#1010 cancelled hooks remain incomplete"
    );
}

#[tokio::test]
async fn polled_dispose_shutdown_reports_cancellation() {
    polled_shutdown(false).await;
}

#[tokio::test]
async fn poisoned_dispose_guard_still_completes() {
    polled_shutdown(true).await;
}

#[tokio::test]
async fn dispose_panic_is_distinct_from_cancellation() {
    let handle = LiveSessionHandle::new(
        Arc::new(ParkedDispose {
            entered: watch::channel(false).0,
            panic: true,
        }),
        None,
    );
    let error = tokio::time::timeout(Duration::from_secs(3), handle.dispose("panic".into()))
        .await
        .expect("#1010 panicked disposal completes boundedly")
        .expect_err("#1010 panicked disposal cannot claim success");
    assert_eq!(
        error.message, "Agent session dispose task panicked.",
        "#1010 actual panic remains distinguishable"
    );
    assert_eq!(
        handle.state(),
        HandleState::Disposed,
        "#1010 panicked drain publishes Disposed"
    );
}

#[tokio::test]
async fn poisoned_handle_drop_still_drains() {
    for progress in [true, false] {
        let session = DropProbe::new();
        let handle = LiveSessionHandle::new(session.clone(), None);
        if progress {
            poison(&handle.0.progress);
        } else {
            poison(&handle.0.lease);
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(handle)));
        assert!(result.is_ok(), "#1010 poisoned handle Drop does not panic");
        tokio::time::timeout(Duration::from_secs(3), session.disposed.notified())
            .await
            .expect("#1010 poisoned handle drain still reaches dispose");
        assert_eq!(
            session.guards.load(Ordering::SeqCst),
            1,
            "#1010 poisoned handle acquires writer guard once"
        );
    }
}

#[tokio::test]
async fn poisoned_activity_drop_still_drains() {
    let session = DropProbe::new();
    let handle = LiveSessionHandle::new(session.clone(), None);
    let activity = handle
        .begin_activity()
        .expect("#1010 activity before poisoning");
    poison(&handle.0.progress);
    drop(handle);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(activity)));
    assert!(
        result.is_ok(),
        "#1010 poisoned activity Drop does not panic"
    );
    tokio::time::timeout(Duration::from_secs(3), session.disposed.notified())
        .await
        .expect("#1010 poisoned activity releases its drain");
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        1,
        "#1010 poisoned activity acquires writer guard once"
    );
}

#[tokio::test]
async fn poisoned_unpolled_fallback_still_completes() {
    let session = DropProbe::new();
    let handle = handle_on_dead_runtime(&session, None);
    poison(&handle.0.progress);
    let waiter = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        handle.dispose("poison".into())
    }))
    .expect("#1010 poisoned fallback starts without panic");
    let error = tokio::time::timeout(Duration::from_secs(3), waiter)
        .await
        .expect("#1010 poisoned fallback completes boundedly")
        .expect_err("#1010 poisoned fallback cannot claim success");
    assert_eq!(
        error.message, "Session lifecycle executor stopped.",
        "#1010 poisoned fallback error"
    );
    let progress = handle
        .0
        .progress
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert_eq!(
        progress.state,
        HandleState::Disposed,
        "#1010 poisoned fallback publishes Disposed"
    );
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        1,
        "#1010 poisoned fallback takes writer guard"
    );
}

/// H1: shutdown while a real HandleActivity keeps the drain parked at changed.
#[tokio::test]
async fn activity_wait_shutdown_is_cancelled_once() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#1010 activity wait executor");
    let session = DropProbe::new();
    let handle = {
        let _entered = executor.enter();
        LiveSessionHandle::new(session.clone(), None)
    };
    let activity = handle
        .begin_activity()
        .expect("#1010 activity wait admitted");
    let (entered, mut observed) = watch::channel(false);
    handle
        .0
        .progress
        .lock()
        .expect("#1010 activity wait progress")
        .activity_waiting = Some(entered);
    let waiter = handle.dispose("activity-wait".into());
    let executor = std::thread::spawn(move || {
        executor.block_on(async {
            tokio::time::timeout(Duration::from_secs(3), observed.wait_for(|v| *v))
                .await
                .expect("#1010 drain polled activity wait boundedly")
                .expect("#1010 activity wait observation alive");
        });
        executor
    })
    .join()
    .expect("#1010 activity wait executor driven");
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        0,
        "#1010 activity blocks session disposer"
    );
    std::thread::spawn(move || drop(executor))
        .join()
        .expect("#1010 activity wait executor shutdown");
    let error = tokio::time::timeout(Duration::from_secs(3), waiter)
        .await
        .expect("#1010 activity cancellation returns boundedly")
        .expect_err("#1010 activity cancellation cannot succeed");
    assert_eq!(
        error.message, "Agent session dispose task cancelled.",
        "#1010 activity cancellation is not fallback or panic"
    );
    let mut completion = {
        let progress = handle
            .0
            .progress
            .lock()
            .expect("#1010 activity terminal progress");
        assert_eq!(
            progress.state,
            HandleState::Disposed,
            "#1010 activity cancellation terminal state"
        );
        assert!(
            !progress.disposal_completed,
            "#1010 activity cancellation remains incomplete"
        );
        progress
            .disposal
            .as_ref()
            .expect("#1010 activity completion receiver")
            .clone()
    };
    completion.borrow_and_update();
    drop(activity);
    drop(handle);
    // A closed watch reports Err even if an unseen value remains: use its
    // stored value, not has_changed alone, to catch a late duplicate result.
    assert_eq!(
        completion
            .borrow()
            .as_ref()
            .expect("#1010 activity terminal result")
            .as_ref()
            .expect_err("#1010 activity terminal failure")
            .message,
        error.message,
        "#1010 activity drop preserves cancellation result"
    );
    assert_eq!(
        session.guards.load(Ordering::SeqCst),
        1,
        "#1010 activity shutdown never rearms fallback"
    );
    assert_eq!(
        session.dispose_count.load(Ordering::SeqCst),
        0,
        "#1010 activity shutdown never restarts disposer"
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(3), completion.changed())
            .await
            .expect("#1010 activity result channel closes boundedly")
            .is_err(),
        "#1010 activity drop publishes no second result"
    );
    let oracle: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/session-drop-release/oracle.json"
    ))
    .expect("#1010 activity TS oracle parses");
    let case = oracle["cases"]
        .as_array()
        .expect("#1010 activity TS cases")
        .iter()
        .find(|c| c["name"] == "handle-activity-wait")
        .expect("#1010 activity TS case");
    assert_eq!(
        case["observations"]["pending"]["onDisposeCalls"], 0,
        "#1010 real TS activity waits before disposal"
    );
}
