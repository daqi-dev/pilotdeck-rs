//! During migration, reflect forwarded turn admission in the same native owner.
//! This does not transfer SDK configuration into the upstream execution runtime.
use crate::{
    metadata::{Files, Metadata, text_field},
    runtime::session::{SessionOwner, TurnReservation},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{collections::HashMap, sync::Mutex};
type Admission = (JsString, JsString, TurnReservation);
pub(crate) struct UpstreamTurns {
    owner: Option<SessionOwner>,
    pending: Mutex<HashMap<JsString, Admission>>,
}
impl UpstreamTurns {
    pub fn new<F: Files>(metadata: Option<&Metadata<F>>) -> Self {
        let owner = metadata
            .and_then(|m| super::native::installed(&m.home, &m.cwd))
            .and_then(|state| state.lock().unwrap().session_owner.clone());
        Self {
            owner,
            pending: Mutex::new(HashMap::new()),
        }
    }
    pub fn request(&self, frame: &JsValue) {
        if text_field(frame, "method").as_deref() != Some("submit_turn") {
            return;
        }
        let Some(owner) = &self.owner else {
            return;
        };
        let Some(id) = frame.get("id").and_then(JsValue::as_string) else {
            return;
        };
        let Some(key) = frame
            .get("params")
            .and_then(|p| p.get("sessionKey"))
            .and_then(JsValue::as_string)
        else {
            return;
        };
        let mut pending = self.pending.lock().unwrap();
        if pending.contains_key(id) {
            return;
        }
        let run: JsString = format!("upstream-{}", uuid::Uuid::new_v4()).into();
        if let Some(turn) = owner.begin_turn(key, &run) {
            pending.insert(id.clone(), (key.clone(), run, turn));
        }
    }
    pub fn response(&self, frame: &JsValue) {
        let terminal = frame.get("final") == Some(&JsValue::Bool(true))
            || text_field(frame, "type").as_deref() == Some("response");
        if terminal
            && let Some(id) = frame.get("id").and_then(JsValue::as_string)
            && let Some((key, run, turn)) = self.pending.lock().unwrap().remove(id)
            && let Some(owner) = &self.owner
        {
            owner.end_turn(&key, Some(&run), Some(&turn));
        }
    }
}
impl Drop for UpstreamTurns {
    fn drop(&mut self) {
        if let Some(owner) = &self.owner {
            for (_, (key, run, turn)) in self.pending.get_mut().unwrap().drain() {
                owner.end_turn(&key, Some(&run), Some(&turn));
            }
        }
    }
}

impl crate::runtime::session::routes::passthrough::UpstreamTurnObserver for UpstreamTurns {
    fn response(&self, frame: &JsValue) {
        UpstreamTurns::response(self, frame);
    }
}
