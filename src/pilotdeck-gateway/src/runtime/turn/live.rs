//! LiveSession production adapter: HostClient → AgentEvent stream.
//!
//! This connects the gateway's submit pipeline to a real sidecar process via
//! the host module transport. TurnRunner.ts:195-217 is the baseline: after
//! recordAcceptedInput, the loop.run() generator yields AgentEvents until
//! turn_completed.
//!
//! StreamIntegrity tracks the turn lifecycle on top of the host's wire
//! sequence/identity checks: turn_started must open the stream, input_accepted
//! appears at most once, turn_failed may repeat (TurnRunner's catch re-emits it
//! after a finalization failure) and the module final is the only authoritative
//! terminal (the host already suppresses a sidecar's non-final turn_completed,
//! agentLoopSidecarClient.ts:680-684). The lifecycle is observed by a
//! synchronous `HostHandler::validate_event` hook on the host's receive path:
//! every event that passed wire validation has been observed by the time the
//! host returns a final, even when its application callback never completed —
//! the host's bounded cancel exit (POST_CANCEL_EXIT_TIMEOUT) may drop queued
//! applications while keeping the already-received final authoritative. The
//! terminal is therefore validated strictly against the received lifecycle; a
//! cancellation request alone never rescues a missing or contradictory one.
//! It does not claim dropped-frame or duplicate-frame detection.

