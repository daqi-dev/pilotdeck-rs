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
    evict_panics: Mutex<HashSet<JsString>>,
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
            on_session_idle_evict: None,
            on_evict: Some(Arc::new(move |key, reason| {
                // A misbehaving eviction callback must not strand the key's
                // closing entry: the completion guard still settles it.
                if evict.0.evict_panics.lock().unwrap().contains(key) {
                    panic!("fixture evict callback panic");
                }
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
fn dirty(owner: &SessionOwner, key: &str) -> bool {
    owner
        .0
        .state
        .lock()
        .unwrap()
        .records
        .get(&key.into())
        .unwrap()
        .dirty
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
async fn dirty_mark_during_recreation_is_cleared_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let old = owner.get_or_create(context("a")).await.unwrap();
    owner.mark_all_dirty("first refresh");
    let gate = Gate::new();
    probe
        .0
        .creates
        .lock()
        .unwrap()
        .insert("a".into(), gate.clone());
    let replacing = owner.get_or_create(context("a"));
    until(|| probe.seen("create:a:2")).await;
    let marked = owner.mark_all_dirty("refresh during recreation");
    gate.open();
    let fresh = bounded(replacing).await.unwrap();
    let next = owner.get_or_create(context("a")).await.unwrap();
    compare(
        "dirty_mark_during_recreation_is_cleared",
        json!({"marked": marked, "replaced": !old.same_identity(&fresh),
            "nextIsFresh": next.same_identity(&fresh)}),
        &probe,
    );
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mark_session_dirty_missing_key_is_noop() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    let b = owner.get_or_create(context("b")).await.unwrap();
    assert!(!owner.mark_session_dirty(&"missing".into(), "sdk_session_config_changed"));
    assert!(!dirty(&owner, "a"));
    assert!(!dirty(&owner, "b"));
    let a_again = owner.get_or_create(context("a")).await.unwrap();
    let b_again = owner.get_or_create(context("b")).await.unwrap();
    assert!(a.same_identity(&a_again));
    assert!(b.same_identity(&b_again));
    assert!(!probe.seen("evict:a:dirty_recreate"));
    assert!(!probe.seen("evict:b:dirty_recreate"));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mark_session_dirty_existing_key_recreates_and_clears() {
    let probe = Probe::default();
    let owner = probe.owner();
    let old = owner.get_or_create(context("a")).await.unwrap();
    assert!(owner.mark_session_dirty(&"a".into(), "sdk_session_config_changed"));
    assert!(dirty(&owner, "a"));
    let fresh = owner.get_or_create(context("a")).await.unwrap();
    assert!(!old.same_identity(&fresh));
    assert!(probe.seen("evict:a:dirty_recreate"));
    assert!(!dirty(&owner, "a"));
    let next = owner.get_or_create(context("a")).await.unwrap();
    assert!(fresh.same_identity(&next));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mark_session_dirty_targets_only_its_session() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    let b = owner.get_or_create(context("b")).await.unwrap();
    assert!(owner.mark_session_dirty(&"a".into(), "sdk_session_config_changed"));
    assert!(dirty(&owner, "a"));
    assert!(!dirty(&owner, "b"));
    let fresh = owner.get_or_create(context("a")).await.unwrap();
    assert!(!a.same_identity(&fresh));
    let b_again = owner.get_or_create(context("b")).await.unwrap();
    assert!(b.same_identity(&b_again));
    assert!(probe.seen("evict:a:dirty_recreate"));
    assert!(!probe.seen("evict:b:dirty_recreate"));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mark_all_dirty_empty_reason_keeps_clean_session() {
    let probe = Probe::default();
    let owner = probe.owner();
    let old = owner.get_or_create(context("a")).await.unwrap();
    assert_eq!(owner.mark_all_dirty(""), 1);
    assert!(!dirty(&owner, "a"));
    let fresh = owner.get_or_create(context("a")).await.unwrap();
    assert!(old.same_identity(&fresh));
    assert!(!probe.seen("evict:a:dirty_recreate"));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mark_all_dirty_empty_reason_clears_prior_dirty() {
    let probe = Probe::default();
    let owner = probe.owner();
    let old = owner.get_or_create(context("a")).await.unwrap();
    assert_eq!(owner.mark_all_dirty("refresh"), 1);
    assert!(dirty(&owner, "a"));
    assert_eq!(owner.mark_all_dirty(""), 1);
    assert!(!dirty(&owner, "a"));
    let fresh = owner.get_or_create(context("a")).await.unwrap();
    assert!(old.same_identity(&fresh));
    assert!(!probe.seen("evict:a:dirty_recreate"));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mark_project_dirty_empty_reason_keeps_clean_session() {
    let probe = Probe::default();
    let owner = probe.owner();
    let old = owner.get_or_create(context("a")).await.unwrap();
    assert_eq!(owner.mark_project_dirty(&"project".into(), ""), 1);
    assert!(!dirty(&owner, "a"));
    let fresh = owner.get_or_create(context("a")).await.unwrap();
    assert!(old.same_identity(&fresh));
    assert!(!probe.seen("evict:a:dirty_recreate"));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mark_project_dirty_empty_reason_clears_prior_dirty() {
    let probe = Probe::default();
    let owner = probe.owner();
    let old = owner.get_or_create(context("a")).await.unwrap();
    assert_eq!(owner.mark_project_dirty(&"project".into(), "refresh"), 1);
    assert!(dirty(&owner, "a"));
    assert_eq!(owner.mark_project_dirty(&"project".into(), ""), 1);
    assert!(!dirty(&owner, "a"));
    let fresh = owner.get_or_create(context("a")).await.unwrap();
    assert!(old.same_identity(&fresh));
    assert!(!probe.seen("evict:a:dirty_recreate"));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mark_session_dirty_empty_reason_keeps_clean_session() {
    let probe = Probe::default();
    let owner = probe.owner();
    let old = owner.get_or_create(context("a")).await.unwrap();
    assert!(owner.mark_session_dirty(&"a".into(), ""));
    assert!(!dirty(&owner, "a"));
    let fresh = owner.get_or_create(context("a")).await.unwrap();
    assert!(old.same_identity(&fresh));
    assert!(!probe.seen("evict:a:dirty_recreate"));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn mark_session_dirty_empty_reason_clears_prior_dirty() {
    let probe = Probe::default();
    let owner = probe.owner();
    let old = owner.get_or_create(context("a")).await.unwrap();
    assert!(owner.mark_session_dirty(&"a".into(), "refresh"));
    assert!(dirty(&owner, "a"));
    assert!(owner.mark_session_dirty(&"a".into(), ""));
    assert!(!dirty(&owner, "a"));
    let fresh = owner.get_or_create(context("a")).await.unwrap();
    assert!(old.same_identity(&fresh));
    assert!(!probe.seen("evict:a:dirty_recreate"));
    owner.shutdown().await.unwrap();
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
    let reservation = owner.begin_maintenance(&"a".into()).unwrap().unwrap();
    let activity = handle.begin_maintenance().unwrap();
    probe.event("maintenance:1");
    assert!(owner.begin_maintenance(&"a".into()).unwrap().is_none());
    probe.time(101);
    let during = json!({"same": owner.begin_turn(&"a".into(), &"blocked".into()).is_some(),
        "other": owner.begin_turn(&"b".into(), &"run".into()).is_some(), "cached": sweep_count(&owner)});
    probe.event(format!(
        "maintenance_done:1:{}",
        activity.cancellation_reason().is_some()
    ));
    drop(activity);
    drop(reservation);
    compare(
        "maintenance_blocks_turn_and_idle",
        json!({"during": during, "after": owner.begin_turn(&"a".into(), &"run".into()).is_some()}),
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
    let stopped = tokio::time::timeout(Duration::from_millis(50), &mut shutdown).await;
    assert!(
        stopped.is_err(),
        "shutdown returned before pending creation drained"
    );
    let during = json!({"stopped": false,
        "admitted": owner.begin_turn(&"b".into(), &"run".into()).is_some(), "cached": count(&owner)});
    gate.open();
    let error = failure(bounded(creating).await);
    bounded(shutdown).await.unwrap();
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
    let reservation = owner.begin_turn(&"a".into(), &"run".into()).unwrap();
    probe.time(101);
    let busy = sweep_count(&owner);
    owner.end_turn(&"a".into(), Some(&"run".into()), Some(&reservation));
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
    assert!(owner.begin_turn(&key, &"old".into()).is_some());
    let old = owner.begin_completion(&key, &"old".into());
    owner.complete_turn(&key, &old);
    owner.mark_all_dirty("refresh");
    owner.get_or_create(context("a")).await.unwrap();
    assert!(owner.begin_turn(&key, &"new".into()).is_some());
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
        .expect("#818 live sink runtime builds");
    let _entered = runtime.enter();
    const ROUNDS: usize = 100;
    let probe = Probe::default();
    let owner = probe.owner();
    let key = JsString::from("a");
    runtime
        .block_on(owner.get_or_create(context("a")))
        .expect("#818 live sink initial session");
    let decided = std::sync::Barrier::new(2);
    let replaced = std::sync::Barrier::new(2);
    for round in 0..ROUNDS {
        let old_run = JsString::from(format!("old-{round}"));
        let new_run = JsString::from(format!("new-{round}"));
        let old_reservation = owner
            .begin_turn(&key, &old_run)
            .expect("#818 live sink old reservation");
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
                            owner
                                .abort(&key, target.as_ref(), &reason)
                                .expect("#818 live sink abort decision");
                            std::future::ready(())
                        });
                drop(operation);
            });
            decided.wait();
            owner.end_turn(&key, Some(&old_run), Some(&old_reservation));
            if let Some(old) = old_fence {
                owner.complete_turn(&key, &old);
            }
            owner.mark_all_dirty("refresh");
            // Run the public replacement on the independent runtime workers.
            let fresh = runtime
                .block_on(owner.get_or_create(context("a")))
                .expect("#818 live sink replacement session");
            assert!(
                owner.is_current(&key, &fresh),
                "#818 live sink replacement is current"
            );
            let new_reservation = owner
                .begin_turn(&key, &new_run)
                .expect("#818 live sink new reservation");
            replaced.wait();
            aborter.join().expect("#818 live sink aborter joins");
            owner.end_turn(&key, Some(&new_run), Some(&new_reservation));
        });
    }
    let hits = probe
        .events()
        .iter()
        .filter(|event| event.starts_with("abort:"))
        .count();
    println!("live replacement hits: {hits}/{ROUNDS}");
    assert_eq!(hits, 0, "#818 an old abort reached the new live writer");
    runtime
        .block_on(owner.shutdown())
        .expect("#818 live sink shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_and_completion_cleanup_preserve_later_same_run_reservation() {
    let probe = Probe::default();
    let owner = probe.owner();
    let key = JsString::from("a");
    let run = JsString::from("same");
    owner.get_or_create(context("a")).await.unwrap();
    let old_reservation = owner.begin_turn(&key, &run).unwrap();
    let old = owner.begin_completion(&key, &run);
    let drain = Gate::new();
    probe.0.disposals.lock().unwrap().insert(1, drain.clone());
    let closing = owner.close(&key);
    until(|| probe.seen("dispose:1")).await;
    let other = owner.clone();
    let other_key = key.clone();
    let other_run = run.clone();
    std::thread::spawn(move || {
        other.end_turn(&other_key, Some(&other_run), Some(&old_reservation));
        assert!(other.begin_turn(&other_key, &other_run).is_some());
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

/// A2/A2b/A3/A4 (TS probe2): close during a shutdown transmits each key's own
/// in-flight eviction — a failing one rejects with the per-key dispose
/// aggregate, a succeeding one (and an unregistered/unknown key) fulfills —
/// and never the whole-shutdown aggregate, which stays shutdown()'s answer.
#[tokio::test]
async fn close_during_shutdown_awaits_the_keys_own_eviction_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    let b = owner.get_or_create(context("b")).await.unwrap();
    let gate = Gate::new();
    let gate_b = Gate::new();
    probe
        .0
        .disposals
        .lock()
        .unwrap()
        .insert(id(&a), gate.clone());
    probe.0.dispose_failures.lock().unwrap().insert(id(&a));
    probe
        .0
        .disposals
        .lock()
        .unwrap()
        .insert(id(&b), gate_b.clone());
    let shutdown = owner.shutdown();
    until(|| probe.seen("dispose:1") && probe.seen("dispose:2")).await;
    let mut close_a = owner.close(&"a".into());
    let close_b = owner.close(&"b".into());
    let close_c = owner.close(&"c".into());
    let parked =
        std::future::poll_fn(|cx| std::task::Poll::Ready(close_a.as_mut().poll(cx).is_ready()))
            .await;
    assert!(
        !parked,
        "#1245 close of a mid-flight shutdown eviction must await it, not answer early"
    );
    gate_b.open();
    // Open a's gate before the awaits too: like the real probe, every close
    // promise resolves only after its eviction settles, and holding a gate
    // closed past an await would hang the whole-shutdown branch instead of
    // failing it with the per-key assertion.
    gate.open();
    bounded(close_b)
        .await
        .expect("#1245 a succeeding mid-flight shutdown eviction must fulfill close");
    bounded(close_c)
        .await
        .expect("#1245 an unregistered key must fulfill close during shutdown");
    let error = bounded(close_a)
        .await
        .expect_err("#1245 a failing mid-flight shutdown eviction must reject close");
    assert!(
        error.message == "Failed to dispose agent handle.",
        "#1245 close during shutdown transmits the key's own dispose aggregate: got {}",
        error.message
    );
    let shutdown_error = bounded(shutdown)
        .await
        .expect_err("#1245 the failing dispose must still fail the shutdown");
    assert!(
        shutdown_error.message == "Failed to shut down all agent sessions.",
        "#1245 the whole-shutdown aggregate stays shutdown's own answer: got {}",
        shutdown_error.message
    );
}

/// A1 (TS probe1): a close in the same tick as `shutdown()` — the eviction
/// not yet registered while a pending creation drains — fulfills.
#[tokio::test]
async fn close_in_the_shutdown_tick_fulfills_before_evictions_register_matches_ts() {
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
    let shutdown = owner.shutdown();
    bounded(owner.close(&"a".into()))
        .await
        .expect("#1245 a same-tick close must fulfill before any eviction registers");
    gate.open();
    let error = bounded(creating)
        .await
        .err()
        .expect("#1245 the pending creation must fail after the stop-new");
    assert!(
        error.message == "Session router shut down during session creation.",
        "#1245 the drained creation reports its own stop-new error: got {}",
        error.message
    );
    bounded(shutdown)
        .await
        .expect("#1245 shutdown with no failed disposal succeeds");
}

/// A2b/A5/A6 (TS probe3/probe4): a mid-flight succeeding eviction fulfills a
/// close during the window; after the eviction settles — failing or not —
/// and after shutdown completes, close still fulfills.
#[tokio::test]
async fn close_during_shutdown_fulfills_around_a_settled_eviction_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    let b = owner.get_or_create(context("b")).await.unwrap();
    let gate = Gate::new();
    probe
        .0
        .disposals
        .lock()
        .unwrap()
        .insert(id(&a), gate.clone());
    probe.0.dispose_failures.lock().unwrap().insert(id(&b));
    let shutdown = owner.shutdown();
    // a's drain stays mid-flight on the gate while b's failing drain settles
    // (its closing entry publishes before the shutdown aggregate forms).
    until(|| probe.seen("dispose:1") && probe.seen("evict:b:shutdown")).await;
    bounded(owner.close(&"b".into()))
        .await
        .expect("#1245 a close of an already-settled shutdown eviction must fulfill");
    // A2b: like the real probe's window cell, the mid-flight close parks on the
    // key's own eviction and only fulfills once that eviction settles.
    let mut close_a = owner.close(&"a".into());
    let parked =
        std::future::poll_fn(|cx| std::task::Poll::Ready(close_a.as_mut().poll(cx).is_ready()))
            .await;
    assert!(
        !parked,
        "#1245 the mid-flight close must await the key's own eviction, not answer early"
    );
    gate.open();
    bounded(close_a)
        .await
        .expect("#1245 a succeeding mid-flight shutdown eviction must fulfill close");
    let shutdown_error = bounded(shutdown)
        .await
        .expect_err("#1245 the failed disposal of b must fail the shutdown");
    assert!(
        shutdown_error.message == "Failed to shut down all agent sessions.",
        "#1245 the settled failure belongs to the shutdown aggregate: got {}",
        shutdown_error.message
    );
    bounded(owner.close(&"a".into()))
        .await
        .expect("#1245 close after the eviction settled and shutdown completed must fulfill");
    bounded(owner.close(&"b".into()))
        .await
        .expect("#1245 close after a failed eviction settled must fulfill");
}

/// B2/B3 (TS probe6/probe7): a second close of a key whose first close is
/// mid-flight awaits the first close's eviction and transmits its own
/// result; after it settles, close fulfills again.
#[tokio::test]
async fn second_close_awaits_the_firsts_own_eviction_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    let gate = Gate::new();
    probe
        .0
        .disposals
        .lock()
        .unwrap()
        .insert(id(&a), gate.clone());
    probe.0.dispose_failures.lock().unwrap().insert(id(&a));
    let mut first = owner.close(&"a".into());
    until(|| probe.seen("dispose:1")).await;
    let mut second = owner.close(&"a".into());
    let parked = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(
            second.as_mut().poll(cx).is_ready() || first.as_mut().poll(cx).is_ready(),
        )
    })
    .await;
    assert!(
        !parked,
        "#1245 both closes must await the mid-flight eviction of the first"
    );
    gate.open();
    for label in ["first", "second"] {
        let error = bounded(if label == "first" {
            &mut first
        } else {
            &mut second
        })
        .await
        .expect_err(
            "#1245 each in-flight close of the key must reject with the eviction's own result",
        );
        assert!(
            error.message == "Failed to dispose agent handle.",
            "#1245 the {label} close must transmit the per-key dispose aggregate: got {}",
            error.message
        );
    }
    bounded(owner.close(&"a".into()))
        .await
        .expect("#1245 a close after the eviction settled must fulfill");
}

/// B4 (TS probe8): a close arriving while an idle eviction is mid-flight
/// awaits that eviction and transmits its own result.
#[tokio::test]
async fn close_during_idle_eviction_awaits_the_idle_eviction_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    let gate = Gate::new();
    probe
        .0
        .disposals
        .lock()
        .unwrap()
        .insert(id(&a), gate.clone());
    probe.0.dispose_failures.lock().unwrap().insert(id(&a));
    probe.time(201);
    owner.sweep_idle();
    until(|| probe.seen("dispose:1")).await;
    let mut closing = owner.close(&"a".into());
    let parked =
        std::future::poll_fn(|cx| std::task::Poll::Ready(closing.as_mut().poll(cx).is_ready()))
            .await;
    assert!(
        !parked,
        "#1245 close during a mid-flight idle eviction must await it, not answer early"
    );
    gate.open();
    let error = bounded(closing)
        .await
        .expect_err("#1245 a failing mid-flight idle eviction must reject close");
    assert!(
        error.message == "Failed to dispose agent handle.",
        "#1245 close during an idle eviction transmits the per-key dispose aggregate: got {}",
        error.message
    );
    assert!(
        probe.seen("evict:a:idle"),
        "#1245 the awaited eviction keeps its idle reason"
    );
}

/// C1 (TS probe9): a close issued before `shutdown()` keeps its own eviction
/// mid-flight; a same-key close during the shutdown awaits that eviction,
/// and the shutdown aggregate still carries the failed disposal.
#[tokio::test]
async fn close_issued_before_shutdown_keeps_its_own_eviction_for_later_closes_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    let gate = Gate::new();
    probe
        .0
        .disposals
        .lock()
        .unwrap()
        .insert(id(&a), gate.clone());
    probe.0.dispose_failures.lock().unwrap().insert(id(&a));
    let mut first = owner.close(&"a".into());
    until(|| probe.seen("dispose:1")).await;
    let shutdown = owner.shutdown();
    let mut second = owner.close(&"a".into());
    let parked = std::future::poll_fn(|cx| {
        std::task::Poll::Ready(
            second.as_mut().poll(cx).is_ready() || first.as_mut().poll(cx).is_ready(),
        )
    })
    .await;
    assert!(
        !parked,
        "#1245 the pre-shutdown close and the close during shutdown must await the eviction"
    );
    gate.open();
    for label in ["pre-shutdown", "during-shutdown"] {
        let error = bounded(if label == "pre-shutdown" {
            &mut first
        } else {
            &mut second
        })
        .await
        .expect_err("#1245 each close of the key must reject with the eviction's own result");
        assert!(
            error.message == "Failed to dispose agent handle.",
            "#1245 the {label} close transmits the per-key dispose aggregate: got {}",
            error.message
        );
    }
    let shutdown_error = bounded(shutdown)
        .await
        .expect_err("#1245 the shutdown must aggregate the failed disposal");
    assert!(
        shutdown_error.message == "Failed to shut down all agent sessions.",
        "#1245 the shutdown aggregate keeps its own message: got {}",
        shutdown_error.message
    );
    assert!(
        probe.seen("evict:a:closed"),
        "#1245 the pre-shutdown close keeps its closed reason"
    );
}

/// D1/D1b (fix-r1 probe1/probe2): a close issued before `shutdown()` in the
/// same tick — the close's eviction registered but its task not yet run —
/// keeps the key's own eviction: a failing dispose rejects the close per-key
/// and joins the shutdown aggregate; a succeeding dispose fulfills both, and
/// the eviction reason stays `closed`.
#[tokio::test]
async fn close_then_shutdown_same_tick_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    probe.0.dispose_failures.lock().unwrap().insert(id(&a));
    let close_a = owner.close(&"a".into());
    let shutdown = owner.shutdown();
    let error = bounded(close_a)
        .await
        .expect_err("#1245 a close that starts before a same-tick shutdown must reject with its own dispose failure");
    assert!(
        error.message == "Failed to dispose agent handle.",
        "#1245 the pre-shutdown close transmits the per-key dispose aggregate: got {}",
        error.message
    );
    let shutdown_error = bounded(shutdown)
        .await
        .expect_err("#1245 the shutdown must aggregate the close's failed disposal");
    assert!(
        shutdown_error.message == "Failed to shut down all agent sessions.",
        "#1245 the whole-shutdown aggregate stays shutdown's own answer: got {}",
        shutdown_error.message
    );
    assert!(
        probe.seen("evict:a:closed"),
        "#1245 the same-tick close keeps its closed reason"
    );
    assert!(
        !probe.seen("evict:a:shutdown"),
        "#1245 the shutdown must not steal the closed eviction's reason"
    );

    let probe = Probe::default();
    let owner = probe.owner();
    owner.get_or_create(context("b")).await.unwrap();
    let close_b = owner.close(&"b".into());
    let shutdown = owner.shutdown();
    bounded(close_b)
        .await
        .expect("#1245 a same-tick close with a succeeding disposal must fulfill");
    bounded(shutdown)
        .await
        .expect("#1245 the shutdown must fulfill when the close's disposal succeeds");
    assert!(
        probe.seen("evict:b:closed"),
        "#1245 the succeeding same-tick close keeps its closed reason"
    );
}

/// D2/D2b (fix-r1 probe3/probe4): a close in the same tick as a preceding
/// `shutdown()` — the per-key drain not yet registered — fulfills even when
/// the drain's dispose fails; the failure stays in shutdown's own aggregate,
/// and the drain keeps its `shutdown` reason.
#[tokio::test]
async fn shutdown_then_close_same_tick_fulfills_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    probe.0.dispose_failures.lock().unwrap().insert(id(&a));
    let shutdown = owner.shutdown();
    bounded(owner.close(&"a".into()))
        .await
        .expect("#1245 a same-tick close after shutdown must fulfill before the drain registers");
    let shutdown_error = bounded(shutdown)
        .await
        .expect_err("#1245 the failing drain must still fail the shutdown");
    assert!(
        shutdown_error.message == "Failed to shut down all agent sessions.",
        "#1245 the shutdown-first aggregate keeps its own message: got {}",
        shutdown_error.message
    );
    assert!(
        probe.seen("evict:a:shutdown"),
        "#1245 the drain keeps its shutdown reason"
    );
    assert!(
        !probe.seen("evict:a:closed"),
        "#1245 a close without a record must not fabricate a closed eviction"
    );

    let probe = Probe::default();
    let owner = probe.owner();
    owner.get_or_create(context("b")).await.unwrap();
    let shutdown = owner.shutdown();
    bounded(owner.close(&"b".into()))
        .await
        .expect("#1245 a same-tick close after shutdown must fulfill on a succeeding drain too");
    bounded(shutdown)
        .await
        .expect("#1245 the shutdown must fulfill when the drain succeeds");
    assert!(
        probe.seen("evict:b:shutdown"),
        "#1245 the succeeding drain keeps its shutdown reason"
    );
}

