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

/// Diagnostic data from the evicted record, sampled after disposal and on_evict.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionEvictionSnapshot {
    pub session_key: JsString,
    pub last_used_at: i64,
    pub context: SessionContext,
    /// Usually a number; preserve JS `messages.length` for dynamic snapshots.
    pub message_count: Option<JsValue>,
}

type IdleEvict = Arc<dyn Fn(&JsString, &SessionEvictionSnapshot) + Send + Sync>;
type Evict = Arc<dyn Fn(&JsString, EvictionReason) + Send + Sync>;
pub struct SessionOwnerOptions {
    pub factory: Arc<dyn SessionFactory>,
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub idle_timeout_ms: i64,
    /// Zero disables the periodic sweep (for an externally driven clock).
    pub idle_sweep_interval_ms: u64,
    pub on_evict: Option<Evict>,
    /// Runs outside owner locks, after on_evict, only for idle eviction (also
    /// after disposal errors). Callbacks must not panic, as with on_evict.
    pub on_session_idle_evict: Option<IdleEvict>,
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
            // TS SessionRouter.ts:74-75: 30 min idle timeout, 60 s sweep interval.
            idle_timeout_ms: 30 * 60 * 1000,
            idle_sweep_interval_ms: 60 * 1000,
            on_evict: None,
            on_session_idle_evict: None,
        }
    }
}

#[derive(Clone)]
struct Record {
    handle: LiveSessionHandle,
    context: SessionContext,
    last_used: i64,
    dirty: bool,
    unreported_creation_error: Option<Arc<RuntimeError>>,
}

#[derive(Clone)]
struct CreationOutcome {
    outcome: Result<LiveSessionHandle, RuntimeError>,
    unreported_error: Option<Arc<RuntimeError>>,
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
    /// TS closingSessions: each key's in-flight eviction completion, from a
    /// close task, the idle sweep or a shutdown drain. A record-less close
    /// awaits the entry; a settled entry behaves as removed.
    closings: HashMap<JsString, watch::Receiver<Option<RuntimeResult>>>,
    paused_projects: HashSet<JsString>,
    shutdown: Option<watch::Receiver<Option<RuntimeResult>>>,
}

struct Inner {
    state: Mutex<State>,
    turns: SessionTurns,
    options: SessionOwnerOptions,
    stopped: watch::Sender<bool>,
    #[cfg(test)]
    creation_sent: watch::Sender<usize>,
}

#[derive(Clone)]
pub struct SessionOwner(Arc<Inner>);

impl SessionOwner {
    #[cfg(test)]
    pub(super) fn creations_sent_for_test(&self) -> watch::Receiver<usize> {
        self.0.creation_sent.subscribe()
    }

