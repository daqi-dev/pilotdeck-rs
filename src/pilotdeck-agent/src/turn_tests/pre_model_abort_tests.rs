use super::*;
use crate::ports::{Cancellation, ExecutionIdentity};
use std::sync::{Arc, Mutex};

type Log = Arc<Mutex<Vec<&'static str>>>;
struct Clock(Log);
impl TurnClock for Clock {
    fn now_iso(&self) -> JsString {
        self.0.lock().unwrap().push("result_clock");
        "2026-01-01T00:00:00.000Z".into()
    }
    fn uuid(&self) -> JsString {
        "id".into()
    }
}
struct Preparation {
    log: Log,
    error: Option<PortError>,
}
impl TurnPreparationPort for Preparation {
    fn finish_pending_effects(&mut self) -> PortResult<()> {
        self.log.lock().unwrap().push("pending_fence");
        self.error.take().map_or(Ok(()), Err)
    }
    fn prepare(
        &mut self,
        _: &mut dyn HostPort,
        _: &CallControl,
        _: &TurnRuntimeConfig,
        _: &TurnInput,
        _: &[ProjectedMessage],
    ) -> PortResult<(CapturedRequest, ModelHints)> {
        panic!("no preparation after terminal cancellation")
    }
    fn project_assembly(&self, _: &BoundarySnapshot) -> PortResult<TurnAssembly> {
        panic!("no stream projection before model invocation")
    }
}
struct Host {
    log: Log,
    error: Option<PortError>,
    hard_stop_after_capture: Option<CancelCause>,
}
impl HostPort for Host {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        module == "context" && operation == "capture_turn"
    }
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
        assert!(
            control.check().is_ok(),
            "capture uses bounded cleanup, not cancelled execution control"
        );
        assert!(control.deadline.is_some());
        let HostCall::CaptureTurn(capture) = call else {
            panic!("no new model/tool effect")
        };
        assert!(!capture.errored);
        assert_eq!(capture.messages.len(), 2);
        self.log.lock().unwrap().push("capture_false");
        let result = self.error.take().map_or(Ok(HostReply::CapturedTurn), Err);
        if let Some(hard_stop) = self.hard_stop_after_capture.take() {
            // The local timeout is already the first cause when a hard stop
            // arrives, before the caller considers publishing Completed.
            control.cancellation.cancel(CancelCause::Deadline);
            control.cancellation.cancel(hard_stop.clone());
            assert_eq!(control.cancellation.cause(), Some(CancelCause::Deadline));
            assert_eq!(control.cancellation.hard_stop_cause(), Some(hard_stop));
        }
        result
    }
}
struct Events(Log);
impl TurnEventPort for Events {
    fn emit(&mut self, _: BoundarySnapshot) -> PortResult<()> {
        panic!("cancelled execution must use accepted delivery")
    }
    fn emit_accepted(&mut self, event: BoundarySnapshot) -> PortResult<()> {
        let kind = event
            .value()
            .get("type")
            .and_then(JsValue::as_string)
            .unwrap();
        let label = if kind.equals_str("agent_status") {
            "status"
        } else if kind.equals_str("turn_completed") {
            "completed"
        } else {
            panic!("unexpected terminal event")
        };
        self.0.lock().unwrap().push(label);
        Ok(())
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
fn unknown() -> PortError {
    PortError::AcceptedUnknown {
        identity: Box::new(ExecutionIdentity {
            session_id: "session".into(),
            turn_id: "turn".into(),
            run_id: "run".into(),
            operation_id: "operation".into(),
            request_id: "accepted-request".into(),
            idempotency_key: None,
            module_instance_id: "module".into(),
            connection_generation: "generation".into(),
        }),
        last_applied_sequence: Some(9.0),
    }
}
#[derive(Clone, Copy)]
enum AbortPath {
    PreModel,
    AfterTools,
    Stream,
}
fn run(
    error: PortError,
    pending: Option<PortError>,
    capture: Option<PortError>,
) -> (PortResult<TurnOutput>, Vec<&'static str>) {
    run_path(AbortPath::PreModel, error, pending, capture)
}
fn run_path(
    path: AbortPath,
    error: PortError,
    pending: Option<PortError>,
    capture: Option<PortError>,
) -> (PortResult<TurnOutput>, Vec<&'static str>) {
    run_path_with_hard_stop(path, error, pending, capture, None)
}
fn run_path_with_hard_stop(
    path: AbortPath,
    error: PortError,
    pending: Option<PortError>,
    capture: Option<PortError>,
    hard_stop_after_capture: Option<CancelCause>,
) -> (PortResult<TurnOutput>, Vec<&'static str>) {
    let log = Arc::new(Mutex::new(vec![]));
    let state = TurnExecutionState {
        messages: vec![message("prior"), message("unconsumed-prompt")],
        turn_count: 1.0,
        usage: empty_usage(),
        permission_denials: vec![],
        final_message: Some(message("prior-final")),
        structured_output: None,
        budget: BudgetLedger::new(None, None, 0.0),
        timeline: TurnTimeline::new("turn".into()),
        transient_prompt_ids: vec!["unconsumed-prompt".into()],
    };
    let mut host = Host {
        log: log.clone(),
        error: capture,
        hard_stop_after_capture,
    };
    let mut preparation = Preparation {
        log: log.clone(),
        error: pending,
    };
    let mut events = Events(log.clone());
    let clock = Clock(log.clone());
    let cleanup = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    };
    let input = input();
    let result = match path {
        AbortPath::PreModel => finish_pre_model_abort(
            state,
            "started".into(),
            error,
            &mut host,
            &mut preparation,
            &mut events,
            &clock,
            &input,
            &cleanup,
        ),
        AbortPath::AfterTools | AbortPath::Stream => {
            let PortError::Cancelled { cause } = error else {
                panic!("abort path requires a cancellation")
            };
            if matches!(path, AbortPath::AfterTools) {
                finish_aborted_after_tools(
                    state,
                    "started".into(),
                    &clock,
                    TerminalTurnPorts {
                        host: &mut host,
                        events: &mut events,
                        input: &input,
                        control: &cleanup,
                    },
                    cause == CancelCause::Deadline,
                )
            } else {
                finish_aborted_stream(
                    state,
                    "started".into(),
                    BoundarySnapshot::capture(
                        &pilotdeck_protocol::parse_json(r#"{"toolCalls":[{}]}"#).unwrap(),
                    ),
                    cause,
                    AbortedStreamBranch::StreamThrew,
                    AbortedTurnPorts {
                        host: &mut host,
                        preparation: &preparation,
                        events: &mut events,
                        clock: &clock,
                        input: &input,
                        control: &cleanup,
                    },
                )
            }
        }
    };
    let observed = log.lock().unwrap().clone();
    (result, observed)
}
fn assert_capture_cancellation_policy(path: AbortPath) {
    for deadline in [false, true] {
        for capture_error in [
            PortError::Cancelled {
                cause: CancelCause::Deadline,
            },
            PortError::Cancelled {
                cause: CancelCause::Requested("local cancel".into()),
            },
            PortError::Cancelled {
                cause: CancelCause::OwnerRetired,
            },
            PortError::Cancelled {
                cause: CancelCause::TransportClosed,
            },
            unknown(),
        ] {
            let hard_stop = matches!(
                capture_error,
                PortError::Cancelled {
                    cause: CancelCause::OwnerRetired | CancelCause::TransportClosed
                }
            );
            let (result, log) = run_path(
                path,
                PortError::Cancelled {
                    cause: if deadline {
                        CancelCause::Deadline
                    } else {
                        CancelCause::Requested("user cancel".into())
                    },
                },
                None,
                Some(capture_error.clone()),
            );
            assert!(log.contains(&"capture_false"));
            if deadline && !hard_stop {
                let output = result.unwrap();
                assert_eq!(output.result.kind, TurnResultKind::Aborted);
                assert_eq!(output.result.stop_reason, TurnStopReason::AbortedStreaming);
                assert_eq!(output.messages.len(), 2);
                assert_eq!(log.last(), Some(&"completed"));
            } else {
                match (result, capture_error) {
                    (
                        Err(PortError::Cancelled { cause }),
                        PortError::Cancelled { cause: expected },
                    ) => assert_eq!(cause, expected),
                    (
                        Err(PortError::AcceptedUnknown {
                            identity,
                            last_applied_sequence,
                        }),
                        PortError::AcceptedUnknown {
                            identity: expected,
                            last_applied_sequence: sequence,
                        },
                    ) => {
                        assert_eq!(identity, expected);
                        assert_eq!(last_applied_sequence, sequence);
                    }
                    (actual, expected) => {
                        panic!("capture error changed: {actual:?}; expected {expected:?}")
                    }
                }
                assert!(!log.contains(&"completed"));
            }
        }
    }
}
#[test]
fn pre_model_capture_local_cancel_completes_only_for_execution_deadline() {
    assert_capture_cancellation_policy(AbortPath::PreModel);
}
#[test]
fn after_tools_capture_local_cancel_completes_only_for_execution_deadline() {
    assert_capture_cancellation_policy(AbortPath::AfterTools);
}
#[test]
fn stream_capture_local_cancel_completes_only_for_execution_deadline() {
    assert_capture_cancellation_policy(AbortPath::Stream);
}
fn assert_capture_deadline_then_hard_stop(path: AbortPath) {
    for hard_stop in [CancelCause::OwnerRetired, CancelCause::TransportClosed] {
        for capture_error in [
            PortError::Cancelled {
                cause: CancelCause::Deadline,
            },
            PortError::Cancelled {
                cause: CancelCause::Requested("local cancel".into()),
            },
            unknown(),
        ] {
            let (result, log) = run_path_with_hard_stop(
                path,
                PortError::Cancelled {
                    cause: CancelCause::Deadline,
                },
                None,
                Some(capture_error.clone()),
                Some(hard_stop.clone()),
            );
            assert!(log.contains(&"capture_false"));
            match (result, capture_error) {
                (Err(PortError::Cancelled { cause }), PortError::Cancelled { cause: expected }) => {
                    assert_eq!(cause, expected);
                }
                (
                    Err(PortError::AcceptedUnknown {
                        identity,
                        last_applied_sequence,
                    }),
                    PortError::AcceptedUnknown {
                        identity: expected,
                        last_applied_sequence: sequence,
                    },
                ) => {
                    assert_eq!(identity, expected);
                    assert_eq!(last_applied_sequence, sequence);
                }
                (actual, expected) => {
                    panic!("capture error changed: {actual:?}; expected {expected:?}")
                }
            }
            assert!(!log.contains(&"completed"));
        }
    }
}
#[test]
fn pre_model_capture_deadline_then_hard_stop_suppresses_completed() {
    assert_capture_deadline_then_hard_stop(AbortPath::PreModel);
}
#[test]
fn after_tools_capture_deadline_then_hard_stop_suppresses_completed() {
    assert_capture_deadline_then_hard_stop(AbortPath::AfterTools);
}
#[test]
fn stream_capture_deadline_then_hard_stop_suppresses_completed() {
    assert_capture_deadline_then_hard_stop(AbortPath::Stream);
}
#[test]
fn known_pre_model_cancel_retains_messages_and_creates_result_before_status_capture() {
    let (result, log) = run(
        PortError::Cancelled {
            cause: CancelCause::Requested("Cancel requested".into()),
        },
        None,
        None,
    );
    let output = result.unwrap();
    assert_eq!(output.result.kind, TurnResultKind::Aborted);
    assert_eq!(output.result.stop_reason, TurnStopReason::AbortedStreaming);
    assert_eq!(output.result.turns, 1.0);
    assert_eq!(output.messages.len(), 2);
    assert!(output.result.final_message.is_some());
    assert_eq!(
        log,
        vec![
            "pending_fence",
            "result_clock",
            "status",
            "capture_false",
            "completed"
        ]
    );
}
#[test]
fn quiet_source_abort_reason_omits_status() {
    let (result, log) = run(
        PortError::Cancelled {
            cause: CancelCause::Requested("plain reason".into()),
        },
        None,
        None,
    );
    assert!(result.is_ok());
    assert_eq!(
        log,
        vec![
            "pending_fence",
            "result_clock",
            "capture_false",
            "completed"
        ]
    );
}
#[test]
fn pending_unknown_prevents_capture_and_known_completion() {
    let (result, log) = run(
        PortError::Cancelled {
            cause: CancelCause::Requested("cancel".into()),
        },
        Some(unknown()),
        None,
    );
    assert!(matches!(
        result,
        Err(PortError::AcceptedUnknown {
            last_applied_sequence: Some(9.0),
            ..
        })
    ));
    assert_eq!(log, vec!["pending_fence"]);
}
#[test]
fn pre_model_deadline_capture_unknown_still_emits_aborted_completion() {
    let (result, log) = run(
        PortError::Cancelled {
            cause: CancelCause::Deadline,
        },
        None,
        Some(unknown()),
    );
    let output = result.unwrap();
    assert_eq!(output.result.kind, TurnResultKind::Aborted);
    assert_eq!(output.messages.len(), 2);
    assert!(output.result.final_message.is_some());
    assert_eq!(
        log,
        vec![
            "pending_fence",
            "result_clock",
            "capture_false",
            "completed"
        ]
    );
}
#[test]
fn capture_unknown_prevents_known_completion() {
    let (result, log) = run(
        PortError::Cancelled {
            cause: CancelCause::Requested("cancel".into()),
        },
        None,
        Some(unknown()),
    );
    assert!(matches!(
        result,
        Err(PortError::AcceptedUnknown {
            last_applied_sequence: Some(9.0),
            ..
        })
    ));
    assert_eq!(
        log,
        vec!["pending_fence", "result_clock", "status", "capture_false"]
    );
}
#[test]
fn transport_loss_retirement_and_existing_unknown_do_not_create_cancel_result() {
    for error in [
        PortError::Cancelled {
            cause: CancelCause::OwnerRetired,
        },
        PortError::Cancelled {
            cause: CancelCause::TransportClosed,
        },
        unknown(),
    ] {
        let (result, log) = run(error, None, None);
        assert!(result.is_err());
        assert!(log.is_empty());
    }
}
