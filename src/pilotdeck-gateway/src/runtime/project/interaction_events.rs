//! Native event port owned by one project generation. The interaction marker
//! does not imply availability: consumers must retrieve this concrete resource
//! and bind it to their leased turn. Permission/lifecycle/steer are independent.
use super::factory::FamilyResource;
use super::interaction_notice::DiscardNotice;
use super::{DisposeFuture, RuntimeError, RuntimeResult};
use crate::host::modules::{ModuleRouter, event};
use crate::host::{HostError, ModuleError};
use crate::runtime::cleanup_task::lock;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    closed: bool,
    releases: usize,
    turns: HashMap<JsString, Arc<Mutex<TurnState>>>,
}
struct TurnState {
    identity: event::TurnIdentity,
    run_id: JsString,
    closed: bool,
    events: Vec<JsObject>,
    // #686: notices from the discarded predecessor are injected only after
    // this turn's `turn_started`, so the stream integrity gate stays valid.
    notices: Vec<DiscardNotice>,
}

/// One real `createAgentEventBuffer` owner per generation, with isolated turn
/// buffers. Old generations remain usable while the registry drains leases.
#[derive(Clone, Default)]
pub struct InteractionEvents {
    state: Arc<Mutex<State>>,
}
impl InteractionEvents {
    /// A session has at most one current run in this resource. Even reuse of
    /// a run ID creates a new opaque token and revokes the previous handle.
    /// The caller must hold the generation's RuntimeLease for the whole turn.
    pub fn bind_turn(
        &self,
        identity: event::TurnIdentity,
        run_id: JsString,
        router: &mut ModuleRouter,
    ) -> Result<TurnEvents, HostError> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(closed());
        }
        let turn = Arc::new(Mutex::new(TurnState {
            identity: identity.clone(),
            run_id,
            closed: false,
            events: Vec::new(),
            notices: Vec::new(),
        }));
        if let Some(previous) = state
            .turns
            .insert(identity.session_id.clone(), turn.clone())
        {
            let mut previous = lock(&previous);
            previous.closed = true;
            previous.events.clear();
            previous.notices.clear();
        }
        let port = TurnEvents {
            generation: self.state.clone(),
            turn,
        };
        router.event = event::Handler::new(identity, Arc::new(port.clone()));
        Ok(port)
    }

    /// Repeated close is harmless; clearing all retained buffers occurs once.
    pub fn close(&self) {
        let mut state = lock(&self.state);
        if state.closed {
            return;
        }
        state.closed = true;
        state.releases += 1;
        for (_, turn) in state.turns.drain() {
            let mut turn = lock(&turn);
            turn.closed = true;
            turn.events.clear();
        }
    }

    pub fn is_closed(&self) -> bool {
        lock(&self.state).closed
    }
    pub fn release_count(&self) -> usize {
        lock(&self.state).releases
    }
}
impl FamilyResource for InteractionEvents {
    fn dispose(&self) -> DisposeFuture<'_> {
        // Closing happens when dispose is called, before polling or cancellation.
        self.close();
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// Opaque run token; no wire-provided generation/run ID can select another
/// run's buffer. All mutations lock generation then turn, including close.
#[derive(Clone)]
pub struct TurnEvents {
    generation: Arc<Mutex<State>>,
    turn: Arc<Mutex<TurnState>>,
}
impl TurnEvents {
    /// The host already owns turn_started. Return only its predecessor notices;
    /// emitting another start through the sidecar buffer would duplicate it.
    pub fn host_started(&self) -> Result<Vec<JsObject>, HostError> {
        let generation = lock(&self.generation);
        let mut turn = lock(&self.turn);
        check(&generation, &turn).map(|()| take_notices(&mut turn))
    }
    /// Checked counterpart of the transport's legacy infallible drain hook.
    pub fn drain(&self) -> Result<Vec<JsObject>, HostError> {
        let generation = lock(&self.generation);
        let mut turn = lock(&self.turn);
        check(&generation, &turn)?;
        Ok(std::mem::take(&mut turn.events))
    }

    pub fn run_id(&self) -> JsString {
        lock(&self.turn).run_id.clone()
    }

    /// Hold predecessor discard notices for this turn. They are appended after
    /// `turn_started` so the stream's lifecycle integrity check accepts them.
    pub fn queue_notices(&self, notices: Vec<DiscardNotice>) {
        if notices.is_empty() {
            return;
        }
        let generation = lock(&self.generation);
        let mut turn = lock(&self.turn);
        if !generation.closed && !turn.closed {
            turn.notices.extend(notices);
        }
    }

