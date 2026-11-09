//! Live writer identity, admission cutoff and resource drain. This is the
//! lifecycle seam for an AgentSession adapter, not a substitute agent loop.
use super::super::project::{DisposeFuture, RuntimeError, RuntimeLease, RuntimeResult};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use std::{future::Future, pin::Pin};
use tokio::sync::watch;

/// One awaited native stream step.
pub type SessionNext<'a> =
    Pin<Box<dyn Future<Output = Result<Option<JsValue>, RuntimeError>> + Send + 'a>>;

/// Native AgentEvent stream, including SessionEnd hooks after turn_completed.
/// input_accepted MUST follow the session's successful recordAcceptedInput;
/// a failed write yields the native failure/status/completion events instead.
/// The host adapter owns that writer (A7.2-pr6); submit never duplicates it.
pub trait LiveTurn: Send {
    fn next(&mut self) -> SessionNext<'_>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionQueuedTurn {
    pub item_id: JsString,
    pub turn_id: JsString,
    pub input: JsValue,
    pub submit_options: JsValue,
}

impl SessionQueuedTurn {
    pub(crate) fn restore(entries: &[JsValue]) -> Result<Vec<Self>, RuntimeError> {
        let mut pending = Vec::new();
        for entry in entries {
            let kind = entry
                .get("type")
                .and_then(JsValue::as_string)
                .and_then(|kind| kind.to_utf8().ok())
                .unwrap_or_default();
            if kind == "agent_turn_enqueued" {
                pending.push(Self {
                    item_id: required_id(entry, "itemId")?,
                    turn_id: required_id(entry, "turnId")?,
                    input: entry
                        .get("input")
                        .cloned()
                        .ok_or_else(|| RuntimeError::new("Queued turn input is missing."))?,
                    submit_options: entry
                        .get("submitOptions")
                        .cloned()
                        .ok_or_else(|| RuntimeError::new("Queued turn options are missing."))?,
                });
                continue;
            }
            let claimed = if kind == "agent_turn_discarded" {
                entry.get("itemId").and_then(JsValue::as_string)
            } else if kind == "turn_started" {
                entry.get("inboxItemId").and_then(JsValue::as_string)
            } else {
                None
            };
            let Some(item_id) = claimed else {
                continue;
            };
            let turn_id = required_id(entry, "turnId")?;
            let index = pending
                .iter()
                .position(|turn: &Self| turn.item_id == *item_id && turn.turn_id == turn_id)
                .ok_or_else(|| RuntimeError::new("Queued turn lifecycle is invalid."))?;
            if kind == "turn_started" && index != 0 {
                return Err(RuntimeError::new("Queued turn lifecycle is not FIFO."));
            }
            pending.remove(index);
        }
        Ok(pending)
    }
}

/// Implementations own the real session writer and its resources. No default
/// no-op implementation is supplied for unavailable providers.
pub trait LiveSession: Send + Sync {
    fn steer(
        &self,
        _turn_id: JsString,
        _message: crate::runtime::steer::SteerMessage,
    ) -> crate::host::HostFuture<crate::runtime::steer::EnqueueResult> {
        Box::pin(async {
            Ok(crate::runtime::steer::EnqueueResult {
                accepted: false,
                reason: Some(crate::runtime::steer::SteerReason::NoActiveTurn),
            })
        })
    }
    fn cancel_steer(
        &self,
        _turn_id: JsString,
        _item_id: JsString,
    ) -> crate::host::HostFuture<crate::runtime::steer::CancelResult> {
        Box::pin(async {
            Ok(crate::runtime::steer::CancelResult {
                cancelled: false,
                reason: Some(crate::runtime::steer::SteerReason::NoActiveTurn),
            })
        })
    }
    /// Explicit capability error preserves compatibility with lifecycle-only
    /// adapters; it cannot silently acknowledge input or start a fake loop.
    fn submit(
        &self,
        _input: JsValue,
        _options: JsValue,
    ) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        Err(RuntimeError::new("Agent session submit is unavailable."))
    }
    fn pending_queued_turns(&self) -> Result<Vec<SessionQueuedTurn>, RuntimeError> {
        Ok(Vec::new())
    }
    fn queued_input_template(&self) -> Option<JsValue> {
        None
    }
    fn enqueue_queued_turn(&self, _turn: &SessionQueuedTurn) -> RuntimeResult {
        Err(RuntimeError::new("Agent session follow-up is unavailable."))
    }
    fn submit_queued_turn(
        &self,
        _turn: &SessionQueuedTurn,
    ) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        Err(RuntimeError::new("Agent session follow-up is unavailable."))
    }
    fn discard_queued_turns(&self, _reason: &str) -> RuntimeResult {
        Ok(())
    }
    fn snapshot(&self) -> JsValue;
    /// May be called repeatedly while `dispose()` is in progress and after its
    /// future has completed: the handle remains Draining through the project
    /// lease release and its own session disposal. Implementations must make
    /// cancellation idempotent and must not panic, including after session
    /// resources have been disposed.
    /// The A7.2-pr6 adapter must make an active LiveTurn settle after abort
    /// (terminal then None, or Err), including session lifecycle hooks. Submit
    /// preserves the native terminal path; wall-clock timeout is its independent
    /// escape for a provider that does not settle. Test this contract in pr6.
    /// Only a fully Disposed handle suppresses subsequent external aborts.
    /// This matches TS AgentHandle.ts:176-179,281-287.
    fn abort(&self, reason: &JsString) -> RuntimeResult;
    /// Runs once immediately after the handle stops admitting work and before
    /// it waits for its own active turn. Scoped owners cancel/drain descendants
    /// here so a parent turn awaiting a child cannot deadlock disposal.
    fn dispose_start(&self, _reason: &JsString) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
    fn dispose(&self) -> DisposeFuture<'_>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandleState {
    Active,
    Draining,
    Disposed,
}

