use super::*;
use crate::ports::{Cancellation, Captured, ExecutionIdentity, LifecycleResult};
use std::sync::{Arc, Mutex};
type Log = Arc<Mutex<Vec<JsString>>>;
struct Clock(Log);
impl TurnClock for Clock {
    fn now_iso(&self) -> JsString {
        self.0.lock().unwrap().push("result_clock".into());
        "end".into()
    }
    fn uuid(&self) -> JsString {
        "id".into()
    }
}
struct Events(Log);
impl TurnEventPort for Events {
    fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()> {
        if event.value().get("type") == Some(&JsValue::from("agent_status")) {
            // TS keeps text on the internal callback, but omits it from
            // the public event while preserving detail unchanged.
            assert!(event.value().get("text").is_none());
            assert_eq!(
                event.value().get("detail"),
                Some(&pilotdeck_protocol::parse_json(r#"{"visible":true}"#).unwrap())
            );
        }
        self.0.lock().unwrap().push(
            event
                .value()
                .get("type")
                .and_then(JsValue::as_string)
                .unwrap()
                .clone(),
        );
        Ok(())
    }
}
struct Host {
    log: Log,
    model_error: BoundarySnapshot,
    capture_error: Option<PortError>,
}
impl HostPort for Host {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        matches!(
            (module, operation),
            ("lifecycle", "dispatch") | ("context", "capture_turn")
        )
    }
    fn call(&mut self, _: &CallControl, call: HostCall) -> PortResult<HostReply> {
        match call {
            HostCall::DispatchLifecycle { event, payload } => {
                assert!(event.equals_str("StopFailure"));
                assert_eq!(
                    payload.unwrap().get("error"),
                    Some(self.model_error.value())
                );
                self.log.lock().unwrap().push("StopFailure".into());
                Ok(HostReply::Lifecycle(Captured {
                    view: LifecycleResult {
                        effects: vec![],
                        messages: vec![],
                        events: vec![],
                        blocking_errors: vec![],
                        non_blocking_errors: vec![],
                        pending_async_hooks: None,
                    },
                    boundary: BoundarySnapshot::capture(&JsValue::Null),
                }))
            }
            HostCall::CaptureTurn(capture) => {
                assert!(capture.errored);
                self.log.lock().unwrap().push("capture_true".into());
                self.capture_error
                    .take()
                    .map_or(Ok(HostReply::CapturedTurn), Err)
            }
            _ => panic!("no model/tool replay in terminal phase"),
        }
    }
}
fn run(
    late_exception: bool,
    capture_error: Option<PortError>,
) -> (PortResult<TurnOutput>, Vec<JsString>) {
    let log: Log = Arc::new(Mutex::new(vec![]));
    let raw = BoundarySnapshot::capture(&pilotdeck_protocol::parse_json(r#"{"code":"network_error","message":"partial\ud800","provider":"parity","protocol":"openai","retryable":false}"#).unwrap());
    let (reason, classified) = classify_ordinary_model_error(&raw).unwrap();
    let mut state = TurnExecutionState {
        messages: vec![],
        turn_count: 2.0,
        usage: empty_usage(),
        permission_denials: vec![],
        final_message: None,
        structured_output: Some(BoundarySnapshot::capture(&JsValue::from(
            "earlier-tool-data",
        ))),
        budget: BudgetLedger::new(None, None, 0.0),
        timeline: TurnTimeline::new("turn".into()),
        transient_prompt_ids: vec![],
    };
    let input = TurnInput {
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
    };
    let control = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    };
    let mut host = Host {
        log: log.clone(),
        model_error: raw.clone(),
        capture_error,
    };
    let mut events = Events(log.clone());
    let clock = Clock(log.clone());
    let result = finish_ordinary_model_error(
        &mut state,
        "start".into(),
        &raw,
        classified,
        reason,
        |_, error| {
            log.lock().unwrap().push("build_status".into());
            if late_exception {
                return Err(PortError::SourceException {
                    message: "providerMessage.trim is not a function".into(),
                    boundary: error.clone(),
                });
            }
            Ok(BoundarySnapshot::capture(&pilotdeck_protocol::parse_json(r#"{"event":"model_request_failed","text":"provider failure\n\nAction: check network","detail":{"visible":true}}"#).unwrap()))
        },
        &clock,
        TerminalTurnPorts {
            host: &mut host,
            events: &mut events,
            input: &input,
            control: &control,
        },
    );
    assert!(
        state.structured_output.is_some(),
        "result omission must not erase turn state"
    );
    let recorded = log.lock().unwrap().clone();
    (result, recorded)
}
#[test]
fn budget_status_projection_preserves_failure_diagnostics_for_both_limits() {
    struct BudgetEvents(Vec<BoundarySnapshot>);
    impl TurnEventPort for BudgetEvents {
        fn emit(&mut self, event: BoundarySnapshot) -> PortResult<()> {
            self.0.push(event);
            Ok(())
        }
    }
    for (stop, event_name, reason) in [
        (
            crate::driver::BudgetStop::Turn,
            "max_budget_reached",
            TurnStopReason::MaxBudget,
        ),
        (
            crate::driver::BudgetStop::Task,
            "task_budget_reached",
            TurnStopReason::TaskBudget,
        ),
    ] {
        let log: Log = Arc::new(Mutex::new(vec![]));
        let mut ledger = BudgetLedger::new(
            (stop == crate::driver::BudgetStop::Turn).then_some(0.1),
            (stop == crate::driver::BudgetStop::Task).then_some(0.1),
            0.0,
        );
        ledger.turn_spent_usd = 0.125;
        ledger.task_spent_usd = 0.125;
        let assistant = ProjectedMessage {
            boundary: BoundarySnapshot::capture(
                &pilotdeck_protocol::parse_json(r#"{"role":"assistant","content":[]}"#).unwrap(),
            ),
            normalized: MessageProjection::BoundaryOnly(vec![]),
        };
        let route = crate::ports::Route {
            provider: "parity".into(),
            model: "model".into(),
        };
        let plan = crate::budget_terminal::OriginalBudgetTerminalPolicy
            .evaluate(crate::budget_terminal::BudgetTerminalInput {
                ledger: &ledger,
                stop,
                last_invocation_cost_usd: Some(0.125),
                route: &route,
                assistant_message: &assistant,
                unexecuted_tool_call_count: 1,
            })
            .unwrap();
        let error = plan.error.clone();
        let diagnostic = error.value().get("message").unwrap().clone();
        assert!(
            diagnostic
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap()
                .starts_with("Reached Gateway-owned")
        );
        let detail = plan.status_detail.clone();
        let state = TurnExecutionState {
            messages: vec![],
            turn_count: 1.0,
            usage: empty_usage(),
            permission_denials: vec![],
            final_message: None,
            structured_output: None,
            budget: ledger,
            timeline: TurnTimeline::new("turn".into()),
            transient_prompt_ids: vec![],
        };
        let input = TurnInput {
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
        };
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: None,
        };
        let mut host = Host {
            log: log.clone(),
            model_error: BoundarySnapshot::capture(&diagnostic),
            capture_error: None,
        };
        let mut events = BudgetEvents(vec![]);
        let output = finish_budget_terminal(
            state,
            "start".into(),
            plan,
            &Clock(log.clone()),
            TerminalTurnPorts {
                host: &mut host,
                events: &mut events,
                input: &input,
                control: &control,
            },
        )
        .unwrap();
        assert_eq!(output.result.kind, TurnResultKind::Error);
        assert_eq!(output.result.stop_reason, reason);
        assert_eq!(output.result.errors.len(), 1);
        assert_eq!(output.result.errors[0].value(), error.value());
        let values: Vec<_> = events.0.iter().map(BoundarySnapshot::value).collect();
        assert_eq!(
            values
                .iter()
                .map(|v| v.get("type").unwrap().clone())
                .collect::<Vec<_>>(),
            [
                "stop_failure",
                "agent_status",
                "turn_failed",
                "turn_completed"
            ]
            .map(JsValue::from)
        );
        assert_eq!(values[0].get("error"), Some(&diagnostic));
        assert_eq!(values[1].get("event"), Some(&JsValue::from(event_name)));
        assert_eq!(values[1].get("detail"), Some(detail.value()));
        // TS sidecarStatusMessage falls back to event.event when these
        // fields are absent. Numeric budget detail must remain intact.
        assert!(values[1].get("text").is_none());
        assert!(detail.value().get("message").is_none());
        assert_eq!(values[2].get("error"), Some(error.value()));
        assert_eq!(
            *log.lock().unwrap(),
            ["StopFailure", "result_clock", "capture_true"].map(JsString::from)
        );
    }
}
#[test]
fn ordinary_terminal_has_source_order_and_omits_prior_structured_output() {
    let (output, log) = run(false, None);
    let output = output.unwrap();
    assert!(matches!(output.result.kind, TurnResultKind::Error));
    assert!(output.result.structured_output.is_none());
    assert_eq!(output.result.errors.len(), 1);
    assert_eq!(
        log,
        [
            "StopFailure",
            "stop_failure",
            "result_clock",
            "build_status",
            "agent_status",
            "turn_failed",
            "capture_true",
            "turn_completed"
        ]
        .map(JsString::from)
    );
}
#[test]
fn late_status_exception_preserves_prior_phases_and_prevents_capture_completion() {
    let (result, log) = run(true, None);
    assert!(matches!(result, Err(PortError::SourceException { .. })));
    assert_eq!(
        log,
        [
            "StopFailure",
            "stop_failure",
            "result_clock",
            "build_status"
        ]
        .map(JsString::from)
    );
}
#[test]
fn accepted_capture_unknown_retains_identity_and_never_publishes_completion() {
    let identity = ExecutionIdentity {
        session_id: "session".into(),
        turn_id: "turn".into(),
        run_id: "run".into(),
        operation_id: "operation".into(),
        request_id: "accepted-capture".into(),
        idempotency_key: None,
        module_instance_id: "module".into(),
        connection_generation: "generation".into(),
    };
    let (result, log) = run(
        false,
        Some(PortError::AcceptedUnknown {
            identity: Box::new(identity.clone()),
            last_applied_sequence: Some(19.0),
        }),
    );
    assert!(
        matches!(result,Err(PortError::AcceptedUnknown { identity:actual,last_applied_sequence:Some(19.0) }) if *actual==identity)
    );
    assert_eq!(log.last(), Some(&JsString::from("capture_true")));
    assert!(!log.iter().any(|kind| kind.equals_str("turn_completed")));
}
