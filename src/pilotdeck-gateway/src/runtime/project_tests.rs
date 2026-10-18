use super::*;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::Semaphore;

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("pdrs-project-runtime-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[derive(Default)]
struct Probe {
    events: Mutex<Vec<String>>,
    next: AtomicUsize,
    fail_at: AtomicUsize,
    fail_cleanup: Mutex<Vec<usize>>,
    gates: Mutex<HashMap<usize, Arc<Semaphore>>>,
}
impl Probe {
    fn event(&self, kind: &str, id: usize) {
        self.events.lock().unwrap().push(format!("{kind}:{id}"));
    }
    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }
    fn gate(&self, id: usize) -> Arc<Semaphore> {
        let gate = Arc::new(Semaphore::new(0));
        self.gates.lock().unwrap().insert(id, Arc::clone(&gate));
        gate
    }
    fn stage(self: &Arc<Self>, _: &Path, resources: &mut ResourceBundle) -> RuntimeResult {
        let id = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        resources.add(FakeResource {
            id,
            probe: Arc::clone(self),
        });
        self.event("stage", id);
        if self.fail_at.load(Ordering::SeqCst) == id {
            Err(RuntimeError::new(format!("stage failed:{id}")))
        } else {
            Ok(())
        }
    }
    fn registry(self: &Arc<Self>, root: &Path) -> Arc<ProjectRuntimeRegistry> {
        let probe = Arc::clone(self);
        Arc::new(ProjectRuntimeRegistry::new(
            root,
            Arc::new(move |root: &Path, resources: &mut ResourceBundle| {
                probe.stage(root, resources)
            }),
        ))
    }
}
struct FakeResource {
    id: usize,
    probe: Arc<Probe>,
}
impl RuntimeResources for FakeResource {
    fn shutdown(&self) {
        self.probe.event("shutdown", self.id);
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async move {
            self.probe.event("dispose", self.id);
            let gate = self.probe.gates.lock().unwrap().get(&self.id).cloned();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            if self.probe.fail_cleanup.lock().unwrap().contains(&self.id) {
                Err(RuntimeError::new(format!("dispose failed:{}", self.id)))
            } else {
                Ok(())
            }
        })
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("lifecycle operation did not progress");
}
async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("lifecycle operation hung")
}
fn error_json(error: &RuntimeError) -> Value {
    json!({"message":error.message, "causes":error.causes.iter().map(error_json).collect::<Vec<_>>()})
}
fn id(runtime: &ProjectRuntime) -> usize {
    runtime.resources().get::<FakeResource>().unwrap().id
}

#[tokio::test]
async fn real_ts_lifecycle_oracle() {
    let text = include_str!("../../tests/fixtures/project-runtime/oracle.json");
    crate::test_baseline::oracle(text);
    let oracle: Value = serde_json::from_str(text).unwrap();
    let cases = oracle["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 9);
    let mut replayed = 0;
    for case in cases {
        let scratch = Scratch::new();
        let probe = Arc::new(Probe::default());
        let registry = probe.registry(&scratch.0.join("fallback"));
        let mut runtimes: HashMap<String, Arc<ProjectRuntime>> = HashMap::new();
        let mut leases: HashMap<String, RuntimeLease> = HashMap::new();
        for step in case["steps"].as_array().unwrap() {
            let input = &step["input"];
            probe.fail_at.store(
                input["failAt"].as_u64().unwrap_or(0) as usize,
                Ordering::SeqCst,
            );
            *probe.fail_cleanup.lock().unwrap() = input["failCleanup"]
                .as_array()
                .map(|a| a.iter().map(|n| n.as_u64().unwrap() as usize).collect())
                .unwrap_or_default();
            let root = input["root"].as_str().map(|key| {
                if key.is_empty() || key == "relative-a" {
                    PathBuf::from(key)
                } else {
                    scratch.0.join(key)
                }
            });
            let mut resolved_id = Value::Null;
            let outcome = match input["op"].as_str().unwrap() {
                "resolve" => registry.resolve(root.as_deref()).map(|runtime| {
                    resolved_id = json!(id(&runtime));
                    if let Some(label) = input["as"].as_str() {
                        runtimes.insert(label.to_owned(), runtime);
                    }
                }),
                "acquire" => runtimes[input["runtime"].as_str().unwrap()]
                    .acquire()
                    .map(|lease| {
                        leases.insert(input["as"].as_str().unwrap().to_owned(), lease);
                    }),
                "release" => {
                    bounded(
                        leases
                            .get_mut(input["lease"].as_str().unwrap())
                            .unwrap()
                            .release(),
                    )
                    .await
                }
                "invalidate" => {
                    registry.invalidate(root.as_deref());
                    Ok(())
                }
                "reload" => bounded(registry.reload()).await,
                "dispose" => bounded(registry.dispose()).await,
                operation => panic!("unhandled oracle operation {operation}"),
            };
            let expected_events = step["events"].as_array().unwrap().len();
            until(|| probe.events().len() >= expected_events).await;
            for (label, runtime) in &runtimes {
                if step["runtimes"][label]["state"] == "disposed" {
                    let _ = bounded(runtime.wait_disposed()).await;
                }
            }
            let actual = json!({
                "input":input, "id":resolved_id, "error":outcome.as_ref().err().map(error_json), "events":probe.events(),
                "runtimes":runtimes.iter().map(|(label,runtime)| (label.clone(), json!({"id":id(runtime),"state":runtime.state().as_str(),"leases":runtime.lease_count()}))).collect::<serde_json::Map<String,Value>>()
            });
            assert_eq!(actual, *step, "{} step {replayed}", case["name"]);
            replayed += 1;
        }
    }
    assert_eq!(replayed, 59, "all recorded steps must be replayed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_acquire_and_dispose_drain_invalidated_generation_once() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let registry = probe.registry(&scratch.0);
    let barrier = Arc::new(tokio::sync::Barrier::new(33));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let registry = Arc::clone(&registry);
        let barrier = Arc::clone(&barrier);
        tasks.spawn(async move {
            barrier.wait().await;
            registry.acquire(None).unwrap()
        });
    }
    barrier.wait().await;
    let mut leases = Vec::new();
    while let Some(lease) = tasks.join_next().await {
        leases.push(lease.unwrap());
    }
    let old = Arc::clone(leases[0].runtime());
    assert_eq!(old.lease_count(), 32);
    assert_eq!(probe.next.load(Ordering::SeqCst), 1);
    registry.invalidate(None);
    let new = registry.resolve(None).unwrap();
    let disposing = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.dispose().await }
    });
    until(|| new.state() == RuntimeState::Disposed).await;
    assert!(
        !disposing.is_finished(),
        "dispose must include the invalidated, still leased generation"
    );
    assert!(old.acquire().is_err());
    assert!(registry.acquire(None).is_err());
    drop(leases);
    bounded(disposing).await.unwrap().unwrap();
    assert_eq!(old.state(), RuntimeState::Disposed);
    assert_eq!(
        probe
            .events()
            .iter()
            .filter(|e| e.as_str() == "dispose:1")
            .count(),
        1
    );
    bounded(registry.dispose()).await.unwrap();
}

