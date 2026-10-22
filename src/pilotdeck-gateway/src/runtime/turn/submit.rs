//! Transport-neutral submitTurn orchestration. InProcessGateway.ts:764-1846.
//!
//! SessionOwner owns admission/identity/abort; TurnPublisher owns projection,
//! registry appends and terminal draining. The embedding host supplies policy,
//! attachment preparation and SDK services, while the LiveSession adapter owns
//! durable input admission. No WS sequence or host-module sequence is allocated.
use std::{future::Future, pin::Pin, sync::Arc};

use pilotdeck_protocol::{JsString, JsValue};
use tokio::sync::{mpsc, oneshot};

use super::{
    coordinator::TurnEventCoordinator,
    projector::{GatewayAgentEventProjectionInput, GatewayAgentEventProjector},
    publish::{
        TurnAcceptance, TurnPublisher, TurnRunRegistry, accept_turn, persist_preflight_event,
    },
};
use crate::{
    metadata::object,
    runtime::{
        refresh::RefreshRouter,
        session::{SessionContext, SessionOwner, TurnCompletionHandle, TurnReservation},
    },
    storage::{RunAcceptInput, RunRef},
};

pub type SubmitFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitError {
    pub code: String,
    pub message: String,
}
impl SubmitError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for SubmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for SubmitError {}

pub struct PreparedTurn {
    /// Resolver result: { source, selection? }, as in resolveTurnModelSelection.
    pub model_selection: JsValue,
    pub input: JsValue,
    pub options: JsValue,
}

/// Host-dependent services are explicit ports, not another session or writer.
/// normalize applies session-model normalization/interaction reconnect;
/// route_input handles commands and trusted-context authorization after input
/// validation. preflight claims replacement transactions before admission;
/// admitted_preflight checks model policy, task budgets and dialog recovery
/// after SDK configuration, with the completion cancellation signal.
/// They return wire events
/// (including usage/command short-circuits); empty means admission may proceed.
/// The production host/WS adapter is assembled by pr5/pr6.
pub trait SubmitHost: Send + Sync {
    fn normalize<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>>;
    fn route_input<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>>;
    fn admitted_preflight<'a>(
        &'a self,
        input: &'a JsValue,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>>;
    /// Permission transitions, model telemetry and task-budget spend run here
    /// before projection; the port never publishes a second copy of an event.
    fn observe_event<'a>(
        &'a self,
        input: &'a JsValue,
        event: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>>;
    fn preflight<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>>;
    fn has_sdk_config(&self) -> bool;
    fn validate_sdk_config(&self, config: &JsValue) -> Result<(), SubmitError>;
    fn set_sdk_config<'a>(
        &'a self,
        input: &'a JsValue,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>>;
    /// Release prepared upload leases and settle pump-owned hooks before the
    /// deferred terminal is flushed. Like TS lease.release, errors are best effort.
    fn finish_pump<'a>(&'a self, run: &'a RunRef) -> SubmitFuture<'a, Result<(), SubmitError>>;
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>>;
    /// Includes awaited attachments/config/model selection. Errors such as a
    /// changed history epoch are surfaced by the ordinary pump error path.
    fn prepare<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<PreparedTurn, SubmitError>>;
    fn commit_accepted_input<'a>(
        &'a self,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<(), SubmitError>>;
    fn record_failure<'a>(
        &'a self,
        run: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>>;
    fn reject_pending(&self, session: &JsString, reason: &str) -> Result<(), SubmitError>;
    fn release_claim(&self, session: &JsString, run: &JsString);
    fn after_turn(&self, run: &RunRef) -> Result<(), SubmitError>;
}

pub struct TurnSubmitter {
    pub owner: SessionOwner,
    pub coordinator: Arc<TurnEventCoordinator>,
    pub projector: Arc<GatewayAgentEventProjector>,
    pub registry: Option<Arc<dyn TurnRunRegistry>>,
    pub host: Arc<dyn SubmitHost>,
}

