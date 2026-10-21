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
        if progress.maintenance {
            progress.cancelled.get_or_insert_with(|| reason.clone());
        }
        let (result, completion) = watch::channel(None);
        progress.disposal = Some(completion.clone());
        drop(progress);
        let abort_error = active.then(|| self.abort(&reason).err()).flatten();
        let handle = self.clone();
        tokio::spawn(async move {
            let mut failures: Vec<_> = abort_error.into_iter().collect();
            let mut changed = handle.0.changed.subscribe();
            loop {
                if !handle.0.progress.lock().unwrap().activity {
                    break;
                }
                let _ = changed.changed().await;
            }
            let mut disposal_failures = Vec::new();
            if let Err(error) = handle.0.session.dispose().await {
                disposal_failures.push(error);
            }
            let lease = handle.0.lease.lock().unwrap().take();
            if let Some(mut lease) = lease
                && let Err(error) = lease.release().await
            {
                disposal_failures.push(error);
            }
            if let Err(error) = aggregate("Failed to dispose agent session.", disposal_failures) {
                failures.push(error);
            }
            handle.0.progress.lock().unwrap().state = HandleState::Disposed;
            result.send_replace(Some(aggregate("Failed to dispose agent handle.", failures)));
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