use crate::{
    host::{
        Cancellation, ExecuteRequest, ExecuteResult, HostClient, HostError, HostErrorKind,
        HostFuture, HostHandler, ReconnectObservation,
    },
    runtime::{
        project::{DisposeFuture, RuntimeError, RuntimeResult},
        session::{LiveSession, LiveTurn, SessionNext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity, parse_json};
use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

type TurnHost = (HostClient, Arc<dyn HostHandler>);
type TurnHostFactory = dyn Fn() -> HostFuture<TurnHost> + Send + Sync;

/// Production LiveSession backed by a fresh HostClient and handler for each turn.
/// HostClient closes its process and disposes its handler after each execution.
pub struct LiveSessionAdapter {
    host_factory: Arc<TurnHostFactory>,
    state: Arc<Mutex<SessionState>>,
}

#[derive(Default)]
struct SessionState {
    disposed: bool,
    active: Option<Cancellation>,
}

struct ActiveTurn(Arc<Mutex<SessionState>>);

impl Drop for ActiveTurn {
    fn drop(&mut self) {
        self.0.lock().unwrap().active = None;
    }
}

impl LiveSessionAdapter {
    /// The factory must create turn-owned resources, including a fresh handler
    /// when its dispose implementation releases module resources.
    pub fn new(host_factory: impl Fn() -> HostFuture<TurnHost> + Send + Sync + 'static) -> Self {
        Self {
            host_factory: Arc::new(host_factory),
            state: Arc::default(),
        }
    }
}

impl LiveSession for LiveSessionAdapter {
    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        // Invalid requests must not occupy the session's active turn slot.
        let request = parse_execute_request(&input, &options)?;
        let cancellation = Cancellation::default();
        {
            let mut state = self.state.lock().unwrap();
            if state.disposed {
                return Err(RuntimeError::new("Agent session disposed."));
            }
            if state.active.is_some() {
                return Err(RuntimeError::new(
                    "Agent session already has an active turn.",
                ));
            }
            state.active = Some(cancellation.clone());
        }
        let active = ActiveTurn(self.state.clone());
        let host_factory = self.host_factory.clone();
        let identity = request.identity.clone();
        // Shared between the host-side validation hook (every received event)
        // and finish()'s strict terminal check; see StreamHandler.
        let integrity = Arc::new(StreamIntegrity::new());
        let hook_integrity = integrity.clone();
        // Keep at most one delivered event ahead of the submit consumer.
        let (events, receiver) = mpsc::channel(1);
        Ok(Box::new(LiveTurnStream {
            execution: Some(Box::pin(async move {
                let (client, inner) = tokio::select! {
                    biased;
                    host = host_factory() => host?,
                    _ = cancellation.cancelled() => {
                        return Ok(ExecuteResult::Aborted { terminal: None });
                    }
                };
                let handler = Arc::new(StreamHandler {
                    inner,
                    events,
                    integrity: hook_integrity,
                });
                client
                    .execute_cancellable(request, handler, cancellation)
                    .await
            })),
            result: None,
            events: receiver,
            identity,
            input,
            integrity,
            active: Some(active),
        }))
    }

    fn snapshot(&self) -> JsValue {
        // Return minimal snapshot; production sessions track state in transcript
        parse_json(r#"{"messages":[]}"#).unwrap()
    }

    fn abort(&self, reason: &JsString) -> RuntimeResult {
        if let Some(cancellation) = &self.state.lock().unwrap().active {
            cancellation.cancel(Some(reason.clone()));
        }
        Ok(())
    }

    fn dispose(&self) -> DisposeFuture<'_> {
        let mut state = self.state.lock().unwrap();
        state.disposed = true;
        if let Some(cancellation) = &state.active {
            cancellation.cancel(None);
        }
        Box::pin(async { Ok(()) })
    }
}

/// Streaming adapter for one turn execution.
struct LiveTurnStream {
    // Owned here rather than spawned: dropping the stream drops execute and
    // lets HostClient clean up its child and outstanding callbacks.
    execution: Option<HostFuture<ExecuteResult>>,
    result: Option<Result<ExecuteResult, HostError>>,
    events: mpsc::Receiver<JsValue>,
    identity: ExecutionIdentity,
    input: JsValue,
    // The same instance the host-side hook observes into, so finish() checks
    // the terminal against everything the sidecar actually sent.
    integrity: Arc<StreamIntegrity>,
    // Release the active slot after the final/error is delivered, or on drop.
    active: Option<ActiveTurn>,
}

impl LiveTurn for LiveTurnStream {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            let mut delivered = None;
            if let Some(execution) = self.execution.as_mut() {
                let mut settled = None;
                tokio::select! {
                    // Drain events already applied by the handler before returning
                    // either the terminal or an execution error. A closed receiver
                    // disables this arm while execute finishes cleanup.
                    biased;
                    Some(event) = self.events.recv() => {
                        delivered = Some(event);
                    }
                    result = execution => settled = Some(result),
                }
                if let Some(result) = settled {
                    self.result = Some(result);
                    self.execution = None;
                }
            }
            if let Some(event) = delivered {
                return Ok(Some(event));
            }
            // execute may have queued an event in the very poll that completed
            // it, after select already polled recv. Keep its result for next().
            if let Ok(event) = self.events.try_recv() {
                return Ok(Some(event));
            }
            match self.result.take() {
                Some(result) => {
                    let result = self.finish(result);
                    self.active.take();
                    result
                }
                None => Ok(None),
            }
        })
    }
}

impl LiveTurnStream {
    fn finish(
        &self,
        result: Result<ExecuteResult, HostError>,
    ) -> Result<Option<JsValue>, RuntimeError> {
        match result {
            Ok(ExecuteResult::Terminal(terminal)) => {
                let outcome = terminal
                    .get("payload")
                    .and_then(|payload| payload.get("result"))
                    .and_then(|result| result.get("type"))
                    .and_then(JsValue::as_string);
                // Strict check against everything the host received. The
                // host's bounded cancel exit (POST_CANCEL_EXIT_TIMEOUT,
                // host/execution.rs) may stop applying events and return an
                // already-validated final while queued applications are lost —
                // but their lifecycle was still observed by the receive-path
                // hook, so a missing input_accepted or turn_failed here is a
                // sidecar violation, not a host cleanup fact, and cancellation
                // changes nothing.
                self.integrity.observe_terminal(outcome)?;
                Ok(Some(self.completed_event(
                    terminal.get("payload").unwrap_or(&JsValue::Undefined),
                )))
            }
            Ok(ExecuteResult::Aborted { terminal }) => {
                self.integrity.observe_aborted()?;
                let terminal = terminal.unwrap_or_else(|| self.aborted_result());
                Ok(Some(self.completed_event(&terminal)))
            }
            Ok(ExecuteResult::Rejected(error)) => Err(RuntimeError {
                message: "Turn execution rejected.".into(),
                causes: vec![RuntimeError::new(format!("{:?}", error))],
            }),
            Err(error) => Err(map_host_error(error)),
        }
    }