enum Message {
    Event(JsValue),
    End,
}
/// One consumer. Dropping it releases the consumer barrier; the owned pump
/// still drains before completion so close/abort cannot strand a writer.
pub struct TurnSubmission {
    receiver: mpsc::UnboundedReceiver<Message>,
    consumed: Option<oneshot::Sender<()>>,
    result: Option<oneshot::Receiver<Result<(), SubmitError>>>,
}
impl TurnSubmission {
    pub async fn next(&mut self) -> Result<Option<JsValue>, SubmitError> {
        match self.receiver.recv().await {
            Some(Message::Event(event)) => Ok(Some(event)),
            Some(Message::End) | None => {
                if let Some(consumed) = self.consumed.take() {
                    let _ = consumed.send(());
                }
                if let Some(result) = self.result.take() {
                    result.await.map_err(|_| {
                        SubmitError::new("gateway_submit_failed", "Submit task stopped.")
                    })??;
                }
                Ok(None)
            }
        }
    }
}
impl TurnSubmitter {
    pub fn submit(self: &Arc<Self>, input: JsValue) -> TurnSubmission {
        let (send, receiver) = mpsc::unbounded_channel();
        let (consumed, barrier) = oneshot::channel();
        let (result, completion) = oneshot::channel();
        let submitter = self.clone();
        tokio::spawn(async move {
            let outcome = submitter.run(input, &send, barrier).await;
            let _ = send.send(Message::End);
            let _ = result.send(outcome);
        });
        TurnSubmission {
            receiver,
            consumed: Some(consumed),
            result: Some(completion),
        }
    }

