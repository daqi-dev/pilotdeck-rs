use super::*;
use crate::ports::{Cancellation, ExecutionIdentity};

struct CaptureHost {
    reply: Option<PortResult<HostReply>>,
    calls: usize,
    advertised: bool,
}
impl HostPort for CaptureHost {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        self.advertised && module == "context" && operation == "capture_turn"
    }
    fn call(&mut self, _: &CallControl, call: HostCall) -> PortResult<HostReply> {
        self.calls += 1;
        let HostCall::CaptureTurn(capture) = call else {
            panic!("unexpected effect")
        };
        assert!(capture.errored);
        assert!(capture.messages.is_empty());
        assert!(capture.session_id.equals_str("capture-session"));
        self.reply.take().expect("capture must never be replayed")
    }
}
fn input() -> TurnInput {
    TurnInput {
        session_id: "capture-session".into(),
        turn_id: "capture-turn".into(),
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
fn run(reply: PortResult<HostReply>) -> (PortResult<()>, usize) {
    let mut host = CaptureHost {
        reply: Some(reply),
        calls: 0,
        advertised: true,
    };
    let control = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    };
    let result = capture_turn_advisory(&mut host, &control, &input(), &[], true);
    (result, host.calls)
}
#[test]
fn capture_unknown_retains_full_owner_and_sequence_without_replay() {
    let identity = ExecutionIdentity {
        session_id: "capture-session".into(),
        turn_id: "capture-turn".into(),
        run_id: "run".into(),
        operation_id: "operation".into(),
        request_id: "request".into(),
        idempotency_key: Some("stable-key".into()),
        module_instance_id: "module".into(),
        connection_generation: "generation".into(),
    };
    let (result, calls) = run(Err(PortError::AcceptedUnknown {
        identity: Box::new(identity.clone()),
        last_applied_sequence: Some(19.0),
    }));
    let Err(PortError::AcceptedUnknown {
        identity: retained,
        last_applied_sequence,
    }) = result
    else {
        panic!("unknown effect must not become completion")
    };
    assert_eq!(*retained, identity);
    assert_eq!(last_applied_sequence, Some(19.0));
    assert_eq!(calls, 1);
}
#[test]
fn capture_known_source_failure_remains_advisory() {
    let (result, calls) = run(Err(PortError::HostFailure {
        code: "known_capture_failure".into(),
        message: "returned failure".into(),
        boundary: BoundarySnapshot::capture(&JsValue::Null),
    }));
    assert!(result.is_ok());
    assert_eq!(calls, 1);
}
#[test]
fn capture_cancelled_is_not_reclassified_as_known_failure() {
    let (result, calls) = run(Err(PortError::Cancelled {
        cause: CancelCause::TransportClosed,
    }));
    assert!(matches!(
        result,
        Err(PortError::Cancelled {
            cause: CancelCause::TransportClosed
        })
    ));
    assert_eq!(calls, 1);
}
#[test]
fn capture_reply_for_another_operation_is_invalid() {
    let (result, calls) = run(Ok(HostReply::BudgetCost(Some(1.0))));
    assert!(matches!(result, Err(PortError::InvalidResponse { .. })));
    assert_eq!(calls, 1);
}
#[test]
fn absent_capture_hook_never_dispatches_an_effect() {
    let mut host = CaptureHost {
        reply: None,
        calls: 0,
        advertised: false,
    };
    let control = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    };
    assert!(capture_turn_advisory(&mut host, &control, &input(), &[], true).is_ok());
    assert_eq!(host.calls, 0);
}
