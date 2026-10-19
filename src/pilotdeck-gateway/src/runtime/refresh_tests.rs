//! Ordering of the bundle over the product `ConfigStore` and
//! `ProjectRuntimeRegistry` (through `RegistryRefresh`). No test orders work
//! with real sleeps: concurrency is forced with barriers/gates and checked on
//! a multi-threaded runtime.
use super::*;
use crate::{
    config::{Env, store::StoreOptions},
    runtime::project::{
        DisposeFuture, ProjectRuntimeFactory, ResourceBundle, RuntimeResources, RuntimeState,
    },
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

const BASE: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n        other: {}\n";

fn other_model() -> String {
    BASE.replace("parity/deterministic", "parity/other")
}

/// A directory this test owns; removed on drop.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-refresh-unit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, yaml: &str) {
        std::fs::write(self.0.join("pilotdeck.yaml"), yaml).unwrap();
    }
    fn store(&self) -> ConfigStore {
        let home = self.0.to_string_lossy().into_owned();
        let mut options = StoreOptions::new(
            &self.0,
            &self.0,
            Env::from([("PILOT_HOME".to_owned(), home)]),
        );
        options.loaded_at = Arc::new(|| "1970-01-01T00:00:00.000Z".to_owned());
        ConfigStore::new(options).unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Records every collaborator call as one line, in order.
#[derive(Default)]
struct Log(Mutex<Vec<String>>);
impl Log {
    fn push(&self, line: String) {
        lock(&self.0).push(line);
    }
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *lock(&self.0))
    }
}
fn utf8(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

struct Fakes(Arc<Log>);
impl RefreshRouter for Fakes {
    fn mark_all_dirty(&self, reason: &str) -> usize {
        self.0.push(format!("markAllDirty:{reason}"));
        0
    }
    fn mark_project_dirty(&self, key: &JsString, reason: &str) -> usize {
        self.0
            .push(format!("markProjectDirty:{}:{reason}", utf8(key)));
        0
    }
    fn cached_session_count(&self) -> usize {
        0
    }
    fn snapshot_session(&self, _: &JsString) -> Option<JsValue> {
        None
    }
}
impl NotificationServer for Fakes {
    fn broadcast_notification(&self, name: &str, payload: &JsValue) {
        let paths = match payload.get("changedPaths") {
            Some(JsValue::Array(items)) => items
                .iter()
                .map(|item| utf8(item.as_string().unwrap()))
                .collect::<Vec<_>>()
                .join(","),
            _ => unreachable!(),
        };
        self.0.push(format!("broadcast:{name}:{paths}"));
    }
}
impl SdkConfigChange for Fakes {
    fn dispatch_sdk_config_change(&self, change: &ConfigChangeNotice) {
        let paths: Vec<_> = change.changed_paths.iter().map(utf8).collect();
        self.0.push(format!("sdk:{}", paths.join(",")));
    }
}
impl MemoryMaintenance for Fakes {
    fn schedule(&self, key: &JsString) {
        self.0.push(format!("schedule:{}", utf8(key)));
    }
}
impl RefreshLog for Fakes {
    fn log(&self, message: &JsString) {
        self.0.push(format!("log:{}", utf8(message)));
    }
    fn warn(&self, message: &str, detail: WarnDetail<'_>) {
        let detail = match detail {
            WarnDetail::Text(text) => utf8(text),
            WarnDetail::Error(error) => error.message.clone(),
        };
        self.0.push(format!("warn:{message}:{detail}"));
    }
}

/// A factory whose N-th stage (1-based) can fail.
#[derive(Default)]
struct Factory {
    log: Arc<Log>,
    calls: AtomicUsize,
    fail_at: Mutex<Vec<usize>>,
    staged: Mutex<Option<std::sync::mpsc::Sender<usize>>>,
}
struct Group;
impl RuntimeResources for Group {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
impl ProjectRuntimeFactory for Factory {
    fn stage(&self, _: &Path, resources: &mut ResourceBundle) -> RuntimeResult {
        let id = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        resources.add(Group);
        self.log.push(format!("stage:{id}"));
        if let Some(sender) = lock(&self.staged).as_ref() {
            let _ = sender.send(id);
        }
        if lock(&self.fail_at).contains(&id) {
            Err(RuntimeError::new(format!("stage failed:{id}")))
        } else {
            Ok(())
        }
    }
}

struct Harness {
    log: Arc<Log>,
    factory: Arc<Factory>,
    registry: Arc<ProjectRuntimeRegistry>,
    bundle: RefreshBundle,
    _scratch: Scratch,
    store: ConfigStore,
}

fn harness() -> Harness {
    harness_with(|log| Arc::new(Fakes(log)))
}

fn harness_with(router: impl FnOnce(Arc<Log>) -> Arc<dyn RefreshRouter>) -> Harness {
    let scratch = Scratch::new();
    scratch.write(BASE);
    let store = scratch.store();
    let log = Arc::new(Log::default());
    let factory = Arc::new(Factory {
        log: Arc::clone(&log),
        ..Factory::default()
    });
    let registry = Arc::new(ProjectRuntimeRegistry::new(
        scratch.0.clone(),
        Arc::clone(&factory) as Arc<dyn ProjectRuntimeFactory>,
    ));
    registry.resolve(None).unwrap();
    assert_eq!(log.take(), ["stage:1"]);
    let fakes = Arc::new(Fakes(Arc::clone(&log)));
    let router = router(Arc::clone(&log));
    let bundle = RefreshBundle::new(RefreshOptions {
        config_store: Arc::new(store.clone()),
        registry: Arc::new(RegistryRefresh::new(Arc::clone(&registry))),
        memory_maintenance: fakes.clone(),
        router: Arc::new(move || Some(Arc::clone(&router))),
        project_root: "/root".into(),
        memory_diagnostics: None,
        sdk: Some(fakes.clone()),
        log: fakes.clone(),
    });
    bundle.attach();
    bundle.bind_server(fakes);
    Harness {
        log,
        factory,
        registry,
        bundle,
        _scratch: scratch,
        store,
    }
}

/// Yields until the spawned settlement tasks are idle and `count` lines were
/// logged (no real sleeps), then checks nothing else arrives.
async fn lines(log: &Log, count: usize) -> Vec<String> {
    // A hang guard only; no ordering depends on time.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while lock(&log.0).len() < count {
        assert!(
            std::time::Instant::now() < deadline,
            "expected {count} lines, got {:?}",
            log.take()
        );
        tokio::task::yield_now().await;
    }
    for _ in 0..200 {
        tokio::task::yield_now().await;
    }
    log.take()
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

const REFRESH: &str = "log:[pilotdeck] Config reloaded, refreshing runtimes: ";

/// Product path: change → log → SDK → registry stage → markAllDirty →
/// broadcast; coalesced RPC reloads publish one event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn config_change_drives_registry_then_router_then_broadcast() {
    let h = harness();
    h._scratch.write(&other_model());
    let (a, b) = tokio::join!(h.bundle.reload_config(), h.bundle.reload_config());
    let paths = vec![
        JsString::from("agent.model.id"),
        JsString::from("agent.model.model"),
    ];
    assert_eq!(a.unwrap().changed_paths, paths);
    assert_eq!(b.unwrap().changed_paths, paths);
    assert_eq!(
        lines(&h.log, 5).await,
        strings(&[
            &format!("{REFRESH}agent.model.id, agent.model.model"),
            "sdk:agent.model.id,agent.model.model",
            "stage:2",
            "markAllDirty:config_changed",
            "broadcast:config_changed:agent.model.id,agent.model.model",
        ])
    );
}

/// Two changes whose first runtime reload fails. On a current-thread
/// runtime the whole sequence is deterministic: both listeners run
/// synchronously, then the first reload rejects (warn, previous runtimes
/// kept) before the second one is staged and broadcast.
#[tokio::test(flavor = "current_thread")]
async fn concurrent_changes_with_failing_first_reload_keep_change_order() {
    let h = harness();
    lock(&h.factory.fail_at).push(2);
    h._scratch.write(&other_model());
    h.bundle.reload_config().await.unwrap();
    h._scratch.write(BASE);
    h.bundle.reload_config().await.unwrap();
    let active = h.registry.resolve(None).unwrap();
    assert_eq!(
        lines(&h.log, 9).await,
        strings(&[
            &format!("{REFRESH}agent.model.id, agent.model.model"),
            "sdk:agent.model.id,agent.model.model",
            &format!("{REFRESH}agent.model.id, agent.model.model"),
            "sdk:agent.model.id,agent.model.model",
            "stage:2",
            "warn:[pilotdeck] Config runtime refresh rejected; keeping previous runtimes::stage failed:2",
            "stage:3",
            "markAllDirty:config_changed",
            "broadcast:config_changed:agent.model.id,agent.model.model",
        ])
    );
    // The failed reload kept generation 1; only the second one replaced it.
    assert_ne!(active.state(), RuntimeState::Active);
    assert_eq!(h.factory.calls.load(Ordering::SeqCst), 3);
    assert!(!Arc::ptr_eq(&active, &h.registry.resolve(None).unwrap()));
    assert!(
        h.log.take().is_empty(),
        "resolving the replacement stages nothing"
    );
}

/// The same race on worker threads, repeated: the listener-side calls and the
/// settlement-side calls are each in change order (their interleaving is
/// genuinely concurrent, as in TS), and the second reload is staged only
/// after the first reload's settlement callbacks ran.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_changes_settle_in_change_order_on_worker_threads() {
    for round in 0..20 {
        let h = harness();
        if round % 2 == 0 {
            lock(&h.factory.fail_at).push(2);
        }
        // Two published changes; their runtime reloads are pending together
        // and settle on worker threads.
        h._scratch.write(&other_model());
        h.bundle.reload_config().await.unwrap();
        h._scratch.write(&format!(
            "{BASE}runPolicy: {{failureGuard: {{enabled: true}}}}\ngateway: {{port: 1}}\n"
        ));
        h.bundle.reload_config().await.unwrap();
        let all = lines(&h.log, 9 + usize::from(round % 2 == 1)).await;
        let (listener, settled): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|line| line.starts_with("log:") || line.starts_with("sdk:"));
        assert_eq!(listener.len(), 4, "round {round}: {listener:?}");
        assert!(
            listener[0].ends_with("agent.model.id, agent.model.model"),
            "round {round}: {listener:?}"
        );
        assert!(
            listener[2].ends_with("agent.model.id, agent.model.model, gateway, runPolicy"),
            "round {round}: {listener:?}"
        );
        let mut expected = vec!["stage:2".to_owned()];
        if round % 2 == 0 {
            expected.push("warn:[pilotdeck] Config runtime refresh rejected; keeping previous runtimes::stage failed:2".into());
        } else {
            expected.push("markAllDirty:config_changed".into());
            expected.push("broadcast:config_changed:agent.model.id,agent.model.model".into());
        }
        expected.push("stage:3".into());
        expected.push("markAllDirty:config_changed".into());
        let tail = settled.last().cloned().unwrap();
        assert!(
            tail.starts_with("broadcast:config_changed:"),
            "round {round}: {settled:?}"
        );
        assert_eq!(settled[..settled.len() - 1], expected[..], "round {round}");
    }
}

