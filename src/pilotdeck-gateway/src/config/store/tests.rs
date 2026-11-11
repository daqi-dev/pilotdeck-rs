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

/// How long a detached watcher task gets to stop once its last owner is gone.
/// It notices the dropped owner on its next poll, so the wait must clear the
/// slowest poll interval the store accepts ([`MAX_POLL_INTERVAL`], 60 s) or a
/// cleanly stopped watcher would be reported as a timeout on that path.
const STOP_TIMEOUT: Duration = Duration::from_secs(90);

/// #1920: pin that relation at compile time, so a future ceiling change
/// cannot silently invert it and turn every clean stop into a timeout
/// report on the slowest-poll path. `Duration`'s `PartialOrd` is not a
/// `const fn`, so the pin compares whole seconds — both constants are whole
/// seconds — and can only fire conservatively (a sub-second relation it
/// cannot resolve fails loudly rather than passing silently).
const _: () = assert!(STOP_TIMEOUT.as_secs() > MAX_POLL_INTERVAL.as_secs());

/// #1899: wait for the detached watcher task and report how it ended.
async fn watcher_stop(task: Option<JoinHandle<()>>) -> Result<(), String> {
    let Some(task) = task else {
        return Err("the watcher task handle was already taken".to_owned());
    };
    match tokio::time::timeout(STOP_TIMEOUT, task).await {
        Err(_) => Err(format!(
            "the watcher task did not stop within {STOP_TIMEOUT:?}"
        )),
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) if err.is_panic() => Err(format!("the watcher task panicked: {err}")),
        Ok(Err(err)) => Err(format!("the watcher task was cancelled: {err}")),
    }
}

/// #1899: `is_finished()` is true for a task that died by panic (tokio 1.48
/// stores `JoinError::panic` as the output and marks the task complete), so it
/// is not evidence that the watcher stopped cleanly — watching the loop panic
/// would silently kill hot reload (#1650). This pins the runtime fact and is
/// the negative control for `watcher_stop`: with `is_finished()`-only evidence
/// it fails at its own marker.
#[tokio::test]
async fn panicked_watcher_task_is_not_a_clean_stop() {
    let task = tokio::spawn(async { panic!("simulated watch_loop failure") });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        task.is_finished(),
        "#1899 tokio marks a panicked task complete (the false evidence)"
    );
    let reason = watcher_stop(Some(task))
        .await
        .expect_err("#1899 a panicked watcher must not look like a clean stop");
    assert!(
        reason.contains("panicked"),
        "#1899 the stop reason must name the panic: {reason}"
    );
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

/// #1946: the baseline claims the in-flight slot synchronously and starts the
/// load on a microtask (`Promise.resolve().then`), so the claiming poll must
/// not run the load inline — a peer that lands between the claim and the load
/// joins this flight instead of starting a second load, and a same-tick
/// snapshot read still sees the previous generation. `Flight::run_or_wait`
/// opens exactly that window with one scheduler hop; this pins it without a
/// scheduler race: the future is polled once by hand, so "the claim happened,
/// the load did not".
#[tokio::test]
async fn the_join_window_defers_the_load_past_the_claiming_poll() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};

    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    root.write(&other_model());
    let reload = store.reload("manual");
    tokio::pin!(reload);
    let mut cx = Context::from_waker(Waker::noop());
    assert!(
        matches!(reload.as_mut().poll(&mut cx), Poll::Pending),
        "#1946 the claiming poll must hop instead of loading inline — the \
         baseline starts the load on a microtask"
    );
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "#1946 the claim alone must not load; only the initial load has run"
    );
    assert_eq!(
        version(&store.snapshot()),
        1.0,
        "#1946 a same-tick snapshot read still sees the previous generation"
    );
    let next = reload.await.expect("reload");
    assert_eq!(version(&next), 2.0);
    assert_eq!(loads.load(Ordering::SeqCst), 2);
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

/// #1833 A1/A2, the rationale end to end: a binding failure must not report
/// the loader's `PilotConfigError` class, because only that name replaces the
/// retained reload diagnostics. TS propagates the raw `YAMLParseError`
/// (yaml's `parse` throws `doc.errors[0]`), the `TypeError` from
/// `path.join(bindingId)` and the fs `Error` from the sops read race — the
/// baseline therefore keeps the previous reload's diagnostics; a Rust-side
/// default name would clear them.
#[tokio::test]
async fn binding_error_classes_keep_the_retained_reload_diagnostics() {
    fn codes(store: &ConfigStore) -> Vec<String> {
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
            .collect()
    }
    let root = Root::new();
    root.write(BASE);
    let mut options = StoreOptions::new(
        &root.0,
        &root.0,
        Env::from([
            (
                "PILOT_HOME".to_owned(),
                root.0.to_string_lossy().into_owned(),
            ),
            (
                "STAFFDECK_PROJECT_BINDING_ENABLED".to_owned(),
                "true".to_owned(),
            ),
            (
                "STAFFDECK_FORMAL_API_ORIGIN".to_owned(),
                "https://staffdeck.invalid".to_owned(),
            ),
            ("STAFFDECK_COPY_TENANT_ID".to_owned(), "tenant".to_owned()),
        ]),
    );
    options.project = Some(root.0.to_string_lossy().into_owned());
    options.loaded_at = Arc::new(|| "1970-01-01T00:00:00.000Z".to_owned());
    let store = ConfigStore::new(options).expect("#1833 initial load");
    // Establish the retained diagnostics with a plain config failure first.
    root.write(&BASE.replace("schemaVersion: 1", "schemaVersion: 2"));
    let error = store.reload("manual").await.unwrap_err();
    assert_eq!(
        error.name, "PilotConfigError",
        "#1833 the setup failure must be a config diagnostic failure"
    );
    assert_eq!(
        codes(&store),
        ["CONFIG_SCHEMA_VERSION_UNSUPPORTED"],
        "#1833 the retained diagnostics start non-empty"
    );
    // The binding store enables the bound project; its sops.json is invalid.
    let staffdeck = root.0.join("staffdeck");
    std::fs::create_dir_all(staffdeck.join("binding-1")).expect("#1833 binding dirs");
    let binding_store = format!(
        r#"{{"version":1,"installationId":"installation","projects":[{{"projectRoot":{},"bindingId":"binding-1","agentId":"agent","status":"bound","origin":"https://staffdeck.invalid","tenantId":"tenant"}}]}}"#,
        serde_json::to_string(&root.0.to_string_lossy().into_owned()).unwrap()
    );
    std::fs::write(staffdeck.join("project-bindings.json"), &binding_store)
        .expect("#1833 binding store");
    std::fs::write(staffdeck.join("binding-1/sops.json"), "sops: [unclosed\n")
        .expect("#1833 sops bundle");
    let error = store.reload("manual").await.unwrap_err();
    assert_eq!(
        error.name, "YAMLParseError",
        "#1833 the sops yaml failure must keep yaml's own class"
    );
    assert_eq!(
        codes(&store),
        ["CONFIG_SCHEMA_VERSION_UNSUPPORTED"],
        "#1833 a YAMLParseError reload keeps the retained diagnostics"
    );
    // A non-string bindingId: `path.join`'s TypeError, same rule.
    std::fs::write(
        staffdeck.join("project-bindings.json"),
        binding_store.replace("\"bindingId\":\"binding-1\"", "\"bindingId\":5"),
    )
    .expect("#1833 rewrite the binding store");
    let error = store.reload("manual").await.unwrap_err();
    assert_eq!(
        error.name, "TypeError",
        "#1833 the path.join failure must keep Node's class"
    );
    assert_eq!(
        codes(&store),
        ["CONFIG_SCHEMA_VERSION_UNSUPPORTED"],
        "#1833 a TypeError reload keeps the retained diagnostics"
    );
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
/// `Drop` only aborts and a `stop` that has not been called yet cannot report
/// a death it has not joined (its report, #1920's [`WatchStop::Panicked`],
/// arrives only if someone stops the watcher afterwards), so hot reload dies
/// with the handle still "alive" (the same silent death as #1650's zero
/// period). The clamp keeps the deadline representable, and `deadline_after`
/// is the second line of defence.
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
    assert_eq!(
        MAX_POLL_INTERVAL,
        Duration::from_secs(60),
        "#1793 the poll ceiling pins the documented one-minute clamp"
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
    advance(100).await;
    // #1899: `weak_count == 0` and "no reload" are also true for a watcher that
    // died before the stop, so prove it was still running (and had already
    // ticked) before the stop.
    assert!(
        !watch
            .task
            .as_ref()
            .expect("#1899 the watcher task handle is present")
            .is_finished(),
        "#1899 the watcher must still be running before stop"
    );
    // #1920: the stop's own report is the "stopped" evidence. A watcher that
    // died by panic before the abort joins `Err(JoinError::Panic)` and comes
    // back `Panicked` (and one that returned on its own, `Finished`), so the
    // release and "no reload" below can no longer be produced by a dead
    // watcher alone — they are corroboration, not the proof.
    assert_eq!(
        watch.stop().await,
        WatchStop::Cancelled,
        "#1920 stop must report the watcher alive until its own abort"
    );
    assert_eq!(
        Arc::weak_count(&store.inner),
        0,
        "#1920 the stopped watcher task released the store"
    );
    root.write(&other_model());
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "#1920 no reload after stop"
    );
}

