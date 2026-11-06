use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const BASE: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n        other: {}\n";

fn other_model() -> String {
    BASE.replace(
        "agent: {model: parity/deterministic}",
        "agent: {model: parity/other}",
    )
}

/// A temporary directory this test owns; removed on drop.
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("pdrs-config-store-unit-{}-{n}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("temp root: {e}"),
            }
        }
    }
    fn write(&self, yaml: &str) {
        std::fs::write(self.0.join("pilotdeck.yaml"), yaml).expect("write yaml");
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Store over `root` whose clock counts loads.
fn store(root: &Root) -> (ConfigStore, Arc<AtomicUsize>) {
    let loads = Arc::new(AtomicUsize::new(0));
    let counter = loads.clone();
    let mut options = StoreOptions::new(
        &root.0,
        &root.0,
        Env::from([(
            "PILOT_HOME".to_owned(),
            root.0.to_string_lossy().into_owned(),
        )]),
    );
    options.loaded_at = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        "1970-01-01T00:00:00.000Z".to_owned()
    });
    (ConfigStore::new(options).expect("initial load"), loads)
}

fn version(snapshot: &JsValue) -> f64 {
    snapshot_version(snapshot)
}

fn recorder(store: &ConfigStore) -> Arc<Mutex<Vec<(f64, f64)>>> {
    let events = Arc::new(Mutex::new(vec![]));
    let sink = events.clone();
    store.subscribe(Arc::new(move |event: &ReloadEvent| {
        lock(&sink).push((
            version(&event.previous_snapshot),
            version(&event.next_snapshot),
        ));
    }));
    events
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reloads_share_one_load_and_one_event() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let events = recorder(&store);
    root.write(&other_model());
    // All calls are issued before any is polled; they share one flight.
    let calls: Vec<_> = (0..16).map(|_| store.reload("manual")).collect();
    let handles: Vec<_> = calls.into_iter().map(tokio::spawn).collect();
    let mut results = vec![];
    for handle in handles {
        results.push(handle.await.expect("join").expect("reload"));
    }
    assert!(results.iter().all(|r| Arc::ptr_eq(r, &results[0])));
    assert_eq!(version(&results[0]), 2.0);
    assert_eq!(loads.load(Ordering::SeqCst), 2, "initial load + one reload");
    assert_eq!(*lock(&events), vec![(1.0, 2.0)]);
    // A reload after the flight settled is a new one.
    let next = store.reload("manual").await.expect("reload");
    assert_eq!(version(&next), 3.0);
    assert_eq!(loads.load(Ordering::SeqCst), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_from_another_task_while_running_reloads_after_it() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    // The first listener call blocks publication (the flight is Running)
    // until released.
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Mutex::new(Some(release_rx));
    store.subscribe(Arc::new(move |_: &ReloadEvent| {
        if let Some(rx) = lock(&release_rx).take() {
            let _ = entered_tx.send(());
            let _ = rx.recv_timeout(Duration::from_secs(30));
        }
    }));
    let first = tokio::spawn(store.reload("manual"));
    tokio::task::spawn_blocking(move || entered_rx.recv().expect("listener entered"))
        .await
        .unwrap();
    // The file changes after the running reload read it: TS can only see
    // this call after `reloading` was cleared, so it re-reads.
    root.write(&other_model());
    let later = tokio::spawn(store.reload("manual"));
    settle().await;
    assert!(
        !later.is_finished(),
        "the later call waits for the running flight"
    );
    release_tx.send(()).unwrap();
    let first = first.await.unwrap().unwrap();
    let later = later.await.unwrap().unwrap();
    assert!(!Arc::ptr_eq(&first, &later));
    assert_eq!((version(&first), version(&later)), (2.0, 3.0));
    assert_eq!(loads.load(Ordering::SeqCst), 3);
}

/// DeepSeek P2-1: a reload claimed after publication but before the flight
/// is cleared must not reuse the published result. The settle gate holds the
/// first flight exactly in that window while another thread reloads.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reload_in_the_done_window_reads_the_file_again() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(Some((entered_tx, release_rx)));
    *lock(&store.inner.settle_gate) = Some(Arc::new(move || {
        if let Some((entered, release)) = lock(&gate).take() {
            let _ = entered.send(());
            let _ = release.recv_timeout(Duration::from_secs(30));
        }
    }));
    let first = tokio::spawn(store.reload("manual"));
    tokio::task::spawn_blocking(move || entered_rx.recv().expect("flight is Done"))
        .await
        .unwrap();
    root.write(&other_model());
    let later = store.reload("rpc");
    release_tx.send(()).unwrap();
    let later = later.await.unwrap();
    let first = first.await.unwrap().unwrap();
    assert_eq!((version(&first), version(&later)), (2.0, 3.0));
    assert_eq!(loads.load(Ordering::SeqCst), 3);
    let model = later
        .get("config")
        .and_then(|c| c.get("agent"))
        .and_then(|a| a.get("model"));
    let id = model
        .and_then(|m| m.get("id"))
        .and_then(JsValue::as_string)
        .unwrap();
    assert!(id.equals_str("parity/other"), "fresh content: {id:?}");
}

