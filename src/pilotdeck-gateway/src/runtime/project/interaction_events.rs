//! Native event port owned by one project generation. The interaction marker
//! does not imply availability: consumers must retrieve this concrete resource
//! and bind it to their leased turn. Permission/lifecycle/steer are independent.
use super::factory::FamilyResource;
use super::{DisposeFuture, RuntimeError, RuntimeResult};
use crate::host::modules::{ModuleRouter, event};
use crate::host::{HostError, ModuleError};
use pilotdeck_protocol::{JsObject, JsString};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

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
        }));
        if let Some(previous) = state
            .turns
            .insert(identity.session_id.clone(), turn.clone())
        {
            let mut previous = lock(&previous);
            previous.closed = true;
            previous.events.clear();
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

    /// End this run without revoking a newer run in the same session.
    pub fn close(&self) {
        let mut generation = lock(&self.generation);
        let mut turn = lock(&self.turn);
        turn.closed = true;
        turn.events.clear();
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
        turn.events.push(event);
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
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
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