/// #1920: `WatchHandle::stop`'s panic arm needs a watcher that really died by
/// panic; the hook makes exactly this store's watcher panic on its first poll
/// (no public input does that anymore — #1650/#1736 clamp every option that
/// used to). The stop must report the panic, not swallow it the way
/// `let _ = task.await` did — #1650's silent hot-reload death. The two
/// assertions after it pin why the old evidence was false green: a dead
/// watcher releases the store and never reloads exactly like a stopped one.
#[tokio::test(start_paused = true)]
async fn stop_reports_a_watcher_that_died_by_panic() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    arm_watch_loop_panic(&store.inner);
    let watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    // The watcher's first poll runs the gate's panic.
    settle().await;
    assert_eq!(
        watch.stop().await,
        WatchStop::Panicked,
        "#1920 stop must report a watcher that died by panic, not a clean stop"
    );
    assert_eq!(
        Arc::weak_count(&store.inner),
        0,
        "#1920 a panicked watcher releases the store too"
    );
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "#1920 a panicked watcher does not reload either"
    );
}

/// #1920 fix-r1: the third stop shape's positive control — the loop had
/// already returned on its own (the store was dropped, the next tick's
/// owner check ended it) before `stop` was called, so the abort lands on a
/// finished task and the join reports the loop's own `Ok(())`. r0 claimed
/// this could not be constructed on the current_thread runtime; measured:
/// the missing ingredient is one poll interval of time, not a scheduler
/// turn — settle alone never fires the ticker (the stop's abort wins,
/// `Cancelled`), while one interval of clock lets the owner check end the
/// loop first (`Finished`). The paused clock makes that interval
/// deterministic.
#[tokio::test(start_paused = true)]
async fn stop_reports_finished_after_loop_returns() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    advance(100).await;
    // #1899: the `Finished` below must not be reachable by a watcher that
    // died early — all three of #1899's death shapes pre-decease this check.
    assert!(
        !watch
            .task
            .as_ref()
            .expect("#1899 the watcher task handle is present")
            .is_finished(),
        "#1899 the watcher must still be running before the store is dropped"
    );
    drop(store);
    // One poll interval of (paused) clock: the loop's owner check ends it on
    // its own, before `stop` is ever called.
    advance(100).await;
    settle().await;
    assert_eq!(
        watch.stop().await,
        WatchStop::Finished,
        "#1920 stop must report a watcher that returned on its own as Finished"
    );
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "#1920 the self-returning watcher reloaded nothing"
    );
}

/// #1920 fix-r1: `stop`'s else arm — the task was already taken out of the
/// handle (the store-half drop test's move; the field is private, so only
/// this crate's tests can do it). The arm used to return `Finished`, an
/// unobserved death labeled as a clean self-return; it now fails a debug
/// assertion instead. The misuse is driven through a spawned task so the
/// panic arrives as a `JoinError` the test can assert on.
#[tokio::test(start_paused = true)]
async fn stop_after_the_task_was_taken_fails_loudly() {
    let root = Root::new();
    root.write(BASE);
    let (store, _loads) = store(&root);
    let mut watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    advance(100).await;
    assert!(
        !watch
            .task
            .as_ref()
            .expect("#1899 the watcher task handle is present")
            .is_finished(),
        "#1899 the watcher must still be running before the store is dropped"
    );
    drop(store);
    // The store-half move: the task is taken out and joined by the test
    // itself (it returns `Ok(())` once the owner is gone).
    watcher_stop(watch.task.take())
        .await
        .expect("#1899 the watcher returns on its own after the store is dropped");
    // The handle now has no task left; this `stop` never observed the
    // watcher's death and must not paper over that with a shape.
    let misuse = tokio::spawn(async move { watch.stop().await })
        .await
        .expect_err("#1920 stop after the task was taken must not return normally");
    assert!(
        misuse.is_panic(),
        "#1920 the unclassifiable stop must die by panic, not return a shape"
    );
    // A `debug_assert!` with a literal message panics with a `&'static str`
    // payload; accept both that and a formatted `String`.
    let message = match misuse.into_panic().downcast::<&'static str>() {
        Ok(message) => (*message).to_owned(),
        Err(payload) => match payload.downcast::<String>() {
            Ok(message) => *message,
            Err(payload) => format!("{payload:?}"),
        },
    };
    assert!(
        message.contains("#1920 stop after the task was already taken"),
        "#1920 the misuse failure must carry the #1920 marker: {message}"
    );
}