    async fn run(
        &self,
        mut input: JsValue,
        send: &mpsc::UnboundedSender<Message>,
        barrier: oneshot::Receiver<()>,
    ) -> Result<(), SubmitError> {
        // Gateway normalization precedes validation and durable acceptance.
        let early = self.host.normalize(&mut input).await?;
        if !early.is_empty() {
            emit_all(send, early);
            return Ok(());
        }
        if let Some(event) = validate(&input) {
            emit_all(send, vec![event]);
            return Ok(());
        }
        let routed = self.host.route_input(&mut input).await?;
        if !routed.is_empty() {
            emit_all(send, routed);
            return Ok(());
        }
        if truthy(field(&input, "sdkPermissionMode")) {
            let mut config = field(&input, "sdkSessionConfig").clone();
            if !matches!(config, JsValue::Object(_)) {
                config = object([]);
            }
            set(
                &mut config,
                "permissionMode",
                field(&input, "sdkPermissionMode").clone(),
            );
            set(&mut input, "sdkSessionConfig", config);
        }
        let run = RunRef {
            session_key: string(field(&input, "sessionKey")),
            project_key: field(&input, "projectKey").as_string().cloned(),
            run_id: field(&input, "runId")
                .as_string()
                .cloned()
                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into()),
        };
        let acceptance = RunAcceptInput {
            run: run.clone(),
            request_material: request_material(&input)?.into(),
        };
        match accept_turn(self.registry.as_deref(), &acceptance).await {
            TurnAcceptance::Replay(events) => {
                emit_all(send, events);
                return Ok(());
            }
            TurnAcceptance::Rejected(event) => {
                emit_all(send, vec![event]);
                return Ok(());
            }
            TurnAcceptance::Accepted => {}
        }
        let mut cleanup = Cleanup {
            submitter: self,
            run: &run,
            reservation: None,
            completion: None,
            released: false,
        };
        let preflight = self.host.preflight(&input).await;
        let early = match preflight {
            Ok(events) => events,
            Err(error) => vec![error_event(&run, &error.code, &error.message, true)],
        };
        if !early.is_empty() {
            cleanup.release();
            self.preflight_events(send, &run, early).await;
            return Ok(());
        }
        let sdk = truthy(field(&input, "sdkSessionConfig"));
        if sdk {
            let error = if !self.host.has_sdk_config() {
                Some(SubmitError::new(
                    "CAPABILITY_UNAVAILABLE",
                    "sdk_session_config is unavailable.",
                ))
            } else {
                self.host
                    .validate_sdk_config(field(&input, "sdkSessionConfig"))
                    .err()
            };
            if let Some(error) = error {
                cleanup.release();
                self.preflight_events(
                    send,
                    &run,
                    vec![error_event(&run, &error.code, &error.message, true)],
                )
                .await;
                return Ok(());
            }
        }
        cleanup.reservation = self.owner.begin_turn(&run.session_key, &run.run_id);
        if cleanup.reservation.is_none() {
            cleanup.release();
            // TS busy distinguishes explicit null/false from an absent field,
            // even though only truthy configs enter the update path.
            let sdk_admission = !matches!(field(&input, "sdkSessionConfig"), JsValue::Undefined);
            let message = if sdk_admission {
                "Cannot change SDK session configuration while a turn is active.".into()
            } else {
                format!(
                    "Session {} already has an active turn.",
                    display(&run.session_key)
                )
            };
            let hint =
                "Wait for the current turn to finish or stop it before sending another message.";
            let exact_message = if sdk_admission {
                JsValue::from(message.as_str())
            } else {
                busy_message(&run.session_key)
            };
            if !sdk_admission {
                let mut busy = status("session_busy", &message, hint, "session");
                let mut detail = field(&busy, "detail").clone();
                set(&mut detail, "message", exact_message.clone());
                set(&mut busy, "detail", detail);
                emit_all(send, vec![busy]);
            }
            let mut event = error_event(
                &run,
                if sdk_admission {
                    "SESSION_BUSY"
                } else {
                    "session_busy"
                },
                &message,
                true,
            );
            set(&mut event, "message", exact_message);
            if !sdk_admission {
                set(&mut event, "userHint", hint.into());
            }
            self.preflight_events(send, &run, vec![event]).await;
            return Ok(());
        }
        let completion = self.owner.begin_completion(&run.session_key, &run.run_id);
        cleanup.completion = Some(completion.clone());
        if sdk {
            let update = self.host.set_sdk_config(&input, &completion).await;
            let event = if completion.is_cancelled() {
                Some(aborted(&run))
            } else {
                match update {
                    Ok(changed) => {
                        if changed {
                            self.owner
                                .mark_session_dirty(&run.session_key, "sdk_session_config_changed");
                        }
                        None
                    }
                    Err(error) => Some(error_event(&run, &error.code, &error.message, true)),
                }
            };
            if let Some(event) = event {
                cleanup.release();
                self.preflight_events(send, &run, vec![event]).await;
                return Ok(());
            }
        }
        let admitted = self.host.admitted_preflight(&input, &completion).await;
        let early = match admitted {
            Ok(events) if events.is_empty() && completion.is_cancelled() => vec![aborted(&run)],
            Ok(events) => events,
            Err(error) => vec![error_event(&run, &error.code, &error.message, true)],
        };
        if !early.is_empty() {
            cleanup.release();
            self.preflight_events(send, &run, early).await;
            return Ok(());
        }
        let publisher = Arc::new(TurnPublisher::start(
            run.clone(),
            self.registry.clone(),
            self.coordinator.clone(),
        ));
        let pump = async {
            let outcome = self.pump(&input, &run, &completion, &publisher).await;
            if let Err(error) = outcome
                && self
                    .owner
                    .is_current_completion(&run.session_key, &completion)
            {
                let managed = error.code == "SDK_MANAGED_MODEL_DENIED"
                    || error.code.starts_with("GATEWAY_ORGANIZATION_");
                let code = if managed {
                    &error.code
                } else {
                    "gateway_submit_failed"
                };
                let hint = if code == "SDK_MANAGED_MODEL_DENIED" {
                    "Adjust the SDK session configuration or model selection, then retry."
                } else if managed {
                    "The Gateway host policy denied this configuration. Select an allowed model or contact the Gateway administrator."
                } else {
                    "PilotDeck failed before the agent turn could finish. Retry this message; if it repeats, check the gateway logs."
                };
                let failure = status(code, &error.message, hint, "turn");
                // Recording gateway status is best effort in TS.
                let record = object([
                    ("event", code.into()),
                    ("kind", "error".into()),
                    ("text", JsValue::String(error.message.clone().into())),
                    ("detail", field(&failure, "detail").clone()),
                ]);
                let _ = self.host.record_failure(&run, &record).await;
                publisher.gateway_event(with_run(failure, &run));
                let mut event = error_event(&run, code, &error.message, false);
                set(&mut event, "userHint", hint.into());
                publisher.gateway_event(event);
            }
            let _ = self.host.finish_pump(&run).await;
            publisher.finish().await;
        };
        let consume = async {
            while let Some(event) = publisher.next().await {
                let _ = send.send(Message::Event(event));
            }
            let _ = send.send(Message::End);
            let _ = barrier.await;
        };
        tokio::join!(pump, consume);
        // Complete the fence after admission cleanup, before afterTurnCompleted.
        // Even a finally callback failure cannot strand abort waiters.
        let rejected = self.host.reject_pending(&run.session_key, "turn_ended");
        cleanup.release();
        rejected?;
        self.host.after_turn(&run)
    }

    async fn preflight_events(
        &self,
        send: &mpsc::UnboundedSender<Message>,
        run: &RunRef,
        events: Vec<JsValue>,
    ) {
        for event in events {
            let event = persist_preflight_event(self.registry.as_deref(), run, event).await;
            let _ = send.send(Message::Event(event));
        }
    }

    async fn pump(
        &self,
        input: &JsValue,
        run: &RunRef,
        completion: &TurnCompletionHandle,
        publisher: &TurnPublisher,
    ) -> Result<(), SubmitError> {
        let _ = self.host.refresh().await; // transient config refresh keeps previous snapshot
        if completion.is_cancelled() {
            publisher.gateway_event(aborted(run));
            return Ok(());
        }
        let handle = self
            .owner
            .get_or_create(SessionContext {
                session_key: run.session_key.clone(),
                project_key: run.project_key.clone(),
                channel_key: string(field(input, "channelKey")),
                allowed_tools: strings(field(input, "allowedTools")),
                disallowed_tools: strings(field(input, "disallowedTools")),
            })
            .await
            .map_err(|error| SubmitError::new("gateway_submit_failed", error.message))?;
        if completion.is_cancelled() {
            publisher.gateway_event(aborted(run));
            return Ok(());
        }
        let prepared = self.host.prepare(input, run).await?;
        if completion.is_cancelled() {
            publisher.gateway_event(aborted(run));
            return Ok(());
        }
        let source = field(&prepared.model_selection, "source").clone();
        let selection = field(&prepared.model_selection, "selection");
        let mut last_model = None;
        if truthy(selection) {
            let mut event = model_event(selection, &source, run);
            for key in ["reasoning", "temperature", "speed"] {
                set(&mut event, key, field(selection, key).clone());
            }
            publisher.gateway_event(event);
            last_model = Some((
                field(selection, "provider").clone(),
                field(selection, "model").clone(),
            ));
        }
        let mut actual_model = None;
        let mut stream = handle
            .submit(prepared.input, prepared.options)
            .map_err(|error| SubmitError::new("gateway_submit_failed", error.message))?;
        while let Some(event) = stream
            .next()
            .await
            .map_err(|error| SubmitError::new("gateway_submit_failed", error.message))?
        {
            if !self
                .owner
                .is_current_completion(&run.session_key, completion)
            {
                break;
            }
            self.host.observe_event(input, &event).await?;
            if is(&event, "type", "input_accepted") {
                self.host.commit_accepted_input(run).await?;
            }
            let model = field(&event, "event");
            if is(&event, "type", "model_event") && is(model, "type", "request_started") {
                actual_model = Some(field(model, "model").clone());
                let key = (
                    field(model, "provider").clone(),
                    field(model, "model").clone(),
                );
                if last_model.as_ref() != Some(&key) {
                    publisher.gateway_event(model_event(model, &source, run));
                    last_model = Some(key);
                }
            }
            let events = self
                .projector
                .project(&GatewayAgentEventProjectionInput {
                    event: &event,
                    run_id: &display(&run.run_id),
                    forward_subagent_text: field(
                        field(input, "sdkSessionConfig"),
                        "forwardSubagentText",
                    ) == &JsValue::Bool(true),
                })
                .map_err(|error| SubmitError::new("gateway_submit_failed", error.to_string()))?;
            for mut event in events {
                if is(&event, "type", "assistant_text_delta")
                    && let Some(model) = &actual_model
                    && truthy(model)
                {
                    set(&mut event, "model", model.clone());
                }
                if is(&event, "type", "input_accepted") && truthy(field(input, "modelSelection")) {
                    set(
                        &mut event,
                        "modelSelection",
                        field(input, "modelSelection").clone(),
                    );
                }
                publisher.gateway_event(event);
            }
        }
        Ok(())
    }
}