/// `RegistryRefresh` orders reloads by call, not by first poll; a dropped,
/// never-polled reload releases its turn. Negative control: the bare
/// registry stages as soon as any reload future is polled.
#[tokio::test(flavor = "current_thread")]
async fn registry_refresh_queues_reloads_in_call_order() {
    let h = harness();
    let refresh = RegistryRefresh::new(Arc::clone(&h.registry));
    let first = refresh.reload().unwrap();
    let second = tokio::spawn(refresh.reload().unwrap());
    let third = refresh.reload().unwrap();
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(
        h.log.take().is_empty(),
        "the second reload must wait for the first"
    );
    drop(first);
    second.await.unwrap().unwrap();
    assert_eq!(h.log.take(), ["stage:2"]);
    third.await.unwrap();
    assert_eq!(h.log.take(), ["stage:3"]);

    // Negative control: without the ticket queue, polling a later-created
    // reload runs it while an earlier one is still pending.
    let earlier = h.registry.reload();
    let later = tokio::spawn({
        let registry = Arc::clone(&h.registry);
        async move { registry.reload().await }
    });
    later.await.unwrap().unwrap();
    assert_eq!(h.log.take(), ["stage:4"]);
    drop(earlier);
}

/// A disposed registry refuses synchronously (the TS `assertActive` throw
/// that the store swallows): nothing follows the SDK callback.
#[tokio::test(flavor = "current_thread")]
async fn disposed_registry_ends_handling_after_sdk_callback() {
    let h = harness();
    h.registry.dispose().await.unwrap();
    h._scratch.write(&other_model());
    h.bundle.reload_config().await.unwrap();
    assert_eq!(
        lines(&h.log, 2).await,
        strings(&[
            &format!("{REFRESH}agent.model.id, agent.model.model"),
            "sdk:agent.model.id,agent.model.model",
        ])
    );
}