    fn completed_event(&self, terminal: &JsValue) -> JsValue {
        let mut event = JsObject::new();
        event.insert("type".into(), "turn_completed".into());
        event.insert(
            "sessionId".into(),
            JsValue::String(self.identity.session_id.clone()),
        );
        event.insert(
            "turnId".into(),
            JsValue::String(self.identity.turn_id.clone()),
        );
        event.insert(
            "result".into(),
            terminal
                .get("result")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        event.insert(
            "messages".into(),
            terminal
                .get("messages")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        JsValue::Object(event)
    }

    fn aborted_result(&self) -> JsValue {
        // agentLoopSidecarClient.ts:1434-1450: both timestamps share one now;
        // an incomplete compaction final must retain the original messages.
        let now = SystemExecutionClock.now_iso();
        let mut result = JsObject::new();
        for (key, value) in [
            ("type", "aborted".into()),
            (
                "sessionId",
                JsValue::String(self.identity.session_id.clone()),
            ),
            ("turnId", JsValue::String(self.identity.turn_id.clone())),
            ("stopReason", "aborted_streaming".into()),
            ("usage", JsValue::Object(JsObject::new())),
            ("permissionDenials", JsValue::Array(vec![])),
            ("turns", JsValue::Number(0.0)),
            ("startedAt", JsValue::String(now.clone())),
            ("completedAt", JsValue::String(now)),
        ] {
            result.insert(key.into(), value);
        }
        let mut terminal = JsObject::new();
        terminal.insert("result".into(), JsValue::Object(result));
        terminal.insert(
            "messages".into(),
            self.input
                .get("messages")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        );
        JsValue::Object(terminal)
    }
}

/// Preserve host module/lifecycle behavior while exposing applied AgentEvents.
///
/// Ordering boundary: `validate_event` observes each event's lifecycle on the
/// host's receive path, before the inner (module-side) handler applies it and
/// before the host ACKs it. A violating event therefore never reaches the
/// inner handler, and a misbehaving sidecar is cut off at its first violating
/// event. `event` awaits `applied` and only then forwards the payload, so the
/// consumer only ever publishes events that were applied successfully — but
/// the integrity state may be ahead of what the consumer has pulled: events
/// whose application callback never completed (including the whole queue
/// dropped by the host's bounded cancel exit) are still observed. `finish()`
/// validates the terminal against that received lifecycle, never against the
/// delivered prefix.
///
/// `validate_event` forwards to `inner.validate_event` after its own lifecycle
/// check: an inner handler that rejects a payload keeps exactly its rejection
/// semantics when wrapped (the wrapper never silently skips it), and either
/// rejection fails the receive path before application and ACK.
struct StreamHandler {
    inner: Arc<dyn HostHandler>,
    events: mpsc::Sender<JsValue>,
    integrity: Arc<StreamIntegrity>,
}

impl HostHandler for StreamHandler {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        self.inner.module_call(call)
    }

    fn validate_event(&self, payload: &JsValue) -> Result<(), HostError> {
        // Own lifecycle check first: an invalid event is cut off before the
        // inner contract sees it, exactly like it is cut off before
        // application. Then forward the inner handler's validation so wrapping
        // it never drops a rejection a directly-embedded handler enforces.
        self.integrity
            .observe_event(payload)
            .map_err(|error| HostError::new(HostErrorKind::Protocol, error.message))?;
        self.inner.validate_event(payload)
    }

    fn event(&self, event: JsValue) -> HostFuture<()> {
        let final_event = event.get("final").and_then(JsValue::as_bool) == Some(true);
        let payload = event.get("payload").cloned();
        let applied = self.inner.event(event);
        let events = self.events.clone();
        Box::pin(async move {
            applied.await?;
            // The authoritative final is projected once from ExecuteResult.
            if !final_event {
                let payload = payload.ok_or_else(|| {
                    HostError::new(HostErrorKind::Protocol, "AgentEvent payload is missing.")
                })?;
                events.send(payload).await.map_err(|_| {
                    HostError::new(HostErrorKind::Callback, "LiveTurn event receiver closed.")
                })?;
            }
            Ok(())
        })
    }

    fn dispose(&self) -> HostFuture<()> {
        self.inner.dispose()
    }

    fn observe_transport(&self, observation: ReconnectObservation) {
        self.inner.observe_transport(observation);
    }
}

/// Turn lifecycle validation against the TurnRunner baseline. The host layer
/// owns wire sequence/identity; this machine only tracks lifecycle order:
/// turn_started opens the turn, input_accepted and turn_failed appear at most
/// once each in that order, and the authoritative terminal is projected exactly
/// once by the session from ExecuteResult. It does not detect unknown dropped
/// frames; the host's strict sequence check (client:879-893) owns that.
struct StreamIntegrity {
    state: Mutex<IntegrityState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntegrityState {
    /// No AgentEvent observed yet.
    Initial,
    /// turn_started observed.
    Started,
    /// input_accepted observed after turn_started.
    InputAccepted,
    /// turn_failed observed; it may legally repeat (TurnRunner's catch
    /// re-emits it after a finalization failure) and intermediate events may
    /// still arrive before the terminal.
    Failed,
    /// The authoritative terminal has been projected from ExecuteResult.
    Completed,
}

impl StreamIntegrity {
    fn new() -> Self {
        Self {
            state: Mutex::new(IntegrityState::Initial),
        }
    }