    pub fn new(options: SessionOwnerOptions) -> Self {
        let interval = options.idle_sweep_interval_ms;
        let owner = Self(Arc::new(Inner {
            state: Mutex::new(State::default()),
            turns: SessionTurns::default(),
            options,
            stopped: watch::channel(false).0,
            #[cfg(test)]
            creation_sent: watch::channel(0).0,
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
        let receiver_owner = self.clone();
        let key = context.session_key.clone();
        tokio::spawn(async move {
            let _exclusive = ticket.enter().await;
            let pending = owner
                .0
                .state
                .lock()
                .unwrap()
                .records
                .get_mut(&context.session_key)
                .and_then(|record| {
                    // #1055 ①: a cancelled caller may leave a transient BUSY
                    // behind. Judge the disposal outcome FIRST: a drain that
                    // already ended in an unreported cleanup error hands that
                    // original error to THIS caller instead of replaying one
                    // more BUSY round; only a clean (or already reported) drain
                    // makes the stale BUSY retryable. Cleanup errors must
                    // still reach the public caller, even if the factory
                    // worker has already acknowledged them.
                    let pending_busy = record
                        .unreported_creation_error
                        .as_ref()
                        .is_some_and(|error| pending_is_busy(error));
                    #[cfg(test)]
                    let pending_busy =
                        pending_busy && option_env!("PDRS_1022_R3_REPLAY_DRAINED_BUSY").is_none();
                    if pending_busy
                        && let Some(error) = record.handle.take_recreation_disposal_error()
                    {
                        record.unreported_creation_error = Some(Arc::new(error));
                    } else if pending_busy && record.handle.recreation_drained() {
                        record.unreported_creation_error.take();
                    }
                    record.unreported_creation_error.clone()
                });
            let outcome = if let Some(error) = &pending {
                Err((**error).clone())
            } else {
                owner.create_active(context.clone()).await
            };
            // The factory waiter is this detached worker, not the public
            // caller. Retain an error on the old record until that caller
            // consumes it, including send-success/receiver-drop races.
            let unreported_error = if pending.is_some() {
                // A concurrent waiter may already have acknowledged this token.
                // Replaying it must not put it back into the record.
                pending
            } else if let Err(error) = &outcome {
                let mut state = owner.0.state.lock().unwrap();
                state.records.get_mut(&context.session_key).map(|record| {
                    let error = Arc::new(error.clone());
                    record.unreported_creation_error = Some(error.clone());
                    error
                })
            } else {
                None
            };
            result.send_replace(Some(Ok(CreationOutcome {
                outcome,
                unreported_error,
            })));
            #[cfg(test)]
            owner.0.creation_sent.send_modify(|count| *count += 1);
            // Failed creation is reported to its caller, not a shutdown error.
            done.send_replace(Some(Ok(())));
        });
        Box::pin(async move {
            let delivered = wait_result(completion).await?;
            if let Some(error) = delivered.unreported_error {
                let mut state = receiver_owner.0.state.lock().unwrap();
                if let Some(record) = state.records.get_mut(&key)
                    && record
                        .unreported_creation_error
                        .as_ref()
                        .is_some_and(|pending| Arc::ptr_eq(pending, &error))
                {
                    record.unreported_creation_error = None;
                }
            }
            delivered.outcome
        })
    }

    async fn create_active(
        &self,
        context: SessionContext,
    ) -> Result<LiveSessionHandle, RuntimeError> {
        // #1020: one restricted retry for a stale previous handoff. The
        // factory marks a previous that was already Disposed when it took it
        // over; such a rejection means the same-key disposal completed after
        // the selection above read the handle state, so re-running the
        // synchronized selection drops the dead handle and resumes fresh.
        #[cfg_attr(test, allow(unused_mut))]
        let mut stale_handoff_retry = true;
        #[cfg(test)]
        let mut stale_handoff_retry =
            stale_handoff_retry && option_env!("PDRS_1020_SKIP_STALE_RETRY").is_none();
        let (cached, context, previous, handle) = loop {
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
                .map_or_else(|| context.clone(), |record| record.context.clone());
            let previous = cached
                .as_ref()
                .filter(|record| {
                    record.handle.state() != super::HandleState::Disposed
                        || record.handle.retains_recreation_source()
                })
                .map(|record| record.handle.clone());
            match self
                .0
                .options
                .factory
                .create(context.clone(), previous.clone())
                .await
            {
                Ok(handle) => break (cached, context, previous, handle),
                Err(error) => {
                    if stale_handoff_retry
                        && previous
                            .as_ref()
                            .is_some_and(LiveSessionHandle::recreation_stale_handoff)
                    {
                        #[cfg(test)]
                        let keep_budget = option_env!("PDRS_1020_UNLIMITED_STALE_RETRY").is_some();
                        #[cfg(not(test))]
                        let keep_budget = false;
                        if !keep_budget {
                            stale_handoff_retry = false;
                        }
                        continue;
                    }
                    return Err(error);
                }
            }
        };
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
                    js_error: None,
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
                        // TS SessionRouter.ts:144-166 clears dirty after replace, including marks during recreation.
                        dirty: false,
                        unreported_creation_error: None,
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
            let already_drained = previous.handle.recreation_drained();
            let mut reason = JsString::from("agent_replaced:").units().to_vec();
            reason.extend_from_slice(context.session_key.units());
            let disposal = previous.handle.dispose(JsString::from_utf16(reason));
            self.emit(&context.session_key, EvictionReason::DirtyRecreate);
            if !already_drained {
                disposal.await?;
            }
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

    pub fn active_turn_run_id(&self, key: &JsString) -> Option<JsString> {
        let _state = self.0.state.lock().unwrap();
        self.0.turns.admission.active_turn_run_id(key)
    }

    pub fn has_active_turn(&self, key: &JsString) -> bool {
        let _state = self.0.state.lock().unwrap();
        self.0.turns.admission.has_active_turn(key)
    }

    /// Project-scoped user admission, excluding Always-On and cron sessions.
    /// A reservation without a cached record, maintenance, or a handle activity
    /// alone is not a user turn. This query does not refresh or sweep sessions.
    pub fn has_active_user_turn(&self, project: &JsString) -> bool {
        let state = self.0.state.lock().unwrap();
        let always_on = JsString::from("always-on/");
        let cron = JsString::from("cron:");
        state.records.iter().any(|(key, record)| {
            !key.units().starts_with(always_on.units())
                && !key.units().starts_with(cron.units())
                && record.context.project_key.as_ref() == Some(project)
                && self.0.turns.admission.has_active_turn(key)
        })
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

    /// Check the gateway consumer's exact fence identity, independently of
    /// admission/runId reuse. A stale pump must stop projecting native events.
    pub fn is_current_completion(&self, key: &JsString, handle: &TurnCompletionHandle) -> bool {
        let _state = self.0.state.lock().unwrap();
        self.0.turns.completion.is_current(key, handle)
    }

    pub fn aborted_turn_reason(&self, key: &JsString, run: &JsString) -> Option<JsString> {
        self.0.turns.aborted_reason(key, run)
    }

    pub fn clear_turn_abort_reason(&self, key: &JsString, run: &JsString) {
        self.0.turns.clear_aborted_reason(key, run);
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

    /// Start the real cancel/Router abort without waiting on the owner fence.
    /// The connection selects its consumer fence at the Router await boundary,
    /// before cleanup can admit another turn.
    pub(crate) fn start_gateway_abort(
        &self,
        key: &JsString,
        run: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> Result<bool, RuntimeError> {
        self.0
            .turns
            .start_abort(key, run, reason, |target, reason| {
                self.abort(key, target.as_ref(), &reason)
            })
            .transpose()
            .map(|result| result.is_some())
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
    pub fn begin_maintenance(
        &self,
        key: &JsString,
    ) -> Result<Option<MaintenanceReservation>, RuntimeError> {
        let state = self.0.state.lock().unwrap();
        if state.stopped {
            return Err(RuntimeError::new("Session router is shut down."));
        }
        Ok(self
            .0
            .turns
            .admission
            .begin_maintenance(key)
            .then(|| MaintenanceReservation {
                owner: self.clone(),
                key: key.clone(),
            }))
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
        // TS SessionRouter.close's else branch (SessionRouter.ts:327-338): a
        // key without a cached record — evicted by an in-flight close, idle
        // sweep or shutdown drain, or after the router shut down — awaits the
        // key's registered in-flight eviction and transmits that eviction's
        // own result; no entry means `await undefined`: fulfilled. The
        // whole-shutdown aggregate stays shutdown()'s own answer
        // (A7.2-SESSION-CLOSE-DURING-SHUTDOWN-001, #1245).
        if let Some(closing) = unsettled_closing(&mut state, key) {
            return Box::pin(wait_result(closing));
        }
        if state.shutdown.is_some() {
            return Box::pin(async { Ok(()) });
        }
        let ticket = ticket(&mut state, key);
        let (done, completion) = watch::channel(None);
        let project = state
            .records
            .get(key)
            .and_then(|record| record.context.project_key.clone());
        track(&mut state, project, completion.clone());
        state.closings.insert(key.clone(), completion.clone());
        let settle = done.clone();
        let owner = self.clone();
        let key = key.clone();
        super::super::cleanup_task::spawn_with_error(
            &tokio::runtime::Handle::current(),
            done,
            RuntimeError::new("Session close task was dropped or panicked before completion."),
            async move {
                // #1245 settle-before-release: the serial ticket moves into
                // the closing guard, whose `Drop` writes the key's closing
                // entry strictly before the ticket is released — after the
                // release a queued same-key get_or_create may publish a new
                // record, and shutdown's skip must never see this entry
                // unsettled then, or that record would escape every drain
                // list. TS has no such gap: getOrCreateActive awaits
                // closingSessions first. On a panic or abort no outcome is
                // staged, so the outer `Completion` guard settles this entry
                // with its dropped error after the ticket's release instead.
                let mut closing = ClosingGuard::new(settle, Some(ticket.enter().await));
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
                    // #1055 ②: an evicted record's unconsumed creation or
                    // cleanup error would otherwise die with it; the closing
                    // caller receives it, aggregated beside a failed
                    // disposal's own error.
                    let pending = record.unreported_creation_error.clone();
                    let result = owner.evict(&key, record, EvictionReason::Closed).await;
                    let _state = owner.0.state.lock().unwrap();
                    owner
                        .0
                        .turns
                        .admission
                        .end_identity(&key, identity.as_ref());
                    combine_pending_error(result, pending)
                } else {
                    Ok(())
                };
                closing.settle(outcome)
            },
        );
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
        super::super::cleanup_task::spawn_with_error(
            &tokio::runtime::Handle::current(),
            done,
            RuntimeError::new(
                "Session project close task was dropped or panicked before completion.",
            ),
            async move {
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
                    outcome.expect("project session close panicked")?;
                }
                Ok(keys)
            },
        );
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
                // Idle eviction has no caller to receive an abandoned error.
                // #1055 ②: a retriable BUSY is reclaimable once the drain it
                // waited on ended clean — after that point no cleanup error can
                // surface from this record. Non-retriable errors stay until a
                // caller consumes them or close/shutdown delivers them.
                let no_pending_error = match record.unreported_creation_error.as_ref() {
                    None => true,
                    Some(pending) => pending_is_busy(pending) && record.handle.recreation_drained(),
                };
                #[cfg(test)]
                let no_pending_error =
                    no_pending_error || option_env!("PDRS_1022_R3_SWEEP_PENDING_ERROR").is_some();
                !state
                    .gates
                    .get(*key)
                    .is_some_and(|gate| gate.strong_count() > 0)
                    && no_pending_error
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
            track(
                &mut state,
                record.context.project_key.clone(),
                completion.clone(),
            );
            state.closings.insert(key.clone(), completion);
            let owner = self.clone();
            let settle = done.clone();
            super::super::cleanup_task::spawn_with_error(
                &tokio::runtime::Handle::current(),
                done,
                RuntimeError::new(
                    "Session idle eviction task was dropped or panicked before completion.",
                ),
                async move {
                    // Same settle-before-release invariant as the close task:
                    // a record published behind this key's ticket must never
                    // land while this closing entry is still unsettled. The
                    // guard writes the entry, then releases the ticket; a
                    // panic or abort leaves the entry to the outer
                    // `Completion` guard instead.
                    let mut closing = ClosingGuard::new(settle, Some(ticket.enter().await));
                    let outcome = owner.evict(&key, record, EvictionReason::Idle).await;
                    closing.settle(outcome)
                },
            );
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
        let mut records = Vec::new();
        for key in order {
            // TS close deletes the key's record synchronously at call time
            // (SessionRouter.ts:329), so a close that registered its eviction
            // but has not yet taken the record — its task is parked behind
            // the serial gate or not yet polled — still owns that eviction.
            // The drain list must not steal the record: the close keeps its
            // own result and `closed` reason, and shutdown aggregates its
            // failure through the pending drain (#1245 fix-r1). The
            // recreation-serialization divergence (A7.2-SESSION-RECREATE-
            // DRAIN-001) keeps the record available for such a close.
            if unsettled_closing(&mut state, &key).is_some() {
                continue;
            }
            if let Some(record) = state.records.remove(&key) {
                records.push((key, record));
            }
        }
        let (done, completion) = watch::channel(None);
        state.shutdown = Some(completion.clone());
        let owner = self.clone();
        super::super::cleanup_task::spawn_with_error(
            &tokio::runtime::Handle::current(),
            done,
            RuntimeError::new("Session shutdown task was dropped or panicked before completion."),
            async move {
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
                    // TS finishShutdown's beginEviction registers each key in
                    // closingSessions as its eviction starts, so a close that
                    // arrives mid-flight transmits this drain's own result
                    // instead of the whole-shutdown aggregate.
                    let (drain_done, drain_completion) = watch::channel(None);
                    owner
                        .0
                        .state
                        .lock()
                        .unwrap()
                        .closings
                        .insert(key.clone(), drain_completion);
                    drains.push(drain_done.subscribe());
                    // Same completion guard as the close and idle tasks: a
                    // panicked or dropped disposal settles the key's closing
                    // entry with the dropped error instead of leaving it
                    // unsettled, so the aggregate still carries the failure
                    // and a later close(key) falls back to the post-shutdown
                    // branch (TS: `await closingSessions.get(key)` after
                    // finishEviction removed the entry → undefined →
                    // fulfilled).
                    super::super::cleanup_task::spawn_with_error(
                        &tokio::runtime::Handle::current(),
                        drain_done,
                        RuntimeError::new(
                            "Session disposal task was dropped or panicked before completion.",
                        ),
                        async move {
                            // #1055 ②: fold each record's unconsumed creation or
                            // cleanup error into the shutdown aggregate, aggregated
                            // beside a failed disposal's own error, so neither is
                            // dropped.
                            let pending = record.unreported_creation_error.clone();
                            combine_pending_error(
                                owner.evict(&key, record, EvictionReason::Shutdown).await,
                                pending,
                            )
                        },
                    );
                }
                for drain in drains {
                    if let Err(error) = wait_result(drain).await {
                        failures.push(error);
                    }
                }
                aggregate("Failed to shut down all agent sessions.", failures)
            },
        );
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
        if reason == EvictionReason::Idle
            && let Some(callback) = &self.0.options.on_session_idle_evict
        {
            // TS catches a failed snapshot and still emits the other fields.
            let message_count = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let snapshot = record.handle.snapshot();
                match snapshot.as_object()?.get_property("messages")? {
                    JsValue::Array(messages) => Some(JsValue::Number(messages.len() as f64)),
                    JsValue::String(text) => Some(JsValue::Number(text.units().len() as f64)),
                    other => other
                        .as_object()?
                        .get_property("length")
                        .filter(|v| !v.is_undefined())
                        .cloned(),
                }
            }))
            .ok()
            .flatten();
            callback(
                key,
                &SessionEvictionSnapshot {
                    session_key: key.clone(),
                    last_used_at: record.last_used,
                    context: record.context,
                    message_count,
                },
            );
        }
        result
    }

    fn emit(&self, key: &JsString, reason: EvictionReason) {
        if let Some(callback) = &self.0.options.on_evict {
            callback(key, reason);
        }
    }
}

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
        #[cfg(test)]
        settle_order::record("ticket-released");
        self.done.send_replace(true);
    }
}

