//! Live session ownership, volatile admission and completion coordination.
//! Transport routing and durable acceptance are assembled by later packages.
mod admission;
mod completion;
pub mod factory;
mod handle;
mod owner;

pub use admission::{SessionAdmission, TurnReservation};
pub use completion::{CompletionAbortReason, TurnCompletionFence, TurnCompletionHandle};
pub use handle::{
    HandleActivity, HandleState, LiveSession, LiveSessionHandle, LiveTurn, SessionNext,
    SessionSubmission,
};
pub use owner::{
    EvictionReason, MaintenanceReservation, SessionContext, SessionEvictionSnapshot,
    SessionFactory, SessionOwner, SessionOwnerOptions,
};

use pilotdeck_protocol::JsString;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Mutex;

/// InProcessGateway.abortTurn's volatile coordination. The live owner supplies
/// the abort sink; this layer does not manufacture or cache AgentSessions.
#[derive(Default)]
pub struct SessionTurns {
    pub admission: SessionAdmission,
    pub completion: TurnCompletionFence,
    aborted_reasons: Mutex<HashMap<JsString, JsString>>,
}

fn turn_key(session_key: &JsString, run_id: &JsString) -> JsString {
    let mut units = session_key.units().to_vec();
    units.push(0);
    units.extend_from_slice(run_id.units());
    JsString::from_utf16(units)
}

impl SessionTurns {
    pub fn aborted_reason(&self, session_key: &JsString, run_id: &JsString) -> Option<JsString> {
        self.aborted_reasons
            .lock()
            .unwrap()
            .get(&turn_key(session_key, run_id))
            .cloned()
    }

    /// Called by submit cleanup after consuming the abort reason.
    pub fn clear_aborted_reason(&self, session_key: &JsString, run_id: &JsString) {
        self.aborted_reasons
            .lock()
            .unwrap()
            .remove(&turn_key(session_key, run_id));
    }

    /// The synchronous prefix matches the TS method's execution before its
    /// first await. The sink receives the requested runId (including an explicit
    /// empty ID) and the reason. Under the live owner's serialization it MUST
    /// compare that ID with the current run before selecting its handle. A stale
    /// target is a no-op. Only None permits a session-wide abort. Selection and
    /// publication/cleanup must use the same owner lock; checking here alone
    /// cannot protect a sink invoked after a concurrent replacement.
    ///
    /// The sink runs outside all component locks and may return an async drain.
    ///
    /// An absent runId retains the baseline's unfenced socket-close behavior; this
    /// method intentionally does not infer a runId from connection history.
    pub fn abort_turn<'a, F, Fut>(
        &'a self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reason: Option<&JsString>,
        abort: F,
    ) -> impl Future<Output = ()> + Send + 'a + use<'a, F, Fut>
    where
        F: FnOnce(Option<JsString>, JsString) -> Fut,
        Fut: Future<Output = ()> + Send + 'a,
    {
        let operation = self.start_abort(session_key, run_id, reason, abort);
        let session_key = session_key.clone();
        async move {
            if let Some(operation) = operation {
                operation.await;
                self.completion.wait_for_completion(&session_key).await;
            }
        }
    }
    /// Synchronous cancel/Router prefix shared with the transport consumer
    /// fence. The caller chooses its wait immediately after Router returns.
    pub(super) fn start_abort<F, T>(
        &self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reason: Option<&JsString>,
        abort: F,
    ) -> Option<T>
    where
        F: FnOnce(Option<JsString>, JsString) -> T,
    {
        let reason = reason.cloned().unwrap_or_else(|| match run_id {
            Some(id) if !id.is_empty() => {
                let mut units = JsString::from("aborted:").units().to_vec();
                units.extend_from_slice(id.units());
                JsString::from_utf16(units)
            }
            _ => "aborted".into(),
        });
        // One admission snapshot supplies both the reason key and fallback
        // predicate. The getter selects/clones it under the admission lock.
        let active = self.admission.active_turn_run_id(session_key);
        if let Some(active) = active.as_ref()
            && !active.is_empty()
            && run_id.is_none_or(|id| id.is_empty() || id == active)
        {
            self.aborted_reasons
                .lock()
                .unwrap()
                .insert(turn_key(session_key, active), reason.clone());
        }
        let cancelled = self.completion.cancel(session_key, Some(&reason), run_id);
        let allowed = run_id.is_none() || cancelled || active.as_ref() == run_id;
        // Invoke the sink now, then select the wait after the sink's drain (as
        // TS does after `await router.abort`). Never hold locks across either.
        allowed.then(|| abort(run_id.cloned(), reason))
    }
}

#[cfg(test)]
mod tests;
