//! Exact handle identity and drain promises for Gateway-submitted turns.
use pilotdeck_protocol::JsString;
use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// AbortController.abort(undefined) has a default DOM AbortError; it is distinct
/// from an explicit empty reason. Host Cancellation has a different default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionAbortReason {
    DefaultAbort,
    Reason(JsString),
}

#[derive(Default)]
struct Progress {
    reason: Option<CompletionAbortReason>,
    done: bool,
}

struct Handle {
    run_id: JsString,
    progress: watch::Sender<Progress>,
}

/// Clones preserve identity, even when two different handles share a runId.
#[derive(Clone)]
pub struct TurnCompletionHandle(Arc<Handle>);

impl TurnCompletionHandle {
    pub fn run_id(&self) -> &JsString {
        &self.0.run_id
    }

    pub fn abort_reason(&self) -> Option<CompletionAbortReason> {
        self.0.progress.borrow().reason.clone()
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.progress.borrow().reason.is_some()
    }

    pub fn is_complete(&self) -> bool {
        self.0.progress.borrow().done
    }

    /// Retained watch state prevents missed wakeups, including a completion
    /// between creating this future and its first poll.
    pub fn done(&self) -> impl Future<Output = ()> + Send + use<> {
        let handle = self.clone();
        let mut receiver = self.0.progress.subscribe();
        async move {
            // A JS promise stays pending even if its resolver is discarded.
            // Keep the sender alive so dropping an obsolete handle cannot be
            // mistaken for completion via watch's channel-closed result.
            let _handle = handle;
            while !receiver.borrow_and_update().done {
                if receiver.changed().await.is_err() {
                    return;
                }
            }
        }
    }

    pub fn cancelled(&self) -> impl Future<Output = ()> + Send + use<> {
        let handle = self.clone();
        let mut receiver = self.0.progress.subscribe();
        async move {
            let _handle = handle;
            while receiver.borrow_and_update().reason.is_none() {
                if receiver.changed().await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Independent of Router admission. begin replaces the current handle without
/// resolving or cancelling the previous one, exactly like the TS fence.
#[derive(Default)]
pub struct TurnCompletionFence(Mutex<HashMap<JsString, TurnCompletionHandle>>);

impl TurnCompletionFence {
    pub fn begin(&self, session_key: &JsString, run_id: &JsString) -> TurnCompletionHandle {
        let handle = TurnCompletionHandle(Arc::new(Handle {
            run_id: run_id.clone(),
            progress: watch::channel(Progress::default()).0,
        }));
        self.0
            .lock()
            .unwrap()
            .insert(session_key.clone(), handle.clone());
        handle
    }

    pub fn is_current(&self, session_key: &JsString, handle: &TurnCompletionHandle) -> bool {
        self.0
            .lock()
            .unwrap()
            .get(session_key)
            .is_some_and(|current| Arc::ptr_eq(&current.0, &handle.0))
    }

    /// The optional runId is checked for presence, not truthiness. Repeated
    /// cancellation still returns true; AbortController keeps the first reason.
    pub fn cancel(
        &self,
        session_key: &JsString,
        reason: Option<&JsString>,
        run_id: Option<&JsString>,
    ) -> bool {
        let state = self.0.lock().unwrap();
        let Some(handle) = state.get(session_key) else {
            return false;
        };
        if run_id.is_some_and(|id| handle.run_id() != id) {
            return false;
        }
        handle.0.progress.send_if_modified(|progress| {
            if progress.reason.is_some() {
                return false;
            }
            progress.reason = Some(
                reason.map_or(CompletionAbortReason::DefaultAbort, |reason| {
                    CompletionAbortReason::Reason(reason.clone())
                }),
            );
            true
        });
        true
    }

    pub fn complete(&self, session_key: &JsString, handle: &TurnCompletionHandle) {
        let mut state = self.0.lock().unwrap();
        if state
            .get(session_key)
            .is_some_and(|current| Arc::ptr_eq(&current.0, &handle.0))
        {
            state.remove(session_key);
        }
        // A stale complete resolves its own waiters but cannot remove the new
        // handle. Completion is independent of the cancellation signal.
        handle.0.progress.send_if_modified(|progress| {
            let changed = !progress.done;
            progress.done = true;
            changed
        });
    }

    /// Snapshot *when called*, rather than on first poll. Replacement after a
    /// wait starts must not redirect that wait to a newer turn.
    pub fn wait_for_completion(
        &self,
        session_key: &JsString,
    ) -> impl Future<Output = ()> + Send + use<> {
        let handle = self.0.lock().unwrap().get(session_key).cloned();
        async move {
            if let Some(handle) = handle {
                handle.done().await;
            }
        }
    }
}