/// E1 (fix-r2 probe skip-only-closing-key): a shutdown whose order mixes a
/// close-owned key (`a`: its close registered the eviction in the same tick,
/// task not yet drained) with a plain record (`b`: no closing entry) skips
/// only `a` — the close keeps its own eviction and `closed` reason, `b` is
/// still drained by shutdown with its `shutdown` reason, and the shutdown
/// aggregate carries `a`'s failed disposal through the pending drain.
#[tokio::test]
async fn shutdown_skips_only_closing_keys_and_drains_the_rest_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let a = owner.get_or_create(context("a")).await.unwrap();
    owner.get_or_create(context("b")).await.unwrap();
    probe.0.dispose_failures.lock().unwrap().insert(id(&a));
    let close_a = owner.close(&"a".into());
    let shutdown = owner.shutdown();
    let error = bounded(close_a)
        .await
        .expect_err("#1245 the close-owned key keeps its own eviction result");
    assert!(
        error.message == "Failed to dispose agent handle.",
        "#1245 the close-owned key transmits its per-key dispose aggregate: got {}",
        error.message
    );
    let shutdown_error = bounded(shutdown)
        .await
        .expect_err("#1245 the mixed shutdown aggregates the close's failed disposal");
    assert!(
        shutdown_error.message == "Failed to shut down all agent sessions.",
        "#1245 the mixed shutdown keeps its own aggregate message: got {}",
        shutdown_error.message
    );
    assert!(
        probe.seen("evict:a:closed"),
        "#1245 the close-owned key keeps its closed reason in a mixed shutdown"
    );
    assert!(
        probe.seen("evict:b:shutdown"),
        "#1245 the shutdown drain must still dispose the plain key after skipping the closing key"
    );
    assert!(
        !probe.seen("evict:a:shutdown"),
        "#1245 the shutdown must not steal the closing key's eviction"
    );
    assert!(
        owner.current_handle(&"b".into()).is_none(),
        "#1245 the plain key's record must not stay cached after the mixed shutdown"
    );
    assert!(
        owner.current_handle(&"a".into()).is_none(),
        "#1245 the close-owned key's record must not stay cached after the mixed shutdown"
    );
}