    /// Observe one AgentEvent received by the host (called from the
    /// `validate_event` hook on the receive path, in wire order, before the
    /// event is applied). Non-lifecycle events (model/tool/status/steer/
    /// file_artifacts/...) are legal while the turn is running; only their
    /// position relative to the lifecycle events and the terminal is
    /// constrained.
    fn observe_event(&self, event: &JsValue) -> Result<(), RuntimeError> {
        let Some(event_type) = event.get("type").and_then(JsValue::as_string) else {
            return Err(RuntimeError::new(
                "Stream integrity violation: AgentEvent without a type.",
            ));
        };
        let name = String::from_utf16_lossy(event_type.units());
        let mut state = self.state.lock().unwrap();
        // TurnRunner.ts:150: turn_started is always the first AgentEvent.
        if event_type.equals_str("turn_started") {
            return match *state {
                IntegrityState::Initial => {
                    *state = IntegrityState::Started;
                    Ok(())
                }
                other => Err(RuntimeError::new(format!(
                    "Stream integrity violation: turn_started after the turn already opened (state: {other:?})."
                ))),
            };
        }
        // TurnRunner.ts:217: input_accepted follows turn_started exactly once.
        if event_type.equals_str("input_accepted") {
            return match *state {
                IntegrityState::Started => {
                    *state = IntegrityState::InputAccepted;
                    Ok(())
                }
                other => Err(RuntimeError::new(format!(
                    "Stream integrity violation: input_accepted out of order (state: {other:?})."
                ))),
            };
        }
        // TurnRunner.ts:209,256,282,437: turn_failed precedes the terminal,
        // possibly before input_accepted (transcript write failure). A second
        // turn_failed is legal: TurnRunner.ts:397-402 forwards the loop's
        // turn_failed, and if a post-loop finalization step (:408-419) then
        // throws, the catch (:437) yields another turn_failed before the same
        // error terminal. hasRecordedVisibleFailureStatus dedupes only the
        // status event, not turn_failed itself.
        if event_type.equals_str("turn_failed") {
            return match *state {
                IntegrityState::Started | IntegrityState::InputAccepted => {
                    *state = IntegrityState::Failed;
                    Ok(())
                }
                IntegrityState::Failed => Ok(()),
                other => Err(RuntimeError::new(format!(
                    "Stream integrity violation: turn_failed out of order (state: {other:?})."
                ))),
            };
        }
        // The host suppresses a sidecar's non-final turn_completed
        // (agentLoopSidecarClient.ts:680-684); only the module final projected
        // by this session is authoritative, so one reaching here is a
        // double-published terminal.
        if event_type.equals_str("turn_completed") {
            return Err(RuntimeError::new(
                "Stream integrity violation: sidecar turn_completed must not reach the session stream; the module final is the authoritative terminal.",
            ));
        }
        match *state {
            IntegrityState::Started | IntegrityState::InputAccepted | IntegrityState::Failed => {
                Ok(())
            }
            IntegrityState::Initial => Err(RuntimeError::new(format!(
                "Stream integrity violation: event '{name}' before turn_started."
            ))),
            IntegrityState::Completed => Err(RuntimeError::new(format!(
                "Stream integrity violation: event '{name}' after the terminal (premature final)."
            ))),
        }
    }