/// #1745: a flight that settled in an earlier turn must not close the
/// same-tick join window for the next claim. Two reloads claimed in one tick
/// after a settle share one flight and bump the generation once, exactly like
/// the baseline's `Promise.resolve().then` deferral.
#[tokio::test(flavor = "current_thread")]
async fn reloads_claimed_after_a_settle_share_one_flight() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let events = recorder(&store);
    root.write(&other_model());
    let first = store.reload("manual").await.expect("the first reload");
    assert_eq!(version(&first), 2.0);
    let before = loads.load(Ordering::SeqCst);
    root.write(BASE);
    // Claimed together, polled in one tick: the second claim must join the
    // first flight instead of starting a load of its own.
    let (a, b) = tokio::join!(async { store.reload("manual").await }, async {
        store.reload("manual").await
    },);
    let (a, b) = (a.expect("the first claim"), b.expect("the second claim"));
    assert!(
        Arc::ptr_eq(&a, &b),
        "#1745 both claims must share one flight"
    );
    assert_eq!(
        loads.load(Ordering::SeqCst) - before,
        1,
        "#1745 the shared claim must load once"
    );
    assert_eq!(version(&a), 3.0);
    assert_eq!(*lock(&events), vec![(1.0, 2.0), (2.0, 3.0)]);
}

/// #1745 on worker threads: the settle happens in another task, so its
/// scheduler turn says nothing about the claiming task's turn. Repeated
/// because the settle and the claims genuinely race across workers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settle_in_another_task_does_not_close_the_join_window() {
    for round in 0..20 {
        let root = Root::new();
        root.write(BASE);
        let (store, loads) = store(&root);
        root.write(&other_model());
        let settling = store.clone();
        tokio::spawn(async move { settling.reload("manual").await })
            .await
            .expect("the settling task")
            .expect("the first reload");
        let before = loads.load(Ordering::SeqCst);
        root.write(BASE);
        let (a, b) = tokio::join!(async { store.reload("manual").await }, async {
            store.reload("manual").await
        },);
        let (a, b) = (a.expect("the first claim"), b.expect("the second claim"));
        assert!(
            Arc::ptr_eq(&a, &b),
            "round {round}: #1745 the claims must share one flight"
        );
        assert_eq!(
            loads.load(Ordering::SeqCst) - before,
            1,
            "round {round}: #1745 one shared load"
        );
    }
}

