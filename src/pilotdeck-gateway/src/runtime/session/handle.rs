//! Live writer identity, admission cutoff and resource drain. This is the
//! lifecycle seam for an AgentSession adapter, not a substitute agent loop.
use super::super::project::{DisposeFuture, RuntimeError, RuntimeLease, RuntimeResult};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::{Arc, Mutex};
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

/// Implementations own the real session writer and its resources. No default
/// no-op implementation is supplied for unavailable providers.
pub trait LiveSession: Send + Sync {
    /// Test-only observation of the actual reload controller, not a fabricated ID.
    #[cfg(test)]
    fn reload_signal_for_test(&self) -> Option<crate::runtime::hooks::Cancellation> {
        None
    }
    /// Runtime reload is a fallible capability, separate from the public view.
    fn reload_snapshot(&self) -> Result<super::factory::ReloadSnapshot, RuntimeError> {
        Err(RuntimeError::new(
            "Agent session runtime reload is unavailable.",
        ))
    }
    /// Factory recreation retains this source through owned disposal and its
    /// completion, when the adapter already rejects ordinary reload. The final
    /// seed must remain readable while the factory retains the predecessor.
    fn draining_reload_snapshot(&self) -> Result<super::factory::ReloadSnapshot, RuntimeError> {
        self.reload_snapshot()
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
    fn snapshot(&self) -> JsValue;
    /// May be called repeatedly while `dispose()` is in progress and after its
    /// future has completed: the handle remains Draining through the project
    /// lease release and its own session disposal. Implementations must make
    /// cancellation idempotent and must not panic, including after session
    /// resources have been disposed.
    /// Abort signals the active LiveTurn; draining includes SessionEnd and
    /// session_ended through None (or Err), not just turn_completed. A hook
    /// that ignores cancellation still owns its await. Submit preserves that
    /// native path; its independent wall-clock timeout bounds uncooperative
    /// providers. Cancellation alone must not drop lifecycle tail work.
    /// Only a fully Disposed handle suppresses subsequent external aborts.
    /// This matches TS AgentHandle.ts:176-179,281-287.
    fn abort(&self, reason: &JsString) -> RuntimeResult;
    /// Transfer owned writer cleanup before abort can unwind. Keep writes open
    /// for final lifecycle hooks; the guard retires/drains the writer on Drop.
    /// Adapters without a factory writer lease have nothing to transfer.
    fn disposal_guard(&self) -> Option<Box<dyn Send>> {
        None
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
    recreation_started: bool,
    disposal_completed: bool,
    recreation_error_reported: bool,
    recreation_stale_handoff: bool,
    #[cfg(test)]
    activity_waiting: Option<watch::Sender<bool>>,
}

struct Inner {
    session: Arc<dyn LiveSession>,
    lease: Mutex<Option<RuntimeLease>>,
    // Arc: the detached drain task holds the progress alongside the session and
    // lease, without a handle clone, so dropping the last clone (#1010) cannot
    // cancel an in-flight disposal.
    progress: Arc<Mutex<Progress>>,
    changed: watch::Sender<u64>,
    /// #1010 captured executor: dispose must also be startable from Drop, on a
    /// thread without a runtime context and after the ambient one went away.
    executor: tokio::runtime::Handle,
}

/// Cloning preserves object identity. A project lease is held until the writer
/// has drained; it is never used as a handle ID or a connection generation.
#[derive(Clone)]
pub struct LiveSessionHandle(Arc<Inner>);

impl LiveSessionHandle {
    #[cfg(test)]
    pub(super) fn session_for_test(&self) -> Arc<dyn LiveSession> {
        self.0.session.clone()
    }
    #[cfg(test)]
    pub(crate) fn reload_signal_for_test(&self) -> Option<crate::runtime::hooks::Cancellation> {
        self.0.session.reload_signal_for_test()
    }
    /// Draining handles remain readable until the underlying session is disposed.
    pub fn reload_snapshot(&self) -> Result<super::factory::ReloadSnapshot, RuntimeError> {
        self.0.session.reload_snapshot()
    }

    pub(super) fn recreation_snapshot(
        &self,
    ) -> Result<super::factory::ReloadSnapshot, RuntimeError> {
        // Capture eligibility once. Disposal may advance after this read; the
        // owned reload source stays valid through close and does not consult
        // the adapter's later disposed flag.
        let snapshot = if self.state() == HandleState::Disposed && !self.retains_recreation_source()
        {
            self.reload_snapshot()
        } else {
            self.0.session.draining_reload_snapshot()
        }?;
        self.0.progress.lock().unwrap().recreation_started = true;
        Ok(snapshot)
    }

    /// Only a factory that passed eligibility may retry from this retained
    /// source. External disposal alone never grants recreation eligibility.
    pub(super) fn retains_recreation_source(&self) -> bool {
        self.0.progress.lock().unwrap().recreation_started
    }

    pub(super) fn recreation_drained(&self) -> bool {
        let progress = self.0.progress.lock().unwrap();
        progress.recreation_started
            && progress.disposal_completed
            && !progress.activity
            && (progress.recreation_error_reported
                || progress
                    .disposal
                    .as_ref()
                    .is_some_and(|result| matches!(&*result.borrow(), Some(Ok(())))))
    }

    /// Acknowledge only when the factory waiter actually receives a disposal
    /// error. Sending to a live channel is not proof that it was consumed.
    pub(super) fn report_recreation_error(&self) {
        self.0.progress.lock().unwrap().recreation_error_reported = true;
    }

    /// #1020: the factory observed this predecessor already Disposed when it
    /// took it over (no retained recreation source), so the post-prepare
    /// snapshot rejection was a stale owner selection — the same-key disposal
    /// completed after the owner chose `previous` but before this attempt took
    /// it over. Only the factory sets this after that rejection; the owner may
    /// retry once. Direct factory callers ignore it and keep the TS error.
    pub(super) fn mark_recreation_stale_handoff(&self) {
        self.0.progress.lock().unwrap().recreation_stale_handoff = true;
    }

    pub(super) fn recreation_stale_handoff(&self) -> bool {
        self.0.progress.lock().unwrap().recreation_stale_handoff
    }

    /// Only factory recreation calls this after its successful owned drain.
    /// The adapter is disposed, but the retained reload source still owns the
    /// final saved seed. Eligibility was checked before disposal started.
    pub(super) fn drained_file_state(
        &self,
    ) -> Result<crate::host::modules::tool_context::SeedState, RuntimeError> {
        self.0
            .session
            .draining_reload_snapshot()
            .map(|s| s.file_state)
    }

    /// Capture the executor that owns this session's asynchronous cleanup.
    ///
    /// # Panics
    /// Must be constructed inside a Tokio runtime. Later disposal and Drop
    /// may run on a plain thread; they continue to use this captured executor.
    pub fn new(session: Arc<dyn LiveSession>, lease: Option<RuntimeLease>) -> Self {
        Self(Arc::new(Inner {
            session,
            lease: Mutex::new(lease),
            progress: Arc::new(Mutex::new(Progress {
                state: HandleState::Active,
                activity: false,
                maintenance: false,
                cancelled: None,
                disposal: None,
                recreation_started: false,
                disposal_completed: false,
                recreation_error_reported: false,
                recreation_stale_handoff: false,
                #[cfg(test)]
                activity_waiting: None,
            })),
            changed: watch::channel(0).0,
            // Same contract as PendingSession/ProjectRuntimeRegistry: the
            // construction site is inside the owning runtime.
            executor: tokio::runtime::Handle::current(),
        }))
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    #[cfg(test)]
    pub(super) fn in_flight_for_test(&self) -> usize {
        usize::from(
            self.0
                .progress
                .lock()
                .expect("#745 handle progress")
                .activity,
        )
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
    }

    pub fn snapshot(&self) -> JsValue {
        self.0.session.snapshot()
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
        let activity = self.begin_activity()?;
        let stream = self.0.session.submit(input, options)?;
        Ok(SessionSubmission {
            stream,
            _activity: Some(activity),
        })
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
        Box::pin(wait_result(self.0.start_dispose(reason)))
    }
}

impl Inner {
    /// Idempotent entry shared by explicit `dispose()` and the drop of the
    /// last handle clone (#1010). The first call owns the single drain task;
    /// every later call — including Drop's — shares its completion channel.
    fn start_dispose(&self, reason: JsString) -> watch::Receiver<Option<RuntimeResult>> {
        let mut progress = self
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(completion) = &progress.disposal {
            return completion.clone();
        }
        progress.state = HandleState::Draining;
        let active = progress.activity && !progress.maintenance;
        if active || progress.maintenance {
            progress.cancelled.get_or_insert_with(|| reason.clone());
        }
        let (result, completion) = watch::channel(None);
        progress.disposal = Some(completion.clone());
        drop(progress);
        // Everything the detached drain needs, without a handle clone: the
        // last clone may already be gone (the Drop path below).
        let session = self.session.clone();
        let progress = self.progress.clone();
        let changed = self.changed.clone();
        let executor = self.executor.clone();
        let lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        // #1010: not a spawn-and-hope. If the executor rejects the task, or a
        // dropped/shut-down runtime drops it unpolled, this guard's own Drop
        // still lands the runtime-free cleanup: the custom writer guard
        // (WriterCloseGuard closes on its own thread) and the lease count.
        // tokio 1.48 drops the unpolled future in every rejection mode, so
        // the fallback cannot be skipped by "successfully" spawning.
        let fallback = DisposeFallback {
            session: Some(session.clone()),
            progress: progress.clone(),
            result: result.clone(),
        };
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            executor.spawn(async move {
                let mut fallback = fallback;
                // The drain owns the cleanup from here on; its own guards
                // (DisposeGuard, _writer_guard) cover every later drop. The
                // fallback stays armed only when this body never runs at all.
                // This must precede disposal_guard(): a session whose guard
                // panics must not have it invoked twice in one unwind.
                fallback.disarm();
                // DisposeGuard is constructed first so every subsequent step —
                // including session.abort() and session.dispose() — is covered.
                // If anything panics, Drop sends a result and releases the lease.
                struct DisposeGuard<'a> {
                    result: &'a watch::Sender<Option<RuntimeResult>>,
                    lease: Option<RuntimeLease>,
                    handle_progress: &'a Arc<Mutex<Progress>>,
                    outcome: Option<RuntimeResult>,
                }
                impl Drop for DisposeGuard<'_> {
                    fn drop(&mut self) {
                        // A custom writer guard can itself panic while dropping,
                        // after the ordinary disposer already returned a result.
                        let completed = self.outcome.is_some() && !std::thread::panicking();
                        {
                            let mut progress = self
                                .handle_progress
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            progress.disposal_completed = completed;
                            progress.state = HandleState::Disposed;
                        }
                        let outcome = if completed { self.outcome.take() } else { None }
                            .unwrap_or_else(|| {
                                Err(RuntimeError::new(if std::thread::panicking() {
                                    "Agent session dispose task panicked."
                                } else {
                                    "Agent session dispose task cancelled."
                                }))
                            });
                        self.result.send_replace(Some(outcome));
                    }
                }
                // Guard takes the lease now; abort and dispose run under its Drop.
                let mut guard = DisposeGuard {
                    result: &result,
                    lease,
                    handle_progress: &progress,
                    outcome: None,
                };
                // Drop before DisposeGuard publishes completion, including when
                // abort panics before session.dispose() can take its own guard.
                let _writer_guard = session.disposal_guard();
                // session.abort() is outside the progress lock (no poison risk).
                // Called before the drain wait so the session can begin cancellation.
                let abort_error = if active {
                    // Like TS AgentHandle.finishDispose, an abort failure does not
                    // skip the active-run drain or the ordinary resource disposer.
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        session.abort(&reason)
                    }))
                    .unwrap_or_else(|panic| {
                        let message = panic
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| panic.downcast_ref::<&str>().copied())
                            .unwrap_or("Agent session abort panicked.");
                        Err(RuntimeError::new(message))
                    })
                    .err()
                } else {
                    None
                };
                let mut failures: Vec<_> = abort_error.into_iter().collect();
                let mut changed = changed.subscribe();
                loop {
                    if !progress
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .activity
                    {
                        break;
                    }
                    #[cfg(test)]
                    {
                        // Observe the actual Pending poll, not merely entry to
                        // the loop: shutdown tests own the executor at this gate.
                        let waiting = progress
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .activity_waiting
                            .clone();
                        let wait = changed.changed();
                        tokio::pin!(wait);
                        let _ = std::future::poll_fn(|cx| {
                            let polled = wait.as_mut().poll(cx);
                            if polled.is_pending()
                                && let Some(waiting) = &waiting
                            {
                                waiting.send_replace(true);
                            }
                            polled
                        })
                        .await;
                    }
                    #[cfg(not(test))]
                    let _ = changed.changed().await;
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
                if let Err(error) = session.dispose().await {
                    disposal_failures.push(error);
                }
                if let Err(error) = aggregate("Failed to dispose agent session.", disposal_failures)
                {
                    failures.push(error);
                }
                guard.outcome = Some(aggregate("Failed to dispose agent handle.", failures));
            })
        }));
        completion
    }
}