/// E2 (fix-r2): a shutdown drain whose eviction callback panics must not leave
/// the key's closing entry unsettled — the completion guard settles it with
/// the dropped error, so the shutdown aggregate still fails and a later
/// record-less close falls back to the post-shutdown branch and fulfills
/// (TS has no panic; the closest analogue is a rejected eviction, which
/// finishEviction still removes from closingSessions). The handle layer
/// already contains session-dispose panics, so the callback is the only
/// panic that reaches the owner's drain task.
#[tokio::test]
async fn shutdown_drain_panic_settles_the_keys_closing_entry() {
    let probe = Probe::default();
    let owner = probe.owner();
    owner.get_or_create(context("a")).await.unwrap();
    probe.0.evict_panics.lock().unwrap().insert("a".into());
    let shutdown = owner.shutdown();
    let error = bounded(shutdown)
        .await
        .expect_err("#1245 a panicked drain must fail the shutdown aggregate");
    assert!(
        error.message == "Failed to shut down all agent sessions.",
        "#1245 the panicked drain keeps the whole-shutdown aggregate message: got {}",
        error.message
    );
    tokio::time::timeout(Duration::from_millis(50), owner.close(&"a".into()))
        .await
        .expect("#1245 a close after a panicked drain must settle through the completion guard")
        .expect("#1245 a record-less close after shutdown must fulfill like TS await undefined");
}