#[tokio::test]
async fn failures_keep_snapshot_version_and_only_config_error_diagnostics() {
    let root = Root::new();
    root.write(BASE);
    let (store, _) = store(&root);
    let events = recorder(&store);
    root.write(&BASE.replace("schemaVersion: 1", "schemaVersion: 2"));
    let error = store.reload("manual").await.unwrap_err();
    assert_eq!(error.name, "PilotConfigError");
    assert_eq!(version(&store.snapshot()), 1.0);
    let codes = |store: &ConfigStore| {
        store
            .diagnostics()
            .iter()
            .map(|d| {
                d.get("code")
                    .and_then(JsValue::as_string)
                    .unwrap()
                    .to_utf8()
                    .unwrap()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(codes(&store), ["CONFIG_SCHEMA_VERSION_UNSUPPORTED"]);
    // A RangeError (alias cycle) is not a PilotConfigError: keep diagnostics.
    root.write(&BASE.replace(
        "      apiKey: mock-only",
        "      apiKey: mock-only\n      extraBody: &a {self: *a}",
    ));
    let error = store.reload("manual").await.unwrap_err();
    assert_eq!(error.name, "RangeError");
    assert_eq!(codes(&store), ["CONFIG_SCHEMA_VERSION_UNSUPPORTED"]);
    // Failures consumed no version; success clears the retained diagnostics.
    root.write(&other_model());
    let next = store.reload("manual").await.unwrap();
    assert_eq!(version(&next), 2.0);
    assert!(codes(&store).is_empty());
    assert_eq!(*lock(&events), vec![(1.0, 2.0)]);
}

#[tokio::test]
async fn panicking_listener_is_isolated() {
    let root = Root::new();
    root.write(BASE);
    let (store, _) = store(&root);
    let first = recorder(&store);
    store.subscribe(Arc::new(|_: &ReloadEvent| panic!("listener failure")));
    let last = recorder(&store);
    let next = store.reload("manual").await.expect("reload succeeds");
    assert_eq!(version(&next), 2.0);
    assert!(Arc::ptr_eq(&store.snapshot(), &next));
    assert_eq!(*lock(&first), vec![(1.0, 2.0)]);
    assert_eq!(*lock(&last), vec![(1.0, 2.0)]);
}

#[tokio::test]
async fn same_listener_once_and_unsubscribe_is_idempotent() {
    let root = Root::new();
    root.write(BASE);
    let (store, _) = store(&root);
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let listener: Listener = Arc::new(move |_: &ReloadEvent| {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let a = store.subscribe(listener.clone());
    let b = store.subscribe(listener);
    store.reload("manual").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    a.unsubscribe();
    b.unsubscribe();
    a.unsubscribe();
    store.reload("manual").await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn debounce_fires_once_at_the_boundary() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let events = recorder(&store);
    let watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    watch.notify_change();
    settle().await;
    tokio::time::advance(Duration::from_millis(249)).await;
    settle().await;
    assert_eq!(loads.load(Ordering::SeqCst), 1, "no reload before 250 ms");
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(loads.load(Ordering::SeqCst), 2, "reload at 250 ms");
    assert_eq!(*lock(&events), vec![(1.0, 2.0)]);
    watch.stop().await;
}

#[tokio::test(start_paused = true)]
async fn each_change_rearms_the_debounce() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    watch.notify_change();
    settle().await;
    tokio::time::advance(Duration::from_millis(200)).await;
    watch.notify_change();
    settle().await;
    tokio::time::advance(Duration::from_millis(249)).await;
    settle().await;
    assert_eq!(loads.load(Ordering::SeqCst), 1, "re-armed at 200 ms");
    tokio::time::advance(Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(loads.load(Ordering::SeqCst), 2, "one reload for the burst");
    // A second burst after the quiet period is a second reload.
    watch.notify_change();
    settle().await;
    tokio::time::advance(Duration::from_millis(250)).await;
    settle().await;
    assert_eq!(loads.load(Ordering::SeqCst), 3);
    watch.stop().await;
}

#[tokio::test(start_paused = true)]
async fn polling_detects_file_changes_and_swallows_failures() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let events = recorder(&store);
    let watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    root.write(&other_model());
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(*lock(&events), vec![(1.0, 2.0)]);
    root.write(&BASE.replace("schemaVersion: 1", "schemaVersion: 2"));
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(loads.load(Ordering::SeqCst), 3, "failed reload ran");
    assert_eq!(version(&store.snapshot()), 2.0);
    assert_eq!(store.diagnostics().len(), 1);
    // No further change: no further reload.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(loads.load(Ordering::SeqCst), 3);
    watch.stop().await;
}

#[tokio::test(start_paused = true)]
async fn zero_poll_interval_is_clamped_instead_of_panicking() {
    // #1650: `tokio::time::interval(Duration::ZERO)` panics, and the panic
    // inside the detached watcher task is invisible: the handle looks alive
    // while hot reload is already dead. The floor must keep the loop sampling.
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let watch = store
        .start_watching(WatchOptions {
            poll_interval: Duration::ZERO,
            ..WatchOptions::default()
        })
        .expect("#879 healthy watcher paths");
    settle().await;
    root.write(&other_model());
    tokio::time::sleep(Duration::from_millis(1000)).await;
    // Liveness first: a watcher that panicked on construction is the mechanism
    // under test, and the load count below would report it as a mere "no
    // reload" instead.
    assert!(
        !watch
            .task
            .as_ref()
            .expect("#1650 watcher task")
            .is_finished(),
        "#1650 the clamped watcher keeps running (a zero interval would panic the task)"
    );
    assert_eq!(
        loads.load(Ordering::SeqCst),
        2,
        "#1650 a zero poll interval is clamped and still samples the change"
    );
    watch.stop().await;
    // The behaviour above is what fails in production, so it runs first; these
    // pin the floor itself, after it. Kept last on purpose: a pure
    // `effective_poll_interval` assertion ahead of the watcher would turn the
    // negative control into "the clamp is gone" instead of "hot reload dies".
    assert_eq!(MIN_POLL_INTERVAL, Duration::from_millis(10));
    // The floor is a clamp, not a zero special case: every sub-floor value
    // lands on it, and the default is untouched.
    for sub_floor in [
        Duration::ZERO,
        Duration::from_micros(500),
        Duration::from_millis(9),
    ] {
        assert_eq!(
            effective_poll_interval(&WatchOptions {
                poll_interval: sub_floor,
                ..WatchOptions::default()
            }),
            MIN_POLL_INTERVAL,
            "#1650 {sub_floor:?} must land on the floor, not pass through"
        );
    }
    assert_eq!(
        effective_poll_interval(&WatchOptions::default()),
        Duration::from_millis(20),
        "#1650 the default cadence is not clamped"
    );
}

/// #1736: an over-ceiling but representable `debounce` is clamped to
/// [`MAX_DEBOUNCE`] — the ceiling twin of #1650's floor — so the reload
/// still lands inside the bound instead of at the multi-hour value the
/// caller asked for. Pre-fix this also panics the detached watcher task
/// once the value stops being representable (see
/// `overflowing_debounce_does_not_kill_the_watcher`).
#[tokio::test(start_paused = true)]
async fn overlong_debounce_is_clamped_and_reloads_within_the_bound() {
    // A 60 s poll keeps the virtual tick count of the one-hour walk at
    // sixty; the debounce arm under test does not depend on the cadence.
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let watch = store
        .start_watching(WatchOptions {
            debounce: 2 * MAX_DEBOUNCE,
            poll_interval: MAX_POLL_INTERVAL,
        })
        .expect("#879 healthy watcher paths");
    settle().await;
    watch.notify_change();
    settle().await;
    tokio::time::advance(MAX_DEBOUNCE - Duration::from_millis(1)).await;
    settle().await;
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "#1736 no reload before the clamped bound"
    );
    tokio::time::advance(Duration::from_millis(2)).await;
    settle().await;
    assert_eq!(
        loads.load(Ordering::SeqCst),
        2,
        "#1736 an overlong debounce lands on MAX_DEBOUNCE, not on the requested value"
    );
    watch.stop().await;
    // The ceiling is a clamp, not a special case for the unrepresentable
    // extreme: every over-ceiling value lands on it, and the default is
    // untouched. Kept after the behavioural half, like #1650's pins.
    assert_eq!(
        effective_debounce(&WatchOptions {
            debounce: 2 * MAX_DEBOUNCE,
            ..WatchOptions::default()
        }),
        MAX_DEBOUNCE,
        "#1736 a representable over-ceiling debounce must land on the ceiling"
    );
    assert_eq!(
        effective_debounce(&WatchOptions {
            debounce: Duration::MAX,
            ..WatchOptions::default()
        }),
        MAX_DEBOUNCE,
        "#1736 an unrepresentable debounce must land on the ceiling"
    );
    assert_eq!(
        effective_debounce(&WatchOptions::default()),
        Duration::from_millis(250),
        "#1736 the default debounce is not clamped"
    );
}

/// #1794 P1-2: the clamp is observable, not an arithmetic rescue. The
/// baseline honours a two-hour `debounceMs` verbatim — `setTimeout` accepts
/// anything below `2^31 - 1` ms and runs it as a real timer (measured on
/// Node v22.22.0: `_tmp/20261106-issue-1736/r2/evidence/settimeout-2h-probe.log`
/// for the timer itself and `ts-baseline-debounce-2h-probe.log` for the real
/// `startWatching` handing 7200000 to `setTimeout`) — so Rust returning one
/// hour where Node waits two is a registered deviation from the baseline,
/// not a fix for an unrepresentable value:
/// `specs/gateway-parity/diffs/A6.3-CONFIG-DEBOUNCE-CEILING-001.md`.
///
/// Pinned on the literal value the ledger negotiates rather than on
/// `2 * MAX_DEBOUNCE`, so raising the ceiling to Node's own limit (the
/// convergence path the entry lists) is caught as the semantic change it
/// is instead of silently passing.
#[test]
fn two_hour_debounce_is_clamped_to_the_one_hour_ceiling() {
    let requested = Duration::from_millis(7_200_000);
    assert_eq!(requested, Duration::from_secs(2 * 60 * 60));
    assert_eq!(MAX_DEBOUNCE, Duration::from_secs(60 * 60));
    assert_eq!(
        effective_debounce(&WatchOptions {
            debounce: requested,
            ..WatchOptions::default()
        }),
        MAX_DEBOUNCE,
        "#1794 a two-hour debounce the baseline honours verbatim lands on the one-hour ceiling"
    );
    // Negative control: deleting the `.min(MAX_DEBOUNCE)` lets 7200 s
    // through unclamped and this assertion fails with left: 7200s,
    // right: 3600s (recorded in the ledger entry's 机器判据).
}

/// #1736: `Instant + Duration` panics once `debounce` stops being
/// representable, and the panic happens inside the detached watcher task —
/// `WatchHandle::stop` swallows the `JoinError` and `Drop` only aborts, so
/// hot reload dies with the handle still "alive" (the same silent death as
/// #1650's zero period). The clamp keeps the deadline representable, and
/// `deadline_after` is the second line of defence.
#[tokio::test(start_paused = true)]
async fn overflowing_debounce_does_not_kill_the_watcher() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let watch = store
        .start_watching(WatchOptions {
            debounce: Duration::MAX,
            poll_interval: MAX_POLL_INTERVAL,
        })
        .expect("#879 healthy watcher paths");
    settle().await;
    watch.notify_change();
    settle().await;
    tokio::time::advance(MAX_DEBOUNCE + Duration::from_millis(1)).await;
    settle().await;
    assert!(
        !watch
            .task
            .as_ref()
            .expect("#1736 watcher task")
            .is_finished(),
        "#1736 the watcher task must survive an unrepresentable debounce"
    );
    assert_eq!(
        loads.load(Ordering::SeqCst),
        2,
        "#1736 an unrepresentable debounce is clamped and still reloads"
    );
    watch.stop().await;
}

/// #1736: `tokio::time::interval` maps an overflowing period to
/// `Instant::far_future` (tokio 1.48 `Interval::poll_tick`), so an
/// over-long `poll_interval` buys one sample and then a ticker that never
/// fires again — no panic, no error, the handle still "alive". The ceiling
/// keeps the watcher sampling.
#[tokio::test(start_paused = true)]
async fn huge_poll_interval_is_clamped_and_keeps_sampling() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let events = recorder(&store);
    let watch = store
        .start_watching(WatchOptions {
            poll_interval: Duration::MAX,
            ..WatchOptions::default()
        })
        .expect("#879 healthy watcher paths");
    settle().await;
    root.write(&other_model());
    // Well past the clamped ceiling: the change must have been sampled and
    // reloaded (the first tick alone — the far_future trap — cannot see a
    // change written after the watcher started).
    tokio::time::sleep(2 * MAX_POLL_INTERVAL).await;
    assert_eq!(
        loads.load(Ordering::SeqCst),
        2,
        "#1736 a huge poll interval is clamped and the change is still sampled"
    );
    assert_eq!(*lock(&events), vec![(1.0, 2.0)]);
    assert!(
        !watch
            .task
            .as_ref()
            .expect("#1736 watcher task")
            .is_finished(),
        "#1736 the watcher task keeps sampling past the ceiling"
    );
    watch.stop().await;
    // The ceiling is a clamp, not a zero-style special case; #1650's pins
    // below the floor stay in its own test.
    assert_eq!(
        effective_poll_interval(&WatchOptions {
            poll_interval: Duration::MAX,
            ..WatchOptions::default()
        }),
        MAX_POLL_INTERVAL,
        "#1736 an unrepresentable poll interval must land on the ceiling"
    );
    assert_eq!(
        effective_poll_interval(&WatchOptions {
            poll_interval: 2 * MAX_POLL_INTERVAL,
            ..WatchOptions::default()
        }),
        MAX_POLL_INTERVAL,
        "#1736 a representable over-ceiling poll interval must land on the ceiling"
    );
}

/// #1736: the overflow fallback itself must be panic-free and deterministic.
#[test]
fn deadline_after_handles_unrepresentable_durations() {
    let now = Instant::now();
    let fallback = now
        .checked_add(MAX_DEBOUNCE)
        .expect("#1736 now + MAX_DEBOUNCE must be representable");
    let deadline = std::panic::catch_unwind(|| deadline_after(now, Duration::MAX))
        .expect("#1736 deadline_after must not panic on an unrepresentable duration");
    assert_eq!(
        deadline, fallback,
        "#1736 an unrepresentable duration falls back to the ceiling"
    );
    assert_eq!(
        deadline_after(now, Duration::ZERO),
        now,
        "#1736 a representable duration is used as-is (zero)"
    );
    assert_eq!(
        deadline_after(now, Duration::from_secs(1)),
        now + Duration::from_secs(1),
        "#1736 a representable duration is used as-is"
    );
}

#[tokio::test(start_paused = true)]
async fn stop_cancels_pending_reload_and_leaves_no_task() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    watch.notify_change();
    settle().await;
    tokio::time::advance(Duration::from_millis(100)).await;
    watch.stop().await;
    assert_eq!(
        Arc::weak_count(&store.inner),
        0,
        "watcher task released the store"
    );
    root.write(&other_model());
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(loads.load(Ordering::SeqCst), 1, "no reload after stop");
}