/// run-policy-only and restart-required changes from the product store never
/// reload runtimes; a failed config reload publishes nothing and reaches
/// every coalesced caller; dispose stops handling.
#[tokio::test(flavor = "current_thread")]
async fn store_branches_without_runtime_reload() {
    let h = harness();
    h._scratch.write(&format!(
        "{BASE}runPolicy: {{failureGuard: {{enabled: true}}}}\n"
    ));
    h.bundle.reload_config().await.unwrap();
    assert_eq!(
        lines(&h.log, 3).await,
        strings(&[
            "log:[pilotdeck] Config reloaded (runPolicy applies next turn): runPolicy",
            "sdk:runPolicy",
            "broadcast:config_changed:runPolicy",
        ])
    );
    h._scratch.write(&format!(
        "{BASE}runPolicy: {{failureGuard: {{enabled: true}}}}\nmodules: {{a: 1}}\n"
    ));
    h.bundle.refresh_config_before_turn().await.unwrap();
    assert_eq!(
        lines(&h.log, 1).await,
        strings(&["warn:[pilotdeck] Config change requires process restart::modules"])
    );
    h._scratch
        .write(&BASE.replace("schemaVersion: 1", "schemaVersion: 2"));
    let (a, b) = tokio::join!(h.bundle.reload_config(), h.bundle.reload_config());
    let (a, b) = (a.unwrap_err(), b.unwrap_err());
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(a.code, "CONFIG_SCHEMA_VERSION_UNSUPPORTED");
    assert!(lines(&h.log, 0).await.is_empty());
    h.bundle.dispose();
    h._scratch.write(&other_model());
    h.store.reload("manual").await.unwrap();
    assert!(lines(&h.log, 0).await.is_empty());
}