    /// Observe the authoritative terminal projected from ExecuteResult.
    /// `outcome` is the terminal result's type (success/error/max_turns/aborted).
    /// The check is strict against the lifecycle received on the wire (the
    /// receive-path hook observed every event the host accepted, applied or
    /// not): a cancellation request alone never rescues a missing or
    /// contradictory lifecycle, because the host's cancel exit only drops
    /// unapplied applications, never received events. The final itself was
    /// already validated by the host layer.
    fn observe_terminal(&self, outcome: Option<&JsString>) -> Result<(), RuntimeError> {
        let outcome = outcome.map(|outcome| String::from_utf16_lossy(outcome.units()));
        let mut state = self.state.lock().unwrap();
        let current = *state;
        let accepted = match outcome.as_deref() {
            // Cancellation may stop a turn at any point, including before
            // turn_started reached the session (cancel before execute).
            Some("aborted") => true,
            // TurnRunner.ts:217: a successful loop always accepted input.
            Some("success") => current == IntegrityState::InputAccepted,
            // TurnRunner.ts:209,256,282,437: every error path yields turn_failed.
            Some("error") => current == IntegrityState::Failed,
            // AgentLoop.ts:2213-2224: max_turns yields status + turn_completed
            // only, without turn_failed; input was still accepted.
            Some("max_turns") => current == IntegrityState::InputAccepted,
            // The host never validates unknown outcomes into a terminal.
            _ => false,
        };
        if accepted {
            *state = IntegrityState::Completed;
            Ok(())
        } else {
            Err(RuntimeError::new(format!(
                "Stream integrity violation: terminal without proper lifecycle (outcome: {:?}, state: {current:?}).",
                outcome
            )))
        }
    }