#[tokio::test(start_paused = true)]
async fn dropping_the_handle_or_the_store_ends_the_task() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    watch.notify_change();
    drop(watch);
    settle().await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert_eq!(Arc::weak_count(&store.inner), 0);

    let mut watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    drop(store);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(watch.task.as_ref().unwrap().is_finished());
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    watch.task.take();
}

#[test]
fn watched_paths_follow_exists_then_dirname() {
    let root = Root::new();
    root.write(BASE);
    let project = root.0.join("project");
    std::fs::create_dir(&project).unwrap();
    let mut options = StoreOptions::new(
        &root.0,
        &root.0,
        Env::from([(
            "PILOT_HOME".to_owned(),
            root.0.to_string_lossy().into_owned(),
        )]),
    );
    options.project = Some(project.to_string_lossy().into_owned());
    let store = ConfigStore::new(options).unwrap();
    let watched = |store: &ConfigStore| {
        store
            .watched_paths()
            .expect("#879 healthy watched paths")
            .into_iter()
            .filter_map(WatchedPath::new)
            .map(|w| (w.path, w.directory))
            .collect::<Vec<_>>()
    };
    // Missing `<project>/.pilotdeck`: fs.watch would throw; not watched.
    assert_eq!(watched(&store), [(root.0.join("pilotdeck.yaml"), false)]);
    std::fs::create_dir(project.join(".pilotdeck")).unwrap();
    assert_eq!(
        watched(&store),
        [
            (root.0.join("pilotdeck.yaml"), false),
            (project.join(".pilotdeck"), true)
        ]
    );
}

