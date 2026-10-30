//! OWNING-path tests for Original AgentLoop.ts:1382-1441 (ordinary model
//! error with assembled tool calls). Each test drives `run_admitted_turn`
//! through in-memory ports with the SAME ten controlled logical profiles
//! the genuine Original (b243fa0) ran in
//! evidence/CORE01-ordinary-recovery-original-coordinator-a3 (profiles.json,
//! original-full-loop-probe.mjs). Expectations are the decoded Original
//! observations translated to the Rust port protocol:
//! - Original model.stream() invocation  -> one `stream_next` pull returning
//!   the whole controlled batch (`done:true`; a source `error` event closes
//!   the iterator exactly as in the Original).
//! - Original tool port executeAll        -> HostCall::ExecuteBatch (counted;
//!   ordinary profiles fail the batch, host_unknown replies AcceptedUnknown).
//! - Original context.recoverFromModelError give_up -> RecoverModelErrorBoundary
//!   replying `{type:"give_up"}`; captureTurn -> CaptureTurn (counted).
//! - Original `onDurableMessage` is NOT called by the Rust engine. The TS
//!   sidecar client invokes it after the `assistant_message` /
//!   `tool_results_projected` events. The in-memory TurnEventPort therefore
//!   models only the ENGINE translation of a durable failure: the port
//!   returns Err on that emission and the engine must stop (no further pull,
//!   dispatch, capture or event) preserving the error class. This is an
//!   engine-translation claim, not a proof of the production client timing.
use super::*;
use crate::ports::{
    Cancellation, ExecutionHints, ExecutionIdentity, RequestProjection, Route,
    RuntimeContextSurface, StreamChunk, StreamEvent,
};
use crate::{ToolContent, ToolExecutionResult, ToolResultDisposition};
use pilotdeck_protocol::model::{PreparedWireRequest, ProviderEcho, ToolCall};
use std::cell::Cell;
use std::sync::{Arc, Mutex};