/// #1010 synchronous fallback for a drain task that never runs. Constructed
/// before the spawn and moved into the task: the runtime drops every rejected
/// or unpolled future, so its Drop executes in all shutdown modes.
struct DisposeFallback {
    session: Option<Arc<dyn LiveSession>>,
    progress: Arc<Mutex<Progress>>,
    result: watch::Sender<Option<RuntimeResult>>,
}
impl DisposeFallback {
    /// The drain body took over: its DisposeGuard and writer guard now own
    /// every later drop, including panics mid-body. The fallback stays armed
    /// only for the never-polled / spawn-rejected task.
    fn disarm(&mut self) {
        self.session = None;
    }
}
impl Drop for DisposeFallback {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            // The custom writer guard closes on its own thread, without any
            // runtime (see WriterCloseGuard); its Drop releases the lease.
            // A session whose guard itself panics must not abort whatever is
            // already unwinding around this fallback.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drop(session.disposal_guard());
            }));
            // No asynchronous hook completed. Publish a terminal error for
            // every explicit waiter, without granting recreation eligibility.
            self.progress
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .state = HandleState::Disposed;
            self.result.send_replace(Some(Err(RuntimeError::new(
                "Session lifecycle executor stopped.",
            ))));
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        // #1010: dropping the last clone without an explicit dispose must not
        // skip the custom async release/close. TS has no counterpart (an
        // AgentHandle only runs onDispose through an explicit dispose(); GC
        // is nondeterministic — tests/fixtures/session-drop-release); the
        // intentional difference is registered as
        // specs/gateway-parity/diffs/A7.2-SESSION-HANDLE-DROP-001.md. The
        // drain starts here on the captured executor and stays idempotent
        // with any concurrent or completed explicit disposal.
        #[cfg(test)]
        let skip = option_env!("PDRS_1010_SKIP_DROP_DISPOSE").is_some();
        #[cfg(not(test))]
        let skip = false;
        if !skip {
            self.start_dispose("agent_handle_dropped".into());
        }
    }
}