/// Records `(previous, next, changed path count)` per event.
fn path_recorder(store: &ConfigStore) -> Arc<Mutex<Vec<(f64, f64, usize)>>> {
    let events = Arc::new(Mutex::new(vec![]));
    let sink = events.clone();
    store.subscribe(Arc::new(move |event: &ReloadEvent| {
        lock(&sink).push((
            version(&event.previous_snapshot),
            version(&event.next_snapshot),
            event.changed_paths.len(),
        ));
    }));
    events
}

async fn advance(ms: u64) {
    for _ in 0..ms {
        tokio::time::advance(Duration::from_millis(1)).await;
        settle().await;
    }
}

/// Pins the sampling behaviour with the default 20 ms poll. Changes land
/// 5 ms after a tick, so each is sampled 15 ms late.
#[tokio::test(start_paused = true)]
async fn change_spacing_around_the_debounce_window() {
    let run_policy = format!("{BASE}runPolicy: {{failureGuard: {{enabled: true}}}}\n");
    // 300 ms apart: two reloads with each change's own content, as in TS
    // (BASE -> other: 2 agent.model paths; other -> runPolicy: 3 paths).
    {
        let root = Root::new();
        root.write(BASE);
        let (store, loads) = store(&root);
        let events = path_recorder(&store);
        let watch = store
            .start_watching(WatchOptions::default())
            .expect("#879 healthy watcher paths");
        settle().await;
        advance(5).await;
        root.write(&other_model());
        advance(300).await;
        root.write(&run_policy);
        advance(600).await;
        assert_eq!(loads.load(Ordering::SeqCst), 3);
        assert_eq!(*lock(&events), vec![(1.0, 2.0, 2), (2.0, 3.0, 3)]);
        watch.stop().await;
    }
    // 260 ms apart: TS reloads at 255 ms (first content) and 515 ms (second).
    // Here the first change is sampled at 20 ms, so the first reload runs at
    // 270 ms and already reads the second change (written at 265 ms); the
    // second change is sampled at 280 ms and yields an identity reload.
    {
        let root = Root::new();
        root.write(BASE);
        let (store, loads) = store(&root);
        let events = path_recorder(&store);
        let watch = store
            .start_watching(WatchOptions::default())
            .expect("#879 healthy watcher paths");
        settle().await;
        advance(5).await;
        root.write(&other_model());
        advance(260).await;
        root.write(&run_policy);
        advance(600).await;
        assert_eq!(loads.load(Ordering::SeqCst), 3);
        // BASE -> runPolicy is one path (`runPolicy`): the first change's
        // `agent.model` edit was never published as its own snapshot.
        assert_eq!(*lock(&events), vec![(1.0, 2.0, 1), (2.0, 3.0, 0)]);
        watch.stop().await;
    }
}

