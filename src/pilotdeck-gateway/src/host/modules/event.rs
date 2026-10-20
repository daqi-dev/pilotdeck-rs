//! `event.emit` host adapter (TS `sidecarDefaultModuleDispatcher.ts:197-201,471`).
//!
//! The dispatcher only owns the wire check and the active-turn identity gate.
//! Delivery and the optional replay buffer are host port concerns: the TS port
//! is `Readonly<{ emit: AgentEventEmitter; drain?: () => AgentEvent[] }>`
//! (`sidecarHostModulePorts.ts:112-115`), and the client loop drains that buffer
//! between protocol steps (`agentLoopSidecarClient.ts:379,388,393`).
use super::wire;
use crate::host::{HostError, HostFuture, JsObject, JsValue, ModuleError};
use pilotdeck_protocol::JsString;
use std::sync::Arc;

/// Session and turn the dispatcher was bound to for this sidecar execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnIdentity {
    pub session_id: JsString,
    pub turn_id: JsString,
}
impl TurnIdentity {
    pub fn new(session_id: impl Into<JsString>, turn_id: impl Into<JsString>) -> Self {
        Self {
            session_id: session_id.into(),
            turn_id: turn_id.into(),
        }
    }
}

/// Host sink for events that passed the dispatcher identity gate.
///
/// `emit` is synchronous like TS `AgentEventEmitter = (event) => void`
/// (`agent/protocol/events.ts:151`): an emitter records or hands off the event
/// without suspending the module callback, so implementations must not block.
///
/// Ordering and concurrency: `emit` is called on the host driver task, one
/// call at a time, in the arrival order of the sidecar's `event.emit` module
/// calls (the driver constructs each callback future in arrival order and
/// this handler emits during construction). An implementation may therefore
/// append to its buffer without its own sequencing.
/// `drain` mirrors the optional TS port member and
/// `createAgentEventBuffer().drain`, which removes and returns every buffered
/// event in emission order (`events.ts:155-162`). Returning an empty vector
/// models a port that was installed without a buffer.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: JsObject) -> Result<(), HostError>;
    fn drain(&self) -> Vec<JsObject> {
        Vec::new()
    }
}

#[derive(Clone)]
struct Ports {
    identity: TurnIdentity,
    sink: Arc<dyn EventSink>,
}

/// One handler instance belongs to one turn. The default value has no sink, so
/// a `ModuleRouter` assembled before event wiring fails closed on `emit`
/// instead of silently discarding host events.
#[derive(Clone, Default)]
pub struct Handler {
    ports: Option<Ports>,
}
impl Handler {
    pub fn new(identity: TurnIdentity, sink: Arc<dyn EventSink>) -> Self {
        Self {
            ports: Some(Ports { identity, sink }),
        }
    }
    /// Validates and emits synchronously, before the returned future is
    /// polled: the host driver calls `module_call` in arrival order, so the
    /// sink sees events in the order the sidecar sent them even though the
    /// callback futures then run concurrently (TS: the async handler reaches
    /// `emit` without an await, in message order).
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        let result = self.emit_now(&call);
        Box::pin(async move { result })
    }
    fn emit_now(&self, call: &JsValue) -> Result<JsObject, HostError> {
        let payload = call.get("payload").and_then(JsValue::as_object);
        let operation = wire::string_field(payload, "operation")?;
        if !operation.equals_str("emit") {
            return Err(ModuleError::new("Unsupported sidecar event operation.").into());
        }
        let ports = self
            .ports
            .as_ref()
            .ok_or_else(|| ModuleError::new("Host did not provide an event sink."))?;
        let event = read_host_emitted_event(payload.and_then(|p| p.get("event")), &ports.identity)?;
        ports.sink.emit(event)?;
        let mut response = JsObject::new();
        response.insert("result".into(), JsValue::Null);
        Ok(response)
    }
    /// Hand the buffered host events to the turn loop. Draining is owned by the
    /// client loop and is not an operation of the `event` module.
    pub fn drain(&self) -> Vec<JsObject> {
        self.ports
            .as_ref()
            .map(|ports| ports.sink.drain())
            .unwrap_or_default()
    }
    pub fn dispose(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}

/// Read the event shape accepted by `readHostEmittedEvent` (`dispatcher:471`).
///
/// `turnId` is optional by property presence, not by truthiness: a present key
/// must equal the active turn even when its value is `undefined`. `type` only
/// has to be a string, so the empty string and lone surrogates are accepted.
fn read_host_emitted_event(
    value: Option<&JsValue>,
    identity: &TurnIdentity,
) -> Result<JsObject, HostError> {
    let event = value
        .and_then(JsValue::as_object)
        .ok_or_else(identity_mismatch)?;
    let type_matches = matches!(event.get("type"), Some(JsValue::String(_)));
    let session_matches = event
        .get("sessionId")
        .and_then(JsValue::as_string)
        .is_some_and(|session_id| session_id == &identity.session_id);
    let turn_matches = match event.get("turnId") {
        None => true,
        Some(turn_id) => turn_id.as_string() == Some(&identity.turn_id),
    };
    if !type_matches || !session_matches || !turn_matches {
        return Err(identity_mismatch());
    }
    Ok(event.clone())
}

fn identity_mismatch() -> HostError {
    ModuleError::new("Sidecar host event does not match the active turn.").into()
}
