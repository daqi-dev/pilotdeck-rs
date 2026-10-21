use super::super::{HandleState, LiveSession};
use super::*;
use crate::runtime::{
    project::{
        DisposeFuture, ProjectRuntimeRegistry, ResourceBundle, RuntimeError, RuntimeResources,
        RuntimeResult,
    },
    refresh::{BoxFuture, RefreshRouter},
};
use pilotdeck_protocol::{JsValue, parse_json};
use serde_json::{Value, json};
use std::sync::Arc;
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicI64, AtomicUsize, Ordering},
    time::Duration,
};
use tokio::sync::watch;

const ORACLE: &str = include_str!("../../../tests/fixtures/session-owner/oracle.json");

#[derive(Clone)]
struct Gate(watch::Sender<bool>);
impl Gate {
    fn new() -> Self {
        Self(watch::channel(false).0)
    }
    fn open(&self) {
        self.0.send_replace(true);
    }
    async fn wait(&self) {
        let _ = self.0.subscribe().wait_for(|open| *open).await;
    }
}

#[derive(Default)]
struct ProbeState {
    events: Mutex<Vec<String>>,
    next: AtomicUsize,
    now: AtomicI64,
    creates: Mutex<HashMap<JsString, Gate>>,
    disposals: Mutex<HashMap<usize, Gate>>,
    setup_failures: Mutex<HashSet<usize>>,
    dispose_failures: Mutex<HashSet<usize>>,
    factory_failures: Mutex<HashSet<JsString>>,
    abort_failures: Mutex<HashSet<usize>>,
}
#[derive(Clone, Default)]
struct Probe(Arc<ProbeState>);
impl Probe {
    fn event(&self, event: impl Into<String>) {
        self.0.events.lock().unwrap().push(event.into());
    }
    fn events(&self) -> Vec<String> {
        self.0.events.lock().unwrap().clone()
    }
    fn seen(&self, event: &str) -> bool {
        self.0
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|item| item == event)
    }
    fn owner(&self) -> SessionOwner {
        let probe = self.clone();
        let evict = self.clone();
        SessionOwner::new(SessionOwnerOptions {
            factory: Arc::new(self.clone()),
            now_ms: Arc::new(move || probe.0.now.load(Ordering::SeqCst)),
            idle_timeout_ms: 100,
            idle_sweep_interval_ms: 0,
            on_evict: Some(Arc::new(move |key, reason| {
                let reason = match reason {
                    EvictionReason::Idle => "idle",
                    EvictionReason::Closed => "closed",
                    EvictionReason::DirtyRecreate => "dirty_recreate",
                    EvictionReason::Shutdown => "shutdown",
                };
                evict.event(format!("evict:{}:{reason}", key.to_utf8().unwrap()));
            })),
        })
    }
    fn time(&self, time: i64) {
        self.0.now.store(time, Ordering::SeqCst);
    }
}
struct FakeSession {
    id: usize,
    probe: Probe,
}
impl LiveSession for FakeSession {
    fn snapshot(&self) -> JsValue {
        parse_json(&format!(
            "{{\"sessionId\":\"session-{}\",\"messages\":[]}}",
            self.id
        ))
        .unwrap()
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        self.probe
            .event(format!("abort:{}:{}", self.id, reason.to_utf8().unwrap()));
        if self
            .probe
            .0
            .abort_failures
            .lock()
            .unwrap()
            .contains(&self.id)
        {
            Err(RuntimeError::new("abort failed"))
        } else {
            Ok(())
        }
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async move {
            self.probe.event(format!("dispose:{}", self.id));
            let gate = self
                .probe
                .0
                .disposals
                .lock()
                .unwrap()
                .get(&self.id)
                .cloned();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            if self
                .probe
                .0
                .dispose_failures
                .lock()
                .unwrap()
                .contains(&self.id)
            {
                Err(RuntimeError::new("dispose failed"))
            } else {
                Ok(())
            }
        })
    }
}
fn id(handle: &LiveSessionHandle) -> usize {
    handle
        .snapshot()
        .get("sessionId")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap()[8..]
        .parse()
        .unwrap()
}
impl SessionFactory for Probe {
    fn create(
        &self,
        context: SessionContext,
        _previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        let probe = self.clone();
        Box::pin(async move {
            let id = probe.0.next.fetch_add(1, Ordering::SeqCst) + 1;
            probe.event(format!(
                "create:{}:{id}",
                context.session_key.to_utf8().unwrap()
            ));
            let gate = probe
                .0
                .creates
                .lock()
                .unwrap()
                .get(&context.session_key)
                .cloned();
            if let Some(gate) = gate {
                gate.wait().await;
            }
            if probe
                .0
                .factory_failures
                .lock()
                .unwrap()
                .contains(&context.session_key)
            {
                return Err(RuntimeError::new("factory failed"));
            }
            Ok(LiveSessionHandle::new(
                Arc::new(FakeSession { id, probe }),
                None,
            ))
        })
    }
    fn can_recreate(&self) -> bool {
        true
    }
    fn setup(
        &self,
        _context: SessionContext,
        handle: LiveSessionHandle,
        _previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<RuntimeResult> {
        let probe = self.clone();
        Box::pin(async move {
            let id = id(&handle);
            probe.event(format!("setup:{id}"));
            if probe.0.setup_failures.lock().unwrap().contains(&id) {
                Err(RuntimeError::new("setup failed"))
            } else {
                Ok(())
            }
        })
    }
}
fn context(key: &str) -> SessionContext {
    SessionContext {
        session_key: key.into(),
        project_key: Some("project".into()),
        channel_key: "web".into(),
        allowed_tools: None,
        disallowed_tools: None,
    }
}
fn state(handle: &LiveSessionHandle) -> &'static str {
    match handle.state() {
        HandleState::Active => "active",
        HandleState::Draining => "draining",
        HandleState::Disposed => "disposed",
    }
}
async fn until(mut condition: impl FnMut() -> bool) {
    bounded(async {
        while !condition() {
            tokio::task::yield_now().await;
        }
    })
    .await;
}
async fn bounded<F: Future>(future: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("controlled lifecycle must progress independently")
}
fn count(owner: &SessionOwner) -> usize {
    owner.cached_session_count()
}
fn sweep_count(owner: &SessionOwner) -> usize {
    owner.sweep_idle();
    count(owner)
}
fn failure<T>(result: Result<T, RuntimeError>) -> String {
    match result {
        Ok(_) => panic!("expected failure"),
        Err(error) => error.message,
    }
}
fn case(name: &str) -> Value {
    crate::test_baseline::oracle(ORACLE);
    serde_json::from_str::<Value>(ORACLE).unwrap()["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap()
        .clone()
}
fn compare(name: &str, value: Value, probe: &Probe) {
    let expected = case(name);
    assert_eq!(value, expected["value"], "{name}: value");
    assert_eq!(
        json!(probe.events()),
        expected["events"],
        "{name}: resource/publication order"
    );
}

#[tokio::test]
async fn dirty_identity_and_publication_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let old = owner.get_or_create(context("a")).await.unwrap();
    let gate = Gate::new();
    probe.0.disposals.lock().unwrap().insert(1, gate.clone());
    owner.mark_all_dirty("refresh");
    let mut pending = owner.get_or_create(context("a"));
    until(|| probe.seen("evict:a:dirty_recreate")).await;
    let current = owner.current_handle(&"a".into()).unwrap();
    let during = json!({"oldCurrent": owner.is_current(&"a".into(), &old),
        "newPublished": !current.same_identity(&old), "oldState": state(&old),
        "returned": std::future::poll_fn(|cx| std::task::Poll::Ready(pending.as_mut().poll(cx).is_ready())).await,
        "cached": count(&owner)});
    gate.open();
    let fresh = pending.await.unwrap();
    compare(
        "dirty_identity_and_publication",
        json!({"during": during,
        "oldCurrent": owner.is_current(&"a".into(), &old), "freshCurrent": owner.is_current(&"a".into(), &fresh),
        "oldState": state(&old)}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn slow_factory_is_per_session_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let gate = Gate::new();
    probe
        .0
        .creates
        .lock()
        .unwrap()
        .insert("a".into(), gate.clone());
    let mut a = owner.get_or_create(context("a"));
    until(|| probe.seen("create:a:1")).await;
    bounded(owner.get_or_create(context("b"))).await.unwrap();
    let a_done =
        std::future::poll_fn(|cx| std::task::Poll::Ready(a.as_mut().poll(cx).is_ready())).await;
    let during = json!({"aDone": a_done, "cached": count(&owner)});
    gate.open();
    a.await.unwrap();
    compare(
        "slow_factory_is_per_session",
        json!({"during": during, "cached": count(&owner)}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn maintenance_blocks_turn_and_idle_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let handle = owner.get_or_create(context("a")).await.unwrap();
    let reservation = owner.begin_maintenance(&"a".into()).unwrap();
    let activity = handle.begin_maintenance().unwrap();
    probe.event("maintenance:1");
    assert!(owner.begin_maintenance(&"a".into()).is_none());
    probe.time(101);
    let during = json!({"same": owner.begin_turn(&"a".into(), &"blocked".into()),
        "other": owner.begin_turn(&"b".into(), &"run".into()), "cached": sweep_count(&owner)});
    probe.event(format!(
        "maintenance_done:1:{}",
        activity.cancellation_reason().is_some()
    ));
    drop(activity);
    drop(reservation);
    compare(
        "maintenance_blocks_turn_and_idle",
        json!({"during": during, "after": owner.begin_turn(&"a".into(), &"run".into())}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_during_creation_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let gate = Gate::new();
    probe
        .0
        .creates
        .lock()
        .unwrap()
        .insert("a".into(), gate.clone());
    let creating = owner.get_or_create(context("a"));
    until(|| probe.seen("create:a:1")).await;
    let mut shutdown = owner.shutdown();
    let during = json!({"stopped": std::future::poll_fn(|cx| std::task::Poll::Ready(shutdown.as_mut().poll(cx).is_ready())).await,
        "admitted": owner.begin_turn(&"b".into(), &"run".into()), "cached": count(&owner)});
    gate.open();
    let error = failure(creating.await);
    shutdown.await.unwrap();
    let late_error = failure(owner.get_or_create(context("b")).await);
    compare(
        "shutdown_during_creation",
        json!({"during": during, "error": error, "lateError": late_error,
        "cached": count(&owner), "registry": usize::from(owner.current_handle(&"a".into()).is_some())}),
        &probe,
    );
}

#[tokio::test]
async fn close_drains_before_recreate_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    owner.get_or_create(context("a")).await.unwrap();
    let gate = Gate::new();
    probe.0.disposals.lock().unwrap().insert(1, gate.clone());
    let closing = owner.close(&"a".into());
    until(|| probe.seen("dispose:1")).await;
    let mut creating = owner.get_or_create(context("a"));
    let during = json!({"done": std::future::poll_fn(|cx| std::task::Poll::Ready(creating.as_mut().poll(cx).is_ready())).await,
        "cached": count(&owner)});
    gate.open();
    closing.await.unwrap();
    creating.await.unwrap();
    compare(
        "close_drains_before_recreate",
        json!({"during": during, "cached": count(&owner)}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn idle_strict_boundary_and_turn_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    owner.get_or_create(context("a")).await.unwrap();
    probe.time(100);
    let boundary = sweep_count(&owner);
    assert!(owner.begin_turn(&"a".into(), &"run".into()));
    probe.time(101);
    let busy = sweep_count(&owner);
    owner.end_turn(&"a".into(), Some(&"run".into()));
    probe.time(201);
    let touched = sweep_count(&owner);
    probe.time(202);
    let evicted = sweep_count(&owner);
    until(|| probe.seen("evict:a:idle")).await;
    compare(
        "idle_strict_boundary_and_turn",
        json!({"boundary": boundary, "busy": busy, "touched": touched, "evicted": evicted}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn setup_and_factory_failures_keep_old_writer_matches_ts() {
    for setup in [true, false] {
        let probe = Probe::default();
        let owner = probe.owner();
        let old = owner.get_or_create(context("a")).await.unwrap();
        if setup {
            probe.0.setup_failures.lock().unwrap().insert(2);
            owner.mark_all_dirty("refresh");
        } else {
            probe.0.factory_failures.lock().unwrap().insert("a".into());
            owner.mark_project_dirty(&"project".into(), "refresh");
        }
        let error = failure(owner.get_or_create(context("a")).await);
        let mut value = json!({"error": error, "oldCurrent": owner.is_current(&"a".into(), &old), "oldState": state(&old)});
        if setup {
            value["cached"] = json!(count(&owner));
        }
        compare(
            if setup {
                "setup_failure_keeps_old_writer"
            } else {
                "factory_failure_keeps_old_writer"
            },
            value,
            &probe,
        );
        owner.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn tool_filters_cache_and_dirty_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let mut ctx = context("a");
    ctx.allowed_tools = Some(vec!["read".into()]);
    let first = owner.get_or_create(ctx).await.unwrap();
    let mut ctx = context("a");
    ctx.project_key = Some("other".into());
    ctx.channel_key = "".into();
    let retained = owner.get_or_create(ctx).await.unwrap();
    let mut ctx = context("a");
    ctx.allowed_tools = Some(vec!["write".into()]);
    let fresh = owner.get_or_create(ctx).await.unwrap();
    compare(
        "tool_filters_cache_and_dirty",
        json!({"retained": first.same_identity(&retained),
        "replaced": !fresh.same_identity(&first), "cached": count(&owner)}),
        &probe,
    );
    assert_eq!(
        owner.mark_project_dirty(&"other".into(), "wrong-project"),
        0
    );
    assert_eq!(
        owner.mark_project_dirty(&"project".into(), "right-project"),
        1
    );
    assert_eq!(
        owner.snapshot_session(&"a".into()),
        Some(JsValue::Array(vec![]))
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn close_failure_still_unpublishes_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let handle = owner.get_or_create(context("a")).await.unwrap();
    probe.0.dispose_failures.lock().unwrap().insert(1);
    let error = failure(owner.close(&"a".into()).await);
    compare(
        "close_failure_still_unpublishes",
        json!({"error": error, "state": state(&handle),
        "cached": count(&owner), "active": owner.has_active_turn(&"a".into())}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn handle_dispose_drains_maintenance_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let handle = owner.get_or_create(context("a")).await.unwrap();
    let activity = handle.begin_maintenance().unwrap();
    probe.event("maintenance:1");
    let mut disposal = handle.dispose("stop".into());
    let during = json!({"done": std::future::poll_fn(|cx| std::task::Poll::Ready(disposal.as_mut().poll(cx).is_ready())).await,
        "state": state(&handle)});
    let cancelled = activity.cancellation_reason().is_some();
    drop(activity);
    disposal.await.unwrap();
    compare(
        "handle_dispose_drains_maintenance",
        json!({"during": during, "cancelled": cancelled, "state": state(&handle)}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

struct ProjectResource(Arc<Mutex<Vec<String>>>);
impl RuntimeResources for ProjectResource {
    fn shutdown(&self) {
        self.0.lock().unwrap().push("project_shutdown".into());
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async {
            self.0.lock().unwrap().push("project_dispose".into());
            Ok(())
        })
    }
}
#[tokio::test]
async fn project_lease_identity_matches_ts_and_handle_holds_lease_until_drain() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let project_events = events.clone();
    let registry = ProjectRuntimeRegistry::new(
        "/tmp",
        Arc::new(move |_: &std::path::Path, resources: &mut ResourceBundle| {
            resources.add(ProjectResource(project_events.clone()));
            Ok(())
        }),
    );
    let lease = registry.acquire(None).unwrap();
    let old = lease.runtime().clone();
    let probe = Probe::default();
    let handle = LiveSessionHandle::new(
        Arc::new(FakeSession {
            id: 1,
            probe: probe.clone(),
        }),
        Some(lease),
    );
    let activity = handle.begin_activity().unwrap();
    registry.invalidate(None);
    let before = json!({"state": old.state().as_str(), "leases": old.lease_count(), "events": *events.lock().unwrap()});
    let new = registry.resolve(None).unwrap();
    assert!(!Arc::ptr_eq(&old, &new));
    assert!(old.acquire().is_err());
    let disposal = handle.dispose("stop".into());
    assert_eq!(old.lease_count(), 1);
    assert!(events.lock().unwrap().is_empty());
    assert_eq!(activity.cancellation_reason(), Some("stop".into()));
    assert!(handle.begin_activity().is_err());
    drop(activity);
    disposal.await.unwrap();
    handle.dispose("again".into()).await.unwrap();
    let value = json!({"before": before, "after": {"state": old.state().as_str(), "leases": old.lease_count(), "events": *events.lock().unwrap()}});
    assert_eq!(value, case("project_lease_identity")["value"]);
    registry.dispose().await.unwrap();
}

#[test]
#[ignore = "requires fixed TS checkout and its existing tsx loader"]
fn real_ts_owner_regenerates_identically() {
    crate::test_baseline::regenerates_identically("session-owner");
}

#[tokio::test]
async fn live_abort_with_fence_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    owner.get_or_create(context("a")).await.unwrap();
    let key = JsString::from("a");
    assert!(owner.begin_turn(&key, &"old".into()));
    let old = owner.begin_completion(&key, &"old".into());
    owner.complete_turn(&key, &old);
    owner.mark_all_dirty("refresh");
    owner.get_or_create(context("a")).await.unwrap();
    assert!(owner.begin_turn(&key, &"new".into()));
    let fresh = owner.begin_completion(&key, &"new".into());
    owner
        .abort_turn(&key, Some(&"old".into()), None)
        .await
        .unwrap();
    owner
        .abort_turn(&key, Some(&"".into()), None)
        .await
        .unwrap();
    let stale_cancelled = fresh.is_cancelled();
    let mut abort = Box::pin(owner.abort_turn(&key, Some(&"new".into()), Some(&"stop".into())));
    let during = json!({"done": std::future::poll_fn(|cx| std::task::Poll::Ready(abort.as_mut().poll(cx).is_ready())).await,
        "cancelled": fresh.is_cancelled()});
    owner.complete_turn(&key, &fresh);
    abort.await.unwrap();
    compare(
        "live_abort_with_fence",
        json!({"staleCancelled": stale_cancelled, "during": during, "done": true}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_drains_all_handles_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    let b = owner.get_or_create(context("b")).await.unwrap();
    let first = Gate::new();
    let second = Gate::new();
    probe.0.disposals.lock().unwrap().insert(1, first.clone());
    probe.0.disposals.lock().unwrap().insert(2, second.clone());
    let mut shutdown = owner.shutdown();
    let cached = count(&owner);
    until(|| probe.seen("dispose:1") && probe.seen("dispose:2")).await;
    let during = json!({"done": std::future::poll_fn(|cx| std::task::Poll::Ready(shutdown.as_mut().poll(cx).is_ready())).await,
        "cached": cached, "a": state(&a), "b": state(&b)});
    second.open();
    until(|| probe.seen("evict:b:shutdown")).await;
    let after_second =
        std::future::poll_fn(|cx| std::task::Poll::Ready(shutdown.as_mut().poll(cx).is_ready()))
            .await;
    first.open();
    shutdown.await.unwrap();
    compare(
        "shutdown_drains_all_handles",
        json!({"during": during, "afterSecond": after_second, "done": true}),
        &probe,
    );
}

/// Force the exact pr1 decision -> actual live sink window on two OS threads.
/// Both the fence-success and admitted-before-fence branches are exercised.
/// Replacement is done through the real factory/publication path while the old
/// abort is parked before selecting its handle, rather than by editing a mock.
#[test]
fn live_sink_rechecks_owner_after_abort_decision_across_threads() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let _entered = runtime.enter();
    const ROUNDS: usize = 100;
    let probe = Probe::default();
    let owner = probe.owner();
    let key = JsString::from("a");
    runtime.block_on(owner.get_or_create(context("a"))).unwrap();
    let decided = std::sync::Barrier::new(2);
    let replaced = std::sync::Barrier::new(2);
    for round in 0..ROUNDS {
        let old_run = JsString::from(format!("old-{round}"));
        let new_run = JsString::from(format!("new-{round}"));
        assert!(owner.begin_turn(&key, &old_run));
        let old_fence = (round % 2 == 0).then(|| owner.begin_completion(&key, &old_run));
        std::thread::scope(|scope| {
            let aborter = scope.spawn(|| {
                let operation =
                    owner
                        .0
                        .turns
                        .abort_turn(&key, Some(&old_run), None, |target, reason| {
                            decided.wait();
                            replaced.wait();
                            owner.abort(&key, target.as_ref(), &reason).unwrap();
                            std::future::ready(())
                        });
                drop(operation);
            });
            decided.wait();
            owner.end_turn(&key, Some(&old_run));
            if let Some(old) = old_fence {
                owner.complete_turn(&key, &old);
            }
            owner.mark_all_dirty("refresh");
            // Run the public replacement on the independent runtime workers.
            let fresh = runtime.block_on(owner.get_or_create(context("a"))).unwrap();
            assert!(owner.is_current(&key, &fresh));
            assert!(owner.begin_turn(&key, &new_run));
            replaced.wait();
            aborter.join().unwrap();
            owner.end_turn(&key, Some(&new_run));
        });
    }
    let hits = probe
        .events()
        .iter()
        .filter(|event| event.starts_with("abort:"))
        .count();
    println!("live replacement hits: {hits}/{ROUNDS}");
    assert_eq!(hits, 0, "an old abort reached the new live writer");
    runtime.block_on(owner.shutdown()).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_and_completion_cleanup_preserve_later_same_run_reservation() {
    let probe = Probe::default();
    let owner = probe.owner();
    let key = JsString::from("a");
    let run = JsString::from("same");
    owner.get_or_create(context("a")).await.unwrap();
    assert!(owner.begin_turn(&key, &run));
    let old = owner.begin_completion(&key, &run);
    let drain = Gate::new();
    probe.0.disposals.lock().unwrap().insert(1, drain.clone());
    let closing = owner.close(&key);
    until(|| probe.seen("dispose:1")).await;
    let other = owner.clone();
    let other_key = key.clone();
    let other_run = run.clone();
    std::thread::spawn(move || {
        other.end_turn(&other_key, Some(&other_run));
        assert!(other.begin_turn(&other_key, &other_run));
    })
    .join()
    .unwrap();
    let fresh = owner.begin_completion(&key, &run);
    owner.complete_turn(&key, &old);
    assert!(owner.has_active_turn(&key));
    assert!(!fresh.is_complete());
    drain.open();
    closing.await.unwrap();
    assert!(
        owner.has_active_turn(&key),
        "old close released a new reservation with the same runId"
    );
    owner.complete_turn(&key, &fresh);
    assert!(!owner.has_active_turn(&key));
    owner.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_key_call_order_and_cancelled_waiter_still_drain() {
    let probe = Probe::default();
    let owner = probe.owner();
    let gate = Gate::new();
    probe
        .0
        .creates
        .lock()
        .unwrap()
        .insert("a".into(), gate.clone());
    let creating = owner.get_or_create(context("a"));
    until(|| probe.seen("create:a:1")).await;
    let closing = owner.close(&"a".into());
    // Neither future needs to be polled/retained for its owned work to finish.
    drop(creating);
    drop(closing);
    let shutdown = owner.shutdown();
    gate.open();
    bounded(shutdown).await.unwrap();
    assert_eq!(count(&owner), 0);
    assert_eq!(
        probe
            .events()
            .iter()
            .filter(|event| event.starts_with("dispose:"))
            .count(),
        1
    );
}

#[tokio::test]
async fn project_close_during_creation_is_local_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let gate = Gate::new();
    probe
        .0
        .creates
        .lock()
        .unwrap()
        .insert("a".into(), gate.clone());
    let creating = owner.get_or_create(context("a"));
    until(|| probe.seen("create:a:1")).await;
    let closing = owner.close_project(&"project".into());
    let blocked = failure(owner.get_or_create(context("blocked")).await);
    let mut other = context("b");
    other.project_key = Some("other".into());
    bounded(owner.get_or_create(other)).await.unwrap();
    let during = count(&owner);
    gate.open();
    let error = failure(creating.await);
    let keys = closing.await.unwrap();
    owner.resume_project(&"project".into());
    owner.get_or_create(context("a")).await.unwrap();
    compare(
        "project_close_during_creation_is_local",
        json!({"blocked": blocked, "during": during,
        "error": error, "keys": keys.iter().map(|key| key.to_utf8().unwrap()).collect::<Vec<_>>(), "cached": count(&owner)}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn periodic_idle_sweep_uses_the_same_eviction_and_stops_with_owner() {
    let probe = Probe::default();
    let mut options = SessionOwnerOptions::new(Arc::new(probe.clone()));
    let clock = probe.clone();
    options.now_ms = Arc::new(move || clock.0.now.load(Ordering::SeqCst));
    options.idle_timeout_ms = 100;
    options.idle_sweep_interval_ms = 10;
    let owner = SessionOwner::new(options);
    owner.get_or_create(context("a")).await.unwrap();
    probe.time(101);
    tokio::time::advance(Duration::from_millis(10)).await;
    until(|| probe.seen("dispose:1")).await;
    assert_eq!(
        count(&owner),
        case("idle_strict_boundary_and_turn")["value"]["evicted"]
            .as_u64()
            .unwrap() as usize
    );
    owner.shutdown().await.unwrap();
    let events = probe.events();
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    assert_eq!(probe.events(), events);
}

#[tokio::test]
async fn abort_failure_does_not_wait_for_completion_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    owner.get_or_create(context("a")).await.unwrap();
    let key = JsString::from("a");
    let run = JsString::from("run");
    assert!(owner.begin_turn(&key, &run));
    probe.0.abort_failures.lock().unwrap().insert(1);
    let handle = owner.begin_completion(&key, &run);
    let error = failure(bounded(owner.abort_turn(&key, Some(&run), Some(&"stop".into()))).await);
    compare(
        "abort_failure_does_not_wait_for_completion",
        json!({"error": error,
        "cancelled": handle.is_cancelled(), "current": owner.0.turns.completion.is_current(&key, &handle)}),
        &probe,
    );
    assert!(!handle.is_complete());
    owner.complete_turn(&key, &handle);
    owner.shutdown().await.unwrap();
}

#[test]
fn publication_cutoff_revokes_cached_handle_before_async_disposal_starts() {
    let probe = Probe::default();
    let handle = LiveSessionHandle::new(
        Arc::new(FakeSession {
            id: 1,
            probe: probe.clone(),
        }),
        None,
    );
    // This part of publication is safe under the owner lock: no callbacks or
    // resource disposal run until the caller starts dispose after unlocking.
    handle.stop_new();
    assert_eq!(
        state(&handle),
        case("dirty_identity_and_publication")["value"]["during"]["oldState"]
            .as_str()
            .unwrap()
    );
    assert!(handle.begin_activity().is_err());
    assert!(handle.begin_maintenance().is_err());
    assert!(probe.events().is_empty());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        handle.dispose("replaced".into()).await.unwrap();
    });
}

#[tokio::test]
async fn completion_owner_without_router_reservation_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    owner.get_or_create(context("a")).await.unwrap();
    let key = JsString::from("a");
    let handle = owner.begin_completion(&key, &"run".into());
    let mut abort = Box::pin(owner.abort_turn(&key, Some(&"run".into()), Some(&"stop".into())));
    let during = json!({"done": std::future::poll_fn(|cx| std::task::Poll::Ready(abort.as_mut().poll(cx).is_ready())).await,
        "cancelled": handle.is_cancelled()});
    owner.complete_turn(&key, &handle);
    abort.await.unwrap();
    compare(
        "completion_owner_without_router_reservation",
        json!({"during": during, "done": true}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}