struct Progress {
    state: HandleState,
    activity: bool,
    maintenance: bool,
    cancelled: Option<JsString>,
    disposal: Option<watch::Receiver<Option<RuntimeResult>>>,
}

struct Inner {
    session: Arc<dyn LiveSession>,
    lease: Mutex<Option<RuntimeLease>>,
    progress: Mutex<Progress>,
    admission: Mutex<()>,
    queued: Mutex<QueuedProgress>,
    changed: watch::Sender<u64>,
}

struct QueuedProgress {
    pending: VecDeque<SessionQueuedTurn>,
    pump_running: bool,
    pump_blocked: bool,
}

/// Cloning preserves object identity. A project lease is held until the writer
/// has drained; it is never used as a handle ID or a connection generation.
#[derive(Clone)]
pub struct LiveSessionHandle(Arc<Inner>);

impl LiveSessionHandle {
    pub fn new(session: Arc<dyn LiveSession>, lease: Option<RuntimeLease>) -> Self {
        let pending = session.pending_queued_turns().unwrap_or_default();
        Self(Arc::new(Inner {
            session,
            lease: Mutex::new(lease),
            progress: Mutex::new(Progress {
                state: HandleState::Active,
                activity: false,
                maintenance: false,
                cancelled: None,
                disposal: None,
            }),
            admission: Mutex::new(()),
            queued: Mutex::new(QueuedProgress {
                pending: pending.into(),
                pump_running: false,
                pump_blocked: false,
            }),
            changed: watch::channel(0).0,
        }))
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn state(&self) -> HandleState {
        self.0.progress.lock().unwrap().state
    }

    /// Publication may revoke the old handle while holding the owner lock.
    /// This cutoff calls no session code; dispose performs cancellation/drain
    /// after that lock has been released.
    pub(super) fn stop_new(&self) {
        let mut progress = self.0.progress.lock().unwrap();
        if progress.state == HandleState::Active {
            progress.state = HandleState::Draining;
        }
        drop(progress);
        self.0.changed.send_modify(|version| *version += 1);
    }

    pub fn snapshot(&self) -> JsValue {
        self.0.session.snapshot()
    }

    pub fn steer(
        &self,
        turn_id: JsString,
        message: crate::runtime::steer::SteerMessage,
    ) -> crate::host::HostFuture<crate::runtime::steer::EnqueueResult> {
        if self.state() != HandleState::Active {
            return Box::pin(async {
                Ok(crate::runtime::steer::EnqueueResult {
                    accepted: false,
                    reason: Some(crate::runtime::steer::SteerReason::NoActiveTurn),
                })
            });
        }
        self.0.session.steer(turn_id, message)
    }

    pub fn cancel_steer(
        &self,
        turn_id: JsString,
        item_id: JsString,
    ) -> crate::host::HostFuture<crate::runtime::steer::CancelResult> {
        if self.state() != HandleState::Active {
            return Box::pin(async {
                Ok(crate::runtime::steer::CancelResult {
                    cancelled: false,
                    reason: Some(crate::runtime::steer::SteerReason::NoActiveTurn),
                })
            });
        }
        self.0.session.cancel_steer(turn_id, item_id)
    }

    /// A live consumer holds this guard for the entire turn/maintenance future.
    /// Dropping a cancelled consumer releases drain even on an error path.
    pub fn begin_activity(&self) -> Result<HandleActivity, RuntimeError> {
        self.reserve_activity(false)
    }

    pub fn begin_maintenance(&self) -> Result<HandleActivity, RuntimeError> {
        self.reserve_activity(true)
    }

    fn reserve_activity(&self, maintenance: bool) -> Result<HandleActivity, RuntimeError> {
        let mut progress = self.0.progress.lock().unwrap();
        if progress.state != HandleState::Active {
            return Err(RuntimeError::new("Agent handle is not active."));
        }
        if progress.activity {
            return Err(RuntimeError::new(if maintenance {
                "Agent handle is not idle for maintenance."
            } else {
                "Agent handle already has an active turn."
            }));
        }
        progress.activity = true;
        progress.maintenance = maintenance;
        progress.cancelled = None;
        Ok(HandleActivity(self.clone()))
    }

    /// Admission is reserved before invoking host code, with no synchronous
    /// lock held across host calls or stream I/O. Dropping the stream drains
    /// the activity even when next() fails or its consumer is cancelled.
    pub fn submit(
        &self,
        input: JsValue,
        options: JsValue,
    ) -> Result<SessionSubmission, RuntimeError> {
        let _admission = self.0.admission.lock().unwrap();
        let queued = self.0.queued.lock().unwrap();
        if !queued.pending.is_empty() || queued.pump_running {
            return Err(RuntimeError::new("Agent handle already has a queued turn."));
        }
        drop(queued);
        let activity = self.begin_activity()?;
        let stream = self.0.session.submit(input, options)?;
        Ok(SessionSubmission {
            stream,
            _activity: activity,
        })
    }

    /// Durability is the admission boundary. Execution is owned by the
    /// background FIFO pump and has no second result stream.
    pub fn enqueue_queued_turn(&self, turn: SessionQueuedTurn) -> RuntimeResult {
        let _admission = self.0.admission.lock().unwrap();
        if self.state() != HandleState::Active {
            return Err(RuntimeError::new("Agent handle is not active."));
        }
        self.0.session.enqueue_queued_turn(&turn)?;
        {
            let mut queued = self.0.queued.lock().unwrap();
            queued.pending.push_back(turn);
            queued.pump_blocked = false;
        }
        drop(_admission);
        self.start_queued_pump();
        Ok(())
    }

    pub fn enqueue_followup(
        &self,
        input: JsValue,
        submit_options: JsValue,
    ) -> Result<SessionQueuedTurn, RuntimeError> {
        let turn = SessionQueuedTurn {
            item_id: uuid::Uuid::new_v4().to_string().into(),
            turn_id: uuid::Uuid::new_v4().to_string().into(),
            input,
            submit_options,
        };
        self.enqueue_queued_turn(turn.clone())?;
        Ok(turn)
    }

    /// Start work restored from the transcript only after the owner has
    /// published the exact live identity and installed session-local tools.
    pub fn resume_queued_turns(&self) {
        self.start_queued_pump();
    }

    fn start_queued_pump(&self) {
        {
            let mut queued = self.0.queued.lock().unwrap();
            if queued.pending.is_empty() || queued.pump_running || queued.pump_blocked {
                return;
            }
            queued.pump_running = true;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.0.queued.lock().unwrap().pump_running = false;
            return;
        };
        let handle = self.clone();
        runtime.spawn(async move {
            let blocked = handle.run_queued_pump().await.is_err();
            let active = handle.state() == HandleState::Active;
            let restart = {
                let mut queued = handle.0.queued.lock().unwrap();
                queued.pump_running = false;
                queued.pump_blocked = blocked;
                !blocked && !queued.pending.is_empty() && active
            };
            handle.0.changed.send_modify(|version| *version += 1);
            if restart {
                handle.start_queued_pump();
            }
        });
    }

    async fn run_queued_pump(&self) -> RuntimeResult {
        loop {
            let mut changed = self.0.changed.subscribe();
            let started = {
                let _admission = self.0.admission.lock().unwrap();
                let mut progress = self.0.progress.lock().unwrap();
                if progress.state != HandleState::Active {
                    return Ok(());
                }
                if progress.activity {
                    None
                } else {
                    let turn = self.0.queued.lock().unwrap().pending.front().cloned();
                    let Some(turn) = turn else {
                        return Ok(());
                    };
                    progress.activity = true;
                    progress.maintenance = false;
                    progress.cancelled = None;
                    drop(progress);
                    let activity = HandleActivity(self.clone());
                    match self.0.session.submit_queued_turn(&turn) {
                        Ok(stream) => {
                            let removed = self.0.queued.lock().unwrap().pending.pop_front();
                            debug_assert_eq!(removed.as_ref(), Some(&turn));
                            Some(Ok(SessionSubmission {
                                stream,
                                _activity: activity,
                            }))
                        }
                        Err(error) => {
                            drop(activity);
                            Some(Err(error))
                        }
                    }
                }
            };
            let Some(started) = started else {
                let _ = changed.changed().await;
                continue;
            };
            let mut submission = started?;
            while submission.next().await?.is_some() {}
        }
    }

    pub fn abort(&self, reason: &JsString) -> RuntimeResult {
        // Set cancelled inside the lock; call session.abort() outside so that
        // a panic does not poison the progress mutex.
        // TS AgentHandle.ts:176-179 forwards every abort until Disposed,
        // including during disposal and the stop_new -> dispose window.
        // Disposal independently aborts an active turn; these are two requests,
        // not an exactly-once dispatch for the lifetime of the handle.
        {
            let mut progress = self.0.progress.lock().unwrap();
            if progress.state == HandleState::Disposed {
                return Ok(());
            }
            progress.cancelled.get_or_insert_with(|| reason.clone());
        }
        self.0.changed.send_modify(|version| *version += 1);
        self.0.session.abort(reason)
    }

    /// Cross the cutoff synchronously; the spawned drain survives cancellation
    /// of every caller. Repeated calls share the same result, including errors.
    pub fn dispose(&self, reason: JsString) -> DisposeFuture<'static> {
        let mut progress = self.0.progress.lock().unwrap();
        if let Some(completion) = &progress.disposal {
            return Box::pin(wait_result(completion.clone()));
        }
        progress.state = HandleState::Draining;
        let active = progress.activity && !progress.maintenance;
        if active || progress.maintenance {
            progress.cancelled.get_or_insert_with(|| reason.clone());
        }
        let (result, completion) = watch::channel(None);
        progress.disposal = Some(completion.clone());
        drop(progress);
        self.0.changed.send_modify(|version| *version += 1);
        let handle = self.clone();
        tokio::spawn(async move {
            // DisposeGuard is constructed first so every subsequent step —
            // including session.abort() and session.dispose() — is covered.
            // If anything panics, Drop sends a result and releases the lease.
            struct DisposeGuard<'a> {
                result: &'a watch::Sender<Option<RuntimeResult>>,
                lease: Option<RuntimeLease>,
                handle_progress: &'a Mutex<Progress>,
                outcome: Option<RuntimeResult>,
            }
            impl Drop for DisposeGuard<'_> {
                fn drop(&mut self) {
                    self.handle_progress.lock().unwrap().state = HandleState::Disposed;
                    let outcome = self.outcome.take().unwrap_or_else(|| {
                        Err(RuntimeError::new("Agent session dispose task panicked."))
                    });
                    self.result.send_replace(Some(outcome));
                }
            }
            // Guard takes the lease now; abort and dispose run under its Drop.
            let lease = handle.0.lease.lock().unwrap().take();
            let mut guard = DisposeGuard {
                result: &result,
                lease,
                handle_progress: &handle.0.progress,
                outcome: None,
            };
            let mut failures = Vec::new();
            if let Err(error) = handle.0.session.dispose_start(&reason).await {
                failures.push(error);
            }
            // session.abort() is outside the progress lock (no poison risk).
            // Called before the drain wait so the session can begin cancellation.
            let abort_error = if active {
                handle.0.session.abort(&reason).err()
            } else {
                None
            };
            failures.extend(abort_error);
            let mut changed = handle.0.changed.subscribe();
            loop {
                let active = handle.0.progress.lock().unwrap().activity;
                let pumping = handle.0.queued.lock().unwrap().pump_running;
                if !active && !pumping {
                    break;
                }
                let _ = changed.changed().await;
            }
            if let Err(error) = handle.0.session.discard_queued_turns("agent_disposed") {
                failures.push(error);
            } else {
                handle.0.queued.lock().unwrap().pending.clear();
            }
            // Production order (TS createAgentSession.ts:223-241 via
            // disposeAgentSessionLifecycle): the gateway bundle's release —
            // the project runtime lease, composed by ProjectSessionFactory
            // composeDisposer inside configuredDisposer — runs before the
            // session-owned runtime resources are disposed.
            let mut disposal_failures = Vec::new();
            if let Some(mut lease) = guard.lease.take()
                && let Err(error) = lease.release().await
            {
                disposal_failures.push(error);
            }
            if let Err(error) = handle.0.session.dispose().await {
                disposal_failures.push(error);
            }
            if let Err(error) = aggregate("Failed to dispose agent session.", disposal_failures) {
                failures.push(error);
            }
            guard.outcome = Some(aggregate("Failed to dispose agent handle.", failures));
        });
        Box::pin(wait_result(completion))
    }
}