/// #327: content and mtime restored between two samples still bump the
/// inode change time, so the burst yields one identity reload like
/// `fs.watch` (it used to be invisible to the length/mtime/content sample).
#[tokio::test(start_paused = true)]
async fn restored_mtime_rewrite_between_samples_is_seen() {
    let root = Root::new();
    root.write(BASE);
    let path = root.0.join("pilotdeck.yaml");
    let (store, loads) = store(&root);
    let watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    advance(5).await;
    let modified = std::fs::metadata(&path)
        .expect("#1739 initial metadata")
        .modified()
        .expect("#1739 initial mtime");
    root.write(&other_model());
    root.write(BASE);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("#1739 reopen for mtime restore")
        .set_modified(modified)
        .expect("#1739 restore mtime");
    advance(600).await;
    assert_eq!(
        loads.load(Ordering::SeqCst),
        2,
        "#1739 a same-length rewrite with mtime restored is still seen (ctime)"
    );
    assert_eq!(
        store.snapshot().get("version").and_then(JsValue::as_number),
        Some(2.0)
    );
    watch.stop().await;
}

/// #327: a config file that appears inside a parent-watched directory has
/// its content sampled, so a same-length rewrite is seen even when the
/// entry's length and mtime are unchanged (a fingerprint of the entry list
/// and metadata alone could not tell `deterministic` from `other`).
#[test]
fn parent_watched_config_file_content_is_sampled() {
    let root = Root::new();
    let path = root.0.join("pilotdeck.yaml");
    let mut watched = WatchedPath::new(path.clone()).expect("parent watched");
    assert!(watched.directory);
    assert_eq!(
        watched.target.as_deref(),
        Some(std::ffi::OsStr::new("pilotdeck.yaml"))
    );
    std::fs::write(&path, "a: 111\n").unwrap();
    assert!(watched.changed(), "creation is seen");
    assert!(!watched.changed(), "an unchanged sample is quiet");
    // The target's content is part of the directory sample; the negative
    // control (no target) is the old entry-metadata-only fingerprint.
    let target = Some(std::ffi::OsStr::new("pilotdeck.yaml"));
    assert_ne!(
        fingerprint(&root.0, true, target, true),
        fingerprint(&root.0, true, None, true)
    );
    let other = Some(std::ffi::OsStr::new("other.yaml"));
    assert_eq!(
        fingerprint(&root.0, true, other, true),
        fingerprint(&root.0, true, None, true)
    );
}