#[tokio::test]
async fn rollback_cancellation_keeps_reload_serial_and_dispose_closes_queued_reload() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let registry = probe.registry(&scratch.0);
    let old = registry.resolve(None).unwrap();
    probe.fail_at.store(2, Ordering::SeqCst);
    let gate = probe.gate(2);
    let reload = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.reload().await }
    });
    until(|| probe.events().contains(&"dispose:2".into())).await;
    reload.abort();
    assert!(reload.await.unwrap_err().is_cancelled());
    assert!(Arc::ptr_eq(&old, &registry.resolve(None).unwrap()));
    assert!(
        registry.reload_gate.try_lock().is_err(),
        "rollback retains the serial lock after cancellation"
    );
    let queued = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.reload().await }
    });
    let dispose = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.dispose().await }
    });
    until(|| registry.state.lock().unwrap().disposed).await;
    assert!(!dispose.is_finished());
    assert!(
        !probe.events().contains(&"shutdown:1".into()),
        "dispose waits for rollback before cleaning published resources"
    );
    gate.add_permits(1);
    bounded(dispose).await.unwrap().unwrap();
    assert_eq!(
        bounded(queued).await.unwrap().unwrap_err().message,
        "Project runtime registry is disposed."
    );
    assert_eq!(
        probe.next.load(Ordering::SeqCst),
        2,
        "closed queued reload must not stage"
    );
}

#[tokio::test]
async fn cancelled_release_and_dispose_are_idempotent_and_keep_cleanup_alive() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let registry = probe.registry(&scratch.0);
    let mut lease = registry.acquire(None).unwrap();
    let runtime = Arc::clone(lease.runtime());
    let gate = probe.gate(1);
    registry.invalidate(None);
    let release = tokio::spawn(async move { lease.release().await });
    until(|| probe.events().contains(&"dispose:1".into())).await;
    release.abort();
    assert!(release.await.unwrap_err().is_cancelled());
    let dispose = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.dispose().await }
    });
    until(|| registry.state.lock().unwrap().disposed).await;
    dispose.abort();
    assert!(dispose.await.unwrap_err().is_cancelled());
    gate.add_permits(1);
    let (first, second) = tokio::join!(registry.dispose(), registry.dispose());
    first.unwrap();
    second.unwrap();
    assert_eq!(runtime.lease_count(), 0);
    assert_eq!(runtime.state(), RuntimeState::Disposed);
    assert_eq!(probe.events(), ["stage:1", "shutdown:1", "dispose:1"]);
}

