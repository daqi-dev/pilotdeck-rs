use super::*;
use crate::ports::{Cancellation, ExecutionIdentity};
struct Host {
    reply: Option<PortResult<HostReply>>,
    advertised: bool,
    calls: usize,
    error: BoundarySnapshot,
}
impl HostPort for Host {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        self.advertised && module == "context" && operation == "recover_from_model_error"
    }
    fn call(&mut self, _: &CallControl, call: HostCall) -> PortResult<HostReply> {
        let HostCall::RecoverModelErrorBoundary(recovery) = call else {
            panic!("only one source recovery probe")
        };
        assert_eq!(recovery.error.value(), self.error.value());
        assert!(!recovery.has_attempted_compact);
        self.calls += 1;
        self.reply.take().expect("never replay accepted probe")
    }
}
fn run(
    reply: PortResult<HostReply>,
    advertised: bool,
) -> (PortResult<Option<crate::ports::RecoveryDecision>>, usize) {
    let error = BoundarySnapshot::capture(
        &pilotdeck_protocol::parse_json(
            r#"{"code":"network_error","message":"failure","future":null}"#,
        )
        .unwrap(),
    );
    let mut host = Host {
        reply: Some(reply),
        advertised,
        calls: 0,
        error: error.clone(),
    };
    let control = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
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
    let result =
        probe_ordinary_model_error_recovery(&mut host, &control, &input, &[], &error, false);
    (result, host.calls)
}
#[test]
fn source_probe_giveup_falsy_future_and_known_exception_do_not_retry() {
    for json in [
        r#"{"type":"give_up"}"#,
        r#"{"type":"future"}"#,
        "null",
        "false",
    ] {
        let (result, calls) = run(
            Ok(HostReply::RecoveryBoundaryOnly(BoundarySnapshot::capture(
                &pilotdeck_protocol::parse_json(json).unwrap(),
            ))),
            true,
        );
        assert!(matches!(result, Ok(None)));
        assert_eq!(calls, 1);
    }
    let (result, calls) = run(
        Err(PortError::HostFailure {
            code: "known".into(),
            message: "failure".into(),
            boundary: BoundarySnapshot::capture(&JsValue::Null),
        }),
        true,
    );
    assert!(matches!(result, Ok(None)));
    assert_eq!(calls, 1);
    let (result, calls) = run(
        Err(PortError::HostFailure {
            code: "not_called".into(),
            message: "not called".into(),
            boundary: BoundarySnapshot::capture(&JsValue::Null),
        }),
        false,
    );
    assert!(matches!(result, Ok(None)));
    assert_eq!(calls, 0);
}
#[test]
fn accepted_unknown_probe_identity_remains_terminal_and_special_action_is_not_faked_giveup() {
    let identity = ExecutionIdentity {
        session_id: "session".into(),
        turn_id: "turn".into(),
        run_id: "run".into(),
        operation_id: "operation".into(),
        request_id: "accepted-recovery".into(),
        idempotency_key: None,
        module_instance_id: "module".into(),
        connection_generation: "generation".into(),
    };
    let (result, calls) = run(
        Err(PortError::AcceptedUnknown {
            identity: Box::new(identity.clone()),
            last_applied_sequence: Some(19.0),
        }),
        true,
    );
    assert!(
        matches!(result,Err(PortError::AcceptedUnknown {identity:actual,last_applied_sequence:Some(19.0)}) if *actual==identity)
    );
    assert_eq!(calls, 1);
    let (result, calls) = run(
        Ok(HostReply::RecoveryBoundaryOnly(BoundarySnapshot::capture(
            &pilotdeck_protocol::parse_json(
                r#"{"type":"truncate_head_and_retry","keepRatio":0.5}"#,
            )
            .unwrap(),
        ))),
        true,
    );
    assert!(result.is_err());
    assert_eq!(calls, 1);
}