/// #1245 fix-r3: the settle-before-release ordering has machine evidence. The
/// close task stages its outcome in a guard that settles the key's closing
/// entry strictly before the serial ticket is released; the order probe
/// records both events on this thread, and the settle write must come first
/// even though the window between the two has no await point an external task
/// could gate on. `take()` before the close drops the fixture creation's own
/// ticket-release record: that task ends its final poll before the awaited
/// creation future resumes.
#[tokio::test]
async fn close_settles_its_closing_entry_before_releasing_the_ticket() {
    let probe = Probe::default();
    let owner = probe.owner();
    let key = JsString::from("a");
    bounded(owner.get_or_create(context("a")))
        .await
        .expect("#1245 the fixture creation must complete before the close starts");
    let _ = settle_order::take();
    bounded(owner.close(&key))
        .await
        .expect("#1245 the close must fulfill with a succeeding disposal");
    let events = settle_order::take();
    assert_eq!(
        events,
        ["closing-settled", "ticket-released"],
        "#1245 the close task must settle its closing entry before releasing the serial ticket"
    );
    assert!(
        owner.current_handle(&key).is_none(),
        "#1245 the closed key must leave no cached record"
    );
}

/// #1245 fix-r3: the same settle-before-release witness for the idle sweep's
/// eviction task. `sweep_idle` spawns the task unpollled; `until` returns only
/// after its final poll recorded both events.
#[tokio::test]
async fn idle_sweep_settles_its_closing_entry_before_releasing_the_ticket() {
    let probe = Probe::default();
    let owner = probe.owner();
    let key = JsString::from("a");
    bounded(owner.get_or_create(context("a")))
        .await
        .expect("#1245 the fixture creation must complete before the idle sweep");
    probe.time(101);
    let _ = settle_order::take();
    owner.sweep_idle();
    until(|| probe.seen("evict:a:idle")).await;
    let events = settle_order::take();
    assert_eq!(
        events,
        ["closing-settled", "ticket-released"],
        "#1245 the idle sweep task must settle its closing entry before releasing the serial ticket"
    );
    assert!(
        owner.current_handle(&key).is_none(),
        "#1245 the swept key must leave no cached record"
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
    assert!(owner.begin_turn(&key, &run).is_some());
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publication_cutoff_revokes_cached_handle_before_async_disposal_starts() {
    let probe = Probe::default();
    let (reached, publication) = tokio::sync::oneshot::channel();
    let (release, resume) = std::sync::mpsc::channel();
    let resume = Mutex::new(resume);
    let pause = Arc::new(Mutex::new(None));
    let clock_pause = pause.clone();
    let mut options = SessionOwnerOptions::new(Arc::new(probe.clone()));
    options.idle_sweep_interval_ms = 0;
    options.now_ms = Arc::new(move || {
        if let Some(reached) = clock_pause.lock().unwrap().take() {
            // Record.last_used is evaluated under the publication lock, after
            // stop_new and before dispose. Pause only this publication, not the
            // sweep that precedes get_or_create. No product test hook is needed.
            let reached: tokio::sync::oneshot::Sender<()> = reached;
            reached.send(()).unwrap();
            resume
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }
        0
    });
    let owner = SessionOwner::new(options);
    let key = JsString::from("a");
    let old = owner.get_or_create(context("a")).await.unwrap();
    owner.mark_all_dirty("refresh");
    let factory_gate = Gate::new();
    probe
        .0
        .creates
        .lock()
        .unwrap()
        .insert(key.clone(), factory_gate.clone());
    let replacing = owner.get_or_create(context("a"));
    until(|| probe.seen("create:a:2")).await;
    *pause.lock().unwrap() = Some(reached);
    factory_gate.open();
    bounded(publication).await.unwrap();
    let old_state = old.state();
    let activity_rejected = old.begin_activity().is_err();
    let maintenance_rejected = old.begin_maintenance().is_err();
    let disposal_started = probe.seen("dispose:1");
    // Always unblock the worker before assertions, including on a mutation.
    release.send(()).unwrap();
    let new = bounded(replacing).await.unwrap();
    assert_eq!(
        old_state,
        HandleState::Draining,
        "publication did not revoke cached handle before disposal"
    );
    assert!(
        activity_rejected,
        "publication admitted activity on the old handle"
    );
    assert!(
        maintenance_rejected,
        "publication admitted maintenance on the old handle"
    );
    assert!(!disposal_started);
    assert!(!old.same_identity(&new));
    assert!(owner.is_current(&key, &new));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn idle_sweep_skips_same_key_creation_and_queued_close() {
    let probe = Probe::default();
    let owner = probe.owner();
    let key = JsString::from("a");
    let old = owner.get_or_create(context("a")).await.unwrap();
    owner.get_or_create(context("b")).await.unwrap();
    let creation_gate = Gate::new();
    let disposal_gate = Gate::new();
    probe
        .0
        .creates
        .lock()
        .unwrap()
        .insert(key.clone(), creation_gate.clone());
    probe
        .0
        .disposals
        .lock()
        .unwrap()
        .insert(1, disposal_gate.clone());
    owner.mark_all_dirty("refresh");
    let replacing = owner.get_or_create(context("a"));
    until(|| probe.seen("create:a:3")).await;
    probe.time(101);
    owner.sweep_idle();
    assert!(
        owner.is_current(&key, &old),
        "idle sweep evicted a session during same-key creation"
    );
    assert!(
        owner.current_handle(&"b".into()).is_none(),
        "unrelated idle session was not evicted"
    );
    creation_gate.open();
    until(|| probe.seen("dispose:1")).await;
    let new = owner.current_handle(&key).unwrap();
    assert!(!old.same_identity(&new));
    // Replacement has published but still owns the serial gate while draining
    // the old writer. A close queued behind it must retain its Closed reason.
    let mut closing = owner.close(&key);
    probe.time(202);
    owner.sweep_idle();
    assert!(
        owner.is_current(&key, &new),
        "idle sweep evicted a session with a same-key close pending"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut closing)
            .await
            .is_err()
    );
    disposal_gate.open();
    assert!(bounded(replacing).await.unwrap().same_identity(&new));
    bounded(closing).await.unwrap();
    assert!(owner.current_handle(&key).is_none());
    assert!(probe.seen("evict:a:closed"));
    assert!(!probe.seen("evict:a:idle"));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn close_waits_for_prior_same_key_close_to_drain() {
    let probe = Probe::default();
    let owner = probe.owner();
    let key = JsString::from("a");
    owner.get_or_create(context("a")).await.unwrap();
    let gate = Gate::new();
    probe.0.disposals.lock().unwrap().insert(1, gate.clone());
    let first = owner.close(&key);
    until(|| probe.seen("dispose:1")).await;
    let mut second = owner.close(&key);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut second)
            .await
            .is_err(),
        "close returned before the prior same-key close drained"
    );
    // Another key must make progress while the first close is blocked.
    bounded(owner.get_or_create(context("b"))).await.unwrap();
    gate.open();
    bounded(first).await.unwrap();
    bounded(second).await.unwrap();
    assert_eq!(
        probe
            .events()
            .iter()
            .filter(|event| *event == "evict:a:closed")
            .count(),
        1,
        "#1245 exactly one same-key closed eviction must run"
    );
    assert!(
        owner.current_handle(&key).is_none(),
        "#1245 the drained key must leave no cached record"
    );
    owner.shutdown().await.unwrap();

    // A second same-key close never reaches the ticket path above — it awaits
    // the first close's registered closing entry — so pin the close task's own
    // serial wait separately: a close issued while a same-key creation is
    // still in flight must queue behind it and evict the record that creation
    // publishes, not settle on a key that is about to come back. TS close
    // fulfills immediately here (no record, no closing entry — see
    // fix-r2/probe/close-during-creation.out); the queueing is the registered
    // A7.2-SESSION-RECREATE-DRAIN-001 divergence.
    let probe = Probe::default();
    let owner = probe.owner();
    let creation_gate = Gate::new();
    probe
        .0
        .creates
        .lock()
        .unwrap()
        .insert(key.clone(), creation_gate.clone());
    let creating = owner.get_or_create(context("a"));
    until(|| probe.seen("create:a:1")).await;
    let mut closing = owner.close(&key);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut closing)
            .await
            .is_err(),
        "#1245 close settled before the in-flight same-key creation published its record"
    );
    creation_gate.open();
    bounded(creating)
        .await
        .expect("#1245 the same-key creation must complete once its gate opens");
    bounded(closing)
        .await
        .expect("#1245 the queued close must drain after the creation publishes");
    assert!(
        owner.current_handle(&key).is_none(),
        "#1245 the queued close must evict the record the same-key creation published"
    );
    assert!(
        probe.seen("evict:a:closed"),
        "#1245 the queued close keeps its closed reason"
    );
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

