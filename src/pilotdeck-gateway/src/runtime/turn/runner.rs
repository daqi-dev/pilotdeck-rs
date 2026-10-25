//! Durable TurnRunner transitions. The LiveSession/AgentLoop wiring is separate.
//! Transitions consume the runner: cancelling one cannot retry a write whose
//! blocking worker is still running. The worker retains the session lease.
use crate::{
    metadata::object,
    runtime::project::RuntimeLease,
    storage::recorder::{Recorder, RecorderError, RecorderResult},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::{server::ExecutionClock, system_clock::SystemExecutionClock};
use std::sync::Arc;

pub enum AgentInput {
    Text(JsString),
    Blocks(Vec<JsValue>),
}

/// History is loop-only; synthetic input is durable but never acknowledged as
/// the user's input. Metadata is the host's acceptedInputMetadata projection.
pub struct TurnInput {
    pub input: AgentInput,
    pub history: Vec<JsValue>,
    pub synthetic: Vec<JsValue>,
    pub metadata: Option<JsObject>,
}

#[derive(PartialEq)]
enum Phase {
    New,
    Started,
    Accepted,
    Completed,
}

/// Owns recorder transitions, not hooks, stream integrity, or the active slot.
/// The caller must retain the returned runner until completion or consumer drop.
pub struct TurnRunner {
    recorder: Arc<Recorder>,
    lease: Arc<RuntimeLease>,
    session: JsString,
    turn: JsString,
    now: Arc<dyn Fn() -> JsString + Send + Sync>,
    phase: Phase,
    messages: Vec<JsValue>,
    failure_status: bool,
}
pub type Transition = RecorderResult<(TurnRunner, Vec<JsValue>)>;

impl TurnRunner {
    pub fn new(
        recorder: Arc<Recorder>,
        lease: Arc<RuntimeLease>,
        session: JsString,
        turn: JsString,
    ) -> Self {
        Self {
            recorder,
            lease,
            session,
            turn,
            now: Arc::new(|| SystemExecutionClock.now_iso()),
            phase: Phase::New,
            messages: vec![],
            failure_status: false,
        }
    }
    pub fn messages(&self) -> &[JsValue] {
        &self.messages
    }
    pub fn is_complete(&self) -> bool {
        self.phase == Phase::Completed
    }

    async fn io(
        &self,
        write: impl FnOnce(&Recorder, &JsString, &JsString) -> RecorderResult<()> + Send + 'static,
    ) -> RecorderResult<()> {
        let (recorder, lease, session, turn) = (
            self.recorder.clone(),
            self.lease.clone(),
            self.session.clone(),
            self.turn.clone(),
        );
        tokio::task::spawn_blocking(move || {
            let result = write(&recorder, &session, &turn);
            drop(lease);
            result
        })
        .await
        .map_err(|e| RecorderError::State(e.to_string()))?
    }
    fn require(&self, phase: Phase) -> RecorderResult<()> {
        if self.phase == phase {
            Ok(())
        } else {
            Err(RecorderError::State(
                "Invalid turn recording transition.".into(),
            ))
        }
    }
    fn event(
        &self,
        kind: &str,
        fields: impl IntoIterator<Item = (&'static str, JsValue)>,
    ) -> JsValue {
        object(
            [
                ("type", kind.into()),
                ("sessionId", JsValue::String(self.session.clone())),
                ("turnId", JsValue::String(self.turn.clone())),
            ]
            .into_iter()
            .chain(fields),
        )
    }
    pub async fn start(mut self) -> Transition {
        self.require(Phase::New)?;
        self.io(|r, s, t| r.start_turn(s, t)).await?;
        self.phase = Phase::Started;
        let event = self.event("turn_started", []);
        Ok((self, vec![event]))
    }
    pub async fn accept(mut self, input: TurnInput) -> Transition {
        self.require(Phase::Started)?;
        let content = match input.input {
            AgentInput::Text(text) => vec![object([
                ("type", "text".into()),
                ("text", JsValue::String(text)),
            ])],
            AgentInput::Blocks(content) => content,
        };
        let accepted = vec![object([
            ("role", "user".into()),
            ("content", JsValue::Array(content)),
        ])];
        let durable: Vec<_> = accepted.iter().cloned().chain(input.synthetic).collect();
        self.messages = input.history;
        let written = durable.clone();
        if let Err(error) = self
            .io(move |r, s, t| {
                Ok(r.writer()
                    .record_accepted_input(s, t, written, input.metadata)?)
            })
            .await
        {
            let error = object([
                ("code", "agent_transcript_error".into()),
                ("message", "Failed to record accepted input.".into()),
                (
                    "details",
                    match error {
                        RecorderError::Envelope { code, .. } => object([
                            ("code", code.into()),
                            ("name", "SessionEventValidationError".into()),
                        ]),
                        RecorderError::Domain { code, .. } => object([
                            ("code", code.into()),
                            ("name", "SessionDomainValidationError".into()),
                        ]),
                        _ => object([]),
                    },
                ),
            ]);
            // JsonlTranscriptWriter structuredClone drops Error custom fields;
            // the live AgentError retains those enumerable code/name fields.
            let mut persisted = error.clone();
            if let JsValue::Object(fields) = &mut persisted {
                fields.insert("details".into(), object([]));
            }
            return self.fail_inner(error, persisted).await;
        }
        self.messages.extend(durable);
        self.phase = Phase::Accepted;
        let event = self.event("input_accepted", [("messages", JsValue::Array(accepted))]);
        Ok((self, vec![event]))
    }
    /// Call for the loop's turn_failed, preserving repeated failures but only
    /// inserting one durable visible failure status before them.
    pub async fn loop_failed(mut self, error: JsValue) -> Transition {
        self.require(Phase::Accepted)?;
        let mut events = vec![];
        if !self.failure_status {
            events.push(self.status(&error).await);
            self.failure_status = true;
        }
        events.push(self.event("turn_failed", [("error", error)]));
        Ok((self, events))
    }
    pub async fn complete(mut self, result: JsValue) -> Transition {
        self.require(Phase::Accepted)?;
        if result.get("sessionId") != Some(&JsValue::String(self.session.clone()))
            || result.get("turnId") != Some(&JsValue::String(self.turn.clone()))
        {
            return Err(RecorderError::State(
                "Turn result identity mismatch.".into(),
            ));
        }
        let written = result.clone();
        if let Err(error) = self.io(move |r, s, t| r.complete_turn(s, t, written)).await {
            return self
                .fail(object([
                    ("code", "agent_invalid_state".into()),
                    ("message", JsValue::String(error.to_string().into())),
                ]))
                .await;
        }
        self.phase = Phase::Completed;
        let event = self.event("turn_completed", [("result", result)]);
        Ok((self, vec![event]))
    }
    /// Catch path: complete first, then best-effort status and visible failure.
    pub async fn fail(self, error: JsValue) -> Transition {
        self.fail_inner(error.clone(), error).await
    }
    async fn fail_inner(mut self, error: JsValue, persisted: JsValue) -> Transition {
        if !matches!(self.phase, Phase::Started | Phase::Accepted) {
            return Err(RecorderError::State(
                "Invalid turn recording transition.".into(),
            ));
        }
        let now = (self.now)();
        let mut result = object([
            ("type", "error".into()),
            ("sessionId", JsValue::String(self.session.clone())),
            ("turnId", JsValue::String(self.turn.clone())),
            (
                "stopReason",
                if error.get("code") == Some(&"agent_aborted".into()) {
                    "aborted_streaming"
                } else {
                    "model_error"
                }
                .into(),
            ),
            ("usage", object([])),
            ("permissionDenials", JsValue::Array(vec![])),
            ("turns", JsValue::Number(0.0)),
            ("startedAt", JsValue::String(now.clone())),
            ("completedAt", JsValue::String(now)),
            ("errors", JsValue::Array(vec![persisted])),
        ]);
        let written = result.clone();
        self.io(move |r, s, t| r.complete_turn(s, t, written))
            .await?;
        if let JsValue::Object(fields) = &mut result {
            fields.insert("errors".into(), JsValue::Array(vec![error.clone()]));
        }
        let status = self.status(&error).await;
        self.phase = Phase::Completed;
        let events = vec![
            status,
            self.event("turn_failed", [("error", error)]),
            self.event("turn_completed", [("result", result)]),
        ];
        Ok((self, events))
    }
    async fn status(&self, error: &JsValue) -> JsValue {
        let message = error.get("message").cloned().unwrap_or(JsValue::Undefined);
        let detail = object([("message", message.clone()), ("code", error.get("code").cloned().unwrap_or(JsValue::Undefined)),
            ("severity", "error".into()), ("visible", JsValue::Bool(true)),
            ("userHint", error.get("userHint").filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined)).cloned().unwrap_or_else(|| "Retry the turn; if it repeats, check the gateway logs or adjust the request.".into())),
            ("scope", "turn".into()), ("source", "agent".into())]);
        let fields = [
            ("event", "turn_failed".into()),
            ("kind", "error".into()),
            ("text", message),
            ("detail", detail),
        ];
        let written = object(fields.clone());
        let _ = self
            .io(move |r, s, t| Ok(r.writer().record_agent_status_message(s, t, &written)?))
            .await;
        self.event("agent_status", fields)
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
