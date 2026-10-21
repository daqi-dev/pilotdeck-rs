//! Transport-neutral SessionRouter lifecycle. Publication, admission and target
//! selection share one short lock. Only operations on the same key await one
//! another; no factory, setup, abort callback or drain runs under that lock.
use super::super::{
    project::{RuntimeError, RuntimeResult},
    refresh::{BoxFuture, RefreshRouter},
};
use super::{
    LiveSessionHandle, SessionTurns, TurnCompletionHandle, TurnReservation,
    handle::{aggregate, wait_result},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionContext {
    pub session_key: JsString,
    pub project_key: Option<JsString>,
    pub channel_key: JsString,
    pub allowed_tools: Option<Vec<JsString>>,
    pub disallowed_tools: Option<Vec<JsString>>,
}

impl SessionContext {
    fn merge(&mut self, next: &Self) -> bool {
        let changed = (next.allowed_tools.is_some() && next.allowed_tools != self.allowed_tools)
            || (next.disallowed_tools.is_some() && next.disallowed_tools != self.disallowed_tools);
        if !next.channel_key.is_empty() {
            self.channel_key = next.channel_key.clone();
        }
        if self.project_key.is_none() {
            self.project_key = next.project_key.clone();
        }
        if next.allowed_tools.is_some() {
            self.allowed_tools = next.allowed_tools.clone();
        }
        if next.disallowed_tools.is_some() {
            self.disallowed_tools = next.disallowed_tools.clone();
        }
        changed
    }
}

/// Native session construction and setup are injected, with the previous live
/// writer available for a runtime-reload snapshot. Concrete MCP/session-loop
/// adapters are assembled by later domain packages, not stubbed here.
pub trait SessionFactory: Send + Sync {
    fn create(
        &self,
        context: SessionContext,
        previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>>;
    fn can_recreate(&self) -> bool {
        false
    }
    fn setup(
        &self,
        _context: SessionContext,
        _handle: LiveSessionHandle,
        _previous: Option<LiveSessionHandle>,
    ) -> BoxFuture<RuntimeResult> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EvictionReason {
    Idle,
    Closed,
    DirtyRecreate,
    Shutdown,
}

type Evict = Arc<dyn Fn(&JsString, EvictionReason) + Send + Sync>;
pub struct SessionOwnerOptions {
    pub factory: Arc<dyn SessionFactory>,
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub idle_timeout_ms: i64,
    /// Zero disables the periodic sweep (for an externally driven clock).
    pub idle_sweep_interval_ms: u64,
    pub on_evict: Option<Evict>,
}

impl SessionOwnerOptions {
    pub fn new(factory: Arc<dyn SessionFactory>) -> Self {
        Self {
            factory,
            now_ms: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64
            }),
            idle_timeout_ms: 30 * 60 * 1000,
            idle_sweep_interval_ms: 60 * 1000,
            on_evict: None,
        }
    }
}

#[derive(Clone)]
struct Record {
    handle: LiveSessionHandle,
    context: SessionContext,
    last_used: i64,
    dirty: bool,
}

struct Pending {
    project: Option<JsString>,
    completion: watch::Receiver<Option<RuntimeResult>>,
}

struct CompletionOwner {
    completion: TurnCompletionHandle,
    handle: Option<LiveSessionHandle>,
    admission: Option<Arc<()>>,
}

#[derive(Default)]
struct State {
    stopped: bool,
    records: HashMap<JsString, Record>,
    completion_owners: HashMap<JsString, CompletionOwner>,
    order: Vec<JsString>,
    gates: HashMap<JsString, Weak<Serial>>,
    pending: Vec<Pending>,
    paused_projects: HashSet<JsString>,
    shutdown: Option<watch::Receiver<Option<RuntimeResult>>>,
}

struct Inner {
    state: Mutex<State>,
    turns: SessionTurns,
    options: SessionOwnerOptions,
    stopped: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct SessionOwner(Arc<Inner>);

impl SessionOwner {
    pub fn new(options: SessionOwnerOptions) -> Self {
        let interval = options.idle_sweep_interval_ms;
        let owner = Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            turns: SessionTurns::default(),
            options,
            stopped: watch::channel(false).0,
        }));
        if interval > 0 {
            let weak = Arc::downgrade(&owner.0);
            let mut stopped = owner.0.stopped.subscribe();
            let period = std::time::Duration::from_millis(interval);
            let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = stopped.wait_for(|stopped| *stopped) => break,
                        _ = timer.tick() => {
                            let Some(inner) = weak.upgrade() else { break; };
                            Self(inner).sweep_idle();
                        }
                    }
                }
            });
        }
        owner
    }

    /// Starts/tracks the operation before returning the future, just like the
    /// synchronous prefix of a TS async function. Dropping the future cannot
    /// abandon an unpublished handle or remove it from shutdown's drain set.
    pub fn get_or_create(
        &self,
        context: SessionContext,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        self.sweep_idle();
        let mut state = self.0.state.lock().unwrap();
        if state.stopped {
            return Box::pin(async { Err(RuntimeError::new("Session router is shut down.")) });
        }
        if project_paused(&state, context.project_key.as_ref()) {
            return Box::pin(async { Err(RuntimeError::new("Project is being deleted.")) });
        }
        let ticket = ticket(&mut state, &context.session_key);
        let (done, completion) = watch::channel(None);
        track(&mut state, context.project_key.clone(), completion);
        let (result, completion) = watch::channel(None);
        let owner = self.clone();
        tokio::spawn(async move {
            let _exclusive = ticket.enter().await;
            let outcome = owner.create_active(context).await;
            result.send_replace(Some(outcome));
            // Failed creation is reported to its caller, not a shutdown error.
            done.send_replace(Some(Ok(())));
        });
        Box::pin(wait_result(completion))
    }

    async fn create_active(
        &self,
        context: SessionContext,
    ) -> Result<LiveSessionHandle, RuntimeError> {
        let cached = {
            let mut state = self.0.state.lock().unwrap();
            if state.stopped {
                return Err(RuntimeError::new("Session router is shut down."));
            }
            if project_paused(&state, context.project_key.as_ref()) {
                return Err(RuntimeError::new("Project is being deleted."));
            }
            if let Some(record) = state.records.get_mut(&context.session_key) {
                record.dirty |= record.context.merge(&context);
                if !record.dirty || !self.0.options.factory.can_recreate() {
                    record.last_used = (self.0.options.now_ms)();
                    return Ok(record.handle.clone());
                }
                Some(record.clone())
            } else {
                None
            }
        };
        let context = cached
            .as_ref()
            .map_or(context, |record| record.context.clone());
        let previous = cached.as_ref().map(|record| record.handle.clone());
        let handle = self
            .0
            .options
            .factory
            .create(context.clone(), previous.clone())
            .await?;
        if let Err(cause) = self
            .0
            .options
            .factory
            .setup(context.clone(), handle.clone(), previous)
            .await
        {
            let rollback = handle.dispose("agent_publication_rollback".into()).await;
            return Err(match rollback {
                Ok(()) => cause,
                Err(error) => RuntimeError {
                    message: format!(
                        "Failed to roll back unpublished resources while attempting to {} agent {}.",
                        if cached.is_some() {
                            "replace"
                        } else {
                            "create"
                        },
                        String::from_utf16_lossy(context.session_key.units())
                    ),
                    causes: vec![cause, error],
                },
            });
        }
        let publication_error = {
            let mut state = self.0.state.lock().unwrap();
            if state.stopped {
                Some(if cached.is_some() {
                    "Session router shut down during session recreation."
                } else {
                    "Session router shut down during session creation."
                })
            } else if project_paused(&state, context.project_key.as_ref()) {
                Some("Project is being deleted; the created session was discarded.")
            } else {
                if let Some(previous) = &cached {
                    previous.handle.stop_new();
                    state.completion_owners.remove(&context.session_key);
                }
                if !state.records.contains_key(&context.session_key) {
                    state.order.push(context.session_key.clone());
                }
                state.records.insert(
                    context.session_key.clone(),
                    Record {
                        handle: handle.clone(),
                        context: context.clone(),
                        last_used: (self.0.options.now_ms)(),
                        dirty: false,
                    },
                );
                None
            }
        };
        if let Some(message) = publication_error {
            handle.dispose("session_router_shutdown".into()).await?;
            return Err(RuntimeError::new(message));
        }
        if let Some(previous) = cached {
            let mut reason = JsString::from("agent_replaced:").units().to_vec();
            reason.extend_from_slice(context.session_key.units());
            let disposal = previous.handle.dispose(JsString::from_utf16(reason));
            self.emit(&context.session_key, EvictionReason::DirtyRecreate);
            disposal.await?;
        }
        Ok(handle)
    }

    pub fn is_current(&self, key: &JsString, handle: &LiveSessionHandle) -> bool {
        self.0
            .state
            .lock()
            .unwrap()
            .records
            .get(key)
            .is_some_and(|record| record.handle.same_identity(handle))
    }

    pub fn current_handle(&self, key: &JsString) -> Option<LiveSessionHandle> {
        self.0
            .state
            .lock()
            .unwrap()
            .records
            .get(key)
            .map(|record| record.handle.clone())
    }

    pub fn has_active_turn(&self, key: &JsString) -> bool {
        let _state = self.0.state.lock().unwrap();
        self.0.turns.admission.has_active_turn(key)
    }

    pub fn begin_completion(&self, key: &JsString, run: &JsString) -> TurnCompletionHandle {
        let mut state = self.0.state.lock().unwrap();
        let completion = self.0.turns.completion.begin(key, run);
        let handle = state.records.get(key).map(|record| record.handle.clone());
        let admission = self.0.turns.admission.turn_identity(key);
        state.completion_owners.insert(
            key.clone(),
            CompletionOwner {
                completion: completion.clone(),
                handle,
                admission,
            },
        );
        completion
    }

    pub fn complete_turn(&self, key: &JsString, handle: &TurnCompletionHandle) {
        let mut state = self.0.state.lock().unwrap();
        let current = self.0.turns.completion.is_current(key, handle);
        self.0.turns.completion.complete(key, handle);
        if current && let Some(owner) = state.completion_owners.remove(key) {
            self.0
                .turns
                .admission
                .end_identity(key, owner.admission.as_ref());
        }
        if let Some(record) = state.records.get_mut(key) {
            record.last_used = (self.0.options.now_ms)();
        }
    }

    /// Full pr1 coordination, with this owner's identity-filtering live sink.
    pub fn abort_turn<'a>(
        &'a self,
        key: &JsString,
        run: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> impl std::future::Future<Output = RuntimeResult> + Send + 'a {
        let result = Arc::new(Mutex::new(Ok(())));
        let sink_result = result.clone();
        let owner = self.clone();
        let target_key = key.clone();
        let operation = self
            .0
            .turns
            .abort_turn(key, run, reason, move |target, reason| {
                *sink_result.lock().unwrap() = owner.abort(&target_key, target.as_ref(), &reason);
                std::future::ready(())
            });
        let result = result.lock().unwrap().clone();
        async move {
            result?;
            operation.await;
            Ok(())
        }
    }

    pub fn begin_turn(&self, key: &JsString, run: &JsString) -> Option<TurnReservation> {
        self.sweep_idle();
        let state = self.0.state.lock().unwrap();
        if state.stopped {
            None
        } else {
            self.0.turns.admission.begin_turn(key, run)
        }
    }

    /// Consumers retain the identity returned by `begin_turn` until cleanup.
    pub fn end_turn(
        &self,
        key: &JsString,
        run: Option<&JsString>,
        reservation: Option<&TurnReservation>,
    ) {
        let mut state = self.0.state.lock().unwrap();
        self.0.turns.admission.end_turn(key, run, reservation);
        if let Some(record) = state.records.get_mut(key) {
            record.last_used = (self.0.options.now_ms)();
        }
    }

    /// The guard owns only admission. Actual seed/compact consumers are A7.5/6.
    pub fn begin_maintenance(&self, key: &JsString) -> Option<MaintenanceReservation> {
        let state = self.0.state.lock().unwrap();
        (!state.stopped && self.0.turns.admission.begin_maintenance(key)).then(|| {
            MaintenanceReservation {
                owner: self.clone(),
                key: key.clone(),
            }
        })
    }

    /// This is the pr1 live abort sink: the requested run is revalidated while
    /// selecting the exact handle under the publication/admission lock. Calling
    /// out after unlock can only affect that handle, never a replacement.
    pub fn abort(
        &self,
        key: &JsString,
        run: Option<&JsString>,
        reason: &JsString,
    ) -> RuntimeResult {
        let target = {
            let mut state = self.0.state.lock().unwrap();
            if let Some(run) = run {
                let active = self.0.turns.admission.active_turn_run_id(key);
                let exact_completion_owner =
                    state.completion_owners.get(key).is_some_and(|owner| {
                        owner.completion.run_id() == run
                            && self.0.turns.completion.is_current(key, &owner.completion)
                            && owner.handle.as_ref().is_some_and(|handle| {
                                state
                                    .records
                                    .get(key)
                                    .is_some_and(|record| record.handle.same_identity(handle))
                            })
                    });
                if active
                    .as_ref()
                    .map_or(!exact_completion_owner, |active| active != run)
                {
                    return Ok(());
                }
            }
            state.records.get_mut(key).map(|record| {
                record.last_used = (self.0.options.now_ms)();
                record.handle.clone()
            })
        };
        target.map_or(Ok(()), |handle| handle.abort(reason))
    }

    pub fn close(&self, key: &JsString) -> BoxFuture<RuntimeResult> {
        let mut state = self.0.state.lock().unwrap();
        if let Some(shutdown) = &state.shutdown {
            return Box::pin(wait_result(shutdown.clone()));
        }
        let ticket = ticket(&mut state, key);
        let (done, completion) = watch::channel(None);
        let project = state
            .records
            .get(key)
            .and_then(|record| record.context.project_key.clone());
        track(&mut state, project, completion.clone());
        let owner = self.clone();
        let key = key.clone();
        tokio::spawn(async move {
            let _exclusive = ticket.enter().await;
            let (record, identity) = {
                let mut state = owner.0.state.lock().unwrap();
                state.order.retain(|item| item != &key);
                state.completion_owners.remove(&key);
                (
                    state.records.remove(&key),
                    owner.0.turns.admission.turn_identity(&key),
                )
            };
            let outcome = if let Some(record) = record {
                let result = owner.evict(&key, record, EvictionReason::Closed).await;
                let _state = owner.0.state.lock().unwrap();
                owner
                    .0
                    .turns
                    .admission
                    .end_identity(&key, identity.as_ref());
                result
            } else {
                Ok(())
            };
            done.send_replace(Some(outcome));
        });
        Box::pin(wait_result(completion))
    }

    /// Pause one project's new publications, drain its in-flight creations and
    /// closes, then close its cached writers. Other projects never join this wait.
    /// TS treats an empty project as unscoped for pause checks, but still matches
    /// it exactly when draining/closing. New empty-project writers remain allowed.
    pub fn close_project(
        &self,
        project: &JsString,
    ) -> BoxFuture<Result<Vec<JsString>, RuntimeError>> {
        let mut state = self.0.state.lock().unwrap();
        state.paused_projects.insert(project.clone());
        let pending: Vec<_> = state
            .pending
            .iter()
            .filter(|pending| pending.project.as_ref() == Some(project))
            .map(|pending| pending.completion.clone())
            .collect();
        let owner = self.clone();
        let project = project.clone();
        let (done, completion) = watch::channel(None);
        tokio::spawn(async move {
            for pending in pending {
                let _ = wait_result(pending).await;
            }
            let keys: Vec<_> = {
                let state = owner.0.state.lock().unwrap();
                state
                    .order
                    .iter()
                    .filter(|key| {
                        state.records[*key].context.project_key.as_ref() == Some(&project)
                    })
                    .cloned()
                    .collect()
            };
            let mut drains = tokio::task::JoinSet::new();
            for key in &keys {
                drains.spawn(owner.close(key));
            }
            while let Some(outcome) = drains.join_next().await {
                if let Err(error) = outcome.expect("project session close panicked") {
                    done.send_replace(Some(Err(error)));
                    return;
                }
            }
            done.send_replace(Some(Ok(keys)));
        });
        Box::pin(wait_result(completion))
    }

    pub fn resume_project(&self, project: &JsString) {
        self.0.state.lock().unwrap().paused_projects.remove(project);
    }

    pub fn sweep_idle(&self) {
        let mut state = self.0.state.lock().unwrap();
        if state.stopped {
            return;
        }
        let now = (self.0.options.now_ms)();
        let idle: Vec<_> = state
            .order
            .iter()
            .filter(|key| {
                let record = &state.records[*key];
                !state
                    .gates
                    .get(*key)
                    .is_some_and(|gate| gate.strong_count() > 0)
                    && !self.0.turns.admission.has_active_turn(key)
                    && !self.0.turns.admission.has_maintenance(key)
                    && now.saturating_sub(record.last_used) > self.0.options.idle_timeout_ms
            })
            .cloned()
            .collect();
        for key in idle {
            let record = state.records.remove(&key).unwrap();
            state.completion_owners.remove(&key);
            state.order.retain(|item| item != &key);
            let ticket = ticket(&mut state, &key);
            let (done, completion) = watch::channel(None);
            track(&mut state, record.context.project_key.clone(), completion);
            let owner = self.clone();
            tokio::spawn(async move {
                let _exclusive = ticket.enter().await;
                done.send_replace(Some(owner.evict(&key, record, EvictionReason::Idle).await));
            });
        }
        state.gates.retain(|_, gate| gate.strong_count() > 0);
    }

    pub fn shutdown(&self) -> BoxFuture<RuntimeResult> {
        let mut state = self.0.state.lock().unwrap();
        if let Some(shutdown) = &state.shutdown {
            return Box::pin(wait_result(shutdown.clone()));
        }
        state.stopped = true;
        state.completion_owners.clear();
        self.0.stopped.send_replace(true);
        self.0.turns.admission.shutdown();
        let pending: Vec<_> = std::mem::take(&mut state.pending)
            .into_iter()
            .filter(|pending| pending.completion.borrow().is_none())
            .collect();
        let order = std::mem::take(&mut state.order);
        let records: Vec<_> = order
            .into_iter()
            .filter_map(|key| state.records.remove(&key).map(|record| (key, record)))
            .collect();
        let (done, completion) = watch::channel(None);
        state.shutdown = Some(completion.clone());
        let owner = self.clone();
        tokio::spawn(async move {
            let mut failures = Vec::new();
            for pending in pending {
                if let Err(error) = wait_result(pending.completion).await {
                    failures.push(error);
                }
            }
            // Start every disposal before waiting for any one writer to drain.
            let mut drains = Vec::new();
            for (key, record) in records {
                let owner = owner.clone();
                drains.push(tokio::spawn(async move {
                    owner.evict(&key, record, EvictionReason::Shutdown).await
                }));
            }
            for drain in drains {
                if let Err(error) = drain.await.expect("session disposal panicked") {
                    failures.push(error);
                }
            }
            done.send_replace(Some(aggregate(
                "Failed to shut down all agent sessions.",
                failures,
            )));
        });
        Box::pin(wait_result(completion))
    }

    async fn evict(&self, key: &JsString, record: Record, reason: EvictionReason) -> RuntimeResult {
        let label = match reason {
            EvictionReason::Idle => "idle",
            EvictionReason::Closed => "closed",
            EvictionReason::DirtyRecreate => "dirty_recreate",
            EvictionReason::Shutdown => "shutdown",
        };
        let result = record
            .handle
            .dispose(format!("session_{label}").into())
            .await;
        self.emit(key, reason);
        result
    }

    fn emit(&self, key: &JsString, reason: EvictionReason) {
        if let Some(callback) = &self.0.options.on_evict {
            callback(key, reason);
        }
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;

// A ticket is reserved at method call time, not on first poll. This preserves
// same-key close/create order even if executor threads poll tasks in reverse.
struct Serial {
    tail: Mutex<watch::Receiver<bool>>,
}
struct Ticket {
    _serial: Arc<Serial>,
    previous: watch::Receiver<bool>,
    done: watch::Sender<bool>,
}
impl Ticket {
    async fn enter(mut self) -> Self {
        let _ = self.previous.wait_for(|done| *done).await;
        self
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.done.send_replace(true);
    }
}
fn ticket(state: &mut State, key: &JsString) -> Ticket {
    let serial = state
        .gates
        .get(key)
        .and_then(Weak::upgrade)
        .unwrap_or_else(|| {
            let serial = Arc::new(Serial {
                tail: Mutex::new(watch::channel(true).1),
            });
            state.gates.insert(key.clone(), Arc::downgrade(&serial));
            serial
        });
    let (done, next) = watch::channel(false);
    let previous = std::mem::replace(&mut *serial.tail.lock().unwrap(), next);
    Ticket {
        _serial: serial,
        previous,
        done,
    }
}

fn project_paused(state: &State, project: Option<&JsString>) -> bool {
    project.is_some_and(|project| !project.is_empty() && state.paused_projects.contains(project))
}

fn track(
    state: &mut State,
    project: Option<JsString>,
    completion: watch::Receiver<Option<RuntimeResult>>,
) {
    state
        .pending
        .retain(|pending| pending.completion.borrow().is_none());
    state.pending.push(Pending {
        project,
        completion,
    });
}

pub struct MaintenanceReservation {
    owner: SessionOwner,
    key: JsString,
}
impl Drop for MaintenanceReservation {
    fn drop(&mut self) {
        let mut state = self.owner.0.state.lock().unwrap();
        self.owner.0.turns.admission.end_maintenance(&self.key);
        if let Some(record) = state.records.get_mut(&self.key) {
            record.last_used = (self.owner.0.options.now_ms)();
        }
    }
}

impl RefreshRouter for SessionOwner {
    fn mark_all_dirty(&self, _reason: &str) -> usize {
        let mut state = self.0.state.lock().unwrap();
        for record in state.records.values_mut() {
            record.dirty = true;
        }
        state.records.len()
    }
    fn mark_project_dirty(&self, project: &JsString, _reason: &str) -> usize {
        let mut state = self.0.state.lock().unwrap();
        let mut count = 0;
        for record in state.records.values_mut() {
            if record.context.project_key.as_ref() == Some(project) {
                record.dirty = true;
                count += 1;
            }
        }
        count
    }
    fn cached_session_count(&self) -> usize {
        self.0.state.lock().unwrap().records.len()
    }
    fn snapshot_session(&self, key: &JsString) -> Option<JsValue> {
        let handle = self
            .0
            .state
            .lock()
            .unwrap()
            .records
            .get(key)
            .map(|record| record.handle.clone());
        handle.and_then(|handle| handle.snapshot().get("messages").cloned())
    }
}
