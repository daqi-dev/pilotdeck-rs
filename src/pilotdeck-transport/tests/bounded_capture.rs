//! Real owning turn + RemoteHost + concurrent ConnectionOwner pending/receipt tests.
use pilotdeck_agent::ports::*;
use pilotdeck_agent::turn::*;
use pilotdeck_agent::{MessageProjection, ProjectedMessage};
use pilotdeck_protocol::admission::{CallEffect, SidecarBinding};
use pilotdeck_protocol::model::{BoundarySnapshot, PreparedWireRequest};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::stdio::{ConnectionOwner, DispatchPhase};
use pilotdeck_transport::{AdvertisedMethods, HostExchange, RemoteHost};
use std::io::{self, Write};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
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
struct Preparation {
    log: Log,
    arm_in_prepare: Option<(Cancellation, &'static str)>,
}
impl TurnPreparationPort for Preparation {
    fn project_tool_calls(
        &self,
        calls: &[BoundarySnapshot],
    ) -> PortResult<Vec<pilotdeck_protocol::model::ToolCall>> {
        calls
            .iter()
            .map(|call| {
                Ok(pilotdeck_protocol::model::ToolCall {
                    timeline: None,
                    id: call.value().get("id").unwrap().as_string().unwrap().clone(),
                    name: call
                        .value()
                        .get("name")
                        .unwrap()
                        .as_string()
                        .unwrap()
                        .clone(),
                    input: call.value().get("input").unwrap().clone(),
                    raw: None,
                })
            })
            .collect()
    }
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
                    pilotdeck_agent::ProjectionGap::OtherBoundaryShape,
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
        push(&self.0, label);
        Ok(())
    }
    fn emit_accepted(&mut self, event: BoundarySnapshot) -> PortResult<()> {
        if Self::label(&event) == "turn_completed" {
            let result = event.value().get("result").unwrap();
            assert_eq!(result.get("type"), Some(&JsValue::from("aborted")));
            assert_eq!(
                result.get("stopReason"),
                Some(&JsValue::from("aborted_streaming"))
            );
        }
        push(&self.0, Self::label(&event));
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
        normalized: MessageProjection::BoundaryOnly(vec![
            pilotdeck_agent::ProjectionGap::OtherBoundaryShape,
        ]),
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

struct Wire(mpsc::Sender<Vec<u8>>, Vec<u8>);
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.1.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.send(std::mem::take(&mut self.1)).unwrap();
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum Capture {
    Ack,
    KnownError,
    Pending,
    Absent,
    Unknown,
    Closed,
    RetiredBefore,
    RetiredDuring,
    UncancelledUnknown,
}
fn run_pending(capture: Capture, requested: bool, tool: bool) {
    let owner = ConnectionOwner::new(
        SidecarBinding {
            module_id: "pilotdeck-agent-loop".into(),
            module_instance_id: "instance".into(),
            connection_generation: "generation".into(),
            capabilities_version: "1".into(),
        },
        8,
    );
    let identity = ExecutionIdentity {
        session_id: "session".into(),
        turn_id: "turn".into(),
        run_id: "run".into(),
        operation_id: "op".into(),
        request_id: "initial".into(),
        idempotency_key: None,
        module_instance_id: "instance".into(),
        connection_generation: "generation".into(),
    };
    let cancellation = Cancellation::default();
    let control = CallControl {
        cancellation: cancellation.clone(),
        deadline: Some(Instant::now() + Duration::from_millis(120)),
    };
    let cleanup = CallControl {
        cancellation: cancellation.clone(),
        deadline: Some(Instant::now() - Duration::from_secs(1)),
    };
    let (sent, received) = mpsc::channel();
    let (capture_seen, capture_received) = mpsc::channel();
    let writer_owner = owner.clone();
    let writer = std::thread::spawn(move || writer_owner.drive_writer(Wire(sent, vec![])));
    let peer_owner = owner.clone();
    let receipts = Arc::new(Mutex::new(Vec::new()));
    let observed = receipts.clone();
    let peer = std::thread::spawn(move || {
        let mut pulls = 0;
        while let Ok(bytes) = received.recv_timeout(Duration::from_secs(7)) {
            let request = json(std::str::from_utf8(&bytes).unwrap());
            let id = request
                .get("messageId")
                .unwrap()
                .as_string()
                .unwrap()
                .clone();
            let operation = request
                .get("operation")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            observed
                .lock()
                .unwrap()
                .push((id.clone(), operation.clone()));
            if operation == "stream_next" {
                pulls += 1;
                if pulls == 1 {
                    let mut response = json(
                        r#"{"kind":"response","messageId":"partial","ok":true,"payload":{"events":[{"type":"text_delta","text":"safe partial text"}],"done":false}}"#,
                    );
                    if tool {
                        response = json(
                            r#"{"kind":"response","messageId":"tool","ok":true,"payload":{"events":[{"type":"message_start","role":"assistant"},{"type":"tool_call_start","id":"call","name":"owned_tool"},{"type":"tool_call_end","toolCall":{"id":"call","name":"owned_tool","input":{},"raw":{}}},{"type":"message_end","finishReason":"tool_calls"}],"done":true}}"#,
                        );
                    }
                    if let JsValue::Object(ref mut o) = response {
                        o.insert("inReplyTo".into(), JsValue::String(id));
                    }
                    peer_owner.receive_response(&response);
                    continue;
                }
                if matches!(capture, Capture::UncancelledUnknown) {
                    let mut response = json(
                        r#"{"kind":"response","messageId":"unknown","ok":false,"outcome":"result_unknown","error":{"code":"RESULT_UNKNOWN","message":"original unresolved"}}"#,
                    );
                    if let JsValue::Object(ref mut o) = response {
                        o.insert("inReplyTo".into(), JsValue::String(id));
                    }
                    peer_owner.receive_response(&response);
                }
                if requested {
                    cancellation.cancel_with_reconciliation_deadline(
                        CancelCause::Requested("user cancel".into()),
                        Instant::now(),
                    );
                } else if !matches!(capture, Capture::UncancelledUnknown) {
                    // The execution deadline listener sets the owning cause;
                    // a local CallControl bound alone must not freeze late ACKs.
                    cancellation.cancel(CancelCause::Deadline);
                }
                if matches!(capture, Capture::RetiredBefore) {
                    assert!(!cancellation.cancel(CancelCause::OwnerRetired));
                }
                continue; // actual accepted stream with no response
            }
            if operation == "execute_batch" {
                if requested {
                    cancellation.cancel_with_reconciliation_deadline(
                        CancelCause::Requested("user cancel".into()),
                        Instant::now(),
                    );
                } else {
                    cancellation.cancel(CancelCause::Deadline);
                }
                continue;
            }
            assert_eq!(operation, "capture_turn", "no model/tool replay");
            capture_seen.send(()).unwrap();
            assert_eq!(
                request
                    .get("payload")
                    .unwrap()
                    .get("input")
                    .unwrap()
                    .get("errored"),
                Some(&JsValue::Bool(false))
            );
            let input = request.get("payload").unwrap().get("input").unwrap();
            let Some(JsValue::Array(messages)) = input.get("messages") else {
                panic!("capture messages");
            };
            assert_eq!(messages.len(), 2, "safe partial assistant is retained");
            if !tool {
                let Some(JsValue::Array(content)) = messages[1].get("content") else {
                    panic!("safe partial content");
                };
                assert_eq!(content.len(), 1);
                assert_eq!(content[0].get("type"), Some(&JsValue::from("text")));
                assert_eq!(
                    content[0].get("text"),
                    Some(&JsValue::from("safe partial text"))
                );
                assert_eq!(content[0].get("blockId"), Some(&JsValue::from("id:text:0")));
                assert_eq!(content[0].get("timeline").is_some(), !requested);
            }
            if matches!(capture, Capture::RetiredDuring) {
                assert!(!cancellation.cancel(CancelCause::OwnerRetired));
                continue;
            }
            if matches!(capture, Capture::Closed) {
                peer_owner.close();
                break;
            }
            if matches!(capture, Capture::Pending) {
                continue;
            }
            let mut response = match capture {
                Capture::KnownError => json(
                    r#"{"kind":"response","messageId":"ack","ok":false,"error":{"code":"capture_failed","message":"known failure"}}"#,
                ),
                Capture::Unknown => json(
                    r#"{"kind":"response","messageId":"ack","ok":false,"outcome":"result_unknown","lastAppliedSequence":19,"error":{"code":"RESULT_UNKNOWN","message":"unresolved capture"}}"#,
                ),
                _ => json(
                    r#"{"kind":"response","messageId":"ack","ok":true,"payload":{"result":null}}"#,
                ),
            };
            if let JsValue::Object(ref mut o) = response {
                o.insert("inReplyTo".into(), JsValue::String(id));
            }
            peer_owner.receive_response(&response);
        }
    });
    let mut modules = vec![("model".into(), vec!["stream_next".into()])];
    if !matches!(capture, Capture::Absent) {
        modules.push(("context".into(), vec!["capture_turn".into()]));
    }
    if tool {
        modules.push(("capability".into(), vec!["execute_batch".into()]));
    }
    let mut host = RemoteHost::new(
        owner.bound_exchange(identity.clone()).unwrap(),
        AdvertisedMethods { modules },
    );
    let log: Log = Arc::default();
    let mut preparation = Preparation {
        log: log.clone(),
        arm_in_prepare: None,
    };
    let start = Instant::now();
    let result = run_admitted_turn(
        &mut config(),
        &input(),
        vec![message("prompt")],
        &mut host,
        &mut preparation,
        &mut Events(log.clone()),
        &Clock,
        &control,
        &cleanup,
        &mut tool_hints(),
    );
    let elapsed = start.elapsed();
    if !matches!(
        capture,
        Capture::Absent | Capture::RetiredBefore | Capture::UncancelledUnknown
    ) {
        capture_received
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
    }
    let records = receipts.lock().unwrap().clone();
    assert_eq!(
        records.iter().filter(|(_, op)| op == "stream_next").count(),
        if tool { 1 } else { 2 }
    );
    assert_eq!(
        records
            .iter()
            .filter(|(_, op)| op == "capture_turn")
            .count(),
        usize::from(!matches!(
            capture,
            Capture::Absent | Capture::RetiredBefore | Capture::UncancelledUnknown
        )),
        "missing or duplicate capture"
    );
    let original = owner.call_record(&records[1].0).unwrap();
    assert!(
        matches!(result, Err(PortError::AcceptedUnknown { identity: actual, last_applied_sequence: None }) if *actual == original.identity)
    );
    let completed = !requested && !matches!(capture, Capture::UncancelledUnknown);
    let events: Vec<_> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|e| !matches!(e.as_str(), "prepare" | "project_assembly"))
        .cloned()
        .collect();
    if completed {
        assert_eq!(
            events,
            if tool {
                vec![
                    "model_request_started",
                    "model_event",
                    "model_event",
                    "model_event",
                    "model_event",
                    "assistant_message",
                    "tool_calls_detected",
                    "turn_completed",
                ]
            } else {
                vec![
                    "model_request_started",
                    "model_event",
                    "assistant_message",
                    "turn_completed",
                ]
            },
            "deadline publishes source aborted events without tool results/status"
        );
    } else {
        assert!(!events.iter().any(|e| e == "turn_completed"));
        if requested {
            assert_eq!(
                events,
                if tool {
                    vec![
                        "model_request_started",
                        "model_event",
                        "model_event",
                        "model_event",
                        "model_event",
                        "assistant_message",
                        "tool_calls_detected",
                    ]
                } else {
                    vec!["model_request_started", "model_event"]
                },
                "unresolved user cancellation keeps its original event sequence"
            );
        }
    }
    assert!(
        elapsed < Duration::from_secs(6),
        "capture did not respect its bound: {elapsed:?}"
    );
    if matches!(capture, Capture::Pending | Capture::Unknown) {
        let retained = owner.call_record(&records[2].0).unwrap();
        assert_ne!(retained.identity, original.identity);
        assert!(retained.phase == DispatchPhase::Unresolved || retained.response.is_some());
    }
    if !requested && !matches!(capture, Capture::UncancelledUnknown) {
        let mut late_ack = JsObject::new();
        late_ack.insert("kind".into(), "response".into());
        late_ack.insert("messageId".into(), "late-ack".into());
        late_ack.insert("inReplyTo".into(), JsValue::String(records[1].0.clone()));
        late_ack.insert("ok".into(), JsValue::Bool(true));
        let late_ack = JsValue::Object(late_ack);
        owner.receive_response(&late_ack);
        assert_eq!(
            owner.call_record(&records[1].0).unwrap().phase,
            DispatchPhase::Unresolved
        );
        assert_eq!(
            owner.receive_response(&late_ack),
            pilotdeck_transport::stdio::ResponseDisposition::Duplicate
        );
    }
    owner.release_known_calls(&identity);
    // Replay and unrelated advancement remain blocked by the ORIGINAL identity.
    let mut exchange = host.into_exchange();
    let fresh = CallControl {
        cancellation: Cancellation::default(),
        deadline: Some(Instant::now() + Duration::from_secs(1)),
    };
    let mut payload = JsObject::new();
    payload.insert(
        "input".into(),
        json(r#"{"sessionId":"session","turnId":"turn","errored":false,"messages":[{},{"content":[{"type":"text","text":"safe partial text","blockId":"id:text:0"}]}]}"#),
    );
    if !matches!(capture, Capture::Absent) {
        assert!(
            matches!(exchange.exchange(&fresh, "context", "capture_turn", CallEffect::SideEffect, payload), Err(PortError::AcceptedUnknown { identity, .. }) if *identity == original.identity)
        );
    }
    assert!(
        matches!(exchange.exchange(&fresh, "model", "stream_next", CallEffect::Advance, JsObject::new()), Err(PortError::AcceptedUnknown { identity, .. }) if *identity == original.identity)
    );
    owner.close();
    writer.join().unwrap().unwrap();
    peer.join().unwrap();
}
#[test]
fn bounded_capture_owning_pending_model_deadline_ack() {
    run_pending(Capture::Ack, false, false);
}
#[test]
fn bounded_capture_owning_pending_model_requested_ack() {
    run_pending(Capture::Ack, true, false);
}
#[test]
fn bounded_capture_known_error_preserves_original() {
    run_pending(Capture::KnownError, false, false);
}
#[test]
fn bounded_capture_unknown_preserves_both_receipts() {
    run_pending(Capture::Unknown, false, false);
}
#[test]
fn bounded_capture_pending_expires_without_replay() {
    run_pending(Capture::Pending, false, false);
}
#[test]
fn bounded_capture_absent_never_dispatches() {
    run_pending(Capture::Absent, false, false);
}

#[test]
fn bounded_capture_connection_close_preserves_original() {
    run_pending(Capture::Closed, false, false);
}

#[test]
fn bounded_capture_requested_then_connection_close() {
    run_pending(Capture::Closed, true, false);
}
#[test]
fn bounded_capture_during_tool_deadline() {
    run_pending(Capture::Ack, false, true);
}
#[test]
fn bounded_capture_during_tool_requested() {
    run_pending(Capture::Ack, true, true);
}

#[test]
fn bounded_capture_requested_then_retired_before_capture() {
    run_pending(Capture::RetiredBefore, true, false);
}
#[test]
fn bounded_capture_requested_then_retired_during_capture() {
    run_pending(Capture::RetiredDuring, true, false);
}

#[test]
fn bounded_capture_uncancelled_unknown_stays_closed() {
    run_pending(Capture::UncancelledUnknown, false, false);
}

fn run_loop_top_without_capture_ack(deadline: bool) {
    let owner = ConnectionOwner::new(
        SidecarBinding {
            module_id: "pilotdeck-agent-loop".into(),
            module_instance_id: "instance".into(),
            connection_generation: "generation".into(),
            capabilities_version: "1".into(),
        },
        8,
    );
    let identity = ExecutionIdentity {
        session_id: "session".into(),
        turn_id: "turn".into(),
        run_id: "run".into(),
        operation_id: "op".into(),
        request_id: "initial".into(),
        idempotency_key: None,
        module_instance_id: "instance".into(),
        connection_generation: "generation".into(),
    };
    let cancellation = Cancellation::default();
    cancellation.cancel(if deadline {
        CancelCause::Deadline
    } else {
        CancelCause::Requested("user cancel".into())
    });
    let control = CallControl {
        cancellation: cancellation.clone(),
        deadline: None,
    };
    let cleanup = CallControl {
        cancellation: cancellation.cleanup_scope(),
        deadline: None,
    };
    let (sent, received) = mpsc::channel();
    let writer_owner = owner.clone();
    let writer = std::thread::spawn(move || writer_owner.drive_writer(Wire(sent, vec![])));
    let mut host = RemoteHost::new(
        owner.bound_exchange(identity.clone()).unwrap(),
        AdvertisedMethods {
            modules: vec![("context".into(), vec!["capture_turn".into()])],
        },
    );
    let log: Log = Arc::default();
    let start = Instant::now();
    let result = run_admitted_turn(
        &mut config(),
        &input(),
        vec![message("prompt")],
        &mut host,
        &mut Preparation {
            log: log.clone(),
            arm_in_prepare: None,
        },
        &mut Events(log.clone()),
        &Clock,
        &control,
        &cleanup,
        &mut tool_hints(),
    );
    assert!(
        start.elapsed() < Duration::from_secs(6),
        "loop-top capture exceeded its local bound"
    );
    let bytes = received.recv_timeout(Duration::from_secs(1)).unwrap();
    let request = json(std::str::from_utf8(&bytes).unwrap());
    assert_eq!(
        request.get("operation"),
        Some(&JsValue::from("capture_turn"))
    );
    let id = request.get("messageId").unwrap().as_string().unwrap();
    let record = owner.call_record(id).unwrap();
    assert_eq!(record.phase, DispatchPhase::Unresolved);
    assert_eq!(
        request
            .get("payload")
            .unwrap()
            .get("input")
            .unwrap()
            .get("errored"),
        Some(&JsValue::Bool(false))
    );
    if deadline {
        let output = result.unwrap();
        assert_eq!(output.result.kind, TurnResultKind::Aborted);
        assert_eq!(output.result.stop_reason, TurnStopReason::AbortedStreaming);
        assert_eq!(output.messages.len(), 1);
        assert_eq!(*log.lock().unwrap(), vec!["turn_completed"]);
    } else {
        assert!(
            matches!(result, Err(PortError::AcceptedUnknown { identity, .. }) if *identity == record.identity)
        );
        assert_eq!(*log.lock().unwrap(), vec!["agent_status:turn_aborted"]);
    }
    assert!(
        received.try_recv().is_err(),
        "loop-top abort issued another host call"
    );
    // Even informational Completed cannot settle the unanswered capture.
    let fresh = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    };
    assert!(
        matches!(host.into_exchange().exchange(&fresh, "model", "prepare", CallEffect::SideEffect, JsObject::new()), Err(PortError::AcceptedUnknown { identity, .. }) if *identity == record.identity)
    );
    owner.close();
    assert!(writer.join().unwrap().is_ok());
}

#[test]
fn loop_top_deadline_without_capture_ack_still_publishes_aborted_completed() {
    run_loop_top_without_capture_ack(true);
}

#[test]
fn loop_top_requested_without_capture_ack_retains_unknown_and_omits_completed() {
    run_loop_top_without_capture_ack(false);
}

// Hold the writer inside the prior call's flush after its bytes reached the
// peer. The peer can ACK that call while the next capture stays Queued.
struct StalledWire {
    wire: Wire,
    resume: Option<mpsc::Receiver<()>>,
}
impl Write for StalledWire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.wire.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.wire.flush()?;
        if let Some(resume) = self.resume.take() {
            resume
                .recv_timeout(Duration::from_secs(10))
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}
fn run_loop_top_with_replied_prior_and_stalled_writer(deadline: bool) {
    let owner = ConnectionOwner::new(
        SidecarBinding {
            module_id: "pilotdeck-agent-loop".into(),
            module_instance_id: "instance".into(),
            connection_generation: "generation".into(),
            capabilities_version: "1".into(),
        },
        8,
    );
    let identity = ExecutionIdentity {
        session_id: "session".into(),
        turn_id: "turn".into(),
        run_id: "run".into(),
        operation_id: "op".into(),
        request_id: "initial".into(),
        idempotency_key: None,
        module_instance_id: "instance".into(),
        connection_generation: "generation".into(),
    };
    let cancellation = Cancellation::default();
    let control = CallControl {
        cancellation: cancellation.clone(),
        deadline: None,
    };
    let cleanup = CallControl {
        cancellation: cancellation.cleanup_scope(),
        deadline: None,
    };
    let (sent, received) = mpsc::channel();
    let (resume, stalled) = mpsc::channel();
    let writer_owner = owner.clone();
    let writer = std::thread::spawn(move || {
        writer_owner.drive_writer(StalledWire {
            wire: Wire(sent, vec![]),
            resume: Some(stalled),
        })
    });
    let peer_owner = owner.clone();
    let peer = std::thread::spawn(move || {
        let bytes = received.recv_timeout(Duration::from_secs(1)).unwrap();
        let request = json(std::str::from_utf8(&bytes).unwrap());
        assert_eq!(
            request.get("operation"),
            Some(&JsValue::from("stream_next"))
        );
        let id = request
            .get("messageId")
            .unwrap()
            .as_string()
            .unwrap()
            .clone();
        let mut ack = JsObject::new();
        ack.insert("kind".into(), "response".into());
        ack.insert("messageId".into(), "ack".into());
        ack.insert("inReplyTo".into(), JsValue::String(id.clone()));
        ack.insert("ok".into(), JsValue::Bool(true));
        peer_owner.receive_response(&JsValue::Object(ack));
        (received, id)
    });
    let mut exchange = owner.bound_exchange(identity).unwrap();
    exchange
        .exchange(
            &control,
            "model",
            "stream_next",
            CallEffect::SideEffect,
            JsObject::new(),
        )
        .unwrap();
    let (received, prior_id) = peer.join().unwrap();
    assert_eq!(
        owner.call_record(&prior_id).unwrap().phase,
        DispatchPhase::Replied
    );
    assert!(cancellation.cancel(if deadline {
        CancelCause::Deadline
    } else {
        CancelCause::Requested("user cancel".into())
    }));
    let mut host = RemoteHost::new(
        exchange,
        AdvertisedMethods {
            modules: vec![("context".into(), vec!["capture_turn".into()])],
        },
    );
    let log: Log = Arc::default();
    let start = Instant::now();
    let result = run_admitted_turn(
        &mut config(),
        &input(),
        vec![message("prompt")],
        &mut host,
        &mut Preparation {
            log: log.clone(),
            arm_in_prepare: None,
        },
        &mut Events(log.clone()),
        &Clock,
        &control,
        &cleanup,
        &mut tool_hints(),
    );
    let elapsed = start.elapsed();
    // Release before asserting so a failing regression also joins its writer.
    resume.send(()).unwrap();
    // A removed Queued capture must never reach the peer when the writer resumes.
    assert!(received.recv_timeout(Duration::from_millis(100)).is_err());
    owner.close();
    assert!(writer.join().unwrap().is_ok());
    assert!(
        elapsed >= Duration::from_secs(5),
        "writer did not remain stalled for the capture budget"
    );
    assert!(
        elapsed < Duration::from_secs(7),
        "capture exceeded its local bound"
    );
    assert_eq!(
        owner.call_record(&prior_id).unwrap().phase,
        DispatchPhase::Replied
    );
    if deadline {
        let output = result.unwrap();
        assert_eq!(output.result.kind, TurnResultKind::Aborted);
        assert_eq!(output.result.stop_reason, TurnStopReason::AbortedStreaming);
        assert_eq!(output.messages.len(), 1);
        assert_eq!(*log.lock().unwrap(), vec!["turn_completed"]);
    } else {
        assert!(matches!(
            result,
            Err(PortError::Cancelled {
                cause: CancelCause::Deadline
            })
        ));
        assert_eq!(*log.lock().unwrap(), vec!["agent_status:turn_aborted"]);
    }
}
#[test]
fn loop_top_deadline_replied_prior_stalled_writer_still_publishes_aborted_completed() {
    run_loop_top_with_replied_prior_and_stalled_writer(true);
}
#[test]
fn loop_top_requested_replied_prior_stalled_writer_retains_local_cancel_and_omits_completed() {
    run_loop_top_with_replied_prior_and_stalled_writer(false);
}