/// TS SessionRouter.endTurn releases the new string-matched turn here.
/// The consumer's captured identity is the explicit Rust concurrency safeguard.
#[tokio::test]
async fn stale_end_after_close_and_same_run_readmission_preserves_new_reservation() {
    let probe = Probe::default();
    let owner = probe.owner();
    let key = JsString::from("a");
    let run = JsString::from("same");
    owner.get_or_create(context("a")).await.unwrap();
    let old = owner.begin_turn(&key, &run).unwrap();
    owner.close(&key).await.unwrap();
    assert!(!owner.has_active_turn(&key));
    let fresh = owner.begin_turn(&key, &run).unwrap();
    owner.end_turn(&key, Some(&run), Some(&old));
    assert!(
        owner.has_active_turn(&key),
        "stale end_turn released the readmitted same-run reservation"
    );
    owner.end_turn(&key, Some(&run), Some(&fresh));
    assert!(!owner.has_active_turn(&key));
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn empty_project_pause_and_close_matches_ts() {
    let probe = Probe::default();
    let owner = probe.owner();
    let empty = |key| {
        let mut ctx = context(key);
        ctx.project_key = Some("".into());
        ctx
    };
    owner.get_or_create(empty("existing")).await.unwrap();
    let mut unscoped = context("unscoped");
    unscoped.project_key = None;
    owner.get_or_create(unscoped).await.unwrap();
    let gate = Gate::new();
    probe
        .0
        .creates
        .lock()
        .unwrap()
        .insert("pending".into(), gate.clone());
    let pending = owner.get_or_create(empty("pending"));
    until(|| probe.seen("create:pending:3")).await;
    let mut closing = owner.close_project(&"".into());
    let waited_for_creation =
        std::future::poll_fn(|cx| std::task::Poll::Ready(closing.as_mut().poll(cx).is_pending()))
            .await;
    bounded(owner.get_or_create(empty("during"))).await.unwrap();
    gate.open();
    pending.await.unwrap();
    let keys: Vec<_> = closing
        .await
        .unwrap()
        .iter()
        .map(|key| key.to_utf8().unwrap())
        .collect();
    let during_closed = owner.current_handle(&"during".into()).is_none();
    owner.get_or_create(empty("after")).await.unwrap();
    let value = json!({"waitedForCreation": waited_for_creation, "keys": keys,
        "existingClosed": owner.current_handle(&"existing".into()).is_none(),
        "pendingClosed": owner.current_handle(&"pending".into()).is_none(),
        "duringClosed": during_closed,
        "unscopedKept": owner.current_handle(&"unscoped".into()).is_some(),
        "afterAllowed": owner.current_handle(&"after".into()).is_some()});
    // close_project drains keys concurrently; compare the exact TS value and
    // lifecycle event multiset without imposing a cross-key eviction order.
    assert_eq!(value, case("empty_project_pause_and_close")["value"]);
    let mut events = probe.events();
    let mut expected: Vec<String> =
        serde_json::from_value(case("empty_project_pause_and_close")["events"].clone()).unwrap();
    events.sort();
    expected.sort();
    assert_eq!(events, expected);
    owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn maintenance_shutdown_is_distinct_from_busy() {
    let owner = Probe::default().owner();
    let reservation = owner.begin_maintenance(&"a".into()).unwrap().unwrap();
    assert!(owner.begin_maintenance(&"a".into()).unwrap().is_none());
    owner.shutdown().await.unwrap();
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/replacement/oracle.json"
    ));
    let JsValue::Array(maintenance) = oracle.get("maintenance").unwrap() else {
        panic!("maintenance golden array");
    };
    assert_eq!(
        owner.begin_maintenance(&"a".into()).err().unwrap().message,
        maintenance[1]
            .get("message")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap()
    );
    drop(reservation);
}