/// #327: files over the sample limit are not read on every poll; they are
/// still watched through their metadata.
#[test]
fn oversized_config_file_is_sampled_by_metadata_only() {
    let root = Root::new();
    let path = root.0.join("pilotdeck.yaml");
    let big = vec![b'#'; CONTENT_SAMPLE_LIMIT as usize + 1];
    std::fs::write(&path, &big).unwrap();
    let mut with_content = DefaultHasher::new();
    hash_entry(&path, &mut with_content, true);
    let mut metadata_only = DefaultHasher::new();
    hash_entry(&path, &mut metadata_only, false);
    assert_eq!(with_content.finish(), metadata_only.finish());
    let mut watched = WatchedPath::new(path.clone()).expect("file watched");
    std::fs::write(&path, &big).unwrap();
    assert!(watched.changed(), "a rewrite still bumps ctime");
    // Negative control: at the limit the content is part of the sample.
    std::fs::write(&path, &big[..CONTENT_SAMPLE_LIMIT as usize]).unwrap();
    let mut with_content = DefaultHasher::new();
    hash_entry(&path, &mut with_content, true);
    let mut metadata_only = DefaultHasher::new();
    hash_entry(&path, &mut metadata_only, false);
    assert_ne!(with_content.finish(), metadata_only.finish());
}