/// #1245 settle-before-release: the key's closing entry must settle strictly
/// before the serial ticket is released. Holding the ticket inside this guard
/// makes the settle write and the release one drop sequence — `Drop` writes
/// the staged outcome while `exclusive` is still a live field, and the field
/// is dropped only after the `Drop` body returns — so no ordering slip at the
/// call site can reopen the window where a queued same-key creation publishes
/// a record that a later shutdown's unsettled-closing skip would never drain.
/// A task that panics or is aborted stages no outcome: the guard then releases
/// the ticket without settling, and the outer `Completion` guard settles the
/// entry with its dropped error after the release.
struct ClosingGuard {
    settle: watch::Sender<Option<RuntimeResult>>,
    outcome: Option<RuntimeResult>,
    exclusive: Option<Ticket>,
}
impl ClosingGuard {
    fn new(settle: watch::Sender<Option<RuntimeResult>>, exclusive: Option<Ticket>) -> Self {
        Self {
            settle,
            outcome: None,
            exclusive,
        }
    }
    /// Stage the outcome for the settle-on-drop; the closing entry itself is
    /// written in `Drop`, before the ticket is released.
    fn settle(&mut self, outcome: RuntimeResult) -> RuntimeResult {
        self.outcome = Some(outcome.clone());
        outcome
    }
}
impl Drop for ClosingGuard {
    fn drop(&mut self) {
        if let Some(outcome) = self.outcome.take() {
            #[cfg(test)]
            settle_order::record("closing-settled");
            self.settle.send_replace(Some(outcome));
        }
        // Released only after the settle write.
        self.exclusive = None;
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

/// #1055 ②: only the factory's transient drain-timeout error is retriable.
/// Cleanup, prepare and setup errors stay with the record until a caller or a
/// close/shutdown consumes them.
fn pending_is_busy(error: &RuntimeError) -> bool {
    error
        .js_error
        .as_ref()
        .and_then(|error| error.code.as_deref())
        == Some("SESSION_WRITER_BUSY")
}

/// #1055 ②: after an eviction removed the record, an unconsumed non-retriable
/// creation or cleanup error has no other delivery path left. Return it to
/// the evicting caller when the disposal itself succeeded, and aggregate it
/// beside the disposal error when it failed, so neither is dropped (both may
/// stem from the same cleanup failure and then appear twice). A retriable
/// BUSY is never delivered by close/shutdown.
fn combine_pending_error(
    result: RuntimeResult,
    pending: Option<Arc<RuntimeError>>,
) -> RuntimeResult {
    match (result, pending) {
        (Ok(()), Some(pending)) if !pending_is_busy(&pending) => Err((*pending).clone()),
        (Err(error), Some(pending)) if !pending_is_busy(&pending) => aggregate(
            "Failed to dispose the agent session and report its unconsumed creation or cleanup error.",
            vec![error, (*pending).clone()],
        ),
        (result, _) => result,
    }
}

fn track(
    state: &mut State,
    project: Option<JsString>,
    completion: watch::Receiver<Option<RuntimeResult>>,
) {
    state
        .pending
        .retain(|pending| pending.completion.borrow().is_none());
    state
        .closings
        .retain(|_, closing| closing.borrow().is_none());
    state.pending.push(Pending {
        project,
        completion,
    });
}

/// The in-flight eviction a record-less close awaits (TS closingSessions). A
/// settled entry is finishEviction's removal: every entry owner — a close
/// task, the idle sweep or a shutdown drain — runs under a completion guard
/// that settles the entry even on panic or drop, and the close/idle tasks
/// settle before releasing their serial ticket, so a settled entry never
/// hides a later same-key publication from shutdown's drain list
/// (#1245 fix-r2).
fn unsettled_closing(
    state: &mut State,
    key: &JsString,
) -> Option<watch::Receiver<Option<RuntimeResult>>> {
    state
        .closings
        .get(key)
        .filter(|closing| closing.borrow().is_none())
        .cloned()
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
    fn mark_all_dirty(&self, reason: &str) -> usize {
        let mut state = self.0.state.lock().unwrap();
        for record in state.records.values_mut() {
            record.dirty = !reason.is_empty();
        }
        state.records.len()
    }
    fn mark_project_dirty(&self, project: &JsString, reason: &str) -> usize {
        let mut state = self.0.state.lock().unwrap();
        let mut count = 0;
        for record in state.records.values_mut() {
            if record.context.project_key.as_ref() == Some(project) {
                record.dirty = !reason.is_empty();
                count += 1;
            }
        }
        count
    }
    fn mark_session_dirty(&self, key: &JsString, reason: &str) -> bool {
        let mut state = self.0.state.lock().unwrap();
        let Some(record) = state.records.get_mut(key) else {
            return false;
        };
        record.dirty = !reason.is_empty();
        true
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

/// Test-only ordering probe for #1245 settle-before-release: records the
/// settle write and every serial-ticket release, in execution order, on the
/// running thread. The guarded window sits between two non-awaiting code
/// spans, so no external task can gate on it; the recorded order is the one
/// deterministic witness that the settle happens while the ticket is held.
#[cfg(test)]
mod settle_order {
    use std::cell::RefCell;

    thread_local! {
        static EVENTS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(event: &'static str) {
        EVENTS.with(|events| events.borrow_mut().push(event));
    }

    pub(super) fn take() -> Vec<&'static str> {
        EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
    }
}

#[cfg(test)]
#[path = "owner_tests.rs"]
mod tests;