#[tokio::test]
async fn replacement_adapter_drains_real_owner_before_storage() {
    use crate::runtime::replacement::{ReplacementCoordinator, ReplacementStorage, SubmitClaim};
    let probe = Probe::default();
    let owner = probe.owner();
    let handle = owner.get_or_create(context("a")).await.unwrap();
    let run = JsString::from("old");
    let reservation = owner.begin_turn(&"a".into(), &run).unwrap();
    let completion = owner.begin_completion(&"a".into(), &run);
    let storage_probe = probe.clone();
    let coordinator = ReplacementCoordinator::new(
        ReplacementStorage {
            replace_last_turn: Some(Arc::new(move |_| {
                storage_probe.event("storage_replace");
                Box::pin(async { Ok(parse_json(r#"{"transactionId":"tx-a"}"#).unwrap()) })
            })),
            finalize_last_turn_replacement: Some(Arc::new(|_| {
                Box::pin(async { Ok(JsValue::Null) })
            })),
            ..Default::default()
        },
        Arc::new(owner.clone()),
        60_000,
        None,
    );
    let replacing = coordinator.replace_last_turn(
        parse_json(r#"{"sessionKey":"a","expectedTurnId":"old","replacementTurnId":"new"}"#)
            .unwrap(),
    );
    until(|| probe.seen("abort:1:message_replaced")).await;
    assert!(
        !probe.seen("storage_replace"),
        "storage must wait for the completion fence"
    );
    owner.complete_turn(&"a".into(), &completion);
    replacing.await.unwrap();
    let events = probe.events();
    assert!(
        events.iter().position(|e| e == "dispose:1").unwrap()
            < events.iter().position(|e| e == "storage_replace").unwrap()
    );
    assert_eq!(handle.state(), HandleState::Disposed);
    assert!(owner.current_handle(&"a".into()).is_none());
    assert_eq!(
        coordinator.claim_for_submit(&"a".into(), &"new".into()),
        SubmitClaim::Claimed
    );
    coordinator
        .commit_accepted_input(&"a".into(), &"new".into())
        .await;
    coordinator.dispose();
    owner.end_turn(&"a".into(), Some(&run), Some(&reservation));
    owner.shutdown().await.unwrap();
}

#[path = "owner_hooks_tests.rs"]
mod hooks;

fn observer_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

#[test]
fn session_close_completion_survives_stopped_executor() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let probe = Probe::default();
    let owner = probe.owner();
    executor.block_on(async {
        owner.get_or_create(context("a")).await.unwrap();
    });
    let pending = {
        let _entered = executor.enter();
        owner.close(&"a".into())
    };
    drop(executor); // close task queued but never polled
    observer_runtime().block_on(async {
        let error = tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("session close completion must survive stopped executor")
            .unwrap_err();
        assert_eq!(
            error.message,
            "Session close task was dropped or panicked before completion."
        );
    });
}

#[test]
fn session_close_project_completion_survives_stopped_executor() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let probe = Probe::default();
    let owner = probe.owner();
    executor.block_on(async {
        owner.get_or_create(context("a")).await.unwrap();
    });
    let pending = {
        let _entered = executor.enter();
        owner.close_project(&"project".into())
    };
    drop(executor);
    observer_runtime().block_on(async {
        let error = tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("close_project completion must survive stopped executor")
            .unwrap_err();
        assert_eq!(
            error.message,
            "Session project close task was dropped or panicked before completion."
        );
    });
}

#[test]
fn session_idle_eviction_completion_survives_stopped_executor() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let probe = Probe::default();
    let owner = probe.owner();
    executor.block_on(async {
        owner.get_or_create(context("a")).await.unwrap();
    });
    let completion = {
        let _entered = executor.enter();
        probe.time(1_000);
        owner.sweep_idle();
        owner
            .0
            .state
            .lock()
            .unwrap()
            .pending
            .last()
            .expect("sweep must track the idle eviction")
            .completion
            .clone()
    };
    drop(executor);
    observer_runtime().block_on(async {
        let error = tokio::time::timeout(Duration::from_secs(1), wait_result(completion))
            .await
            .expect("idle eviction completion must survive stopped executor")
            .unwrap_err();
        assert_eq!(
            error.message,
            "Session idle eviction task was dropped or panicked before completion."
        );
    });
}

#[test]
fn session_shutdown_completion_survives_stopped_executor() {
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let probe = Probe::default();
    let owner = probe.owner();
    executor.block_on(async {
        owner.get_or_create(context("a")).await.unwrap();
    });
    let pending = {
        let _entered = executor.enter();
        owner.shutdown()
    };
    drop(executor);
    let observer = observer_runtime();
    observer.block_on(async {
        let error = tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .expect("shutdown completion must survive stopped executor")
            .unwrap_err();
        assert_eq!(
            error.message,
            "Session shutdown task was dropped or panicked before completion."
        );
        // Repeated observation through the memoized shutdown receiver.
        let repeated = owner.shutdown().await.unwrap_err();
        assert_eq!(repeated.message, error.message);
    });
}