#[tokio::test]
async fn dispose_preserves_project_order_and_aggregates_failures() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let registry = probe.registry(&scratch.0);
    let a = registry.resolve(Some(&scratch.0.join("a"))).unwrap();
    let b = registry.resolve(Some(&scratch.0.join("b"))).unwrap();
    *probe.fail_cleanup.lock().unwrap() = vec![1, 2];
    let gate = probe.gate(1);
    let dispose = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.dispose().await }
    });
    until(|| probe.events().contains(&"dispose:1".into())).await;
    assert_eq!(b.state(), RuntimeState::Retired);
    assert!(b.acquire().is_err());
    assert!(!probe.events().contains(&"shutdown:2".into()));
    gate.add_permits(1);
    let error = bounded(dispose).await.unwrap().unwrap_err();
    assert_eq!(
        error
            .causes
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>(),
        ["dispose failed:1", "dispose failed:2"]
    );
    assert_eq!(registry.dispose().await.unwrap_err(), error);
    assert_eq!(a.state(), RuntimeState::Disposed);
    assert_eq!(b.state(), RuntimeState::Disposed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn synchronous_stage_serializes_reload_with_acquire_invalidate_and_dispose() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let release_stage = Arc::new(std::sync::Barrier::new(2));
    let factory_probe = Arc::clone(&probe);
    let factory_barrier = Arc::clone(&release_stage);
    let registry = Arc::new(ProjectRuntimeRegistry::new(
        &scratch.0,
        Arc::new(move |root: &Path, resources: &mut ResourceBundle| {
            let result = factory_probe.stage(root, resources);
            if factory_probe.next.load(Ordering::SeqCst) == 2 {
                entered_tx.send(()).unwrap();
                factory_barrier.wait();
            }
            result
        }),
    ));
    let lease = registry.acquire(None).unwrap();
    let old = Arc::clone(lease.runtime());
    let handle = Handle::current();
    let reload = std::thread::spawn({
        let registry = Arc::clone(&registry);
        move || handle.block_on(registry.reload())
    });
    tokio::task::spawn_blocking(move || entered_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    assert_eq!(old.state(), RuntimeState::Active);
    // A direct lease on the old generation is safe during synchronous staging.
    let old_second = old.acquire().unwrap();
    let handle = Handle::current();
    let dispose = std::thread::spawn({
        let registry = Arc::clone(&registry);
        move || handle.block_on(registry.dispose())
    });
    let acquire = std::thread::spawn({
        let registry = Arc::clone(&registry);
        move || registry.acquire(None)
    });
    let invalidate = std::thread::spawn({
        let registry = Arc::clone(&registry);
        move || registry.invalidate(None)
    });
    release_stage.wait();
    tokio::task::spawn_blocking(move || reload.join().unwrap())
        .await
        .unwrap()
        .unwrap();
    // Either admission won before dispose or it was rejected; neither path
    // can lease a retired generation or lose a generation from the drain.
    let concurrent_lease = tokio::task::spawn_blocking(move || acquire.join().unwrap())
        .await
        .unwrap();
    drop(concurrent_lease);
    drop(lease);
    drop(old_second);
    tokio::task::spawn_blocking(move || invalidate.join().unwrap())
        .await
        .unwrap();
    bounded(tokio::task::spawn_blocking(move || dispose.join().unwrap()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.state(), RuntimeState::Disposed);
    assert!(registry.resolve(None).is_err());
    let built = probe.next.load(Ordering::SeqCst);
    let events = probe.events();
    for id in 1..=built {
        assert_eq!(
            events
                .iter()
                .filter(|event| *event == &format!("dispose:{id}"))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn resource_group_panic_does_not_hang_or_skip_other_groups() {
    struct Panicking;
    impl RuntimeResources for Panicking {
        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async { panic!("injected cleanup panic") })
        }
    }
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let factory_probe = Arc::clone(&probe);
    let registry = ProjectRuntimeRegistry::new(
        &scratch.0,
        Arc::new(move |root: &Path, resources: &mut ResourceBundle| {
            resources.add(Panicking);
            factory_probe.stage(root, resources)
        }),
    );
    registry.resolve(None).unwrap();
    let error = bounded(registry.dispose()).await.unwrap_err();
    assert!(error.message.contains("cleanup task failed"));
    assert_eq!(probe.events(), ["stage:1", "shutdown:1", "dispose:1"]);
}

#[tokio::test]
async fn invalidate_uses_raw_key_and_completed_retired_failure_is_not_replayed() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let registry = probe.registry(&scratch.0);
    let runtime = registry.resolve(None).unwrap();
    registry.invalidate(Some(&scratch.0.join(".")));
    assert_eq!(runtime.state(), RuntimeState::Active);
    *probe.fail_cleanup.lock().unwrap() = vec![1];
    registry.invalidate(None);
    assert_eq!(
        bounded(runtime.wait_disposed()).await.unwrap_err().message,
        "dispose failed:1"
    );
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn reload_rejects_during_dispose_without_waiting_for_its_callers_lease() {
    let scratch = Scratch::new();
    let probe = Arc::new(Probe::default());
    let registry = probe.registry(&scratch.0);
    let lease = registry.acquire(None).unwrap();
    let dispose = tokio::spawn({
        let registry = Arc::clone(&registry);
        async move { registry.dispose().await }
    });
    until(|| registry.state.lock().unwrap().disposed).await;
    assert_eq!(
        bounded(registry.reload()).await.unwrap_err().message,
        "Project runtime registry is disposed."
    );
    assert!(!dispose.is_finished());
    drop(lease);
    bounded(dispose).await.unwrap().unwrap();
}