struct Cleanup<'a> {
    submitter: &'a TurnSubmitter,
    run: &'a RunRef,
    reservation: Option<TurnReservation>,
    completion: Option<TurnCompletionHandle>,
    released: bool,
}
impl Cleanup<'_> {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if let Some(reservation) = &self.reservation {
            self.submitter.owner.end_turn(
                &self.run.session_key,
                Some(&self.run.run_id),
                Some(reservation),
            );
        }
        if let Some(completion) = self.completion.take() {
            self.submitter
                .owner
                .clear_turn_abort_reason(&self.run.session_key, &self.run.run_id);
            self.submitter
                .owner
                .complete_turn(&self.run.session_key, &completion);
        }
        self.submitter
            .host
            .release_claim(&self.run.session_key, &self.run.run_id);
    }
}
impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

fn emit_all(send: &mpsc::UnboundedSender<Message>, events: Vec<JsValue>) {
    for event in events {
        let _ = send.send(Message::Event(event));
    }
}
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or({
        static UNDEFINED: JsValue = JsValue::Undefined;
        &UNDEFINED
    })
}
fn string(value: &JsValue) -> JsString {
    value.as_string().cloned().unwrap_or_default()
}
fn display(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}
fn strings(value: &JsValue) -> Option<Vec<JsString>> {
    if let JsValue::Array(values) = value {
        Some(values.iter().map(string).collect())
    } else {
        None
    }
}
fn is(value: &JsValue, key: &str, text: &str) -> bool {
    field(value, key)
        .as_string()
        .is_some_and(|value| value.equals_str(text))
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn set(value: &mut JsValue, key: &str, item: JsValue) {
    if let JsValue::Object(value) = value {
        value.insert(key.into(), item);
    }
}
fn with_run(mut value: JsValue, run: &RunRef) -> JsValue {
    set(&mut value, "runId", JsValue::String(run.run_id.clone()));
    value
}
fn error_event(run: &RunRef, code: &str, message: &str, recoverable: bool) -> JsValue {
    object([
        ("type", "error".into()),
        ("runId", JsValue::String(run.run_id.clone())),
        ("code", code.into()),
        ("message", message.into()),
        ("recoverable", JsValue::Bool(recoverable)),
    ])
}
fn busy_message(session: &JsString) -> JsValue {
    let mut units: Vec<u16> = "Session ".encode_utf16().collect();
    units.extend_from_slice(session.units());
    units.extend(" already has an active turn.".encode_utf16());
    JsValue::String(JsString::from_utf16(units))
}

fn model_event(selection: &JsValue, source: &JsValue, run: &RunRef) -> JsValue {
    object([
        ("type", "model_selection_changed".into()),
        ("provider", field(selection, "provider").clone()),
        ("model", field(selection, "model").clone()),
        ("source", source.clone()),
        ("runId", JsValue::String(run.run_id.clone())),
    ])
}
fn aborted(run: &RunRef) -> JsValue {
    object([
        ("type", "turn_completed".into()),
        ("runId", JsValue::String(run.run_id.clone())),
        ("usage", object([])),
        ("finishReason", "aborted_streaming".into()),
    ])
}
fn status(code: &str, message: &str, hint: &str, scope: &str) -> JsValue {
    object([
        ("type", "agent_status".into()),
        ("event", code.into()),
        (
            "detail",
            object([
                ("message", message.into()),
                ("code", code.into()),
                ("severity", "error".into()),
                ("visible", JsValue::Bool(true)),
                ("userHint", hint.into()),
                ("scope", scope.into()),
                ("source", "gateway".into()),
            ]),
        ),
    ])
}

/// Validation before runRegistry.accept; undefined runId is generated only after
/// this point. No filesystem or host calls are made by these checks.
fn ascii_alphanumeric(v: u16) -> bool {
    v <= 127 && (v as u8).is_ascii_alphanumeric()
}
fn validate(input: &JsValue) -> Option<JsValue> {
    let check = |code: &str, message: JsString, include_run: bool| {
        let mut event = object([
            ("type", "error".into()),
            ("code", code.into()),
            ("message", JsValue::String(message)),
            ("recoverable", JsValue::Bool(true)),
        ]);
        if include_run && let JsValue::String(run) = field(input, "runId") {
            set(&mut event, "runId", JsValue::String(run.clone()));
        }
        event
    };
    for (key, allowed) in [
        ("mode", &["default", "plan", "bypassPermissions"][..]),
        ("basePermissionMode", &["default", "bypassPermissions"][..]),
    ] {
        let value = field(input, key);
        if !matches!(value, JsValue::Undefined | JsValue::Null)
            && value != &JsValue::from("")
            && !allowed
                .iter()
                .any(|s| value.as_string().is_some_and(|v| v.equals_str(s)))
        {
            let mut message: Vec<u16> = format!("Invalid {key}: ").encode_utf16().collect();
            message.extend(crate::js_value::to_string_units(value).unwrap_or_default());
            message.push(u16::from(b'.'));
            return Some(check(
                "INVALID_PERMISSION_MODE",
                JsString::from_utf16(message),
                false,
            ));
        }
    }
    let id = field(input, "runId");
    if !matches!(id, JsValue::Undefined)
        && !id.as_string().is_some_and(|id| {
            let units = id.units();
            (1..=128).contains(&units.len())
                && ascii_alphanumeric(units[0])
                && units
                    .iter()
                    .all(|v| ascii_alphanumeric(*v) || b"._:-".iter().any(|c| u16::from(*c) == *v))
        })
    {
        return Some(check("INVALID_RUN_ID", "runId must be 1-128 characters and contain only letters, numbers, '.', '_', ':', or '-'.".into(), true));
    }
    if let value = field(input, "trustedContext")
        && !matches!(value, JsValue::Undefined)
    {
        let JsValue::Array(values) = value else {
            return Some(check(
                "INVALID_TRUSTED_CONTEXT",
                "trustedContext must be an array.".into(),
                true,
            ));
        };
        for (index, item) in values.iter().enumerate() {
            let nonempty = |key| {
                field(item, key)
                    .as_string()
                    .is_some_and(|s| !crate::metadata::js_trim(&display(s)).is_empty())
            };
            if !nonempty("text")
                || !nonempty("source")
                || !["material_context", "skill_context", "application_context"]
                    .iter()
                    .any(|purpose| is(item, "purpose", purpose))
                || !is(item, "scope", "turn")
            {
                return Some(check(
                    "INVALID_TRUSTED_CONTEXT",
                    format!("trustedContext[{index}] is invalid.").into(),
                    true,
                ));
            }
        }
    }
    let budget = field(input, "maxBudgetUsd");
    if !matches!(budget, JsValue::Undefined)
        && !matches!(budget, JsValue::Number(v) if v.is_finite() && *v > 0.0)
    {
        return Some(check(
            "INVALID_MAX_BUDGET_USD",
            "maxBudgetUsd must be a positive finite number.".into(),
            false,
        ));
    }
    None
}

/// Stable execution fingerprint excludes only reconnect/telemetry metadata.
fn request_material(input: &JsValue) -> Result<String, SubmitError> {
    enum Part<'a> {
        Value(&'a JsValue, bool),
        Key(&'a JsString),
        Raw(&'static str),
    }
    let stringify = |value: &JsValue| {
        pilotdeck_protocol::stringify_json(value)
            .map(|value| value.unwrap_or_else(|| "null".into()))
            .map_err(|error| SubmitError::new("INVALID_INPUT", error.to_string()))
    };
    let mut output = String::new();
    let mut stack = vec![Part::Value(input, true)];
    while let Some(part) = stack.pop() {
        match part {
            Part::Raw(text) => output.push_str(text),
            Part::Key(key) => output.push_str(&stringify(&JsValue::String(key.clone()))?),
            Part::Value(JsValue::Object(value), top) => {
                let mut entries: Vec<_> = value
                    .entries()
                    .iter()
                    .filter(|(key, value)| {
                        !matches!(value, JsValue::Undefined)
                            && !(top
                                && ["runId", "interactionBinding", "telemetry"]
                                    .iter()
                                    .any(|name| key.equals_str(name)))
                    })
                    .collect();
                entries.sort_by(|a, b| a.0.units().cmp(b.0.units()));
                output.push('{');
                stack.push(Part::Raw("}"));
                for (index, (key, value)) in entries.into_iter().enumerate().rev() {
                    stack.push(Part::Value(value, false));
                    stack.push(Part::Raw(":"));
                    stack.push(Part::Key(key));
                    if index != 0 {
                        stack.push(Part::Raw(","));
                    }
                }
            }
            Part::Value(JsValue::Array(values), _) => {
                output.push('[');
                stack.push(Part::Raw("]"));
                for (index, value) in values.iter().enumerate().rev() {
                    stack.push(Part::Value(value, false));
                    if index != 0 {
                        stack.push(Part::Raw(","));
                    }
                }
            }
            Part::Value(value, _) => output.push_str(&stringify(value)?),
        }
    }
    Ok(output)
}

#[cfg(test)]
#[path = "submit_tests.rs"]
mod tests;