type Log = Arc<Mutex<Vec<String>>>;
fn push(log: &Log, entry: impl Into<String>) {
    log.lock().unwrap().push(entry.into());
}
fn json(text: &str) -> JsValue {
    pilotdeck_protocol::parse_json(text).unwrap()
}
fn raw_event(text: &str) -> StreamEvent {
    StreamEvent::BoundaryOnly(BoundarySnapshot::capture(&json(text)))
}
fn str_of(value: &JsValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .map(|s| s.to_utf8().unwrap())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DurableReject {
    None,
    Assistant,
    Pair,
}
/// One entry of the Original run's profiles.json (same ids, call ids,
/// names, inputs, codes and flags).
struct Profile {
    id: &'static str,
    error_attempts: usize,
    calls: usize,
    json_self_correct: bool,
    error_code: &'static str,
    reject_durable: DurableReject,
    callback_unknown: bool,
    host_unknown: bool,
}
const PROFILES: [Profile; 10] = [
    Profile {
        id: "ordinary-original-a1-one-completed-call",
        error_attempts: 1,
        calls: 1,
        json_self_correct: false,
        error_code: "ordinary_fixture_failure",
        reject_durable: DurableReject::None,
        callback_unknown: false,
        host_unknown: false,
    },
    Profile {
        id: "ordinary-original-a1-two-completed-calls",
        error_attempts: 1,
        calls: 2,
        json_self_correct: false,
        error_code: "ordinary_fixture_failure",
        reject_durable: DurableReject::None,
        callback_unknown: false,
        host_unknown: false,
    },
    Profile {
        id: "ordinary-original-a1-json-three-then-success",
        error_attempts: 3,
        calls: 1,
        json_self_correct: true,
        error_code: "invalid_tool_arguments",
        reject_durable: DurableReject::None,
        callback_unknown: false,
        host_unknown: false,
    },
    Profile {
        id: "ordinary-original-a1-json-four-errors",
        error_attempts: 4,
        calls: 1,
        json_self_correct: true,
        error_code: "invalid_tool_arguments",
        reject_durable: DurableReject::None,
        callback_unknown: false,
        host_unknown: false,
    },
    Profile {
        id: "ordinary-original-a1-json-disabled",
        error_attempts: 1,
        calls: 1,
        json_self_correct: false,
        error_code: "invalid_tool_arguments",
        reject_durable: DurableReject::None,
        callback_unknown: false,
        host_unknown: false,
    },
    Profile {
        id: "ordinary-original-a1-json-other-code",
        error_attempts: 1,
        calls: 1,
        json_self_correct: true,
        error_code: "other_fixture_error",
        reject_durable: DurableReject::None,
        callback_unknown: false,
        host_unknown: false,
    },
    Profile {
        id: "ordinary-original-a1-durable-assistant-reject",
        error_attempts: 1,
        calls: 1,
        json_self_correct: true,
        error_code: "invalid_tool_arguments",
        reject_durable: DurableReject::Assistant,
        callback_unknown: false,
        host_unknown: false,
    },
    Profile {
        id: "ordinary-original-a1-durable-pair-reject",
        error_attempts: 1,
        calls: 2,
        json_self_correct: true,
        error_code: "invalid_tool_arguments",
        reject_durable: DurableReject::Pair,
        callback_unknown: false,
        host_unknown: false,
    },
    Profile {
        id: "ordinary-original-a1-durable-pair-unknown",
        error_attempts: 1,
        calls: 2,
        json_self_correct: true,
        error_code: "invalid_tool_arguments",
        reject_durable: DurableReject::Pair,
        callback_unknown: true,
        host_unknown: false,
    },
    Profile {
        id: "ordinary-original-a1-host-tool-result-unknown",
        error_attempts: 0,
        calls: 1,
        json_self_correct: false,
        error_code: "ordinary_fixture_failure",
        reject_durable: DurableReject::None,
        callback_unknown: false,
        host_unknown: true,
    },
];
const SUCCESS_TEXT: &str = "controlled success";
const MISSING_RESULT_TEXT: &str = "TOOL_ERROR[tool_execution_failed][owned_memory_tool][environment_issue]\nSummary: Model error interrupted tool execution.\nEvidence:\n- Model error interrupted tool execution.\nOriginal error:\nModel error interrupted tool execution.\nNext actions:\n1. Inspect the evidence, change the approach, then retry only with corrected inputs. Current permission mode: bypassPermissions.";
const JSON_PROMPT: &str = "Your previous tool call contained invalid JSON in the arguments and could not be parsed. Please retry with valid JSON. Common issues: missing quotes around keys/values, trailing commas, unescaped special characters in strings.";
const NOW: &str = "2026-10-10T00:00:00.000Z";

fn call_id(p: &Profile, ordinal: usize) -> String {
    format!("{}-call-{}", p.id, ordinal + 1)
}
/// The probe's controlled `stream()` for invocation `number` (1-based).
fn controlled_stream(p: &Profile, number: usize) -> Vec<StreamEvent> {
    let mut events = vec![raw_event(r#"{"type":"message_start","role":"assistant"}"#)];
    let failure = number <= p.error_attempts;
    if failure || p.host_unknown {
        for ordinal in 0..p.calls {
            let id = call_id(p, ordinal);
            events.push(raw_event(&format!(
                r#"{{"type":"tool_call_start","id":"{id}","name":"owned_memory_tool"}}"#
            )));
            events.push(raw_event(&format!(
                    r#"{{"type":"tool_call_end","toolCall":{{"id":"{id}","name":"owned_memory_tool","input":{{"ordinal":{ordinal}}},"raw":{{"controlledSource":"complete provider event"}}}}}}"#
                )));
        }
        if failure {
            events.push(raw_event(&format!(
                    r#"{{"type":"error","error":{{"provider":"fixture-provider","protocol":"openai","code":"{}","message":"Controlled ordinary model error","retryable":false}}}}"#,
                    p.error_code
                )));
        } else {
            events.push(raw_event(
                r#"{"type":"message_end","finishReason":"tool_calls"}"#,
            ));
        }
    } else {
        events.push(raw_event(&format!(
            r#"{{"type":"text_delta","text":"{SUCCESS_TEXT}"}}"#
        )));
        events.push(raw_event(r#"{"type":"message_end","finishReason":"stop"}"#));
    }
    events
}
fn identity(p: &Profile, request_id: &str) -> ExecutionIdentity {
    ExecutionIdentity {
        session_id: format!("{}-session", p.id).into(),
        turn_id: format!("{}-turn", p.id).into(),
        run_id: "run".into(),
        operation_id: "op".into(),
        request_id: request_id.into(),
        idempotency_key: None,
        module_instance_id: "controlled-module".into(),
        connection_generation: "generation-1".into(),
    }
}
fn host_unknown_identity(p: &Profile) -> ExecutionIdentity {
    identity(p, &format!("tool-{}-host-2", p.id))
}
fn durable_unknown_identity(p: &Profile) -> ExecutionIdentity {
    identity(p, &format!("{}-durable-2", p.id))
}
struct Host {
    profile: &'static Profile,
    log: Log,
    pulls: usize,
    dispatches: usize,
    declines: usize,
    captures: Vec<(bool, usize)>,
}
impl HostPort for Host {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        matches!(
            (module, operation),
            ("model", "stream_next")
                | ("context", "capture_turn")
                | ("context", "recover_from_model_error")
                | ("capability", "execute_batch")
        )
    }
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
        assert!(control.check().is_ok(), "no host call after cancellation");
        match call {
            HostCall::ModelPull { .. } => {
                self.pulls += 1;
                assert!(self.pulls <= 5, "Model bound5 (Original probe bound)");
                push(&self.log, format!("host:stream_next:{}", self.pulls));
                Ok(HostReply::ModelChunk(StreamChunk {
                    events: controlled_stream(self.profile, self.pulls),
                    done: true,
                }))
            }
            HostCall::ExecuteBatch { calls, .. } => {
                self.dispatches += 1;
                push(&self.log, format!("host:execute_batch:{}", calls.len()));
                if self.profile.host_unknown {
                    assert_eq!(self.dispatches, 1, "No Unknown retry");
                    assert_eq!(calls.len(), 1);
                    assert!(calls[0].tool_call_id.equals_str(&call_id(self.profile, 0)));
                    Err(PortError::AcceptedUnknown {
                        identity: Box::new(host_unknown_identity(self.profile)),
                        last_applied_sequence: Some(1.0),
                    })
                } else {
                    // Original probe: executeAll throws for ordinary profiles.
                    push(&self.log, "UNEXPECTED_TOOL_DISPATCH");
                    Err(PortError::HostFailure {
                        code: "controlled_fixture".into(),
                        message: "Ordinary-error profile cannot execute tools".into(),
                        boundary: BoundarySnapshot::capture(&JsValue::Null),
                    })
                }
            }
            HostCall::RecoverModelErrorBoundary(probe) => {
                self.declines += 1;
                assert!(!probe.has_attempted_compact);
                assert_eq!(
                    str_of(probe.error.value(), "code").as_deref(),
                    Some(self.profile.error_code)
                );
                push(&self.log, "host:recover_from_model_error:give_up");
                Ok(HostReply::RecoveryBoundaryOnly(BoundarySnapshot::capture(
                    &json(r#"{"type":"give_up","reason":"bounded fixture decline"}"#),
                )))
            }
            HostCall::CaptureTurn(capture) => {
                self.captures
                    .push((capture.errored, capture.messages.len()));
                push(
                    &self.log,
                    format!(
                        "host:capture_turn:{}:{}",
                        if capture.errored { "errored" } else { "ok" },
                        capture.messages.len()
                    ),
                );
                Ok(HostReply::CapturedTurn)
            }
            other => panic!("unexpected host call {:?}", other.method()),
        }
    }
}
struct Preparation {
    log: Log,
    prepares: Vec<Vec<BoundarySnapshot>>,
}
impl TurnPreparationPort for Preparation {
    fn prepare(
        &mut self,
        _: &mut dyn HostPort,
        _: &CallControl,
        _: &TurnRuntimeConfig,
        _: &TurnInput,
        messages: &[ProjectedMessage],
    ) -> PortResult<(CapturedRequest, ModelHints)> {
        push(&self.log, format!("prepare:{}", messages.len()));
        self.prepares
            .push(messages.iter().map(|m| m.boundary.clone()).collect());
        Ok((
            CapturedRequest {
                route: route(),
                normalized: RequestProjection::BoundaryOnly(Vec::new()),
                wire: PreparedWireRequest::capture(&json(
                    r#"{"provider":"fixture-provider","model":"fixture-model","messages":[]}"#,
                )),
            },
            ModelHints {
                session_id: "session".into(),
                turn_id: "turn".into(),
                execution: ExecutionHints {
                    run_id: "run".into(),
                    operation_id: "op".into(),
                    idempotency_key: None,
                    operation_deadline: None,
                },
                workspace_id: None,
                storage_config_version: None,
                metadata: None,
                model_override: None,
            },
        ))
    }
    fn project_assembly(&self, boundary: &BoundarySnapshot) -> PortResult<TurnAssembly> {
        push(&self.log, "project_assembly");
        let message = boundary
            .value()
            .get("message")
            .cloned()
            .expect("assembled message");
        Ok(TurnAssembly {
            boundary: boundary.clone(),
            message: ProjectedMessage {
                boundary: BoundarySnapshot::capture(&message),
                normalized: MessageProjection::BoundaryOnly(vec![
                    crate::ProjectionGap::OtherBoundaryShape,
                ]),
            },
            usage: None,
            has_repaired_tool_calls: None,
            has_message_end: boundary.value().get("hasMessageEnd") == Some(&JsValue::Bool(true)),
        })
    }
    fn project_tool_calls(&self, calls: &[BoundarySnapshot]) -> PortResult<Vec<ToolCall>> {
        Ok(calls
            .iter()
            .map(|call| {
                let value = call.value();
                ToolCall {
                    timeline: None,
                    id: value
                        .get("id")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .expect("id"),
                    name: value
                        .get("name")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .expect("name"),
                    input: value.get("input").cloned().unwrap_or(JsValue::Undefined),
                    raw: value
                        .get("raw")
                        .map(|raw| ProviderEcho(Arc::new(raw.clone()))),
                }
            })
            .collect())
    }
    /// Typed admission of the complete createMissingToolResult object (the
    /// production factory uses model_admission::tool_result for the same).
    fn project_tool_result(&self, result: &BoundarySnapshot) -> PortResult<ToolExecutionResult> {
        let value = result.value();
        let text = |key: &str| -> JsString {
            value
                .get(key)
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| panic!("missing-result string field {key}"))
        };
        let error = value.get("error").expect("error object");
        let JsValue::Array(blocks) = value.get("content").expect("content") else {
            panic!("content array");
        };
        Ok(ToolExecutionResult {
            tool_call_id: text("toolCallId"),
            tool_name: text("toolName"),
            disposition: ToolResultDisposition::Error {
                code: error
                    .get("code")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap(),
                message: error
                    .get("message")
                    .and_then(JsValue::as_string)
                    .cloned()
                    .unwrap(),
                fields: JsObject::new(),
            },
            content: blocks
                .iter()
                .map(|block| ToolContent::Text {
                    text: block
                        .get("text")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap(),
                    boundary: BoundarySnapshot::capture(block),
                })
                .collect(),
            supplemental_messages: None,
            metadata: value.get("metadata").and_then(JsValue::as_object).cloned(),
            started_at: text("startedAt"),
            completed_at: text("completedAt"),
            boundary: result.clone(),
        })
    }
}
/// Event sink. `fail_on` models the engine-side translation of a durable
/// failure: the port rejects exactly one emission (by label) with the
/// configured error, as the Original's `onDurableMessage` threw once.
struct Events {
    log: Log,
    events: Vec<BoundarySnapshot>,
    fail_on: Option<(&'static str, PortError)>,
}
impl Events {
    fn label(event: &BoundarySnapshot) -> String {
        let value = event.value();
        let kind = str_of(value, "type").unwrap();
        match kind.as_str() {
            "agent_status" => format!("agent_status:{}", str_of(value, "event").unwrap()),
            "turn_continued" => format!("turn_continued:{}", str_of(value, "reason").unwrap()),
            _ => kind,
        }
    }
}
impl TurnEventPort for Events {
    fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()> {
        let label = Self::label(&event);
        self.events.push(event);
        if self
            .fail_on
            .as_ref()
            .is_some_and(|(target, _)| *target == label)
        {
            let (_, error) = self.fail_on.take().unwrap();
            push(&self.log, format!("{label}:REJECTED"));
            return Err(error);
        }
        push(&self.log, label);
        Ok(())
    }
}
struct Clock {
    id: &'static str,
    uuids: Cell<usize>,
}
impl TurnClock for Clock {
    fn now_iso(&self) -> JsString {
        NOW.into()
    }
    fn uuid(&self) -> JsString {
        self.uuids.set(self.uuids.get() + 1);
        format!("{}-uuid-{}", self.id, self.uuids.get()).into()
    }
}
fn route() -> Route {
    Route {
        provider: "fixture-provider".into(),
        model: "fixture-model".into(),
    }
}
fn config(p: &Profile) -> TurnRuntimeConfig {
    TurnRuntimeConfig {
        route: route(),
        cwd: "/controlled-workspace".into(),
        system_prompt: None,
        append_system_prompt: None,
        plan_mode_instructions: None,
        runtime_context_surface: RuntimeContextSurface::SystemPrompt,
        max_output_tokens: None,
        max_context_tokens: Some(32768.0),
        thinking: None,
        tool_choice: None,
        max_context_messages: None,
        stop_on_structured_output: None,
        json_self_correct: Some(p.json_self_correct),
        run_mode: None,
        is_subagent: None,
        subagent_model: None,
        permission_mode_before_plan: None,
        permission: TurnPermissionContext {
            mode: PermissionMode::BypassPermissions,
            can_prompt: false,
            bypass_available: true,
            additional_working_directories: vec![],
            rules: None,
        },
        metadata: None,
    }
}
fn input(p: &Profile) -> TurnInput {
    TurnInput {
        session_id: format!("{}-session", p.id).into(),
        turn_id: format!("{}-turn", p.id).into(),
        messages: vec![],
        max_turns: Some(8.0),
        max_budget_usd: None,
        task_budget_usd: None,
        initial_task_budget_spent_usd: None,
        base_permission_mode: None,
        allow_plan_mode_tools: false,
        can_elicit: false,
        model_override: None,
    }
}
fn tool_hints(p: &Profile) -> ToolHints {
    ToolHints {
        session_id: format!("{}-session", p.id).into(),
        turn_id: format!("{}-turn", p.id).into(),
        cwd: "/controlled-workspace".into(),
        permission_mode: PermissionMode::BypassPermissions,
        run_mode: None,
        can_prompt: false,
        can_elicit: None,
        additional_working_directories: vec![],
        context_extensions: JsObject::new(),
        execution: ExecutionHints {
            run_id: "run".into(),
            operation_id: "op".into(),
            idempotency_key: None,
            operation_deadline: None,
        },
    }
}
fn initial_message() -> ProjectedMessage {
    ProjectedMessage {
        boundary: BoundarySnapshot::capture(&json(
            r#"{"role":"user","content":[{"type":"text","text":"Controlled ordinary recovery input"}]}"#,
        )),
        normalized: MessageProjection::BoundaryOnly(vec![crate::ProjectionGap::OtherBoundaryShape]),
    }
}
struct Observed {
    result: PortResult<TurnOutput>,
    log: Vec<String>,
    events: Vec<BoundarySnapshot>,
    pulls: usize,
    dispatches: usize,
    declines: usize,
    captures: Vec<(bool, usize)>,
    prepares: Vec<Vec<BoundarySnapshot>>,
}
impl Observed {
    fn count(&self, label: &str) -> usize {
        self.log
            .iter()
            .filter(|entry| entry.as_str() == label)
            .count()
    }
    fn events_of(&self, kind: &str) -> Vec<&JsValue> {
        self.events
            .iter()
            .map(BoundarySnapshot::value)
            .filter(|value| str_of(value, "type").as_deref() == Some(kind))
            .collect()
    }
    /// Rust counterpart of the Original `durables` counter: every attempted
    /// `assistant_message` / `tool_results_projected` emission.
    fn durable_equivalent_emissions(&self) -> usize {
        self.events_of("assistant_message").len() + self.events_of("tool_results_projected").len()
    }
    fn terminal(&self) -> String {
        match &self.result {
            Ok(output) => format!(
                "returned:{:?}/{:?}",
                output.result.kind, output.result.stop_reason
            ),
            Err(PortError::AcceptedUnknown { identity, .. }) => {
                format!(
                    "err:AcceptedUnknown:{}",
                    identity.request_id.to_utf8().unwrap()
                )
            }
            Err(PortError::HostFailure { message, .. }) => {
                format!("err:HostFailure:{}", message.to_utf8().unwrap())
            }
            Err(other) => format!("err:{other:?}"),
        }
    }
}
fn run(p: &'static Profile) -> Observed {
    let log: Log = Arc::default();
    let mut host = Host {
        profile: p,
        log: log.clone(),
        pulls: 0,
        dispatches: 0,
        declines: 0,
        captures: vec![],
    };
    let mut preparation = Preparation {
        log: log.clone(),
        prepares: vec![],
    };
    let rejection = || PortError::HostFailure {
        code: "controlled_durable_rejection".into(),
        message: "Controlled durable callback rejection".into(),
        boundary: BoundarySnapshot::capture(&JsValue::Null),
    };
    let fail_on = match (p.reject_durable, p.callback_unknown) {
        (DurableReject::None, _) => None,
        (DurableReject::Assistant, false) => Some(("assistant_message", rejection())),
        (DurableReject::Pair, false) => Some(("tool_results_projected", rejection())),
        (DurableReject::Assistant, true) => unreachable!("not an Original profile"),
        (DurableReject::Pair, true) => Some((
            "tool_results_projected",
            PortError::AcceptedUnknown {
                identity: Box::new(durable_unknown_identity(p)),
                last_applied_sequence: Some(2.0),
            },
        )),
    };
    let mut events = Events {
        log: log.clone(),
        events: vec![],
        fail_on,
    };
    let clock = Clock {
        id: p.id,
        uuids: Cell::new(0),
    };
    let control = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    };
    let cleanup = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    };
    let mut config = config(p);
    let turn_input = input(p);
    let mut hints = tool_hints(p);
    let result = run_admitted_turn(
        &mut config,
        &turn_input,
        vec![initial_message()],
        &mut host,
        &mut preparation,
        &mut events,
        &clock,
        &control,
        &cleanup,
        &mut hints,
    );
    let observed = Observed {
        result,
        log: log.lock().unwrap().clone(),
        events: events.events,
        pulls: host.pulls,
        dispatches: host.dispatches,
        declines: host.declines,
        captures: host.captures,
        prepares: preparation.prepares,
    };
    println!(
        "RUST_OBSERVED {{\"case_id\":\"{}\",\"models\":{},\"toolDispatches\":{},\"durableEquivalentEmissions\":{},\"captures\":{},\"turn_continued\":{},\"recovery_declines\":{},\"terminal\":\"{}\",\"log\":[{}]}}",
        p.id,
        observed.pulls,
        observed.dispatches,
        observed.durable_equivalent_emissions(),
        observed.captures.len(),
        observed.count("turn_continued:model_error"),
        observed.declines,
        observed.terminal(),
        observed
            .log
            .iter()
            .map(|entry| format!("\"{entry}\""))
            .collect::<Vec<_>>()
            .join(",")
    );
    observed
}
/// Expected Rust log for a profile, derived from the Original public-event
/// order (model_request_started, model_event*, assistant_message,
/// tool_results_projected, turn_continued | terminal) plus the host calls
/// the Original recorded (context_recovery_decline, context_capture).
fn expected_log(p: &Profile) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut messages = 1usize;
    let mut pending_transient = false;
    let model_events_errored = 2 + 2 * p.calls;
    for n in 1..=p.error_attempts {
        out.push(format!(
            "prepare:{}",
            messages + usize::from(pending_transient)
        ));
        out.push("model_request_started".into());
        out.push(format!("host:stream_next:{n}"));
        out.extend(std::iter::repeat_n(
            "model_event".to_string(),
            model_events_errored,
        ));
        out.push("project_assembly".into());
        if p.reject_durable == DurableReject::Assistant {
            out.push("assistant_message:REJECTED".into());
            return out;
        }
        out.push("assistant_message".into());
        if p.reject_durable == DurableReject::Pair {
            out.push("tool_results_projected:REJECTED".into());
            return out;
        }
        out.push("tool_results_projected".into());
        messages += 2;
        if p.json_self_correct && p.error_code == "invalid_tool_arguments" && n <= 3 {
            out.push("turn_continued:model_error".into());
            pending_transient = true;
            continue;
        }
        out.push("host:recover_from_model_error:give_up".into());
        out.push("stop_failure".into());
        out.push("agent_status:model_request_failed".into());
        out.push("turn_failed".into());
        out.push(format!("host:capture_turn:errored:{messages}"));
        out.push("turn_completed".into());
        return out;
    }
    let n = p.error_attempts + 1;
    out.push(format!(
        "prepare:{}",
        messages + usize::from(pending_transient)
    ));
    out.push("model_request_started".into());
    out.push(format!("host:stream_next:{n}"));
    if p.host_unknown {
        out.extend(std::iter::repeat_n(
            "model_event".to_string(),
            2 + 2 * p.calls,
        ));
        out.push("project_assembly".into());
        out.push("assistant_message".into());
        out.push("tool_calls_detected".into());
        out.push("host:execute_batch:1".into());
        return out;
    }
    out.extend(std::iter::repeat_n("model_event".to_string(), 3));
    out.push("project_assembly".into());
    out.push("assistant_message".into());
    out.push("stop_requested".into());
    out.push(format!("host:capture_turn:ok:{}", messages + 1));
    out.push("turn_completed".into());
    out
}
/// (block type, toolCallId, isError) of one wire content block.
type BlockShape = (String, Option<String>, Option<bool>);
/// (role, blocks) of a wire message.
fn shape(message: &JsValue) -> (String, Vec<BlockShape>) {
    let JsValue::Array(blocks) = message.get("content").expect("content") else {
        panic!("content array");
    };
    (
        str_of(message, "role").unwrap(),
        blocks
            .iter()
            .map(|block| {
                (
                    str_of(block, "type").unwrap(),
                    str_of(block, "toolCallId"),
                    block.get("isError").and_then(JsValue::as_bool),
                )
            })
            .collect(),
    )
}
fn assert_projected_pair(p: &Profile, message: &JsValue) {
    let (role, blocks) = shape(message);
    assert_eq!(role, "user");
    assert_eq!(blocks.len(), p.calls, "one tool_result per assembled call");
    for (ordinal, (kind, id, is_error)) in blocks.iter().enumerate() {
        assert_eq!(kind, "tool_result");
        assert_eq!(id.as_deref(), Some(call_id(p, ordinal).as_str()));
        assert_eq!(*is_error, Some(true));
    }
    let JsValue::Array(blocks) = message.get("content").unwrap() else {
        unreachable!()
    };
    for block in blocks {
        let JsValue::Array(inner) = block.get("content").expect("tool_result content") else {
            panic!("inner content array");
        };
        assert_eq!(inner.len(), 1);
        assert_eq!(
            str_of(&inner[0], "text").as_deref(),
            Some(MISSING_RESULT_TEXT)
        );
        let raw = block.get("raw").expect("raw missing result");
        assert_eq!(str_of(raw, "type").as_deref(), Some("error"));
        assert_eq!(
            str_of(raw, "toolName").as_deref(),
            Some("owned_memory_tool")
        );
        assert_eq!(str_of(raw, "startedAt").as_deref(), Some(NOW));
        assert_eq!(str_of(raw, "completedAt").as_deref(), Some(NOW));
        assert_eq!(
            str_of(raw.get("error").unwrap(), "message").as_deref(),
            Some("Model error interrupted tool execution.")
        );
    }
}
fn assert_common(p: &'static Profile, observed: &Observed) {
    assert_eq!(
        observed.log,
        expected_log(p),
        "{}: {:?}",
        p.id,
        observed.log
    );
    // Every assistant message carries the assembled calls (or the success text).
    for message in observed.events_of("assistant_message") {
        let message = message.get("message").unwrap();
        let (role, blocks) = shape(message);
        assert_eq!(role, "assistant");
        if blocks.iter().all(|(kind, _, _)| kind == "text") {
            assert_eq!(
                str_of(&message.get("content").unwrap().as_array_ref()[0], "text").as_deref(),
                Some(SUCCESS_TEXT)
            );
        } else {
            assert_eq!(blocks.len(), p.calls);
            for (ordinal, (kind, _, _)) in blocks.iter().enumerate() {
                assert_eq!(kind, "tool_call");
                let block = &message.get("content").unwrap().as_array_ref()[ordinal];
                assert_eq!(
                    str_of(block, "id").as_deref(),
                    Some(call_id(p, ordinal).as_str())
                );
                assert_eq!(str_of(block, "name").as_deref(), Some("owned_memory_tool"));
            }
        }
    }
    for projected in observed.events_of("tool_results_projected") {
        assert_projected_pair(p, projected.get("message").unwrap());
    }
    // The engine never pulls the model more times than the Original did.
    assert_eq!(observed.pulls, observed.count("model_request_started"));
}
trait ArrayRef {
    fn as_array_ref(&self) -> &[JsValue];
}
impl ArrayRef for JsValue {
    fn as_array_ref(&self) -> &[JsValue] {
        match self {
            JsValue::Array(items) => items,
            _ => panic!("array"),
        }
    }
}
fn assert_terminal_model_error(p: &Profile, observed: &Observed, message_count: usize) {
    let output = observed
        .result
        .as_ref()
        .expect("ordinary model error returns a result");
    assert_eq!(output.result.kind, TurnResultKind::Error);
    assert_eq!(output.result.stop_reason, TurnStopReason::ModelError);
    assert_eq!(output.result.turns, 1.0);
    assert_eq!(output.result.errors.len(), 1);
    let error = output.result.errors[0].value();
    assert_eq!(str_of(error, "code").as_deref(), Some("agent_model_error"));
    assert_eq!(
        str_of(error, "message").as_deref(),
        Some("Controlled ordinary model error")
    );
    assert_eq!(
        str_of(error.get("details").unwrap(), "code").as_deref(),
        Some(p.error_code)
    );
    assert_eq!(output.messages.len(), message_count);
    let roles: Vec<String> = output
        .messages
        .iter()
        .map(|m| str_of(m.boundary.value(), "role").unwrap())
        .collect();
    let mut expected_roles = vec!["user".to_string()];
    for _ in 0..(message_count - 1) / 2 {
        expected_roles.push("assistant".into());
        expected_roles.push("user".into());
    }
    assert_eq!(roles, expected_roles);
    // No transient prompt survives into the returned/captured history.
    assert!(output.messages.iter().all(|m| {
        m.boundary
            .value()
            .get("metadata")
            .and_then(|m| m.get("transient"))
            .is_none()
    }));
    let final_message = output
        .result
        .final_message
        .as_ref()
        .expect("final assistant");
    assert_eq!(shape(final_message.boundary.value()).0, "assistant");
    assert_eq!(
        observed.declines, 1,
        "one reactive recovery probe (give_up)"
    );
    assert_eq!(observed.captures, vec![(true, message_count)]);
}
fn assert_transient_prompt_fed_back(p: &Profile, observed: &Observed) {
    // Original context_prepare 2..4: the last message of every retry
    // request is the transient json_self_correct prompt; it is expired
    // before the next assistant is appended (returned history has none).
    let retries = observed.prepares.len() - 1;
    assert_eq!(retries, 3);
    for (index, messages) in observed.prepares.iter().enumerate().skip(1) {
        let last = messages.last().unwrap().value();
        assert_eq!(str_of(last, "role").as_deref(), Some("user"));
        let metadata = last.get("metadata").expect("transient metadata");
        assert_eq!(metadata.get("synthetic"), Some(&JsValue::Bool(true)));
        assert_eq!(metadata.get("transient"), Some(&JsValue::Bool(true)));
        assert_eq!(
            str_of(metadata, "purpose").as_deref(),
            Some("json_self_correct")
        );
        assert!(str_of(metadata, "transientId").is_some());
        assert_eq!(
            str_of(&last.get("content").unwrap().as_array_ref()[0], "text").as_deref(),
            Some(JSON_PROMPT)
        );
        assert_eq!(
            messages.len(),
            2 * (index + 1),
            "{}: prepare {}",
            p.id,
            index + 1
        );
        let transient_count = messages
            .iter()
            .filter(|m| {
                m.value()
                    .get("metadata")
                    .and_then(|m| m.get("transient"))
                    .is_some()
            })
            .count();
        assert_eq!(
            transient_count, 1,
            "only the newest transient prompt is live"
        );
    }
}

/// Profile 1 (Original: models 1, durables 2, captures 1, decline 1,
/// returned error/model_error, 3 messages).
#[test]
fn profile_1_one_completed_call_projects_missing_result_then_terminal_error() {
    let p = &PROFILES[0];
    let observed = run(p);
    assert_common(p, &observed);
    assert_eq!((observed.pulls, observed.dispatches), (1, 0));
    assert_eq!(observed.durable_equivalent_emissions(), 2);
    assert_eq!(observed.count("turn_continued:model_error"), 0);
    assert_terminal_model_error(p, &observed, 3);
}
/// Profile 2 (Original: one projected user message holding BOTH results,
/// durables 2, 3 messages).
#[test]
fn profile_2_two_completed_calls_project_one_message_with_two_results() {
    let p = &PROFILES[1];
    let observed = run(p);
    assert_common(p, &observed);
    assert_eq!((observed.pulls, observed.dispatches), (1, 0));
    assert_eq!(observed.durable_equivalent_emissions(), 2);
    assert_eq!(observed.events_of("tool_results_projected").len(), 1);
    assert_terminal_model_error(p, &observed, 3);
}
/// Profile 3 (Original: models 4, durables 7, turn_continued x3, captures 1
/// errored=false, returned success/completed, 8 messages).
#[test]
fn profile_3_json_three_errors_then_success_on_fourth_model_call() {
    let p = &PROFILES[2];
    let observed = run(p);
    assert_common(p, &observed);
    assert_eq!((observed.pulls, observed.dispatches), (4, 0));
    assert_eq!(observed.durable_equivalent_emissions(), 7);
    assert_eq!(observed.count("turn_continued:model_error"), 3);
    assert_eq!(observed.declines, 0);
    assert_transient_prompt_fed_back(p, &observed);
    let output = observed.result.as_ref().expect("success");
    assert_eq!(output.result.kind, TurnResultKind::Success);
    assert_eq!(output.result.stop_reason, TurnStopReason::Completed);
    assert_eq!(output.messages.len(), 8);
    assert_eq!(observed.captures, vec![(false, 8)]);
    let final_message = output
        .result
        .final_message
        .as_ref()
        .unwrap()
        .boundary
        .value();
    assert_eq!(
        str_of(
            &final_message.get("content").unwrap().as_array_ref()[0],
            "text"
        )
        .as_deref(),
        Some(SUCCESS_TEXT)
    );
}
/// Profile 4 (Original: models 4 - the 4th error is NOT retried -, durables
/// 8, turn_continued x3, decline 1, returned error/model_error, 9 messages).
#[test]
fn profile_4_json_fourth_error_exhausts_three_corrections_and_fails() {
    let p = &PROFILES[3];
    let observed = run(p);
    assert_common(p, &observed);
    assert_eq!((observed.pulls, observed.dispatches), (4, 0));
    assert_eq!(observed.durable_equivalent_emissions(), 8);
    assert_eq!(observed.count("turn_continued:model_error"), 3);
    assert_transient_prompt_fed_back(p, &observed);
    assert_terminal_model_error(p, &observed, 9);
}
/// Profile 5 (jsonSelfCorrect disabled, code invalid_tool_arguments):
/// Original models 1, no turn_continued.
#[test]
fn profile_5_json_disabled_does_not_retry() {
    let p = &PROFILES[4];
    let observed = run(p);
    assert_common(p, &observed);
    assert_eq!((observed.pulls, observed.dispatches), (1, 0));
    assert_eq!(observed.count("turn_continued:model_error"), 0);
    assert_eq!(observed.durable_equivalent_emissions(), 2);
    assert_terminal_model_error(p, &observed, 3);
}
/// Profile 6 (jsonSelfCorrect enabled, other code): Original models 1.
#[test]
fn profile_6_json_other_code_does_not_retry() {
    let p = &PROFILES[5];
    let observed = run(p);
    assert_common(p, &observed);
    assert_eq!((observed.pulls, observed.dispatches), (1, 0));
    assert_eq!(observed.count("turn_continued:model_error"), 0);
    assert_eq!(observed.durable_equivalent_emissions(), 2);
    assert_terminal_model_error(p, &observed, 3);
}
fn assert_durable_failure_stops_engine(p: &'static Profile, observed: &Observed, emissions: usize) {
    assert_common(p, observed);
    assert_eq!((observed.pulls, observed.dispatches), (1, 0));
    assert_eq!(
        observed.declines, 0,
        "no reactive probe after the durable failure"
    );
    assert!(
        observed.captures.is_empty(),
        "no capture_turn after the durable failure"
    );
    assert_eq!(observed.durable_equivalent_emissions(), emissions);
    assert!(
        observed.log.last().unwrap().ends_with(":REJECTED"),
        "{:?}",
        observed.log
    );
    assert_eq!(observed.count("turn_continued:model_error"), 0);
}
/// Profile 7 (Original: onDurableMessage threw on the assistant; models 1,
/// durables 1, captures 0, run threw the same plain Error).
#[test]
fn profile_7_durable_assistant_rejection_stops_before_projection() {
    let p = &PROFILES[6];
    let observed = run(p);
    assert_durable_failure_stops_engine(p, &observed, 1);
    assert!(observed.events_of("tool_results_projected").is_empty());
    assert!(matches!(
        &observed.result,
        Err(PortError::HostFailure { message, .. })
            if message.equals_str("Controlled durable callback rejection")
    ));
}
/// Profile 8 (Original: onDurableMessage threw on the projected pair;
/// models 1, durables 2, captures 0, run threw the same plain Error).
#[test]
fn profile_8_durable_pair_rejection_stops_before_json_retry() {
    let p = &PROFILES[7];
    let observed = run(p);
    assert_durable_failure_stops_engine(p, &observed, 2);
    assert!(matches!(
        &observed.result,
        Err(PortError::HostFailure { message, .. })
            if message.equals_str("Controlled durable callback rejection")
    ));
}
/// Profile 9 (Original: pair callback threw code RESULT_UNKNOWN; models 1,
/// durables 2, captures 0, run threw with code RESULT_UNKNOWN preserved).
#[test]
fn profile_9_durable_pair_unknown_preserves_accepted_unknown() {
    let p = &PROFILES[8];
    let observed = run(p);
    assert_durable_failure_stops_engine(p, &observed, 2);
    let expected = durable_unknown_identity(p);
    assert!(
        matches!(
            &observed.result,
            Err(PortError::AcceptedUnknown { identity, last_applied_sequence: Some(seq) })
                if **identity == expected && *seq == 2.0
        ),
        "{}",
        observed.terminal()
    );
}
/// Profile 10 (Original: host tool port RESULT_UNKNOWN; models 1,
/// toolDispatches 1, hostModuleCalls 1, durables 1, captures 0, run threw
/// code RESULT_UNKNOWN). Rust: ExecuteBatch -> AcceptedUnknown, fail-closed.
#[test]
fn profile_10_host_tool_result_unknown_fails_closed_after_one_dispatch() {
    let p = &PROFILES[9];
    let observed = run(p);
    assert_common(p, &observed);
    assert_eq!((observed.pulls, observed.dispatches), (1, 1));
    assert_eq!(observed.durable_equivalent_emissions(), 1);
    assert!(observed.captures.is_empty());
    assert_eq!(observed.declines, 0);
    assert!(observed.events_of("tool_results_projected").is_empty());
    assert!(observed.events_of("tool_result").is_empty());
    let expected = host_unknown_identity(p);
    assert!(
        matches!(
            &observed.result,
            Err(PortError::AcceptedUnknown { identity, .. }) if **identity == expected
        ),
        "{}",
        observed.terminal()
    );
}

// ----- Negative controls (each names the mutation it detects) -----

/// Mutation detected: dispatching the assembled calls of an errored
/// response (e.g. dropping the projection branch so the loop falls through
/// to ExecuteBatch). The host counts every ExecuteBatch and would also
/// return a HostFailure the terminal assertions reject.
#[test]
fn control_no_tool_dispatch_after_model_error_for_profiles_1_to_9() {
    for p in &PROFILES[..9] {
        let observed = run(p);
        assert_eq!(observed.dispatches, 0, "{}", p.id);
        assert_eq!(observed.count("UNEXPECTED_TOOL_DISPATCH"), 0, "{}", p.id);
        assert!(
            observed.events_of("tool_calls_detected").is_empty(),
            "{}",
            p.id
        );
    }
}
/// Mutation detected: MAX_JSON_SELF_CORRECT_RETRIES != 3. With 4 the
/// fourth error of profile 4 would be retried (5 pulls, success); with 2
/// profile 3 would terminate in error after 3 pulls.
#[test]
fn control_json_self_correct_limit_is_exactly_three() {
    let four = run(&PROFILES[3]);
    assert_eq!(four.pulls, 4);
    assert_eq!(four.count("turn_continued:model_error"), 3);
    assert_eq!(
        four.result.as_ref().unwrap().result.kind,
        TurnResultKind::Error
    );
    let three = run(&PROFILES[2]);
    assert_eq!(three.pulls, 4);
    assert_eq!(three.count("turn_continued:model_error"), 3);
    assert_eq!(
        three.result.as_ref().unwrap().result.kind,
        TurnResultKind::Success
    );
}
/// Mutation detected: retrying without `jsonSelfCorrect` or on a code other
/// than `invalid_tool_arguments` (loose code matching / ignored config).
#[test]
fn control_no_retry_when_disabled_or_other_code() {
    for p in [&PROFILES[4], &PROFILES[5]] {
        let observed = run(p);
        assert_eq!(observed.pulls, 1, "{}", p.id);
        assert_eq!(observed.count("turn_continued:model_error"), 0, "{}", p.id);
        assert!(observed.prepares.len() == 1, "{}", p.id);
        assert_eq!(
            observed.result.as_ref().unwrap().result.kind,
            TurnResultKind::Error
        );
    }
}
/// Mutation detected: swallowing or retrying a failed assistant/projection
/// emission (e.g. `let _ = emit_event(..)`, or continuing the loop after
/// the failure). Any further pull, dispatch, capture, event or a changed
/// error class fails.
#[test]
fn control_durable_failure_is_not_swallowed_or_retried() {
    for p in &PROFILES[6..9] {
        let observed = run(p);
        assert_eq!(observed.pulls, 1, "{}", p.id);
        assert_eq!(observed.dispatches, 0, "{}", p.id);
        assert!(observed.captures.is_empty(), "{}", p.id);
        assert!(observed.result.is_err(), "{}", p.id);
        let rejected_index = observed
            .log
            .iter()
            .position(|entry| entry.ends_with(":REJECTED"))
            .unwrap_or_else(|| panic!("{}: rejection not observed", p.id));
        assert_eq!(
            rejected_index + 1,
            observed.log.len(),
            "{}: {:?}",
            p.id,
            observed.log
        );
    }
}
/// Mutation detected: downgrading AcceptedUnknown to an ordinary failure
/// (e.g. mapping it to HostFailure/InvalidResponse, or treating the
/// ExecuteBatch failure as a rejected batch with synthetic results) or
/// replaying the accepted effect (a second pull/dispatch).
#[test]
fn control_accepted_unknown_is_not_downgraded_or_replayed() {
    let durable = run(&PROFILES[8]);
    assert!(
        matches!(&durable.result, Err(PortError::AcceptedUnknown { .. })),
        "{}",
        durable.terminal()
    );
    assert_eq!((durable.pulls, durable.dispatches), (1, 0));
    let host = run(&PROFILES[9]);
    assert!(
        matches!(&host.result, Err(PortError::AcceptedUnknown { .. })),
        "{}",
        host.terminal()
    );
    assert_eq!((host.pulls, host.dispatches), (1, 1));
    assert!(host.events_of("tool_results_projected").is_empty());
    assert!(host.events_of("turn_completed").is_empty());
    assert!(host.captures.is_empty());
}
