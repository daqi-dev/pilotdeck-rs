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
async fn reload_called_while_running_joins_the_running_flight() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    // The first listener call blocks publication (the flight is Running)
    // until released; later calls (a regression's extra reload) return.
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
    let joined = tokio::spawn(store.reload("manual"));
    settle().await;
    assert!(
        !joined.is_finished(),
        "joined call waits for the running flight"
    );
    release_tx.send(()).unwrap();
    let first = first.await.unwrap().unwrap();
    let joined = joined.await.unwrap().unwrap();
    assert!(Arc::ptr_eq(&first, &joined));
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
    let watch = store.start_watching(WatchOptions::default());
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
    let watch = store.start_watching(WatchOptions::default());
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
    let watch = store.start_watching(WatchOptions::default());
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
async fn stop_cancels_pending_reload_and_leaves_no_task() {
    let root = Root::new();
    root.write(BASE);
    let (store, loads) = store(&root);
    let watch = store.start_watching(WatchOptions::default());
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
    let watch = store.start_watching(WatchOptions::default());
    settle().await;
    watch.notify_change();
    drop(watch);
    settle().await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert_eq!(Arc::weak_count(&store.inner), 0);

    let mut watch = store.start_watching(WatchOptions::default());
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