pub struct SessionSubmission {
    stream: Box<dyn LiveTurn>,
    _activity: HandleActivity,
}
impl SessionSubmission {
    pub async fn next(&mut self) -> Result<Option<JsValue>, RuntimeError> {
        self.stream.next().await
    }
}

pub struct HandleActivity(LiveSessionHandle);

impl HandleActivity {
    pub fn cancellation_reason(&self) -> Option<JsString> {
        self.0.0.progress.lock().unwrap().cancelled.clone()
    }
}

impl Drop for HandleActivity {
    fn drop(&mut self) {
        self.0.0.progress.lock().unwrap().activity = false;
        self.0.0.changed.send_modify(|version| *version += 1);
    }
}

fn required_id(value: &JsValue, key: &str) -> Result<JsString, RuntimeError> {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| RuntimeError::new(format!("Queued turn {key} is missing.")))
}

pub(super) fn aggregate(message: &str, failures: Vec<RuntimeError>) -> RuntimeResult {
    if failures.is_empty() {
        Ok(())
    } else {
        Err(RuntimeError {
            message: message.into(),
            causes: failures,
        })
    }
}

pub(super) async fn wait_result<T: Clone>(mut receiver: watch::Receiver<Option<T>>) -> T {
    loop {
        if let Some(result) = receiver.borrow_and_update().clone() {
            return result;
        }
        receiver
            .changed()
            .await
            .expect("session lifecycle executor stopped");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::project::{DisposeFuture, RuntimeResult};
    use pilotdeck_protocol::{JsValue, parse_json};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct MemSession {
        aborted: Mutex<Vec<String>>,
        disposed: AtomicBool,
        dispose_gate: tokio::sync::watch::Sender<bool>,
    }
    impl MemSession {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                aborted: Mutex::new(vec![]),
                disposed: AtomicBool::new(false),
                dispose_gate: tokio::sync::watch::channel(true).0,
            })
        }
        fn with_gate(gate: tokio::sync::watch::Sender<bool>) -> Arc<Self> {
            Arc::new(Self {
                aborted: Mutex::new(vec![]),
                disposed: AtomicBool::new(false),
                dispose_gate: gate,
            })
        }
    }
    impl LiveSession for MemSession {
        fn snapshot(&self) -> JsValue {
            parse_json("{}").unwrap()
        }
        fn abort(&self, reason: &JsString) -> RuntimeResult {
            self.aborted.lock().unwrap().push(reason.to_utf8().unwrap());
            Ok(())
        }
        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async {
                let _ = self.dispose_gate.subscribe().wait_for(|open| *open).await;
                self.disposed.store(true, Ordering::SeqCst);
                Ok(())
            })
        }
    }

    struct ParentWaitingOnChildSession {
        child_cancelled: watch::Sender<bool>,
        events: Mutex<Vec<&'static str>>,
    }

    impl ParentWaitingOnChildSession {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                child_cancelled: watch::channel(false).0,
                events: Mutex::new(Vec::new()),
            })
        }
    }

    impl LiveSession for ParentWaitingOnChildSession {
        fn snapshot(&self) -> JsValue {
            JsValue::Object(Default::default())
        }

        fn abort(&self, _reason: &JsString) -> RuntimeResult {
            self.events.lock().unwrap().push("abort");
            Ok(())
        }

        fn dispose_start(&self, _reason: &JsString) -> DisposeFuture<'_> {
            Box::pin(async move {
                self.events.lock().unwrap().push("dispose_start");
                self.child_cancelled.send_replace(true);
                Ok(())
            })
        }

        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async move {
                self.events.lock().unwrap().push("dispose");
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn dispose_start_releases_parent_activity_waiting_on_child() {
        let session = ParentWaitingOnChildSession::new();
        let handle = LiveSessionHandle::new(session.clone(), None);
        let activity = handle.begin_activity().unwrap();
        let mut child_cancelled = session.child_cancelled.subscribe();
        let parent_turn = tokio::spawn(async move {
            child_cancelled
                .wait_for(|cancelled| *cancelled)
                .await
                .unwrap();
            drop(activity);
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            handle.dispose("parent_disposed".into()),
        )
        .await
        .expect("parent disposal deadlocked while its active turn waited on a child")
        .unwrap();
        parent_turn.await.unwrap();
        assert_eq!(
            *session.events.lock().unwrap(),
            vec!["dispose_start", "abort", "dispose"]
        );
    }

    struct QueueSession {
        pending: Mutex<VecDeque<SessionQueuedTurn>>,
        submitted: Mutex<Vec<JsString>>,
        discarded: Mutex<Vec<JsString>>,
        started: watch::Sender<usize>,
        finished: watch::Sender<usize>,
        gate: watch::Sender<bool>,
        yielded: Arc<AtomicUsize>,
        events_per_turn: usize,
    }

    impl QueueSession {
        fn new(
            pending: Vec<SessionQueuedTurn>,
            gate_open: bool,
            events_per_turn: usize,
        ) -> Arc<Self> {
            Arc::new(Self {
                pending: Mutex::new(pending.into()),
                submitted: Mutex::new(Vec::new()),
                discarded: Mutex::new(Vec::new()),
                started: watch::channel(0).0,
                finished: watch::channel(0).0,
                gate: watch::channel(gate_open).0,
                yielded: Arc::new(AtomicUsize::new(0)),
                events_per_turn,
            })
        }
    }

    impl LiveSession for QueueSession {
        fn pending_queued_turns(&self) -> Result<Vec<SessionQueuedTurn>, RuntimeError> {
            Ok(self.pending.lock().unwrap().iter().cloned().collect())
        }

        fn enqueue_queued_turn(&self, turn: &SessionQueuedTurn) -> RuntimeResult {
            self.pending.lock().unwrap().push_back(turn.clone());
            Ok(())
        }

        fn submit_queued_turn(
            &self,
            turn: &SessionQueuedTurn,
        ) -> Result<Box<dyn LiveTurn>, RuntimeError> {
            let next = self
                .pending
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| RuntimeError::new("missing queued turn"))?;
            if next != *turn {
                return Err(RuntimeError::new("queued turn was not FIFO"));
            }
            self.submitted.lock().unwrap().push(turn.turn_id.clone());
            self.started.send_modify(|count| *count += 1);
            Ok(Box::new(QueueTurn {
                remaining: self.events_per_turn,
                finished: self.finished.clone(),
                gate: self.gate.subscribe(),
                yielded: self.yielded.clone(),
                done: false,
            }))
        }

        fn discard_queued_turns(&self, _reason: &str) -> RuntimeResult {
            let mut pending = self.pending.lock().unwrap();
            self.discarded
                .lock()
                .unwrap()
                .extend(pending.drain(..).map(|turn| turn.item_id));
            Ok(())
        }

        fn snapshot(&self) -> JsValue {
            JsValue::Object(Default::default())
        }

        fn abort(&self, _reason: &JsString) -> RuntimeResult {
            Ok(())
        }

        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }

    struct QueueTurn {
        remaining: usize,
        finished: watch::Sender<usize>,
        gate: watch::Receiver<bool>,
        yielded: Arc<AtomicUsize>,
        done: bool,
    }

    impl LiveTurn for QueueTurn {
        fn next(&mut self) -> SessionNext<'_> {
            Box::pin(async move {
                if !*self.gate.borrow() {
                    self.gate
                        .wait_for(|open| *open)
                        .await
                        .map_err(|_| RuntimeError::new("queue gate closed"))?;
                }
                if self.remaining > 0 {
                    self.remaining -= 1;
                    self.yielded.fetch_add(1, Ordering::SeqCst);
                    return Ok(Some(JsValue::Null));
                }
                if !self.done {
                    self.done = true;
                    self.finished.send_modify(|count| *count += 1);
                }
                Ok(None)
            })
        }
    }

    fn queued(item: &str, turn: &str) -> SessionQueuedTurn {
        SessionQueuedTurn {
            item_id: item.into(),
            turn_id: turn.into(),
            input: parse_json(r#"{"type":"text","text":"follow up"}"#).unwrap(),
            submit_options: JsValue::Object(Default::default()),
        }
    }

    #[tokio::test]
    async fn queued_pump_waits_for_foreground_then_drains_fifo_streams() {
        let session = QueueSession::new(Vec::new(), true, 3);
        let handle = LiveSessionHandle::new(session.clone(), None);
        let foreground = handle.begin_activity().unwrap();
        handle.enqueue_queued_turn(queued("i1", "t1")).unwrap();
        handle.enqueue_queued_turn(queued("i2", "t2")).unwrap();
        assert!(session.submitted.lock().unwrap().is_empty());

        let mut finished = session.finished.subscribe();
        drop(foreground);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            finished.wait_for(|count| *count == 2),
        )
        .await
        .expect("queued pump did not wake when the foreground turn became idle")
        .unwrap();

        assert_eq!(
            *session.submitted.lock().unwrap(),
            vec![JsString::from("t1"), JsString::from("t2")]
        );
        assert_eq!(session.yielded.load(Ordering::SeqCst), 6);
        handle.dispose("test_done".into()).await.unwrap();
    }

    #[tokio::test]
    async fn recovered_queue_starts_only_after_explicit_resume() {
        let session = QueueSession::new(vec![queued("i1", "t1"), queued("i2", "t2")], true, 1);
        let handle = LiveSessionHandle::new(session.clone(), None);
        assert!(session.submitted.lock().unwrap().is_empty());
        let mut finished = session.finished.subscribe();

        handle.resume_queued_turns();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            finished.wait_for(|count| *count == 2),
        )
        .await
        .expect("restored queue did not drain")
        .unwrap();
        assert_eq!(
            *session.submitted.lock().unwrap(),
            vec![JsString::from("t1"), JsString::from("t2")]
        );
        handle.dispose("test_done".into()).await.unwrap();
    }

    #[tokio::test]
    async fn followup_admission_returns_before_pump_result() {
        let session = QueueSession::new(Vec::new(), false, 1);
        let handle = LiveSessionHandle::new(session.clone(), None);
        let mut started = session.started.subscribe();
        let mut finished = session.finished.subscribe();

        handle.enqueue_queued_turn(queued("i", "t")).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            started.wait_for(|count| *count == 1),
        )
        .await
        .expect("queued turn was not submitted")
        .unwrap();
        assert_eq!(*finished.borrow(), 0, "admission must not await execution");

        session.gate.send_replace(true);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            finished.wait_for(|count| *count == 1),
        )
        .await
        .expect("queued stream did not finish")
        .unwrap();
        handle.dispose("test_done".into()).await.unwrap();
    }

    #[tokio::test]
    async fn replacement_stops_waiting_queue_and_dispose_discards_it() {
        let session = QueueSession::new(Vec::new(), true, 1);
        let handle = LiveSessionHandle::new(session.clone(), None);
        let foreground = handle.begin_activity().unwrap();
        handle.enqueue_queued_turn(queued("i", "t")).unwrap();

        handle.stop_new();
        let disposal = handle.dispose("agent_replaced".into());
        drop(foreground);
        disposal.await.unwrap();

        assert!(session.submitted.lock().unwrap().is_empty());
        assert_eq!(
            *session.discarded.lock().unwrap(),
            vec![JsString::from("i")]
        );
        assert!(handle.enqueue_queued_turn(queued("late", "late")).is_err());
    }

    /// TS forwards external aborts during disposal as well as the drain's own
    /// abort. The drain calls session.abort() outside the progress lock.
    #[tokio::test]
    async fn abort_during_disposal_reaches_session_matches_ts() {
        let session = MemSession::new();
        let handle = LiveSessionHandle::new(session.clone(), None);
        // Start a turn so dispose() path marks active=true
        let activity = handle.begin_activity().unwrap();
        // dispose() schedules its abort in the spawned drain task.
        let dispose_fut = handle.dispose("dispose-reason".into());
        assert_eq!(handle.state(), HandleState::Draining);
        handle.abort(&"concurrent-abort".into()).unwrap();
        assert_eq!(
            activity.cancellation_reason(),
            Some("dispose-reason".into())
        );
        drop(activity);
        dispose_fut.await.unwrap();
        assert_eq!(handle.state(), HandleState::Disposed);
        // No ordering is imposed on the external call and the drain task.
        let mut aborts = session.aborted.lock().unwrap().clone();
        aborts.sort();
        assert_eq!(aborts, vec!["concurrent-abort", "dispose-reason"]);
    }

    #[tokio::test]
    async fn abort_between_stop_new_and_dispose_reaches_session() {
        let session = MemSession::new();
        let handle = LiveSessionHandle::new(session.clone(), None);
        let activity = handle.begin_activity().unwrap();
        handle.stop_new();
        assert_eq!(handle.state(), HandleState::Draining);
        assert!(handle.begin_activity().is_err());
        assert!(handle.0.progress.lock().unwrap().disposal.is_none());

        handle.abort(&"stop-new-window".into()).unwrap();
        assert_eq!(
            activity.cancellation_reason(),
            Some("stop-new-window".into())
        );
        assert_eq!(*session.aborted.lock().unwrap(), vec!["stop-new-window"]);

        let disposal = handle.dispose("dispose".into());
        drop(activity);
        disposal.await.unwrap();
        assert_eq!(
            *session.aborted.lock().unwrap(),
            vec!["stop-new-window", "dispose"]
        );
    }

    /// Both operations race on four workers while the turn stays in flight.
    /// Each must dispatch once: TS permits two calls, in either order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_abort_and_dispose_dispatch_both_requests_matches_ts() {
        for iteration in 0..256 {
            let session = MemSession::new();
            let handle = LiveSessionHandle::new(session.clone(), None);
            let activity = handle.begin_activity().unwrap();
            let start = Arc::new(tokio::sync::Barrier::new(3));
            let (dispose_started, cutoff) = tokio::sync::oneshot::channel();
            let aborter = {
                let handle = handle.clone();
                let start = start.clone();
                tokio::spawn(async move {
                    start.wait().await;
                    handle.abort(&"external".into()).unwrap();
                })
            };
            let disposer = {
                let handle = handle.clone();
                let start = start.clone();
                tokio::spawn(async move {
                    start.wait().await;
                    let disposal = handle.dispose("dispose".into());
                    dispose_started.send(()).unwrap();
                    disposal.await.unwrap();
                })
            };
            start.wait().await;
            aborter.await.unwrap();
            cutoff.await.unwrap();
            // Keep disposal from completing before both requests have arrived.
            drop(activity);
            tokio::time::timeout(std::time::Duration::from_secs(3), disposer)
                .await
                .expect("drain did not finish")
                .unwrap();
            let mut aborts = session.aborted.lock().unwrap().clone();
            aborts.sort();
            assert_eq!(
                aborts,
                vec!["dispose", "external"],
                "iteration {iteration}: both requests must reach session.abort"
            );
            assert_eq!(handle.state(), HandleState::Disposed);
        }
    }

    /// Calling abort() after Disposed is a no-op with no session call.
    #[tokio::test]
    async fn abort_after_disposed_is_noop() {
        let session = MemSession::new();
        let handle = LiveSessionHandle::new(session.clone(), None);
        handle.dispose("done".into()).await.unwrap();
        assert_eq!(handle.state(), HandleState::Disposed);
        handle.abort(&"late".into()).unwrap();
        assert!(session.aborted.lock().unwrap().is_empty());
    }

    /// DisposeGuard must send a result even when session.dispose() is still
    /// running; await completes only after dispose finishes.
    #[tokio::test]
    async fn dispose_guard_sends_result_on_completion() {
        let (gate_tx, _gate_rx) = tokio::sync::watch::channel(false);
        let session = MemSession::with_gate(gate_tx.clone());
        let handle = LiveSessionHandle::new(session.clone(), None);
        let dispose_fut = handle.dispose("finish".into());
        // Open gate so dispose() returns
        gate_tx.send_replace(true);
        dispose_fut.await.unwrap();
        assert!(session.disposed.load(Ordering::SeqCst));
        assert_eq!(handle.state(), HandleState::Disposed);
    }

    /// Repeated dispose() calls must share the same result channel; only one
    /// drain task must run and session.dispose() must be called exactly once.
    #[tokio::test]
    async fn dispose_repeated_shares_result() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct CountingSession {
            dispose_count: Arc<AtomicUsize>,
        }
        impl LiveSession for CountingSession {
            fn snapshot(&self) -> JsValue {
                parse_json("{}").unwrap()
            }
            fn abort(&self, _reason: &JsString) -> RuntimeResult {
                Ok(())
            }
            fn dispose(&self) -> DisposeFuture<'_> {
                self.dispose_count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(()) })
            }
        }
        let dispose_count = Arc::new(AtomicUsize::new(0));
        let session = Arc::new(CountingSession {
            dispose_count: dispose_count.clone(),
        });
        let handle = LiveSessionHandle::new(session, None);
        let f1 = handle.dispose("first".into());
        let f2 = handle.dispose("second".into());
        f1.await.unwrap();
        f2.await.unwrap();
        assert_eq!(handle.state(), HandleState::Disposed);
        // Only one drain task must have run — shared channel, not two separate drains
        assert_eq!(
            dispose_count.load(Ordering::SeqCst),
            1,
            "session.dispose() must be called exactly once even with repeated dispose() calls"
        );
    }

    /// Negative control: a panic inside the drain task still delivers a
    /// result via DisposeGuard, so wait_result never hangs.
    #[tokio::test]
    async fn dispose_guard_sends_result_on_abort_panic() {
        struct AbortPanicSession;
        impl LiveSession for AbortPanicSession {
            fn snapshot(&self) -> JsValue {
                parse_json("{}").unwrap()
            }
            fn abort(&self, _reason: &JsString) -> RuntimeResult {
                panic!("simulated session.abort() panic");
            }
            fn dispose(&self) -> DisposeFuture<'_> {
                Box::pin(async { Ok(()) })
            }
        }
        let handle = LiveSessionHandle::new(Arc::new(AbortPanicSession), None);
        let activity = handle.begin_activity().unwrap();
        let disposal = handle.dispose("boom".into());
        drop(activity);
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), disposal)
            .await
            .expect("abort panic must still deliver the disposal result")
            .unwrap_err();
        assert_eq!(error.message, "Agent session dispose task panicked.");
        assert_eq!(handle.state(), HandleState::Disposed);
    }

    /// A panic from session.dispose() must also deliver the guarded result.
    #[tokio::test]
    async fn dispose_guard_sends_result_on_panic() {
        struct PanicSession;
        impl LiveSession for PanicSession {
            fn snapshot(&self) -> JsValue {
                parse_json("{}").unwrap()
            }
            fn abort(&self, _reason: &JsString) -> RuntimeResult {
                Ok(())
            }
            fn dispose(&self) -> DisposeFuture<'_> {
                Box::pin(async {
                    panic!("simulated session.dispose() panic");
                })
            }
        }
        let handle = LiveSessionHandle::new(Arc::new(PanicSession), None);
        // If DisposeGuard fails to send, this await hangs forever.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            handle.dispose("boom".into()),
        )
        .await;
        assert!(
            result.is_ok(),
            "dispose did not complete within 3s — DisposeGuard failed to deliver result on panic"
        );
        assert!(
            result.unwrap().is_err(),
            "expected error from panicked dispose"
        );
    }
}

#[cfg(test)]
#[path = "handle_abort_tests.rs"]
mod abort_contract_tests;