    /// End this run without revoking a newer run in the same session.
    pub fn close(&self) {
        let mut generation = lock(&self.generation);
        let mut turn = lock(&self.turn);
        turn.closed = true;
        turn.events.clear();
        turn.notices.clear();
        if generation
            .turns
            .get(&turn.identity.session_id)
            .is_some_and(|current| Arc::ptr_eq(current, &self.turn))
        {
            generation.turns.remove(&turn.identity.session_id);
        }
    }
}
impl event::EventSink for TurnEvents {
    fn emit(&self, event: JsObject) -> Result<(), HostError> {
        let generation = lock(&self.generation);
        let mut turn = lock(&self.turn);
        check(&generation, &turn)?;
        // Keep direct host-side producers under the same identity gate as the
        // sidecar handler; run/generation identity comes from the opaque token.
        if event.get("type").and_then(|v| v.as_string()).is_none()
            || event.get("sessionId").and_then(|v| v.as_string()) != Some(&turn.identity.session_id)
            || event
                .get("turnId")
                .is_some_and(|v| v.as_string() != Some(&turn.identity.turn_id))
        {
            return Err(
                ModuleError::new("Sidecar host event does not match the active turn.").into(),
            );
        }
        let started = event
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|value| value.equals_str("turn_started"));
        turn.events.push(event);
        if started {
            let notices = take_notices(&mut turn);
            turn.events.extend(notices);
        }
        Ok(())
    }
    fn drain_checked(&self) -> Result<Vec<JsObject>, HostError> {
        self.drain()
    }
    fn drain(&self) -> Vec<JsObject> {
        // Existing EventSink::drain cannot carry an error. Host composition
        // must use TurnEvents::drain to observe closed/stale state explicitly.
        self.drain().unwrap_or_default()
    }
}
fn take_notices(turn: &mut TurnState) -> Vec<JsObject> {
    std::mem::take(&mut turn.notices)
        .into_iter()
        .map(|notice| notice.to_warning(&turn.identity.session_id, &turn.identity.turn_id))
        .collect()
}
fn check(generation: &State, turn: &TurnState) -> Result<(), HostError> {
    if generation.closed {
        return Err(closed());
    }
    if turn.closed {
        return Err(ModuleError::new("Interaction event run is closed or superseded.").into());
    }
    Ok(())
}
fn closed() -> HostError {
    ModuleError::new("Interaction event generation is closed.").into()
}

/// Close the concrete event resource as part of factory rollback/drain. An
/// unexpected published type is an assembly error, never a successful no-op.
pub(super) fn close_resource(resource: Option<Arc<dyn FamilyResource>>) -> RuntimeResult {
    if let Some(resource) = resource {
        let events = resource
            .as_any()
            .downcast_ref::<InteractionEvents>()
            .ok_or_else(|| RuntimeError::new("Invalid interaction event resource"))?;
        events.close();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::interaction_notice::DiscardSource;
    use super::*;
    use crate::host::modules::event::EventSink as _;

    fn started() -> JsObject {
        let mut event = JsObject::new();
        event.insert("type".into(), "turn_started".into());
        event.insert("sessionId".into(), "s".into());
        event.insert("turnId".into(), "t".into());
        event
    }
    fn text(event: &JsObject, key: &str) -> String {
        event
            .get(key)
            .and_then(JsValue::as_string)
            .map(|value| value.to_utf8().unwrap())
            .unwrap_or_default()
    }

    #[test]
    fn queued_notice_is_injected_after_turn_started_exactly_once() {
        let resource = InteractionEvents::default();
        let mut router = ModuleRouter::default();
        let turn = resource
            .bind_turn(event::TurnIdentity::new("s", "t"), "r".into(), &mut router)
            .unwrap();
        turn.queue_notices(vec![DiscardNotice::new(
            DiscardSource::Steer,
            2,
            "s".into(),
            "old-run".into(),
        )]);
        turn.emit(started()).unwrap();
        let events = turn.drain().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(text(&events[0], "type"), "turn_started");
        assert_eq!(text(&events[1], "type"), "warning");
        assert_eq!(text(&events[1], "code"), "interaction_pending_discarded");
        assert_eq!(text(&events[1], "turnId"), "t");
        assert_eq!(
            text(&events[1], "message"),
            "Previous turn had 2 pending steer item(s) discarded."
        );
        let metadata = events[1]
            .get("metadata")
            .and_then(JsValue::as_object)
            .unwrap();
        assert_eq!(text(metadata, "source"), "steer");
        assert_eq!(text(metadata, "runId"), "old-run");
        // The notice is one-shot: the next drain has no repeat.
        turn.emit(started()).unwrap();
        let again = turn.drain().unwrap();
        assert_eq!(again.len(), 1);
        turn.close();
    }

    #[test]
    fn queued_notice_is_dropped_when_the_turn_is_already_closed() {
        let resource = InteractionEvents::default();
        let mut router = ModuleRouter::default();
        let turn = resource
            .bind_turn(event::TurnIdentity::new("s", "t"), "r".into(), &mut router)
            .unwrap();
        turn.close();
        turn.queue_notices(vec![DiscardNotice::new(
            DiscardSource::Hooks,
            1,
            "s".into(),
            "old-run".into(),
        )]);
        assert!(turn.drain().is_err());
    }
}
