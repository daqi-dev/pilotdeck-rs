//! LiveSession production adapter: HostClient → AgentEvent stream.
//!
//! This connects the gateway's submit pipeline to a real sidecar process via
//! the host module transport. TurnRunner.ts:195-217 is the baseline: after
//! recordAcceptedInput, the loop.run() generator yields AgentEvents until
//! turn_completed.
//!
//! StreamIntegrity currently checks only the terminal completion sequence.
//! Event observation is not wired here yet, so this adapter does not claim
//! dropped-frame or duplicate-frame detection; unobserved terminals fail closed.

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
                let handler = Arc::new(StreamHandler { inner, events });
                client
                    .execute_cancellable(request, handler, cancellation)
                    .await
            })),
            result: None,
            events: receiver,
            identity,
            input,
            integrity: StreamIntegrity::new(),
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
    integrity: StreamIntegrity,
    // Release the active slot after the final/error is delivered, or on drop.
    active: Option<ActiveTurn>,
}

impl LiveTurn for LiveTurnStream {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            if let Some(execution) = self.execution.as_mut() {
                self.result = Some(tokio::select! {
                    // Drain events already applied by the handler before returning
                    // either the terminal or an execution error. A closed receiver
                    // disables this arm while execute finishes cleanup.
                    biased;
                    Some(event) = self.events.recv() => return Ok(Some(event)),
                    result = execution => result,
                });
                self.execution = None;
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
                // Event observation remains deferred to A7.2-pr6b.
                self.integrity.observe_terminal()?;
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
struct StreamHandler {
    inner: Arc<dyn HostHandler>,
    events: mpsc::Sender<JsValue>,
}

impl HostHandler for StreamHandler {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        self.inner.module_call(call)
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

/// Terminal sequence validation only. Event observation is not yet wired into
/// production, so an unobserved non-aborted terminal is rejected fail-closed.
struct StreamIntegrity {
    state: Mutex<IntegrityState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
enum IntegrityState {
    Initial,
    Started,
    InputAccepted,
    #[allow(dead_code)]
    Terminal,
    Completed,
}

impl StreamIntegrity {
    fn new() -> Self {
        Self {
            state: Mutex::new(IntegrityState::Initial),
        }
    }

    #[allow(dead_code)]
    fn observe_event(&self, event_type: &str) -> Result<(), RuntimeError> {
        let mut state = self.state.lock().unwrap();

        match (*state, event_type) {
            (IntegrityState::Initial, "turn_started") => {
                *state = IntegrityState::Started;
                Ok(())
            }
            (IntegrityState::Started, "input_accepted") => {
                *state = IntegrityState::InputAccepted;
                Ok(())
            }
            (IntegrityState::InputAccepted, "turn_completed") => {
                *state = IntegrityState::Completed;
                Ok(())
            }
            (IntegrityState::Completed, _) => Err(RuntimeError::new(format!(
                "Stream integrity violation: event '{}' after turn_completed (premature final)",
                event_type
            ))),
            (current, event) => {
                // Duplicate or out-of-order event
                Err(RuntimeError::new(format!(
                    "Stream integrity violation: unexpected event '{}' in state {:?}",
                    event, current
                )))
            }
        }
    }

    fn observe_terminal(&self) -> Result<(), RuntimeError> {
        let state = self.state.lock().unwrap();
        match *state {
            IntegrityState::Completed => Ok(()),
            other => Err(RuntimeError::new(format!(
                "Stream integrity violation: terminal without proper completion sequence (state: {:?})",
                other
            ))),
        }
    }

    fn observe_aborted(&self) -> Result<(), RuntimeError> {
        // Aborted turns may not have completed the full sequence
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

    #[test]
    fn stream_integrity_valid_sequence() {
        let integrity = StreamIntegrity::new();
        assert!(integrity.observe_event("turn_started").is_ok());
        assert!(integrity.observe_event("input_accepted").is_ok());
        assert!(integrity.observe_event("turn_completed").is_ok());
        assert!(integrity.observe_terminal().is_ok());
    }

    #[test]
    fn stream_integrity_detects_premature_final() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event("turn_started").unwrap();
        integrity.observe_event("input_accepted").unwrap();
        integrity.observe_event("turn_completed").unwrap();

        // Event after turn_completed should fail
        let result = integrity.observe_event("thinking");
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("premature final"));
    }

    #[test]
    fn stream_integrity_detects_missing_start() {
        let integrity = StreamIntegrity::new();

        // Skipping turn_started should fail
        let result = integrity.observe_event("input_accepted");
        assert!(result.is_err());
    }

    #[test]
    fn stream_integrity_detects_incomplete_terminal() {
        let integrity = StreamIntegrity::new();
        integrity.observe_event("turn_started").unwrap();

        // Terminal without turn_completed should fail
        let result = integrity.observe_terminal();
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .message
                .contains("without proper completion")
        );
    }

    #[test]
    fn stream_integrity_allows_aborted() {
        let integrity = StreamIntegrity::new();
        // Aborted turns don't require full sequence
        assert!(integrity.observe_aborted().is_ok());
    }
}

#[cfg(test)]
#[path = "live_stream_tests.rs"]
mod stream_tests;