    fn observe_aborted(&self) -> Result<(), RuntimeError> {
        // Aborted turns may stop before any lifecycle event was observed,
        // including an input cancelled before execute was sent.
        *self.state.lock().unwrap() = IntegrityState::Completed;
        Ok(())
    }
}

fn parse_execute_request(
    input: &JsValue,
    options: &JsValue,
) -> Result<ExecuteRequest, RuntimeError> {
    // Extract required fields from input/options
    let session_id = extract_string(options, "sessionId")?;
    let turn_id = extract_string(options, "turnId")?;
    let run_id = extract_string(options, "runId").unwrap_or_else(|_| turn_id.clone());
    let operation_id = extract_string(options, "operationId").unwrap_or_else(|_| "default".into());
    let request_id = extract_string(options, "requestId").unwrap_or_else(|_| "default".into());

    // The sidecar reads messages and execution parameters directly from payload.
    // Preserve all fields and JS values, including lone UTF-16 surrogates.
    let payload = input
        .as_object()
        .ok_or_else(|| RuntimeError::new("Expected execution input object"))?
        .clone();

    Ok(ExecuteRequest {
        identity: ExecutionIdentity {
            run_id,
            operation_id,
            request_id,
            session_id,
            turn_id,
        },
        payload,
        idempotency_key: None,
        operation_deadline: None,
    })
}

fn extract_string(obj: &JsValue, key: &str) -> Result<JsString, RuntimeError> {
    match obj {
        JsValue::Object(map) => map
            .get(key)
            .and_then(|v| match v {
                JsValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .ok_or_else(|| RuntimeError::new(format!("Missing or invalid field: {}", key))),
        _ => Err(RuntimeError::new("Expected object")),
    }
}

fn map_host_error(error: HostError) -> RuntimeError {
    RuntimeError::new(format!("Host execution failed: {}", error.message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event_of(event_type: &str) -> JsValue {
        parse_json(&format!(r#"{{"type":"{event_type}"}}"#)).unwrap()
    }

    fn outcome(kind: &str) -> Option<JsString> {
        Some(JsString::from(kind))
    }

    #[test]
    fn success_sequence_with_intermediate_events_is_accepted() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        integrity.observe_event(&event_of("agent_status")).unwrap();
        integrity.observe_event(&event_of("tool_progress")).unwrap();
        integrity
            .observe_event(&event_of("file_artifacts"))
            .unwrap();
        integrity
            .observe_event(&event_of("steer_unapplied"))
            .unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("success").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn early_failure_before_input_accepted_is_accepted() {
        // TurnRunner.ts:208-210: a transcript write failure ends the turn
        // before input_accepted with status, turn_failed, then the terminal.
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity.observe_event(&event_of("agent_status")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("error").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn failure_after_input_accepted_is_accepted() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        // Legal tail events after turn_failed (TurnRunner.ts:431-438).
        integrity
            .observe_event(&event_of("file_artifacts"))
            .unwrap();
        integrity
            .observe_event(&event_of("steer_unapplied"))
            .unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("error").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn second_turn_failed_before_the_error_terminal_is_accepted() {
        // TurnRunner.ts:397-402 forwards the loop's turn_failed; a post-loop
        // finalization step (:408-419) may then throw, and the catch (:437)
        // yields a second turn_failed before the same error terminal (:439).
        // hasRecordedVisibleFailureStatus dedupes only the status event, so
        // two turn_failed events are a legal TS stream.
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("error").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn max_turns_terminal_requires_no_turn_failed() {
        // AgentLoop.ts:2213-2224: max_turns yields status + turn_completed
        // only; requiring turn_failed here would falsely reject it.
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        integrity.observe_event(&event_of("model_event")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("max_turns").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn event_before_turn_started_is_rejected() {
        let integrity = StreamIntegrity::new();
        let result = integrity.observe_event(&event_of("model_event"));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("before turn_started"));
        // The terminal still fails closed afterwards.
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("success").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn duplicate_lifecycle_events_are_rejected() {
        // A second turn_started after the turn already opened.
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        let result = integrity.observe_event(&event_of("turn_started"));
        assert!(result.unwrap_err().message.contains("already opened"));

        // A second input_accepted after the turn is running.
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        let result = integrity.observe_event(&event_of("input_accepted"));
        assert!(result.unwrap_err().message.contains("out of order"));

        // A repeated turn_failed is NOT a duplicate: TurnRunner's catch may
        // re-emit it after a finalization failure before the same error
        // terminal (TurnRunner.ts:397-402, :437), unlike turn_started and
        // input_accepted, which appear exactly once.
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("error").as_ref())
                .is_ok()
        );
    }

    #[test]
    fn input_accepted_before_turn_started_is_rejected() {
        let integrity = StreamIntegrity::new();
        let result = integrity.observe_event(&event_of("input_accepted"));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("out of order"));
    }

    #[test]
    fn turn_failed_before_turn_started_is_rejected() {
        let integrity = StreamIntegrity::new();
        assert!(integrity.observe_event(&event_of("turn_failed")).is_err());
    }

    #[test]
    fn sidecar_turn_completed_is_rejected_as_non_authoritative() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        let result = integrity.observe_event(&event_of("turn_completed"));
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .message
                .contains("the module final is the authoritative terminal")
        );
    }

    #[test]
    fn event_after_terminal_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        integrity
            .observe_terminal(outcome("success").as_ref())
            .unwrap();
        let result = integrity.observe_event(&event_of("model_event"));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("premature final"));
    }

    #[test]
    fn event_without_a_type_is_rejected() {
        let integrity = StreamIntegrity::new();
        let result = integrity.observe_event(&JsValue::Object(JsObject::new()));
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("without a type"));
    }

    #[test]
    fn success_terminal_without_input_accepted_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        let result = integrity.observe_terminal(outcome("success").as_ref());
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("proper lifecycle"));
    }

    #[test]
    fn error_terminal_without_turn_failed_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        let result = integrity.observe_terminal(outcome("error").as_ref());
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("proper lifecycle"));
    }

