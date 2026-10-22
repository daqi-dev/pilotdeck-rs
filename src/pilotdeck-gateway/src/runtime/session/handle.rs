//! Live writer identity, admission cutoff and resource drain. This is the
//! lifecycle seam for an AgentSession adapter, not a substitute agent loop.
use super::super::project::{DisposeFuture, RuntimeError, RuntimeLease, RuntimeResult};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// Implementations own the real session writer and its resources. No default
/// no-op implementation is supplied for unavailable providers.
pub trait LiveSession: Send + Sync {
    fn snapshot(&self) -> JsValue;
    /// May be called repeatedly while `dispose()` is in progress and after its
    /// future has completed: the handle remains Draining through the project
    /// lease release and its own session disposal. Implementations must make
    /// cancellation idempotent and must not panic, including after session
    /// resources have been disposed.
    /// Only a fully Disposed handle suppresses subsequent external aborts.
    /// This matches TS AgentHandle.ts:176-179,281-287.
    fn abort(&self, reason: &JsString) -> RuntimeResult;
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
    changed: watch::Sender<u64>,
}

/// Cloning preserves object identity. A project lease is held until the writer
/// has drained; it is never used as a handle ID or a connection generation.
#[derive(Clone)]
pub struct LiveSessionHandle(Arc<Inner>);

impl LiveSessionHandle {
    pub fn new(session: Arc<dyn LiveSession>, lease: Option<RuntimeLease>) -> Self {
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
            // session.abort() is outside the progress lock (no poison risk).
            // Called before the drain wait so the session can begin cancellation.
            let abort_error = if active {
                handle.0.session.abort(&reason).err()
            } else {
                None
            };
            let mut failures: Vec<_> = abort_error.into_iter().collect();
            let mut changed = handle.0.changed.subscribe();
            loop {
                if !handle.0.progress.lock().unwrap().activity {
                    break;
                }
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