/// `reloadExtensions` keys: exact registry key retires that runtime, an
/// empty key is the all-runtimes branch, a lone-surrogate key misses.
#[tokio::test(flavor = "current_thread")]
async fn reload_extensions_invalidates_by_exact_key() {
    let h = harness();
    let root = h._scratch.0.clone();
    let runtime = h.registry.resolve(None).unwrap();
    let lone = JsString::from_utf16(vec![u16::from(b'/'), 0xd800]);
    let outcome = h.bundle.reload_extensions(ExtensionsReload {
        project_key: Some(lone.clone()),
        changed_paths: None,
    });
    assert_eq!(
        outcome,
        ReloadOutcome {
            reloaded: true,
            changed_paths: vec![]
        }
    );
    assert_eq!(runtime.state(), RuntimeState::Active);
    let logged = h.log.take();
    assert_eq!(logged.len(), 3);
    assert_eq!(
        logged[0],
        "log:[pilotdeck] Extensions reload requested for project /\u{fffd}: (manual)"
    );
    h.bundle.reload_extensions(ExtensionsReload {
        project_key: Some(root.to_string_lossy().as_ref().into()),
        changed_paths: Some(vec!["skills/a.md".into()]),
    });
    assert_eq!(runtime.state(), RuntimeState::Retired);
    let fresh = h.registry.resolve(None).unwrap();
    h.bundle.reload_extensions(ExtensionsReload {
        project_key: Some("".into()),
        changed_paths: None,
    });
    assert_eq!(fresh.state(), RuntimeState::Retired);
    let logged = h.log.take();
    assert_eq!(logged.last().unwrap(), "broadcast:config_changed:");
    assert!(logged.contains(&"markAllDirty:extension_changed".to_owned()));
}