pub struct SessionSubmission {
    stream: Box<dyn LiveTurn>,
    _activity: Option<HandleActivity>,
}
impl SessionSubmission {
    pub async fn next(&mut self) -> Result<Option<JsValue>, RuntimeError> {
        let result = self.stream.next().await;
        if !matches!(result, Ok(Some(_))) {
            self._activity.take();
        }
        result
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
        self.0
            .0
            .progress
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .activity = false;
        self.0.0.changed.send_modify(|version| *version += 1);
    }
}

pub(super) fn aggregate(message: &str, failures: Vec<RuntimeError>) -> RuntimeResult {
    if failures.is_empty() {
        Ok(())
    } else {
        Err(RuntimeError {
            message: message.into(),
            causes: failures,
            js_error: None,
        })
    }
}

/// Results carried by the session lifecycle completion channel. A sender that
/// disappears without a value (for example a detached task rejected by a
/// stopped executor) becomes this stable error instead of a panic.
pub(super) trait LifecycleCompletion: Clone {
    fn lifecycle_stopped() -> Self;
}
impl<T: Clone> LifecycleCompletion for Result<T, RuntimeError> {
    fn lifecycle_stopped() -> Self {
        Err(RuntimeError::new("Session lifecycle executor stopped."))
    }
}

pub(super) async fn wait_result<T: LifecycleCompletion>(
    mut receiver: watch::Receiver<Option<T>>,
) -> T {
    loop {
        if let Some(result) = receiver.borrow_and_update().clone() {
            return result;
        }
        if receiver.changed().await.is_err() {
            return T::lifecycle_stopped();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::project::{DisposeFuture, RuntimeResult};
    use pilotdeck_protocol::{JsValue, parse_json};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
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
                parse_json("{}").expect("#1022 abort handle snapshot")
            }
            fn abort(&self, _reason: &JsString) -> RuntimeResult {
                panic!("simulated session.abort() panic");
            }
            fn dispose(&self) -> DisposeFuture<'_> {
                Box::pin(async { Ok(()) })
            }
        }
        let handle = LiveSessionHandle::new(Arc::new(AbortPanicSession), None);
        let activity = handle
            .begin_activity()
            .expect("#1022 abort handle activity");
        let disposal = handle.dispose("boom".into());
        drop(activity);
        let error = tokio::time::timeout(std::time::Duration::from_secs(3), disposal)
            .await
            .expect("#1022 abort panic must deliver disposal result")
            .expect_err("#1022 abort handle error");
        assert_eq!(
            error.message, "Failed to dispose agent handle.",
            "#1022 abort panic aggregate"
        );
        assert_eq!(
            error
                .causes
                .first()
                .expect("#1022 abort handle cause")
                .message,
            "simulated session.abort() panic",
            "#1022 abort panic cause"
        );
        assert_eq!(
            handle.state(),
            HandleState::Disposed,
            "#1022 abort handle disposed"
        );
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

#[cfg(test)]
#[path = "handle_drop_tests.rs"]
mod drop_contract_tests;