/// #1739: a sample whose metadata is unchanged must not read the config
/// file's content (the per-tick cost of the poll loop is the point of the
/// fix); a changed sample must read it and be seen. The count is observed
/// directly via the [`hash_entry`] test hook, not inferred from timing.
#[test]
fn unchanged_metadata_skips_the_content_read() {
    let root = Root::new();
    root.write(BASE);
    let path = root.0.join("pilotdeck.yaml");
    let mut watched = WatchedPath::new(path.clone()).expect("#1739 file watched");
    let before = content_reads(&path);
    for tick in 0..50 {
        assert!(
            !watched.changed(),
            "#1739 unchanged sample {tick} must be quiet"
        );
    }
    assert_eq!(
        content_reads(&path),
        before,
        "#1739 50 metadata-identical samples must not read the config file content"
    );
    root.write(&other_model());
    assert!(watched.changed(), "#1739 a changed sample is seen");
    assert_eq!(
        content_reads(&path),
        before + 1,
        "#1739 a changed sample reads the content exactly once"
    );
}

/// #1739: the metadata short-circuit leans on ctime — the same-length,
/// mtime-restored rewrite of the watcher-level
/// `restored_mtime_rewrite_between_samples_is_seen` at unit level: the
/// metadata sample still differs (ctime), so the content is still read and
/// the change is still seen.
#[test]
fn restored_mtime_rewrite_still_reads_the_content() {
    let root = Root::new();
    root.write(BASE);
    let path = root.0.join("pilotdeck.yaml");
    let modified = std::fs::metadata(&path)
        .expect("#1739 initial metadata")
        .modified()
        .expect("#1739 initial mtime");
    let mut watched = WatchedPath::new(path.clone()).expect("#1739 file watched");
    let before = content_reads(&path);
    // Same length as BASE ("deterministic" -> "deterministix"), mtime restored.
    root.write(&BASE.replace("deterministic", "deterministix"));
    std::fs::File::options()
        .write(true)
        .open(&path)
        .expect("#1739 reopen for mtime restore")
        .set_modified(modified)
        .expect("#1739 restore mtime");
    assert!(
        watched.changed(),
        "#1739 the ctime bump is seen through the metadata short-circuit"
    );
    assert_eq!(
        content_reads(&path),
        before + 1,
        "#1739 the same-length mtime-restored rewrite is content-sampled once"
    );
}

#[test]
fn relative_global_config_path_is_anchored_at_cwd() {
    let root = Root::new();
    let cwd = root.0.join("cwd");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("cfg.yaml"), BASE).unwrap();
    let options = StoreOptions::new(
        &root.0,
        &cwd,
        Env::from([
            (
                "PILOT_HOME".to_owned(),
                root.0.to_string_lossy().into_owned(),
            ),
            ("PILOTDECK_CONFIG_PATH".to_owned(), "cfg.yaml".to_owned()),
        ]),
    );
    let store = ConfigStore::new(options).expect("loads cwd/cfg.yaml");
    let watched = |store: &ConfigStore| {
        store
            .watched_paths()
            .expect("#879 healthy watched paths")
            .into_iter()
            .filter_map(WatchedPath::new)
            .map(|w| (w.path, w.directory))
            .collect::<Vec<_>>()
    };
    assert_eq!(watched(&store), [(cwd.join("cfg.yaml"), false)]);
    // Missing: watched through `dirname`, i.e. cwd (TS: `dirname("cfg.yaml")`
    // is `.`), not skipped because the bare name has an empty parent.
    std::fs::remove_file(cwd.join("cfg.yaml")).unwrap();
    assert_eq!(watched(&store), [(cwd.clone(), true)]);
}
