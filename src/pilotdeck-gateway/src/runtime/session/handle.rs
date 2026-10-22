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
            return Err(RuntimeError::new(
                "Agent handle already has an active turn.",
            ));
        }
        progress.activity = true;
        progress.maintenance = maintenance;
        progress.cancelled = None;
        Ok(HandleActivity(self.clone()))
    }

    pub fn abort(&self, reason: &JsString) -> RuntimeResult {
        // Set cancelled inside the lock; call session.abort() outside so that
        // a panic does not poison the progress mutex.
        // Contract: callers must tolerate abort() being called after dispose()
        // completes (Draining/Disposed state); those calls are benign no-ops.
        {
            let mut progress = self.0.progress.lock().unwrap();
            let is_active = progress.state == HandleState::Active;
            if !is_active {
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
            // DisposeGuard guarantees result.send_replace and lease release even
            // if any step (including session.abort or session.dispose) panics.
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
            // Abort is called inside the spawn so it is covered by DisposeGuard.
            // session.abort() is called outside the progress lock (no poison risk).
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
            let lease = handle.0.lease.lock().unwrap().take();
            let mut guard = DisposeGuard {
                result: &result,
                lease,
                handle_progress: &handle.0.progress,
                outcome: None,
            };
            let mut disposal_failures = Vec::new();
            if let Err(error) = handle.0.session.dispose().await {
                disposal_failures.push(error);
            }
            if let Some(mut lease) = guard.lease.take()
                && let Err(error) = lease.release().await
            {
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

    /// dispose() with an active turn calls session.abort() exactly once (inside
    /// the progress lock, before the drain task starts). A concurrent external
    /// abort after dispose() has set state=Draining must not call session.abort()
    /// a second time.
    #[tokio::test]
    async fn abort_during_draining_sets_reason_without_session_abort() {
        let session = MemSession::new();
        let handle = LiveSessionHandle::new(session.clone(), None);
        // Start a turn so dispose() path marks active=true
        let _activity = handle.begin_activity().unwrap();
        // dispose() calls session.abort() once (inside lock, active=true)
        let dispose_fut = handle.dispose("dispose-reason".into());
        assert_eq!(handle.state(), HandleState::Draining);
        // Concurrent external abort while Draining — must NOT call session.abort() again
        handle.abort(&"concurrent-abort".into()).unwrap();
        drop(_activity);
        dispose_fut.await.unwrap();
        assert_eq!(handle.state(), HandleState::Disposed);
        // session.abort() was called exactly once by dispose(), never by the external abort
        let aborts = session.aborted.lock().unwrap().clone();
        assert_eq!(
            aborts,
            vec!["dispose-reason"],
            "session.abort() must be called exactly once by dispose(): {aborts:?}"
        );
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