/// Blocks inside the first `markAllDirty` until the next stage is observed
/// or a bound passes, and records which happened.
struct GatedRouter {
    inner: Fakes,
    staged: Mutex<Option<std::sync::mpsc::Receiver<usize>>>,
    overtaken: Arc<Mutex<Option<usize>>>,
}
impl RefreshRouter for GatedRouter {
    fn mark_all_dirty(&self, reason: &str) -> usize {
        if let Some(staged) = lock(&self.staged).take() {
            // The bound only limits how long a broken queue gets to overtake;
            // a correct queue cannot stage while this callback runs.
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
            while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
                match staged.recv_timeout(left) {
                    // This change's own stage; only a later one overtakes.
                    Ok(1 | 2) => continue,
                    Ok(id) => {
                        *lock(&self.overtaken) = Some(id);
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
        self.inner.mark_all_dirty(reason)
    }
    fn mark_project_dirty(&self, key: &JsString, reason: &str) -> usize {
        self.inner.mark_project_dirty(key, reason)
    }
    fn cached_session_count(&self) -> usize {
        0
    }
    fn snapshot_session(&self, _: &JsString) -> Option<JsValue> {
        None
    }
}

/// TS starts the next queued registry reload only after the previous
/// reload's promise reactions (markAllDirty, broadcast) ran. With both
/// settlement tasks on worker threads, the second stage must not run while
/// the first change's `markAllDirty` is still executing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn next_registry_reload_waits_for_previous_settlement_callbacks() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let overtaken = Arc::new(Mutex::new(None));
    let seen = Arc::clone(&overtaken);
    let h = harness_with(move |log| {
        Arc::new(GatedRouter {
            inner: Fakes(log),
            staged: Mutex::new(Some(receiver)),
            overtaken: seen,
        })
    });
    *lock(&h.factory.staged) = Some(sender);
    h._scratch.write(&other_model());
    h.bundle.reload_config().await.unwrap();
    h._scratch.write(&format!(
        "{BASE}runPolicy: {{failureGuard: {{enabled: true}}}}\ngateway: {{port: 1}}\n"
    ));
    h.bundle.reload_config().await.unwrap();
    let all = lines(&h.log, 10).await;
    assert_eq!(
        *lock(&overtaken),
        None,
        "stage ran during markAllDirty: {all:?}"
    );
    let settled: Vec<_> = all
        .into_iter()
        .filter(|line| !line.starts_with("log:") && !line.starts_with("sdk:"))
        .collect();
    assert_eq!(
        settled,
        strings(&[
            "stage:2",
            "markAllDirty:config_changed",
            "broadcast:config_changed:agent.model.id,agent.model.model",
            "stage:3",
            "markAllDirty:config_changed",
            "broadcast:config_changed:agent.model.id,agent.model.model,gateway,runPolicy",
        ])
    );
}

/// GLM P2-1: dropping the handle without `dispose` keeps handling changes
/// (the TS closure holds the bundle); `dispose` releases the bundle.
#[tokio::test(flavor = "current_thread")]
async fn dropped_bundle_keeps_handling_until_disposed() {
    let h = harness();
    let Harness {
        log,
        registry: _registry,
        bundle,
        _scratch: scratch,
        store,
        ..
    } = h;
    let weak = Arc::downgrade(&bundle.inner);
    drop(bundle);
    scratch.write(&other_model());
    store.reload("watch").await.unwrap();
    assert_eq!(
        lines(&log, 5).await,
        strings(&[
            &format!("{REFRESH}agent.model.id, agent.model.model"),
            "sdk:agent.model.id,agent.model.model",
            "stage:2",
            "markAllDirty:config_changed",
            "broadcast:config_changed:agent.model.id,agent.model.model",
        ])
    );
    // Still reachable only through the store's listener: dispose it.
    let bundle = RefreshBundle {
        inner: weak.upgrade().expect("subscription keeps the bundle"),
    };
    bundle.dispose();
    drop(bundle);
    assert!(weak.upgrade().is_none(), "dispose released the bundle");
    scratch.write(BASE);
    store.reload("watch").await.unwrap();
    assert!(lines(&log, 0).await.is_empty());
}