    #[test]
    fn success_terminal_after_turn_failed_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("success").as_ref())
                .is_err()
        );
    }

    #[test]
    fn unknown_terminal_outcome_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("surprise").as_ref())
                .is_err()
        );
        assert!(integrity.observe_terminal(None).is_err());
    }

    #[test]
    fn second_terminal_projection_is_rejected() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        integrity
            .observe_terminal(outcome("success").as_ref())
            .unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("success").as_ref())
                .is_err()
        );
    }

    #[test]
    fn aborted_terminal_is_accepted_from_any_state() {
        for observations in [
            Vec::<&str>::new(),
            vec!["turn_started"],
            vec!["turn_started", "input_accepted"],
            vec!["turn_started", "input_accepted", "turn_failed"],
        ] {
            let integrity = StreamIntegrity::new();
            for event_type in &observations {
                integrity.observe_event(&event_of(event_type)).unwrap();
            }
            assert!(
                integrity
                    .observe_terminal(outcome("aborted").as_ref())
                    .is_ok()
            );
            assert!(integrity.observe_aborted().is_ok());
            // After an aborted terminal the stream is closed.
            assert!(integrity.observe_event(&event_of("model_event")).is_err());
        }
    }

    #[test]
    fn max_turns_terminal_after_turn_failed_is_rejected() {
        // turn_failed is only ever followed by the same error terminal
        // (TurnRunner.ts:437-439); a max_turns final after it contradicts the
        // loop's max-turns path, which never emits turn_failed
        // (AgentLoop.ts:2218-2233).
        let integrity = StreamIntegrity::new();
        integrity.observe_event(&event_of("turn_started")).unwrap();
        integrity
            .observe_event(&event_of("input_accepted"))
            .unwrap();
        integrity.observe_event(&event_of("turn_failed")).unwrap();
        assert!(
            integrity
                .observe_terminal(outcome("max_turns").as_ref())
                .is_err()
        );
    }

    #[test]
    fn a_final_missing_lifecycle_is_rejected_from_every_state() {
        // No cancellation flag can rescue these: the receive-path hook observed
        // what the sidecar actually sent, so a missing input_accepted or
        // turn_failed is a sidecar violation regardless of host cleanup.
        for observations in [Vec::<&str>::new(), vec!["turn_started"]] {
            for kind in ["success", "max_turns"] {
                let integrity = StreamIntegrity::new();
                for event_type in &observations {
                    integrity.observe_event(&event_of(event_type)).unwrap();
                }
                assert!(
                    integrity.observe_terminal(outcome(kind).as_ref()).is_err(),
                    "{kind} after {observations:?} must require input_accepted"
                );
            }
        }
        for observations in [Vec::<&str>::new(), vec!["turn_started"]] {
            let integrity = StreamIntegrity::new();
            for event_type in &observations {
                integrity.observe_event(&event_of(event_type)).unwrap();
            }
            assert!(
                integrity
                    .observe_terminal(outcome("error").as_ref())
                    .is_err(),
                "error after {observations:?} must require turn_failed"
            );
        }
    }
}

#[cfg(test)]
#[path = "live_stream_tests.rs"]
mod stream_tests;
