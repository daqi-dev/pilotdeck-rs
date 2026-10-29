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

#[path = "runner_recording.rs"]
mod recording;
pub use recording::{Recording, RecordingAction};
#[path = "runner_prompt.rs"]
mod prompt;
#[path = "runner_tail.rs"]
pub mod tail;
/// Host-only prefix, shared by emission and sidecar source validation.
pub fn host_lifecycle_events() -> [&'static str; 3] {
    if crate::mutants::on("tail-host-prefix-constant") {
        [
            "turn_started",
            "input_accepted",
            "mutated_user_prompt_submitted",
        ]
    } else {
        ["turn_started", "input_accepted", "user_prompt_submitted"]
    }
}
pub(crate) use prompt::PromptHooks;

#[derive(Clone)]
pub enum AgentInput {
    Text(JsString),
    Blocks(Vec<JsValue>),
}

/// History is loop-only; synthetic input is durable but never acknowledged as
/// the user's input. Metadata is the host's acceptedInputMetadata projection.
#[derive(Clone)]
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

/// Owns recorder transitions and prompt hooks, not stream integrity or the active slot.
/// The caller must retain the returned runner until completion or consumer drop.
pub struct TurnRunner {
    recorder: Arc<Recorder>,
    lease: Arc<RuntimeLease>,
    owner: Option<Arc<dyn Send + Sync>>,
    session: JsString,
    turn: JsString,
    now: Arc<dyn Fn() -> JsString + Send + Sync>,
    phase: Phase,
    messages: Vec<JsValue>,
    failure_status: bool,
    callback_error: Option<String>,
    prompt_hooks: Option<PromptHooks>,
    prompt_submitted: bool,
    tail: Option<tail::TurnTail>,
    progress:
        Option<tokio::sync::mpsc::UnboundedSender<(JsValue, tokio::sync::oneshot::Sender<()>)>>,
    tail_events: Vec<JsValue>,
    prompt_blocked: bool,
    interaction_events: Option<crate::runtime::project::interaction_events::TurnEvents>,
    session_steer: Option<crate::runtime::project::interaction_steer::TurnSteer>,
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
            owner: None,
            session,
            turn,
            now: Arc::new(|| SystemExecutionClock.now_iso()),
            phase: Phase::New,
            messages: vec![],
            failure_status: false,
            callback_error: None,
            prompt_hooks: None,
            prompt_submitted: false,
            tail: None,
            progress: None,
            tail_events: vec![],
            prompt_blocked: false,
            interaction_events: None,
            session_steer: None,
        }
    }
    pub(crate) fn with_interaction_events(
        mut self,
        events: Option<crate::runtime::project::interaction_events::TurnEvents>,
    ) -> Self {
        self.interaction_events = if crate::mutants::on("notices-notify") {
            None
        } else {
            events
        };
        self
    }
    pub(crate) fn with_session_steer(
        mut self,
        steer: Option<crate::runtime::project::interaction_steer::TurnSteer>,
    ) -> Self {
        self.session_steer = steer;
        self
    }
    async fn close_session_steer(&mut self) -> RecorderResult<()> {
        if let Some(steer) = self.session_steer.take() {
            let discarded = steer
                .close_for_session()
                .await
                .map_err(|error| RecorderError::State(error.message))?;
            for message in discarded {
                self.emit_tail(self.event(
                    "steer_unapplied",
                    [
                        ("itemId", JsValue::String(message.item_id)),
                        ("reason", "turn_ended".into()),
                    ],
                ))
                .await;
            }
        }
        Ok(())
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
        let owner = if crate::mutants::on("gates-release-pending-slot") {
            None
        } else {
            self.owner.clone()
        };
        tokio::task::spawn_blocking(move || {
            let result = write(&recorder, &session, &turn);
            drop(owner);
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
        if crate::mutants::on("notices-after-durable")
            && let Some(port) = &self.interaction_events
        {
            let _ = port.host_started();
        }
        if crate::mutants::on("gates-skip-start") {
            let _ = self.io(|r, s, t| r.start_turn(s, t)).await;
        } else {
            self.io(|r, s, t| r.start_turn(s, t)).await?;
        }
        self.phase = Phase::Started;
        let event = self.event(host_lifecycle_events()[0], []);
        let mut events = if crate::mutants::on("notices-start-order") {
            Vec::new()
        } else {
            vec![event]
        };
        if let Some(port) = &self.interaction_events {
            events.extend(
                port.host_started()
                    .map_err(|e| RecorderError::State(e.to_string()))?
                    .into_iter()
                    .map(JsValue::Object),
            );
        }
        Ok((self, events))
    }
    pub async fn accept(mut self, input: TurnInput) -> Transition {
        self.require(Phase::Started)?;
        let accepted = input.accepted_messages();
        let durable: Vec<_> = accepted.iter().cloned().chain(input.synthetic).collect();
        self.messages = input.history;
        let listing_metadata = input.metadata.clone();
        let written = durable.clone();
        if let Err(error) = self
            .io(move |r, s, t| {
                if crate::mutants::on("gates-skip-accept") {
                    let _ = (r, s, t, written, input.metadata);
                    Ok(())
                } else {
                    Ok(r.writer()
                        .record_accepted_input(s, t, written, input.metadata)?)
                }
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
        let history = self.messages.clone();
        self.listing(&history, &accepted, listing_metadata.as_ref())
            .await;
        self.messages.extend(durable);
        self.phase = Phase::Accepted;
        let event = self.event(
            host_lifecycle_events()[1],
            [("messages", JsValue::Array(accepted))],
        );
        Ok((self, vec![event]))
    }
    /// Call for the loop's turn_failed, preserving repeated failures but only
    /// inserting one durable visible failure status before them.
    pub async fn loop_failed(mut self, error: JsValue) -> Transition {
        self.require(Phase::Accepted)?;
        let mut events = vec![];
        if !self.failure_status {
            if matches!(error, JsValue::Null | JsValue::Undefined) {
                let value = if error == JsValue::Null {
                    "null"
                } else {
                    "undefined"
                };
                self.callback_error = Some(format!(
                    "Cannot read properties of {value} (reading 'message')"
                ));
                return Ok((self, events));
            }
            events.push(self.status(&error).await);
            self.failure_status = true;
        }
        events.push(self.event("turn_failed", [("error", error)]));
        Ok((self, events))
    }
    /// Sidecar wire callback (host-buffer statuses deliberately do not call it).
    pub async fn loop_status(mut self, event: JsValue) -> Transition {
        self.require(Phase::Accepted)?;
        let present = |v: Option<&JsValue>| {
            v.filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .cloned()
        };
        let name = event.get("event").cloned().unwrap_or(JsValue::Undefined);
        let detail = event.get("detail");
        let failure = detail.and_then(|d| d.get("severity")) == Some(&"error".into())
            || [
                "max_budget_reached",
                "task_budget_reached",
                "model_request_failed",
                "tool_call_recovery_exhausted",
                "max_turns_reached",
                "max_output_recovery_exhausted",
                "empty_response",
                "content_filter",
                "unknown_finish_reason",
            ]
            .iter()
            .any(|v| name == (*v).into());
        let kind = present(event.get("kind"))
            .unwrap_or_else(|| if failure { "error" } else { "status" }.into());
        // TS sets this before the awaited write, irrespective of detail.visible.
        self.failure_status |= kind == "error".into() && name != "turn_failed".into();
        let text = present(event.get("text")).unwrap_or_else(|| {
            detail
                .and_then(|d| d.get("message"))
                .filter(|v| matches!(v, JsValue::String(_)))
                .cloned()
                .unwrap_or_else(|| name.clone())
        });
        let mut status = object([("event", name), ("kind", kind), ("text", text)]);
        if let (Some(detail), JsValue::Object(status)) = (detail, &mut status) {
            status.insert("detail".into(), detail.clone());
        }
        if let Err(error) = self
            .io(move |r, s, t| Ok(r.writer().record_agent_status_message(s, t, &status)?))
            .await
        {
            self.callback_error = Some(error.to_string());
            return Ok((self, vec![]));
        }
        Ok((self, vec![event]))
    }
    /// Await the sidecar's post-yield durable callback before polling its successor.
    pub async fn durable_message(mut self, message: JsValue) -> Transition {
        self.require(Phase::Accepted)?;
        let kind = match classify_durable_message(&message) {
            Ok(kind) => kind,
            Err(error) => {
                self.callback_error = Some(error);
                return Ok((self, vec![]));
            }
        };
        let written = message.clone();
        if let Err(error) = self
            .io(move |r, s, t| {
                let draft = object([("type", kind.into()), ("message", written)]);
                Ok(r.writer().record_session_event(
                    s,
                    t,
                    draft.as_object().expect("durable draft").clone(),
                )?)
            })
            .await
        {
            self.callback_error = Some(error.to_string());
            return Ok((self, vec![]));
        }
        self.messages.push(message);
        Ok((self, vec![]))
    }
    pub async fn complete(mut self, result: JsValue) -> Transition {
        self.require(Phase::Accepted)?;
        if result.get("sessionId") != Some(&JsValue::String(self.session.clone()))
            || result.get("turnId") != Some(&JsValue::String(self.turn.clone()))
        {
            // Invalid loop output must still finish the owned turn. Numbers and
            // arrays silently dedup in TS; deliberately close them here (see
            // A7.2-TURN-INVALID-RESULT-001).
            let message = match &result {
                JsValue::Null => "Cannot read properties of null (reading 'turnId')".into(),
                JsValue::Undefined => {
                    "Cannot read properties of undefined (reading 'turnId')".into()
                }
                JsValue::Object(fields) => match fields.get("turnId") {
                    Some(turn)
                        if !matches!(turn, JsValue::Undefined)
                            && turn != &JsValue::String(self.turn.clone()) =>
                    {
                        match crate::js_value::to_string_units_checked(turn) {
                            Ok(units) => JsString::from_utf16(
                                "Durable turn "
                                    .encode_utf16()
                                    .chain(units)
                                    .chain(" has not started.".encode_utf16())
                                    .collect(),
                            ),
                            Err(error) => error.message().into(),
                        }
                    }
                    _ => "Turn result identity mismatch.".into(),
                },
                _ => "Turn result identity mismatch.".into(),
            };
            return self
                .fail(object([
                    ("code", "agent_invalid_state".into()),
                    ("message", JsValue::String(message)),
                ]))
                .await;
        }
        self.close_session_steer().await?;
        self.finish_artifacts(&result).await;
        let written = result.clone();
        if let Err(error) = self
            .io(move |r, s, t| {
                if crate::mutants::on("gates-skip-complete") {
                    drop(written);
                    Ok(())
                } else {
                    r.complete_turn(s, t, written)
                }
            })
            .await
        {
            return self
                .fail(object([
                    ("code", "agent_invalid_state".into()),
                    ("message", JsValue::String(error.to_string().into())),
                ]))
                .await;
        }
        self.suggestion(&result).await;
        self.finalize_metadata().await;
        self.phase = Phase::Completed;
        let event = self.event("turn_completed", [("result", result)]);
        let mut events = std::mem::take(&mut self.tail_events);
        events.push(event);
        Ok((self, events))
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
        self.close_session_steer().await?;
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
        if !self.prompt_blocked {
            self.finish_artifacts(&result).await;
        }
        let written = result.clone();
        self.io(move |r, s, t| {
            if crate::mutants::on("gates-skip-failure-complete") {
                drop(written);
                Ok(())
            } else {
                r.complete_turn(s, t, written)
            }
        })
        .await?;
        if let JsValue::Object(fields) = &mut result {
            fields.insert("errors".into(), JsValue::Array(vec![error.clone()]));
        }
        if self.prompt_blocked {
            self.finish_artifacts(&result).await;
        }
        // TS TurnRunner catch writes another status even after a loop failure.
        // failure_status deduplicates only loop failures, never this catch.
        let status = self.status(&error).await;
        if self.phase == Phase::Accepted {
            self.emit_tail(status).await;
            self.finalize_metadata().await;
        } else {
            self.tail_events.push(status);
        }
        self.phase = Phase::Completed;
        let mut events = std::mem::take(&mut self.tail_events);
        events.extend([
            self.event("turn_failed", [("error", error)]),
            self.event("turn_completed", [("result", result)]),
        ]);
        Ok((self, events))
    }
    async fn emit_tail(&mut self, event: JsValue) {
        if let Some(progress) = self.progress.as_ref().filter(|_| self.tail.is_some()) {
            let (ack, resume) = tokio::sync::oneshot::channel();
            if progress.send((event, ack)).is_ok() {
                let _ = resume.await;
            }
        } else {
            self.tail_events.push(event);
        }
    }
    async fn status(&self, error: &JsValue) -> JsValue {
        let message = error.get("message").cloned().unwrap_or(JsValue::Undefined);
        let detail = object([("message", message.clone()), ("code", error.get("code").cloned().unwrap_or(JsValue::Undefined)),
            ("severity", "error".into()), ("visible", JsValue::Bool(true)),
            ("userHint", error.get("userHint").filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined)).cloned().unwrap_or_else(|| "Retry the turn; if it repeats, check the gateway logs or adjust the request.".into())),
            ("scope", "turn".into()), ("source", "agent".into())].into_iter().filter(|(_, value)| !matches!(value, JsValue::Undefined)));
        let fields: Vec<_> = [
            ("event", "turn_failed".into()),
            ("kind", "error".into()),
            ("text", message),
            ("detail", detail),
        ]
        .into_iter()
        .filter(|(_, field)| !matches!(field, JsValue::Undefined))
        .collect();
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

/// Preserve the TS property-read/Array.some classification before any write.
pub(crate) fn classify_durable_message(message: &JsValue) -> Result<&'static str, String> {
    fn readable(value: &JsValue, key: &str) -> Result<(), String> {
        let kind = match value {
            JsValue::Undefined => "undefined",
            JsValue::Null => "null",
            _ => return Ok(()),
        };
        Err(format!(
            "Cannot read properties of {kind} (reading '{key}')"
        ))
    }
    readable(message, "role")?;
    if message.get("role") == Some(&"assistant".into()) {
        return Ok("assistant_message");
    }
    let undefined = JsValue::Undefined;
    let content = message.get("content").unwrap_or(&undefined);
    readable(content, "some")?;
    let JsValue::Array(blocks) = content else {
        return Err("message.content.some is not a function".into());
    };
    for block in blocks {
        readable(block, "type")?;
        if block.get("type") == Some(&"tool_result".into()) {
            return Ok("tool_result_message");
        }
    }
    Ok("durable_message")
}
