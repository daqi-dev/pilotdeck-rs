//! Volatile admission and completion primitives for the future live session
//! owner. No transport routing or durable acceptance is enabled here.
mod admission;
mod completion;

pub use admission::SessionAdmission;
pub use completion::{CompletionAbortReason, TurnCompletionFence, TurnCompletionHandle};

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
    /// first await. The sink must synchronously target the live handle (and may
    /// return an async drain). Owner publication/cleanup must use the same owner
    /// serialization when this is integrated with live sessions in pr2.
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
        F: FnOnce(JsString) -> Fut,
        Fut: Future<Output = ()> + Send + 'a,
    {
        let reason = reason.cloned().unwrap_or_else(|| match run_id {
            Some(id) if !id.is_empty() => {
                let mut units = JsString::from("aborted:").units().to_vec();
                units.extend_from_slice(id.units());
                JsString::from_utf16(units)
            }
            _ => "aborted".into(),
        });
        if let Some(active) = self.admission.active_turn_run_id(session_key)
            && !active.is_empty()
            && run_id.is_none_or(|id| id.is_empty() || id == &active)
        {
            self.aborted_reasons
                .lock()
                .unwrap()
                .insert(turn_key(session_key, &active), reason.clone());
        }
        let cancelled = self.completion.cancel(session_key, Some(&reason), run_id);
        let allowed = run_id.is_none()
            || cancelled
            || self.admission.active_turn_run_id(session_key).as_ref() == run_id;
        // Invoke the sink now, then select the wait after the sink's drain (as
        // TS does after `await router.abort`). Never hold locks across either.
        let operation = allowed.then(|| abort(reason));
        let session_key = session_key.clone();
        async move {
            if let Some(operation) = operation {
                operation.await;
                self.completion.wait_for_completion(&session_key).await;
            }
        }
    }
}

#[cfg(test)]
mod tests;