/// #1899/#1920: the handle half of the old two-in-one drop test. Dropping the
/// handle aborts the task and detaches it, so the terminal shape cannot be
/// joined here — the evidence chain is the liveness precondition above plus
/// the release below, and the window between them is what keeps a panic from
/// slipping through: the precondition, `notify_change` and the drop are all
/// synchronous, with no await — no scheduler turn — in between on this
/// single-threaded runtime, so a watcher that was alive at the check cannot
/// die before the abort lands.
#[tokio::test(start_paused = true)]
async fn dropping_the_handle_ends_the_task() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    advance(100).await;
    // #1899: `weak_count == 0` and "no reload" are also true for a watcher that
    // died on its own, so prove it was running (past its first in-loop tick,
    // not merely un-finished at spawn time) before the handle is dropped.
    assert!(
        !watch
            .task
            .as_ref()
            .expect("#1899 the watcher task handle is present")
            .is_finished(),
        "#1899 the watcher must still be running before the handle is dropped"
    );
    watch.notify_change();
    drop(watch);
    settle().await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "#1920 no reload after the handle is dropped"
    );
    assert_eq!(
        Arc::weak_count(&store.inner),
        0,
        "#1920 the aborted watcher released the store"
    );
}

/// #1899/#1920: the store half of the old two-in-one drop test. Here the
/// terminal shape *is* observable — the handle's task is joined directly —
/// so the clean return is asserted, not inferred from `weak_count`.
#[tokio::test(start_paused = true)]
async fn dropping_the_store_ends_the_task() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let mut watch = store
        .start_watching(WatchOptions::default())
        .expect("#879 healthy watcher paths");
    settle().await;
    advance(100).await;
    // #1899: prove the watcher was running (past its first in-loop tick)
    // before the store is dropped.
    assert!(
        !watch
            .task
            .as_ref()
            .expect("#1899 the watcher task handle is present")
            .is_finished(),
        "#1899 the watcher must still be running before the store is dropped"
    );
    drop(store);
    assert_eq!(
        watcher_stop(watch.task.take()).await,
        Ok(()),
        "#1899 dropping the store must stop the watcher cleanly"
    );
    assert_eq!(
        loads.load(Ordering::SeqCst),
        1,
        "#1920 no reload after the store is gone"
    );
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
    assert_eq!(
        watched(&store),
        [(root.0.join("pilotdeck.yaml"), Some(false))]
    );
    std::fs::create_dir(project.join(".pilotdeck")).unwrap();
    assert_eq!(
        watched(&store),
        [
            (root.0.join("pilotdeck.yaml"), Some(false)),
            (project.join(".pilotdeck"), Some(true))
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
    assert_eq!(watched.directory, Some(true));
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
        fingerprint(&root.0, true, target, true).0,
        fingerprint(&root.0, true, None, true).0
    );
    let other = Some(std::ffi::OsStr::new("other.yaml"));
    assert_eq!(
        fingerprint(&root.0, true, other, true).0,
        fingerprint(&root.0, true, None, true).0
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
    hash_entry(&path, &mut with_content, None, true);
    let mut metadata_only = DefaultHasher::new();
    hash_entry(&path, &mut metadata_only, None, false);
    assert_eq!(with_content.finish(), metadata_only.finish());
    let mut watched = WatchedPath::new(path.clone()).expect("file watched");
    std::fs::write(&path, &big).unwrap();
    assert!(watched.changed(), "a rewrite still bumps ctime");
    // Negative control: at the limit the content is part of the sample.
    std::fs::write(&path, &big[..CONTENT_SAMPLE_LIMIT as usize]).unwrap();
    let mut with_content = DefaultHasher::new();
    hash_entry(&path, &mut with_content, None, true);
    let mut metadata_only = DefaultHasher::new();
    hash_entry(&path, &mut metadata_only, None, false);
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
    // #1987 A2: the startup sample commits no metadata shortcut, so the first
    // tick re-samples in full — one content read — before the short-circuit
    // takes over; the count baseline starts after it.
    assert!(
        !watched.changed(),
        "#1739 the first re-sample of an unchanged file is quiet"
    );
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
    // #1987 A2: the startup sample commits no metadata shortcut, so without a
    // quiet tick first this test's single sample re-reads the content
    // unconditionally — the ctime mutant would survive. One quiet tick
    // commits the shortcut, which is what this test is about.
    assert!(
        !watched.changed(),
        "#1739 the first re-sample of an unchanged file is quiet"
    );
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

/// #1792: a transient content-read failure (EMFILE/ENFILE-class) must not
/// commit the metadata fingerprint — committing anyway closed the recovery
/// path for good, because every later sample with identical metadata
/// short-circuited before retrying the read. The failure is injected through
/// the per-path hook (no fd exhaustion, no chmod — those disturb the shared
/// fd table or bump ctime and mask the bug). Asserted in order: the change is
/// reported once while failing, stays quiet while the failure persists (no
/// reload storm), and — with no metadata change at all — is re-reported once
/// the read recovers.
#[test]
fn transient_read_failure_is_retried_until_the_content_is_seen() {
    let root = Root::new();
    root.write(BASE);
    let path = root.0.join("pilotdeck.yaml");
    let mut watched = WatchedPath::new(path.clone()).expect("#1792 file watched");
    let before = content_reads(&path);
    // The write that must be seen; the next three content reads fail.
    root.write(&other_model());
    arm_content_read_failures(&path, 3);
    // Sample 1: the read fails, so the sample is incomplete — the change is
    // reported once (the full fingerprint folds the failed read to `None`),
    // and no metadata fingerprint is committed.
    assert!(
        watched.changed(),
        "#1792 the first (incomplete) sample still reports the change once"
    );
    // Samples 2 and 3: the read keeps failing, so every tick re-samples —
    // but the same unreadable state must not re-report (no reload storm).
    assert!(
        !watched.changed(),
        "#1792 a persistently unreadable sample stays quiet"
    );
    assert!(
        !watched.changed(),
        "#1792 a persistently unreadable sample stays quiet"
    );
    assert_eq!(
        content_reads(&path),
        before,
        "#1792 the failed reads are not counted as content reads"
    );
    // The armed failures are exhausted; nothing else changes — no rewrite, no
    // mtime/chmod touch, so the metadata is bit-for-bit identical. The next
    // tick must still retry the read and report the recovered content.
    assert!(
        watched.changed(),
        "#1792 the read is retried after recovery with no metadata change"
    );
    assert_eq!(
        content_reads(&path),
        before + 1,
        "#1792 the recovered sample reads the content exactly once"
    );
    assert_eq!(
        watched.metadata_fingerprint, None,
        "#1792 r2 the reported tick commits no metadata shortcut"
    );
    // Back on the committed fingerprints: the reported tick committed no
    // shortcut, so this one re-reads the content once more and only then
    // commits.
    assert!(
        !watched.changed(),
        "#1792 a complete sample commits the metadata short-circuit again"
    );
    assert_eq!(
        content_reads(&path),
        before + 2,
        "#1792 r2 the tick after a reported change re-reads the content once"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1792 r2 the quiet re-read commits the metadata shortcut"
    );
    assert_eq!(content_reads(&path), before + 2);
    // The committed state matches a fresh complete sample of the same file.
    // #1987 A2: a fresh startup commits no metadata shortcut anymore, so only
    // the full fingerprints line up.
    let fresh = WatchedPath::new(path.clone()).expect("#1792 fresh watch");
    assert_eq!(fresh.fingerprint, watched.fingerprint);
    assert_eq!(
        fresh.metadata_fingerprint, None,
        "#1987 A2 a fresh startup sample commits no metadata shortcut"
    );
    clear_content_read_failures(&path);
}

/// #1792: the startup sample follows the same rule — a file that was
/// unreadable when watching started commits no metadata fingerprint, so the
/// tick after the read recovers re-samples and reports, without any metadata
/// change in between.
#[test]
fn startup_read_failure_is_retried_until_the_content_is_seen() {
    let root = Root::new();
    root.write(BASE);
    let path = root.0.join("pilotdeck.yaml");
    // The startup content read fails; the metadata-only pass succeeds.
    arm_content_read_failures(&path, 1);
    let mut watched = WatchedPath::new(path.clone()).expect("#1792 file watched");
    assert_eq!(
        content_reads(&path),
        0,
        "#1792 the failed startup read is not counted"
    );
    assert_eq!(
        watched.metadata_fingerprint, None,
        "#1792 an incomplete startup sample commits no metadata fingerprint"
    );
    // No write, no metadata change: the next tick retries the read and sees
    // the content that startup could not.
    assert!(
        watched.changed(),
        "#1792 recovery from a failed startup sample needs no metadata change"
    );
    assert_eq!(
        content_reads(&path),
        1,
        "#1792 the retry reads the content exactly once"
    );
    assert_eq!(
        watched.metadata_fingerprint, None,
        "#1792 r2 the reported tick commits no metadata shortcut"
    );
    // The reported tick committed no shortcut (r2 D11): this one re-reads
    // the content once more, is quiet, and only then commits.
    assert!(!watched.changed(), "#1792 back on the short-circuit");
    assert_eq!(
        content_reads(&path),
        2,
        "#1792 r2 the tick after a reported change re-reads the content once"
    );
    assert_eq!(
        watched.metadata_fingerprint,
        Some(fingerprint(&path, false, None, false).0),
        "#1792 a complete sample commits the metadata fingerprint"
    );
    clear_content_read_failures(&path);
}

/// #1792 r1 (P2-1) / r2 (D9): a failed `stat` — not a `NotFound` — must
/// leave the metadata fingerprint uncommitted and report nothing while it
/// fails (the folded sample is not evidence of a change). It must also
/// commit no *full* fingerprint (r1 did): recovery compares against the
/// last complete sample, so an unchanged recovery reports nothing and a
/// real change — including one made during the outage — is still seen.
#[test]
fn stat_failure_leaves_the_metadata_fingerprint_uncommitted() {
    let root = Root::new();
    root.write(BASE);
    let path = root.0.join("pilotdeck.yaml");
    let mut watched = WatchedPath::new(path.clone()).expect("#1792 file watched");
    // Two obscured ticks: each consumes three armed stats (the type probe,
    // the metadata-only sample and the folded full sample).
    arm_read_failures(&path, ReadOp::Stat, 6);
    for tick in 0..2 {
        assert!(
            !watched.changed(),
            "#1792 a stat-failed sample {tick} reports no change"
        );
        assert_eq!(
            watched.metadata_fingerprint, None,
            "#1792 a stat-failed sample {tick} commits no metadata fingerprint"
        );
    }
    // The failures are exhausted; nothing about the file changed in between,
    // so recovery is quiet (D9) — not an identity reload.
    assert!(
        !watched.changed(),
        "#1792 r2 an unchanged recovery after stat failures reports nothing"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1792 the recovered complete sample commits the metadata fingerprint"
    );
    assert!(
        !watched.changed(),
        "#1792 the recovered sample re-commits the short-circuit"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1792 the short-circuited sample keeps the metadata fingerprint"
    );
    // A real change after recovery is still seen.
    root.write(&other_model());
    assert!(
        watched.changed(),
        "#1792 r2 a change after stat recovery is still seen"
    );
    clear_read_failures(&path, ReadOp::Stat);
}

/// #1792 r2 (D9 core regression): a change made *during* a stat outage —
/// a rewrite, and above all a delete — must be seen after recovery. The r1
/// code committed the folded obscured sample as the full fingerprint; the
/// folded fingerprint hashes a failed stat exactly like a missing entry, so
/// a delete during the outage compared equal and was never reported.
#[test]
fn stat_failure_during_change_is_seen_after_recovery() {
    let root = Root::new();
    root.write(BASE);
    let path = root.0.join("pilotdeck.yaml");
    let mut watched = WatchedPath::new(path.clone()).expect("#1792 r2 file watched");
    // Three obscured ticks (three armed stats each: type probe, metadata
    // sample, folded full sample).
    arm_read_failures(&path, ReadOp::Stat, 9);
    assert!(
        !watched.changed(),
        "#1792 r2 the first obscured tick reports nothing"
    );
    // A rewrite made during the outage.
    root.write(&other_model());
    assert!(
        !watched.changed(),
        "#1792 r2 an obscured tick after a rewrite reports nothing"
    );
    assert!(
        !watched.changed(),
        "#1792 r2 the same unreadable state stays quiet"
    );
    // The failures are exhausted: the recovery sample differs from the last
    // complete one, so the rewrite is reported.
    assert!(
        watched.changed(),
        "#1792 r2 a rewrite made during a stat outage is seen after recovery"
    );
    // A delete made during a second outage: the recovery sample is a
    // complete observation of absence and must differ from the last complete
    // sample — comparing it against the folded sample instead would find
    // them identical and drop the delete.
    arm_read_failures(&path, ReadOp::Stat, 6);
    assert!(
        !watched.changed(),
        "#1792 r2 the first obscured tick of the second outage reports nothing"
    );
    std::fs::remove_file(&path).expect("#1792 r2 remove during the outage");
    assert!(
        !watched.changed(),
        "#1792 r2 an obscured tick after the delete reports nothing"
    );
    assert!(
        watched.changed(),
        "#1792 r2 a delete made during a stat outage is seen after recovery"
    );
    clear_read_failures(&path, ReadOp::Stat);
}

/// #1792 r1 (P2-1) / r2 (D9): a failed `read_dir` on a watched directory
/// follows the same rule as a failed stat — nothing committed, nothing
/// reported while it fails, and an unchanged recovery is quiet — and the
/// directory still sees later changes once the reads recover.
#[test]
fn read_dir_failure_leaves_the_metadata_fingerprint_uncommitted() {
    let root = Root::new();
    let dir = root.0.join("watched");
    std::fs::create_dir(&dir).expect("#1792 watch directory");
    std::fs::write(dir.join("seed.yaml"), "a: 1\n").expect("#1792 seed entry");
    let mut watched = WatchedPath::new(dir.clone()).expect("#1792 directory watched");
    assert_eq!(
        watched.directory,
        Some(true),
        "#1792 the startup probe classifies the directory"
    );
    // Two obscured ticks: each consumes two armed `read_dir`s (the
    // metadata-only sample and the folded full sample).
    arm_read_failures(&dir, ReadOp::ReadDir, 4);
    for tick in 0..2 {
        assert!(
            !watched.changed(),
            "#1792 a read_dir-failed sample {tick} reports no change"
        );
        assert_eq!(
            watched.metadata_fingerprint, None,
            "#1792 a read_dir-failed sample {tick} commits no metadata fingerprint"
        );
    }
    // The failures are exhausted; the directory did not change in between,
    // so recovery is quiet (D9) and commits again.
    assert!(
        !watched.changed(),
        "#1792 r2 an unchanged recovery after read_dir failures reports nothing"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1792 the recovered complete sample commits the metadata fingerprint"
    );
    // A change in the recovered directory is still seen.
    std::fs::write(dir.join("late.yaml"), "b: 2\n").expect("#1792 late entry");
    assert!(
        watched.changed(),
        "#1792 a new entry in the recovered directory is still seen"
    );
    clear_read_failures(&dir, ReadOp::ReadDir);
}

/// #1792 r1 (P2-2, D1): a dangling symlink inside a watched directory is a
/// complete observation of absence, not an incomplete sample — the
/// metadata fingerprint must commit instead of staying `None` forever —
/// and the entry resolving is still seen. #1792 r2 (fix-r2 review P2-1):
/// the symlink's target lives *outside* the watched directory, so its
/// creation changes neither the directory's own metadata nor its name
/// list — only the entry's stat result flips from `NotFound` to `Ok`, which
/// is the cell this test exists to pin.
#[test]
fn missing_entry_is_a_complete_observation() {
    let root = Root::new();
    let dir = root.0.join("watched");
    std::fs::create_dir(&dir).expect("#1792 watch directory");
    let target = root.0.join("outside.yaml");
    std::os::unix::fs::symlink(&target, dir.join("dangling"))
        .expect("#1792 dangling symlink to outside");
    let mut watched = WatchedPath::new(dir.clone()).expect("#1792 directory watched");
    // #1987 A2: the startup sample commits no metadata shortcut; the first
    // tick re-samples in full, is quiet, and commits — the dangling entry is
    // a complete observation, not a permanent None.
    assert!(
        !watched.changed(),
        "#1792 an unchanged dangling entry is quiet"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1792 a dangling entry is a complete observation, not a permanent None"
    );
    assert!(
        !watched.changed(),
        "#1792 an unchanged dangling entry stays quiet on the shortcut"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1792 an unchanged dangling entry keeps the metadata fingerprint"
    );
    std::fs::write(&target, "a: 1\n").expect("#1792 symlink target outside the watch");
    assert!(
        watched.changed(),
        "#1792 the dangling entry resolving is seen"
    );
}

/// #1792 r1 (P1-1): a startup probe failure must not drop the path. The
/// path is kept with an unknown mode and no sample; the first `changed()`
/// re-probes and classifies. #1792 r2 (D8): the config may have changed
/// while the probe — and every sample — was failing, so the first complete
/// sample after the failed probe must report; only the tick after that is
/// quiet, and a real change is still seen.
#[test]
fn startup_stat_failure_keeps_the_path_watched() {
    let root = Root::new();
    let dir = root.0.join("watched");
    std::fs::create_dir(&dir).expect("#1792 watch directory");
    arm_read_failures(&dir, ReadOp::Stat, 1);
    let mut watched = WatchedPath::new(dir.clone())
        .expect("#1792 a stat failure at startup must not drop the path");
    assert_eq!(
        watched.directory, None,
        "#1792 the startup probe failure leaves the mode unknown"
    );
    assert_eq!(
        watched.fingerprint, None,
        "#1792 the startup probe failure commits no sample"
    );
    assert_eq!(
        watched.metadata_fingerprint, None,
        "#1792 the startup probe failure commits no metadata fingerprint"
    );
    // The config appears while the probe's failure is still armed: the first
    // complete sample must report it (D8) — the probe never saw a predecessor
    // to compare against.
    std::fs::write(dir.join("pilotdeck.yaml"), BASE)
        .expect("#1792 r2 config appears during the failed probe");
    assert!(
        watched.changed(),
        "#1792 r2 the first complete sample after a failed probe reports the change made during the failure"
    );
    assert_eq!(
        watched.directory,
        Some(true),
        "#1792 the re-probe classifies the directory"
    );
    // The reported tick committed no shortcut (D11); this one is quiet and
    // commits it.
    assert!(
        !watched.changed(),
        "#1792 r2 the tick after the reported one is quiet"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1792 the quiet tick after the reported one commits the metadata fingerprint"
    );
    std::fs::write(dir.join("late.yaml"), "a: 1\n").expect("#1792 r2 late entry");
    assert!(
        watched.changed(),
        "#1792 a change after a failed startup probe is still seen"
    );
    clear_read_failures(&dir, ReadOp::Stat);
}

/// #1792 r2 (D10, review P1-3): a startup probe failure leaves the mode
/// unknown, and a `NotFound` on the re-probe is not evidence of a file —
/// the path may still appear as a directory later. The mode must stay
/// unknown (re-probed every tick, never short-circuited) until a probe
/// classifies it; a mode fixed to file would sample only the parent's own
/// metadata and miss an in-place rewrite of the config inside.
#[test]
fn probe_failure_then_directory_is_reclassified() {
    let root = Root::new();
    let dir = root.0.join("watched");
    let file = dir.join("pilotdeck.yaml");
    arm_read_failures(&dir, ReadOp::Stat, 1);
    let mut watched = WatchedPath::new(file.clone())
        .expect("#1792 r2 an unclassifiable startup probe keeps the path");
    assert_eq!(
        watched.directory, None,
        "#1792 r2 the mode is unknown after the probe failure"
    );
    // The parent is really missing: two ticks must not panic or error.
    let _ = watched.changed();
    let _ = watched.changed();
    assert!(
        watched.directory.is_none(),
        "#1792 r2 a NotFound re-probe must not fix the mode to file"
    );
    // The parent appears as a directory with the config inside.
    std::fs::create_dir_all(&dir).expect("#1792 r2 parent directory appears");
    std::fs::write(&file, BASE).expect("#1792 r2 config appears in the parent");
    assert!(
        watched.changed(),
        "#1792 r2 the reclassified directory watch sees the config appear"
    );
    // An in-place rewrite of the config leaves the parent's own metadata
    // alone; only the directory-mode sample (entry metadata plus the target
    // content) can see it.
    std::fs::write(&file, other_model().as_str()).expect("#1792 r2 in-place rewrite");
    assert!(
        watched.changed(),
        "#1792 r2 an in-place rewrite is seen through the directory mode"
    );
    clear_read_failures(&dir, ReadOp::Stat);
}

/// #1792 r2 (D11, review P1-4): the tick that reports a change must not
/// also commit the metadata shortcut — the metadata-only and the full
/// sample happen at different times, and a match on the first is not
/// evidence the second saw the same state (a file moved away and back with
/// the same inode between the two samples would otherwise leave the
/// snapshot stuck on "missing"). The next tick re-reads the content once,
/// is quiet, and only then commits.
#[test]
fn a_reported_change_does_not_commit_the_metadata_shortcut() {
    let root = Root::new();
    root.write(BASE);
    let path = root.0.join("pilotdeck.yaml");
    let mut watched = WatchedPath::new(path.clone()).expect("#1792 r2 file watched");
    root.write(&other_model());
    assert!(watched.changed(), "#1792 r2 the rewrite is reported");
    assert_eq!(
        watched.metadata_fingerprint, None,
        "#1792 r2 the reported tick commits no metadata shortcut"
    );
    let reads_after_report = content_reads(&path);
    assert!(
        !watched.changed(),
        "#1792 r2 the tick after the reported one is quiet"
    );
    assert_eq!(
        content_reads(&path),
        reads_after_report + 1,
        "#1792 r2 the tick after a reported change re-reads the content exactly once"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1792 r2 the quiet re-read commits the metadata shortcut"
    );
    assert!(!watched.changed(), "#1792 r2 the shortcut holds");
    assert_eq!(
        content_reads(&path),
        reads_after_report + 1,
        "#1792 r2 the shortcut skips the content read"
    );
}

/// #1792 r2 (D16, review boundary P2-1 raised to P1): a permanently
/// unreadable sibling entry (an ELOOP-class stat failure) obscures every
/// sample of the watched directory; the config file itself stays readable,
/// and its changes must still be reported. Obscured samples are compared
/// against the previous obscured sample only, so entering the obstruction
/// reports nothing and the same obstructed state reports at most once.
#[test]
fn a_persistent_obstruction_still_reports_observable_changes() {
    let root = Root::new();
    let dir = root.0.join("watched");
    let file = dir.join("pilotdeck.yaml");
    let obstructed = dir.join("obstructed");
    std::fs::create_dir(&dir).expect("#1792 r2 watch directory");
    std::fs::write(&obstructed, "x").expect("#1792 r2 permanently unreadable sibling");
    // Enough armed failures for every stat of the sibling this test makes.
    arm_read_failures(&obstructed, ReadOp::Stat, 64);
    let mut watched = WatchedPath::new(dir.clone()).expect("#1792 r2 directory watched");
    assert!(
        !watched.changed(),
        "#1792 r2 entering an obstruction reports nothing"
    );
    std::fs::write(&file, "a: 2\n").expect("#1792 r2 config inside the obstructed directory");
    assert!(
        watched.changed(),
        "#1792 r2 a persistent obstruction must not silence observable changes"
    );
    assert!(
        !watched.changed(),
        "#1792 r2 the same obstructed state reports at most once"
    );
    clear_read_failures(&obstructed, ReadOp::Stat);
}

/// #1792 r2 (D17, review boundary P2-3): a vanished watched directory is a
/// complete observation of absence — reported once, quiet while it stays
/// gone — and its return is reported too. The `read_dir` `NotFound` branch
/// previously had no test and no negative control at all.
#[test]
fn a_removed_directory_is_a_complete_observation() {
    let root = Root::new();
    let dir = root.0.join("watched");
    let file = dir.join("pilotdeck.yaml");
    std::fs::create_dir(&dir).expect("#1792 r2 watch directory");
    std::fs::write(&file, "a: 1\n").expect("#1792 r2 config entry");
    let mut watched = WatchedPath::new(dir.clone()).expect("#1792 r2 directory watched");
    assert!(
        !watched.changed(),
        "#1792 r2 an unchanged directory is quiet"
    );
    std::fs::remove_dir_all(&dir).expect("#1792 r2 remove the watched directory");
    assert!(
        watched.changed(),
        "#1792 r2 a vanished directory is a complete observation and reports once"
    );
    assert!(
        !watched.changed(),
        "#1792 r2 the vanished directory stays quiet"
    );
    std::fs::create_dir_all(&dir).expect("#1792 r2 the directory returns");
    std::fs::write(&file, "a: 2\n").expect("#1792 r2 the config returns");
    assert!(
        watched.changed(),
        "#1792 r2 the directory coming back reports"
    );
}

/// #1792 r2 (D15, review concurrency P2-1 raised to P1): a watched
/// directory replaced by a plain file would hit `read_dir` ENOTDIR on
/// every tick — under the r1 obscured early-return that meant Obscured
/// forever and no reload ever again. One stat per tick keeps the mode in
/// step with the path, so the replacement is seen and later rewrites of
/// the replacement file are seen too.
#[test]
fn a_directory_replaced_by_a_file_is_seen() {
    let root = Root::new();
    let dir = root.0.join("watched");
    std::fs::create_dir(&dir).expect("#1792 r2 watch directory");
    std::fs::write(dir.join("seed.yaml"), "a: 1\n").expect("#1792 r2 seed entry");
    let mut watched = WatchedPath::new(dir.clone()).expect("#1792 r2 directory watched");
    assert_eq!(
        watched.directory,
        Some(true),
        "#1792 r2 the startup probe classifies the directory"
    );
    std::fs::remove_dir_all(&dir).expect("#1792 r2 remove the directory");
    std::fs::write(&dir, "x").expect("#1792 r2 replace it with a file");
    assert!(
        watched.changed(),
        "#1792 r2 a directory replaced by a file is seen"
    );
    std::fs::write(&dir, "y").expect("#1792 r2 rewrite the replacement file");
    assert!(
        watched.changed(),
        "#1792 r2 a later rewrite of the replacement file is still seen"
    );
}

/// #1987 A1: the tick that transitions from a complete sample into an
/// obstruction must not swallow a target rewrite landing on that same tick.
/// Pre-fix, the transition tick had no obscured predecessor to compare
/// against and the rewrite waited for the obstruction to clear before being
/// reported (the ledger's "observable changes are still reported during a
/// persistent obstruction" only held from the *second* obscured tick on).
/// The watch is the parent-directory shape — the config file missing at
/// startup, so it is the watched target — and the rewrite is the target's
/// appearance; only the target is comparable across the transition (the
/// other entries are hashed from failed reads).
#[test]
fn obscured_transition_reports_a_same_tick_target_rewrite() {
    let root = Root::new();
    // The config file is missing at startup: the parent directory is watched
    // with the file as its target (`existsSync` -> `dirname` fallback).
    let path = root.0.join("pilotdeck.yaml");
    let obstructed = root.0.join("obstructed");
    std::fs::write(&obstructed, "x").expect("#1987 A1 unreadable sibling");
    let mut watched = WatchedPath::new(path.clone()).expect("#1987 A1 parent watched");
    assert!(
        watched.fingerprint.is_some(),
        "#1987 A1 the startup sample is complete"
    );
    // The obstruction starts on the next tick; the target appears (and is
    // written) before it, so the change lands on the transition tick itself.
    // Each obscured tick consumes two armed sibling stats: the metadata-only
    // and the folded sample.
    arm_read_failures(&obstructed, ReadOp::Stat, 6);
    std::fs::write(&path, other_model().as_str()).expect("#1987 A1 same-tick target rewrite");
    assert!(
        watched.changed(),
        "#1987 A1 a target rewrite on the obstruction's first tick must be reported"
    );
    assert!(
        !watched.changed(),
        "#1987 A1 the same obstructed state reports at most once"
    );
    // Recovery still compares against the last complete sample, so the
    // rewrite is reported once more there — the registered shape: the
    // transition tick caught it early, recovery confirms it against the
    // complete baseline.
    clear_read_failures(&obstructed, ReadOp::Stat);
    assert!(
        watched.changed(),
        "#1987 A1 recovery reports the difference against the last complete sample"
    );
}

/// #1987 A2: a write landing between the startup's two samples must be
/// reported. Pre-fix, the startup committed the *first* (metadata-only)
/// sample's fingerprint while the full sample absorbed the write, so neither
/// the short-circuit nor the full comparison ever reported it — the D11
/// guard never reached the startup path.
#[test]
fn a_write_between_the_startup_samples_is_reported() {
    let root = Root::new();
    root.write(BASE);
    let path = root.0.join("pilotdeck.yaml");
    // The gate fires between the startup's samples (pre-fix) / right after
    // the startup sample (post-fix): the write it makes is the one the
    // startup window can swallow.
    *lock(&STARTUP_SAMPLE_WRITE) = Some((path.clone(), other_model().into_bytes()));
    let mut watched = WatchedPath::new(path.clone()).expect("#1987 A2 file watched");
    assert!(
        watched.changed(),
        "#1987 A2 a write between the startup samples is reported"
    );
    assert_eq!(
        watched.metadata_fingerprint, None,
        "#1987 A2 the startup sample commits no metadata shortcut"
    );
    // Back on the short-circuit: quiet, and the next tick commits.
    assert!(
        !watched.changed(),
        "#1987 A2 the tick after the reported one is quiet"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1987 A2 the quiet re-read commits the metadata shortcut"
    );
    *lock(&STARTUP_SAMPLE_WRITE) = None;
}

/// #1987 A4: a mixed tick — the metadata sample fails but the same tick's
/// folded sample succeeds — is a complete observation: it must be compared
/// against the last complete fingerprint and committed as the new baseline,
/// not routed through the obscured path. Pre-fix the complete folded sample
/// was written into the obscured chain (the change was deferred a tick, and
/// a later readability flap compared a complete folded value against an
/// obscured one and false-reported).
#[test]
fn a_mixed_tick_compares_against_the_last_complete_fingerprint() {
    let root = Root::new();
    let dir = root.0.join("watched");
    let file = dir.join("pilotdeck.yaml");
    let flapping = dir.join("flapping");
    std::fs::create_dir(&dir).expect("#1987 A4 watch directory");
    std::fs::write(&file, BASE).expect("#1987 A4 config entry");
    std::fs::write(&flapping, "x").expect("#1987 A4 flap-prone sibling");
    let mut watched = WatchedPath::new(dir.clone()).expect("#1987 A4 directory watched");
    // Mixed tick: the metadata sample's stat of the sibling fails, the folded
    // sample's succeeds — exactly one armed failure — and the target is
    // rewritten on that same tick.
    arm_read_failures(&flapping, ReadOp::Stat, 1);
    std::fs::write(&file, other_model().as_str()).expect("#1987 A4 same-tick target rewrite");
    assert!(
        watched.changed(),
        "#1987 A4 a mixed tick reports against the last complete fingerprint"
    );
    assert!(
        !watched.changed(),
        "#1987 A4 the tick after the mixed one is quiet (the baseline was committed)"
    );
    // A readability flap with no real change must not report: the next
    // obscured tick compares against the previous obscured sample, and the
    // mixed tick no longer pollutes that chain with a complete folded value.
    arm_read_failures(&flapping, ReadOp::Stat, 2);
    assert!(
        !watched.changed(),
        "#1987 A4 a readability flap with no real change reports nothing"
    );
    clear_read_failures(&flapping, ReadOp::Stat);
}

/// #1987 B5: a startup sample that is already obscured commits the folded
/// value into the obscured chain only — never into the complete fingerprint
/// slot. The r1 shape (the reverse assignment) made every change during the
/// obstruction invisible and the recovery report on D8 disappear; no test
/// pinned that arm, so a regression to it stayed green.
#[test]
fn a_startup_obstruction_commits_no_complete_fingerprint() {
    let root = Root::new();
    let path = root.0.join("pilotdeck.yaml");
    let obstructed = root.0.join("obstructed");
    std::fs::write(&obstructed, "x").expect("#1987 B5 unreadable sibling");
    // The sibling is unreadable from the very first startup sample on; the
    // per-sample count covers the startup sample and two full ticks.
    arm_read_failures(&obstructed, ReadOp::Stat, 5);
    let mut watched = WatchedPath::new(path.clone()).expect("#1987 B5 parent watched");
    assert_eq!(
        watched.fingerprint, None,
        "#1987 B5 an obscured startup sample commits no complete fingerprint"
    );
    assert!(
        watched.obscured_fingerprint.is_some(),
        "#1987 B5 an obscured startup sample seeds the obscured chain"
    );
    assert_eq!(
        watched.metadata_fingerprint, None,
        "#1987 B5 an obscured startup sample commits no metadata shortcut"
    );
    // A change during the obstruction is reported through the folded chain
    // (the second obscured tick compares against the startup's).
    std::fs::write(&path, other_model().as_str())
        .expect("#1987 B5 the target appears during the obstruction");
    assert!(
        watched.changed(),
        "#1987 B5 a change during the obstruction is reported through the folded chain"
    );
    assert!(
        !watched.changed(),
        "#1987 B5 the same obstructed state reports at most once"
    );
    // Recovery is D8's to report: no complete fingerprint was ever committed,
    // so the first complete sample reports once more.
    clear_read_failures(&obstructed, ReadOp::Stat);
    assert!(
        watched.changed(),
        "#1987 B5 recovery reports against the never-committed complete baseline"
    );
    assert!(
        watched.fingerprint.is_some(),
        "#1987 B5 the recovered sample commits the complete fingerprint"
    );
}

/// #1987 B6: a `read_dir` iteration that fails mid-listing — after yielding
/// some entries, an EMFILE-class error — obscures the sample exactly like a
/// failed open: no report while it fails (the folded sample is not evidence
/// of a change), no metadata shortcut committed, and the entries the
/// truncated listing did not reach must not be mistaken for vanished. An
/// unchanged recovery is quiet (D9) and a later entry is still seen.
#[test]
fn read_dir_iteration_failure_obscures_the_sample() {
    let root = Root::new();
    let dir = root.0.join("watched");
    std::fs::create_dir(&dir).expect("#1987 B6 watch directory");
    std::fs::write(dir.join("a.yaml"), "a: 1\n").expect("#1987 B6 entry a");
    std::fs::write(dir.join("b.yaml"), "b: 1\n").expect("#1987 B6 entry b");
    let mut watched = WatchedPath::new(dir.clone()).expect("#1987 B6 directory watched");
    assert!(
        watched.fingerprint.is_some(),
        "#1987 B6 the startup sample is complete"
    );
    // Both entries are swallowed before either sample of either of two ticks
    // reaches them: the whole listing is poisoned per sample.
    arm_read_failures(&dir, ReadOp::ReadDirEntry, 4);
    assert!(
        !watched.changed(),
        "#1987 B6 an obscured sample reports nothing"
    );
    assert_eq!(
        watched.metadata_fingerprint, None,
        "#1987 B6 an obscured sample commits no metadata shortcut"
    );
    assert!(
        !watched.changed(),
        "#1987 B6 the second obscured tick stays quiet"
    );
    // Recovery: nothing really changed, so D9 keeps it quiet.
    clear_read_failures(&dir, ReadOp::ReadDirEntry);
    assert!(
        !watched.changed(),
        "#1987 B6 an unchanged recovery after obscured ticks reports nothing"
    );
    assert!(
        watched.metadata_fingerprint.is_some(),
        "#1987 B6 the recovered sample commits the metadata shortcut again"
    );
    std::fs::write(dir.join("c.yaml"), "c: 1\n").expect("#1987 B6 late entry");
    assert!(
        watched.changed(),
        "#1987 B6 an entry created after recovery is still seen"
    );
}

/// #1987 A3 (registration only): a startup probe failure on the *parent* of a
/// missing config file keeps watching that parent (the `existsSync` ->
/// `dirname` fallback's target) instead of dropping the path — the mode stays
/// unknown until a probe succeeds. While the probe keeps failing nothing is
/// observable — a fixture-level deviation, not yet in the diff ledger: the TS
/// baseline falls back a single level
/// (`existsSync(path) ? path : dirname(path)`, with no grandparent hop) and
/// then `watch(dirname)` throws EACCES on the unreadable parent, which the
/// bare `catch {}` swallows, so that path has **no watcher at all** (only a
/// manual reload remains; measured on Node v22.22.0). Rust keeps the parent
/// registered and reports the missed change once the probe recovers —
/// aligning to TS turns the recovery assert red, not the mid-test one.
#[test]
fn startup_parent_probe_failure_watches_the_parent_dir() {
    let root = Root::new();
    let dir = root.0.join("watched");
    let path = dir.join("pilotdeck.yaml");
    std::fs::create_dir(&dir).expect("#1987 A3 watch directory");
    // The config file is missing and its parent's stat fails (three armed
    // failures: the startup probe plus the first two `changed()` re-probes).
    arm_read_failures(&dir, ReadOp::Stat, 3);
    let mut watched = WatchedPath::new(path.clone()).expect("#1987 A3 the parent is kept");
    assert_eq!(
        watched.path, dir,
        "#1987 A3 the parent directory is watched, not dropped"
    );
    assert_eq!(
        watched.directory, None,
        "#1987 A3 the mode stays unknown while the probe fails"
    );
    assert_eq!(
        watched.target.as_deref(),
        Some(std::ffi::OsStr::new("pilotdeck.yaml")),
        "#1987 A3 the config file stays the target"
    );
    // Registered gap (fixture-level, not yet in the diff ledger): a sibling
    // change during the probe failure is not reported. TS has no watcher on
    // this path at all (`watch(dirname)` throws EACCES and the bare `catch`
    // swallows it), so aligning to the baseline turns the recovery assert
    // below red, not this one.
    std::fs::write(dir.join("sibling.yaml"), "x").expect("#1987 A3 sibling entry");
    assert!(
        !watched.changed(),
        "#1987 A3 (registered gap) a sibling change during the probe failure is not reported"
    );
    // Recovery: the re-probe classifies the directory and the first sample
    // (the sibling appeared while nothing was observable) reports.
    clear_read_failures(&dir, ReadOp::Stat);
    assert!(
        watched.changed(),
        "#1987 A3 recovery classifies the parent and reports the missed change"
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
    assert_eq!(watched(&store), [(cwd.join("cfg.yaml"), Some(false))]);
    // Missing: watched through `dirname`, i.e. cwd (TS: `dirname("cfg.yaml")`
    // is `.`), not skipped because the bare name has an empty parent.
    std::fs::remove_file(cwd.join("cfg.yaml")).unwrap();
    assert_eq!(watched(&store), [(cwd.clone(), Some(true))]);
}
