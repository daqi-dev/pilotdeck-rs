//! OWNING-path regressions: each test drives `run_admitted_turn` through
//! real ports so the branch is selected from the live InvocationDisposition,
//! not handed to the helper. Each expectation names the genuine Original
//! probe branch it mirrors (original-cancel-branch-probe-v1.json, Original
//! b243fa0: loop_top=1 post_create=1 stream_throw=0 stream_eos=1 quiet=0).
//! Ground truth: AgentLoop.ts:859-885 (iterator threw while aborted -> no
//! status) vs :916-946 (iterator completed, abort observed -> status when
//! shouldSurfaceAbortStatus(reason), :394-397).
use super::*;
use crate::ports::{
    Cancellation, ExecutionHints, RequestProjection, Route, RuntimeContextSurface, StreamChunk,
    StreamEvent,
};
use pilotdeck_protocol::model::PreparedWireRequest;
use std::sync::{Arc, Mutex};

type Log = Arc<Mutex<Vec<String>>>;
fn push(log: &Log, entry: impl Into<String>) {
    log.lock().unwrap().push(entry.into());
}
fn arm(cancellation: &Cancellation, reason: &str) {
    assert!(cancellation.cancel(CancelCause::Requested(reason.into())));
}
fn json(text: &str) -> JsValue {
    pilotdeck_protocol::parse_json(text).unwrap()
}
fn raw_event(text: &str) -> StreamEvent {
    StreamEvent::BoundaryOnly(BoundarySnapshot::capture(&json(text)))
}
fn text_batch() -> Vec<StreamEvent> {
    vec![
        raw_event(r#"{"type":"text_delta","text":"partial "}"#),
        raw_event(r#"{"type":"text_delta","text":"answer"}"#),
    ]
}
/// How the fake model host behaves on the second `stream_next` pull. The
/// first pull always returns a text batch with `done:false`, so both stream
/// branches own the same partial assistant text before the abort fires.
#[derive(Clone, Copy)]
enum StreamPlan {
    /// Arm the abort, then the iterator throws (:859 catch while aborted).
    ThrowWhileAborted,
    /// Arm the abort, then the iterator completes normally (`done:true`),
    /// so the abort is observed only after the stream (:916).
    CompleteThenAbort,
    /// A pre-stream abort must never reach the model host at all.
    Unreached,
    DeadlineUnknown,
    DeadlineUnknownBadAssembly,
}
struct Host {
    log: Log,
    plan: StreamPlan,
    cancellation: Cancellation,
    reason: &'static str,
    pulls: usize,
}
impl HostPort for Host {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        matches!(
            (module, operation),
            ("context", "capture_turn") | ("model", "stream_next")
        )
    }
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
        match call {
            HostCall::ModelPull { .. } => {
                assert!(
                    control.check().is_ok(),
                    "the owning loop never pulls with the abort already set"
                );
                self.pulls += 1;
                push(&self.log, format!("host:stream_next:{}", self.pulls));
                if self.pulls == 1 {
                    return Ok(HostReply::ModelChunk(StreamChunk {
                        events: if matches!(self.plan, StreamPlan::DeadlineUnknownBadAssembly) {
                            vec![raw_event(
                                r#"{"type":"tool_call_end","toolCall":{"id":true,"name":"owned_tool","input":{}}}"#,
                            )]
                        } else {
                            text_batch()
                        },
                        done: false,
                    }));
                }
                match self.plan {
                    StreamPlan::ThrowWhileAborted => {
                        arm(&self.cancellation, self.reason);
                        Err(PortError::HostFailure {
                            code: "model_stream_failed".into(),
                            message: "The operation was aborted".into(),
                            boundary: BoundarySnapshot::capture(&JsValue::Null),
                        })
                    }
                    StreamPlan::CompleteThenAbort => {
                        arm(&self.cancellation, self.reason);
                        Ok(HostReply::ModelChunk(StreamChunk {
                            events: vec![raw_event(r#"{"type":"message_end"}"#)],
                            done: true,
                        }))
                    }
                    StreamPlan::DeadlineUnknown | StreamPlan::DeadlineUnknownBadAssembly => {
                        assert!(self.cancellation.cancel(CancelCause::Deadline));
                        Err(PortError::AcceptedUnknown {
                            identity: Box::new(crate::ports::ExecutionIdentity {
                                session_id: "session".into(),
                                turn_id: "turn".into(),
                                run_id: "run".into(),
                                operation_id: "op".into(),
                                request_id: "accepted-model".into(),
                                idempotency_key: None,
                                module_instance_id: "module".into(),
                                connection_generation: "generation".into(),
                            }),
                            last_applied_sequence: Some(7.0),
                        })
                    }
                    StreamPlan::Unreached => panic!("pre-stream abort reached the model host"),
                }
            }
            HostCall::CaptureTurn(capture) => {
                assert!(control.deadline.is_some(), "capture uses bounded cleanup");
                assert!(!capture.errored);
                push(
                    &self.log,
                    format!("host:capture_turn:{}", capture.messages.len()),
                );
                Ok(HostReply::CapturedTurn)
            }
            other => panic!("unexpected host call {:?}", other.method()),
        }
    }
}
struct Preparation {
    log: Log,
    arm_in_prepare: Option<(Cancellation, &'static str)>,
}
impl TurnPreparationPort for Preparation {
    fn prepare(
        &mut self,
        _: &mut dyn HostPort,
        _: &CallControl,
        _: &TurnRuntimeConfig,
        _: &TurnInput,
        _: &[ProjectedMessage],
    ) -> PortResult<(CapturedRequest, ModelHints)> {
        push(&self.log, "prepare");
        if let Some((cancellation, reason)) = self.arm_in_prepare.take() {
            // Original post_create probe: the abort fires inside
            // prepare_for_model and is observed after createModelRequest.
            arm(&cancellation, reason);
        }
        Ok((
            CapturedRequest {
                route: route(),
                normalized: RequestProjection::BoundaryOnly(Vec::new()),
                wire: PreparedWireRequest::capture(&json(
                    r#"{"provider":"test","model":"model","messages":[]}"#,
                )),
            },
            ModelHints {
                session_id: "session".into(),
                turn_id: "turn".into(),
                execution: execution(),
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
            has_message_end: true,
        })
    }
}
struct Events(Log);
impl Events {
    fn label(event: &BoundarySnapshot) -> String {
        let value = event.value();
        let kind = value
            .get("type")
            .and_then(JsValue::as_string)
            .unwrap()
            .to_utf8()
            .unwrap();
        if kind == "agent_status" {
            let status = value
                .get("event")
                .and_then(JsValue::as_string)
                .unwrap()
                .to_utf8()
                .unwrap();
            format!("agent_status:{status}")
        } else {
            kind
        }
    }
}
impl TurnEventPort for Events {
    fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()> {
        let label = Self::label(&event);
        assert_eq!(
            label, "model_request_started",
            "after the abort every event must use accepted delivery"
        );
        push(&self.0, label);
        Ok(())
    }
    fn emit_accepted(&mut self, event: BoundarySnapshot) -> PortResult<()> {
        let label = Self::label(&event);
        if label == "assistant_message" {
            assert_partial_text(event.value().get("message").expect("assistant message"));
        }
        if label == "turn_completed" {
            let result = event.value().get("result").expect("completion result");
            assert_eq!(result.get("type"), Some(&JsValue::from("aborted")));
            assert_eq!(
                result.get("stopReason"),
                Some(&JsValue::from("aborted_streaming"))
            );
            if self
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|entry| entry == "assistant_message")
            {
                assert_partial_text(
                    result
                        .get("finalMessage")
                        .expect("completed partial message"),
                );
            }
        }
        push(&self.0, label);
        Ok(())
    }
}
struct Clock;
impl TurnClock for Clock {
    fn now_iso(&self) -> JsString {
        "2026-01-01T00:00:00.000Z".into()
    }
    fn uuid(&self) -> JsString {
        "id".into()
    }
}
fn route() -> Route {
    Route {
        provider: "test".into(),
        model: "model".into(),
    }
}
fn execution() -> ExecutionHints {
    ExecutionHints {
        run_id: "run".into(),
        operation_id: "op".into(),
        idempotency_key: None,
        operation_deadline: None,
    }
}
fn message(text: &str) -> ProjectedMessage {
    let mut value = JsObject::new();
    value.insert("role".into(), "user".into());
    value.insert("id".into(), text.into());
    ProjectedMessage {
        boundary: BoundarySnapshot::capture(&JsValue::Object(value)),
        normalized: MessageProjection::BoundaryOnly(vec![crate::ProjectionGap::OtherBoundaryShape]),
    }
}
fn config() -> TurnRuntimeConfig {
    TurnRuntimeConfig {
        route: route(),
        cwd: "/controlled/project".into(),
        system_prompt: None,
        append_system_prompt: None,
        plan_mode_instructions: None,
        runtime_context_surface: RuntimeContextSurface::SystemPrompt,
        max_output_tokens: None,
        max_context_tokens: None,
        thinking: None,
        tool_choice: None,
        max_context_messages: None,
        stop_on_structured_output: None,
        json_self_correct: None,
        run_mode: None,
        is_subagent: None,
        subagent_model: None,
        permission_mode_before_plan: None,
        permission: TurnPermissionContext {
            mode: PermissionMode::Default,
            can_prompt: false,
            bypass_available: false,
            additional_working_directories: vec![],
            rules: None,
        },
        metadata: None,
    }
}
fn input() -> TurnInput {
    TurnInput {
        session_id: "session".into(),
        turn_id: "turn".into(),
        messages: vec![],
        max_turns: None,
        max_budget_usd: None,
        task_budget_usd: None,
        initial_task_budget_spent_usd: None,
        base_permission_mode: None,
        allow_plan_mode_tools: false,
        can_elicit: false,
        model_override: None,
    }
}
fn tool_hints() -> ToolHints {
    ToolHints {
        session_id: "session".into(),
        turn_id: "turn".into(),
        cwd: "/controlled/project".into(),
        permission_mode: PermissionMode::Default,
        run_mode: None,
        can_prompt: false,
        can_elicit: None,
        additional_working_directories: vec![],
        context_extensions: JsObject::new(),
        execution: execution(),
    }
}
/// Which Original probe trigger the fake ports reproduce.
#[derive(Clone, Copy)]
enum Trigger {
    /// `pre_armed`: the abort is set before the loop top check.
    PreArmed,
    /// `in_prepare_for_model`: the abort fires inside request preparation.
    InPrepare,
    /// `stream_throw`: the abort fires and the iterator throws.
    StreamThrow,
    /// `stream_eos`: the abort fires and the iterator completes normally.
    StreamEos,
    DeadlineUnknown,
    DeadlineUnknownBadAssembly,
}
fn run_result(trigger: Trigger, reason: &'static str) -> (PortResult<TurnOutput>, Vec<String>) {
    let log: Log = Arc::default();
    let cancellation = Cancellation::default();
    let plan = match trigger {
        Trigger::PreArmed => {
            arm(&cancellation, reason);
            StreamPlan::Unreached
        }
        Trigger::InPrepare => StreamPlan::Unreached,
        Trigger::StreamThrow => StreamPlan::ThrowWhileAborted,
        Trigger::StreamEos => StreamPlan::CompleteThenAbort,
        Trigger::DeadlineUnknown => StreamPlan::DeadlineUnknown,
        Trigger::DeadlineUnknownBadAssembly => StreamPlan::DeadlineUnknownBadAssembly,
    };
    let mut host = Host {
        log: log.clone(),
        plan,
        cancellation: cancellation.clone(),
        reason,
        pulls: 0,
    };
    let mut preparation = Preparation {
        log: log.clone(),
        arm_in_prepare: matches!(trigger, Trigger::InPrepare)
            .then(|| (cancellation.clone(), reason)),
    };
    let mut events = Events(log.clone());
    let control = CallControl {
        cancellation,
        deadline: None,
    };
    let cleanup = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    };
    let mut config = config();
    let turn_input = input();
    let mut hints = tool_hints();
    let output = run_admitted_turn(
        &mut config,
        &turn_input,
        vec![message("prior"), message("prompt")],
        &mut host,
        &mut preparation,
        &mut events,
        &Clock,
        &control,
        &cleanup,
        &mut hints,
    );
    let observed = log.lock().unwrap().clone();
    (output, observed)
}
fn run(trigger: Trigger, reason: &'static str) -> (TurnOutput, Vec<String>) {
    let (result, log) = run_result(trigger, reason);
    let output = result.expect("owning cancellation completes as a known aborted turn");
    assert_eq!(output.result.kind, TurnResultKind::Aborted);
    assert_eq!(output.result.stop_reason, TurnStopReason::AbortedStreaming);
    (output, log)
}
#[test]
fn deadline_unknown_with_partial_text_completes_turn_without_settling_model_call() {
    // TS publishes an aborted turn before its DEADLINE_EXCEEDED outer
    // result_unknown terminal. Completion must not erase the pending call.
    let (result, log) = run_result(Trigger::DeadlineUnknown, "unused");
    let Err(PortError::AcceptedUnknown {
        identity,
        last_applied_sequence,
    }) = result
    else {
        panic!("aborted completion must retain the accepted model uncertainty");
    };
    assert!(identity.request_id.equals_str("accepted-model"));
    assert_eq!(last_applied_sequence, Some(7.0));
    assert_eq!(
        log,
        vec![
            "prepare",
            "model_request_started",
            "host:stream_next:1",
            "model_event",
            "model_event",
            "host:stream_next:2",
            "project_assembly",
            "assistant_message",
            "host:capture_turn:3",
            "turn_completed",
        ]
    );
}
#[test]
fn deadline_unknown_with_assembly_type_error_preserves_ts_no_completion_boundary() {
    let (result, log) = run_result(Trigger::DeadlineUnknownBadAssembly, "unused");
    let Err(PortError::AcceptedUnknown {
        identity,
        last_applied_sequence,
    }) = result
    else {
        panic!("assembly failure must retain the original accepted model uncertainty");
    };
    assert!(identity.request_id.equals_str("accepted-model"));
    assert_eq!(last_applied_sequence, Some(7.0));
    assert_eq!(
        log,
        vec![
            "prepare",
            "model_request_started",
            "host:stream_next:1",
            "model_event",
            "host:stream_next:2"
        ]
    );
}
fn status_count(log: &[String]) -> usize {
    log.iter()
        .filter(|entry| entry.as_str() == "agent_status:turn_aborted")
        .count()
}
fn assert_partial_assistant_kept(output: &TurnOutput) {
    // Both stream branches keep the safe partial text message (:861-873
    // and :917-929): durableMessageRoles ["assistant"] in the probe.
    assert_eq!(output.messages.len(), 3);
    assert_partial_text(
        output
            .result
            .final_message
            .as_ref()
            .expect("partial assistant message is the final message")
            .boundary
            .value(),
    );
}
fn assert_partial_text(message: &JsValue) {
    let text = message
        .get("content")
        .and_then(|content| match content {
            JsValue::Array(blocks) => blocks.first(),
            _ => None,
        })
        .and_then(|block| block.get("text"))
        .and_then(JsValue::as_string)
        .cloned()
        .expect("text block");
    assert!(text.equals_str("partial answer"));
}

/// Probe branch `during_stream_throw_parity_cancel` (turnAbortedStatusCount
/// 0, eventTypes model_request_started, model_event*, assistant_message,
/// turn_completed). Original :859-885.
#[test]
fn owning_stream_throw_while_aborted_omits_turn_aborted_status() {
    let (output, log) = run(Trigger::StreamThrow, "parity_cancel");
    assert_eq!(status_count(&log), 0, "{log:?}");
    assert_eq!(
        log,
        vec![
            "prepare",
            "model_request_started",
            "host:stream_next:1",
            "model_event",
            "model_event",
            "host:stream_next:2",
            "project_assembly",
            "assistant_message",
            "host:capture_turn:3",
            "turn_completed",
        ]
    );
    assert_partial_assistant_kept(&output);
}
/// Probe branch `during_stream_eos_parity_cancel` (turnAbortedStatusCount 1,
/// eventTypes ..., assistant_message, agent_status:turn_aborted,
/// turn_completed). Original :916-946 with shouldSurfaceAbortStatus true.
/// The Rust owning path has no durable onAgentStatusMessage counterpart;
/// the accepted agent_status event is the only status surface.
#[test]
fn owning_stream_completed_then_abort_emits_exactly_one_turn_aborted_status() {
    let (output, log) = run(Trigger::StreamEos, "parity_cancel");
    assert_eq!(status_count(&log), 1, "{log:?}");
    assert_eq!(
        log,
        vec![
            "prepare",
            "model_request_started",
            "host:stream_next:1",
            "model_event",
            "model_event",
            "host:stream_next:2",
            "model_event",
            "project_assembly",
            "assistant_message",
            "agent_status:turn_aborted",
            "host:capture_turn:3",
            "turn_completed",
        ]
    );
    assert_partial_assistant_kept(&output);
}
/// Probe branch `during_stream_eos_parity_cancel` timing with a reason that
/// shouldSurfaceAbortStatus(:395) rejects: no status even though the
/// iterator completed (:939-942 yields nothing).
#[test]
fn owning_stream_completed_with_non_surfaced_reason_omits_status() {
    for reason in ["compaction_persistence_failed", "user_stop"] {
        let (output, log) = run(Trigger::StreamEos, reason);
        assert_eq!(status_count(&log), 0, "{reason}: {log:?}");
        assert!(log.contains(&"assistant_message".to_string()));
        assert_eq!(log.last().map(String::as_str), Some("turn_completed"));
        assert_partial_assistant_kept(&output);
    }
}
/// Probe branch `loop_top_prearmed_parity_cancel` (turnAbortedStatusCount 1,
/// modelRequests 0, eventTypes agent_status:turn_aborted, turn_completed).
#[test]
fn owning_loop_top_prearmed_cancel_emits_one_status_without_model_request() {
    let (output, log) = run(Trigger::PreArmed, "parity_cancel");
    assert_eq!(
        log,
        vec![
            "agent_status:turn_aborted",
            "host:capture_turn:2",
            "turn_completed"
        ]
    );
    assert_eq!(output.messages.len(), 2);
    assert!(output.result.final_message.is_none());
}
/// Probe branch `loop_top_prearmed_quiet_reason` (turnAbortedStatusCount 0,
/// eventTypes turn_completed only).
#[test]
fn owning_loop_top_prearmed_quiet_reason_omits_status() {
    let (_, log) = run(Trigger::PreArmed, "user_stop");
    assert_eq!(log, vec!["host:capture_turn:2", "turn_completed"]);
}
/// Probe branch `post_create_model_request_parity_cancel`
/// (turnAbortedStatusCount 1, modelRequests 0): the abort fires inside
/// prepare_for_model and is observed before any model request starts.
#[test]
fn owning_abort_during_prepare_emits_one_status_without_model_request() {
    let (output, log) = run(Trigger::InPrepare, "parity_cancel");
    assert_eq!(
        log,
        vec![
            "prepare",
            "agent_status:turn_aborted",
            "host:capture_turn:2",
            "turn_completed"
        ]
    );
    assert_eq!(output.messages.len(), 2);
}
/// Probe branch `during_stream_throw_parity_cancel` timing with a reason
/// that would be surfaced on the other branch (`timeout:run-1`): :859-885
/// never consults createAbortStatus, so still no status.
#[test]
fn owning_stream_throw_with_surfaced_reason_still_omits_status() {
    let (_, log) = run(Trigger::StreamThrow, "timeout:run-1");
    assert_eq!(status_count(&log), 0, "{log:?}");
}
