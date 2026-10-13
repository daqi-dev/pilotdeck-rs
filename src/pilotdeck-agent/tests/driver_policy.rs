//! Supplementary controlled port tests; not real formal host/NDJSON acceptance.
use pilotdeck_agent::driver::*;
use pilotdeck_agent::ports::*;
use pilotdeck_agent::{CapturedModelEvent, assemble_assistant_message, create_model_assembler};
use pilotdeck_protocol::model::{BoundarySnapshot, ModelEvent, PreparedWireRequest, Usage};
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn snap(json: &str) -> BoundarySnapshot {
    BoundarySnapshot::capture(&parse_json(json).unwrap())
}
fn control() -> CallControl {
    CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    }
}
fn route() -> Route {
    Route {
        provider: "test".into(),
        model: "model".into(),
    }
}
fn hints() -> ModelHints {
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
    }
}
fn prepared() -> PreparedInvocation {
    PreparedInvocation {
        handle: PreparedRequestHandle("prepared-A".into()),
        request: CapturedRequest {
            route: route(),
            normalized: RequestProjection::BoundaryOnly(Vec::new()),
            wire: PreparedWireRequest::capture(
                &parse_json(r#"{"provider":"test","model":"model","messages":[]}"#).unwrap(),
            ),
        },
        route: route(),
        max_context_tokens: None,
        max_output_tokens: None,
        metadata: None,
        boundary: snap(
            r#"{"request":{"provider":"test","model":"model","messages":[]},"provider":"test","model":"model"}"#,
        ),
    }
}
type PortBehavior = Box<dyn FnMut(&CallControl, &HostCall) -> PortResult<HostReply> + Send>;
struct ControlledHost {
    replies: VecDeque<PortResult<HostReply>>,
    calls: Arc<Mutex<Vec<(&'static str, &'static str)>>>,
    behavior: Option<PortBehavior>,
    pull: bool,
}
impl ControlledHost {
    fn new(replies: Vec<PortResult<HostReply>>) -> Self {
        Self {
            replies: replies.into(),
            calls: Arc::default(),
            behavior: None,
            pull: true,
        }
    }
}
impl HostPort for ControlledHost {
    fn advertised(&self, _: &str, operation: &str) -> bool {
        operation != "stream_next" || self.pull
    }
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
        self.calls.lock().unwrap().push(call.method());
        if let Some(behavior) = &mut self.behavior {
            return behavior(control, &call);
        }
        self.replies
            .pop_front()
            .expect("unexpected additional host dispatch")
    }
}
fn text_event() -> StreamEvent {
    StreamEvent::Known(Box::new(CapturedModelEvent {
        event: ModelEvent::TextDelta {
            text: "prefix".into(),
            raw: None,
        },
        boundary: snap(r#"{"type":"text_delta","text":"prefix"}"#),
    }))
}
fn unknown_event() -> StreamEvent {
    StreamEvent::BoundaryOnly(snap(r#"{"type":"provider_extension","extra":"kept"}"#))
}

#[test]
fn real_condvar_cancellation_wakes_and_retains_first_cause() {
    let token = Cancellation::default();
    let waiter = token.clone();
    let thread =
        std::thread::spawn(move || waiter.wait_until(Instant::now() + Duration::from_secs(5)));
    assert!(token.cancel(CancelCause::Requested("first".into())));
    assert!(!token.cancel(CancelCause::OwnerRetired));
    assert_eq!(
        thread.join().unwrap(),
        Some(CancelCause::Requested("first".into()))
    );
}
#[test]
fn successful_compaction_reply_cannot_override_owner_abort() {
    let mut host = ControlledHost::new(Vec::new());
    host.behavior = Some(Box::new(|control, call| {
        assert!(matches!(call, HostCall::TurnPersistCompaction { .. }));
        control
            .cancellation
            .cancel(CancelCause::Requested("persist failed".into()));
        Ok(HostReply::CompactionCommitted(CompactionCommit {
            response: snap(r#"{"ok":true}"#),
        }))
    }));
    let result = persist_compaction_before_followup(
        &mut host,
        &control(),
        CompactBoundary {
            boundary: snap("{}"),
        },
        Vec::new(),
    );
    assert!(matches!(result, Err(PortError::Cancelled { .. })));
    assert_eq!(
        *host.calls.lock().unwrap(),
        [("turn", "persist_compaction")]
    );
}
#[test]
fn source_cost_unknown_keeps_ledger_unchanged_and_exact_usage_reaches_host() {
    let raw = snap(r#"{"futureFinite":-0,"nativeCost":0.5,"details":{"retained":true}}"#);
    let expected = raw.clone();
    let mut host = ControlledHost::new(vec![]);
    host.behavior = Some(Box::new(move |_, call| {
        let HostCall::BudgetCostSnapshot {
            usage: Some(actual),
            route: actual_route,
        } = call
        else {
            panic!("closed usage reconstruction reached host")
        };
        assert_eq!(actual.value(), expected.value());
        assert_eq!(actual_route, &route());
        Err(PortError::AcceptedUnknown {
            identity: Box::new(ExecutionIdentity {
                session_id: "s".into(),
                turn_id: "t".into(),
                run_id: "r".into(),
                operation_id: "o".into(),
                request_id: "q".into(),
                idempotency_key: Some("key".into()),
                module_instance_id: "i".into(),
                connection_generation: "g".into(),
            }),
            last_applied_sequence: Some(7.0),
        })
    }));
    let mut ledger = BudgetLedger::new(Some(1.0), Some(3.0), 2.0);
    assert!(matches!(
        ledger.charge_source_usage(&mut host, &control(), Some(&raw), route()),
        Err(PortError::AcceptedUnknown {
            last_applied_sequence: Some(7.0),
            ..
        })
    ));
    assert_eq!(ledger.task_spent_usd, 2.0);
    assert_eq!(ledger.turn_spent_usd, 0.0);
    assert_eq!(host.calls.lock().unwrap().len(), 1);
}

#[test]
fn source_cost_zero_overrides_native_and_missing_estimate_falls_back_before_turn_ceiling() {
    let mut host = ControlledHost::new(vec![
        Ok(HostReply::BudgetCost(Some(0.0))),
        Ok(HostReply::BudgetCost(None)),
        Ok(HostReply::BudgetCost(Some(0.5))),
    ]);
    let raw = snap(r#"{"futureFinite":2,"nativeCost":0.5}"#);
    let mut ledger = BudgetLedger::new(Some(1.0), Some(3.0), 2.0);
    assert_eq!(
        ledger
            .charge_source_usage(&mut host, &control(), Some(&raw), route())
            .unwrap(),
        (Some(0.0), None)
    );
    assert_eq!(
        ledger
            .charge_source_usage(&mut host, &control(), Some(&raw), route())
            .unwrap(),
        (Some(0.5), None)
    );
    assert_eq!(
        ledger
            .charge_source_usage(&mut host, &control(), None, route())
            .unwrap(),
        (Some(0.5), Some(BudgetStop::Turn))
    );
    assert_eq!((ledger.task_spent_usd, ledger.turn_spent_usd), (3.0, 1.0));
}

#[test]
fn null_cost_falls_back_to_native_zero_does_not_and_turn_wins_both_ceilings() {
    let mut host = ControlledHost::new(vec![
        Ok(HostReply::BudgetCost(None)),
        Ok(HostReply::BudgetCost(Some(0.0))),
        Ok(HostReply::BudgetCost(Some(0.5))),
    ]);
    let mut ledger = BudgetLedger::new(Some(1.0), Some(3.0), 2.0);
    let usage = Usage {
        native_cost: Some(0.5),
        ..Usage::default()
    };
    assert_eq!(
        ledger
            .charge(&mut host, &control(), Some(&usage), route())
            .unwrap(),
        (Some(0.5), None)
    );
    assert_eq!(
        ledger
            .charge(&mut host, &control(), Some(&usage), route())
            .unwrap(),
        (Some(0.0), None)
    );
    assert_eq!(
        ledger.charge(&mut host, &control(), None, route()).unwrap(),
        (Some(0.5), Some(BudgetStop::Turn))
    );
    assert_eq!(ledger.task_spent_usd, 3.0);
    assert_eq!(ledger.turn_spent_usd, 1.0);
}
#[test]
fn pull_forwards_unknown_events_and_never_authorizes_missing_end_tools() {
    let mut host = ControlledHost::new(vec![Ok(HostReply::ModelChunk(StreamChunk {
        events: vec![unknown_event(), text_event()],
        done: true,
    }))]);
    let mut emitted = Vec::new();
    let result = run_model_invocation(
        &mut host,
        &control(),
        &control(),
        &prepared(),
        &hints(),
        "response".into(),
        &mut |e| {
            emitted.push(e);
            Ok(())
        },
    );
    assert_eq!(emitted.len(), 2);
    assert_eq!(
        emitted[0]
            .boundary
            .value()
            .get("extra")
            .and_then(JsValue::as_string),
        Some(&JsString::from("kept"))
    );
    assert!(!result.assembled.has_message_end);
    assert!(!invocation_can_execute_tools(&result));
    assert_eq!(*host.calls.lock().unwrap(), [("model", "stream_next")]);
}
#[test]
fn cancellation_during_live_emission_drops_remaining_advanced_events_and_closes_once() {
    let mut host = ControlledHost::new(vec![
        Ok(HostReply::ModelChunk(StreamChunk {
            events: vec![text_event(), text_event()],
            done: false,
        })),
        Ok(HostReply::ModelClosed),
    ]);
    let ctrl = control();
    let token = ctrl.cancellation.clone();
    let mut count = 0;
    let result = run_model_invocation(
        &mut host,
        &ctrl,
        &control(),
        &prepared(),
        &hints(),
        "response".into(),
        &mut |_| {
            count += 1;
            token.cancel(CancelCause::Requested("cancel".into()));
            Ok(())
        },
    );
    assert_eq!(count, 1);
    assert!(matches!(
        result.disposition,
        InvocationDisposition::Cancelled(..)
    ));
    assert!(!invocation_can_execute_tools(&result));
    assert_eq!(
        *host.calls.lock().unwrap(),
        [("model", "stream_next"), ("model", "close_stream")]
    );
}
#[test]
fn advancing_reply_unknown_is_preserved_and_never_pulled_again() {
    let identity = ExecutionIdentity {
        session_id: "session".into(),
        turn_id: "turn".into(),
        run_id: "run".into(),
        operation_id: "op".into(),
        request_id: "pull-1".into(),
        idempotency_key: None,
        module_instance_id: "instance".into(),
        connection_generation: "generation".into(),
    };
    let mut host = ControlledHost::new(vec![
        Err(PortError::AcceptedUnknown {
            identity: Box::new(identity),
            last_applied_sequence: Some(3.0),
        }),
        Ok(HostReply::ModelClosed),
    ]);
    let result = run_model_invocation(
        &mut host,
        &control(),
        &control(),
        &prepared(),
        &hints(),
        "response".into(),
        &mut |_| panic!("uncertain pull must not fabricate live events"),
    );
    assert!(
        matches!(result.disposition, InvocationDisposition::PortFailure(error) if matches!(*error, PortError::AcceptedUnknown { .. }))
    );
    assert_eq!(
        *host.calls.lock().unwrap(),
        [("model", "stream_next"), ("model", "close_stream")]
    );
}
#[test]
fn unknown_finish_and_stream_retry_limits_are_distinct_and_reset_only_on_respective_success() {
    let mut guards = RetryGuards::default();
    assert!(guards.retry_stream_interruption());
    assert!(guards.retry_unknown_finish());
    assert!(guards.retry_stream_interruption());
    assert!(!guards.retry_stream_interruption());
    guards.observed_non_interrupted();
    assert!(guards.retry_stream_interruption());
    assert!(guards.retry_unknown_finish());
    assert!(!guards.retry_unknown_finish());
    assert!(guards.retry_json());
    assert!(guards.retry_json());
    assert!(guards.retry_json());
    assert!(!guards.retry_json());
    for _ in 0..50 {
        assert!(guards.continue_output());
    }
    assert!(!guards.continue_output());
    guards.compact_attempted = true;
    guards.reasoning_attempted = true;
    guards.image_strip_attempted = true;
    assert_eq!(guards.observe_tool_batch(&[]), InvalidBatchAction::Continue);
    assert!(guards.compact_attempted && guards.reasoning_attempted);
    assert_eq!(guards.json_corrections, 3);
    assert!(!guards.image_strip_attempted);
}
#[test]
fn policy_refuses_even_complete_unknown_finish() {
    let mut assembler = create_model_assembler(None);
    assembler.has_message_end = true;
    assembler.finish_reason = Some(pilotdeck_protocol::model::FinishReason::Unknown);
    let report = InvocationReport {
        assembled: Box::new(assemble_assistant_message(&mut assembler)),
        executed: None,
        disposition: InvocationDisposition::Assembled,
    };
    assert!(!invocation_can_execute_tools(&report));
}

#[test]
fn shallow_error_after_structured_end_stops_and_preserves_error_without_defaults() {
    use pilotdeck_protocol::model::{FinishReason, ToolCall};
    let call = ToolCall {
        id: "call".into(),
        name: "write".into(),
        input: JsValue::Object(Default::default()),
        timeline: None,
        raw: None,
    };
    let events = vec![
        StreamEvent::Known(Box::new(CapturedModelEvent {
            event: ModelEvent::ToolCallEnd {
                tool_call: call,
                was_repaired: None,
                raw: None,
            },
            boundary: snap(
                r#"{"type":"tool_call_end","toolCall":{"id":"call","name":"write","input":{}}}"#,
            ),
        })),
        StreamEvent::Known(Box::new(CapturedModelEvent {
            event: ModelEvent::MessageEnd {
                finish_reason: FinishReason::ToolCall,
                raw: None,
            },
            boundary: snap(r#"{"type":"message_end","finishReason":"tool_call"}"#),
        })),
        StreamEvent::ErrorBoundaryOnly {
            boundary: snap(r#"{"type":"error","error":{"code":"shallow","message":"failure"}}"#),
            error: snap(r#"{"code":"shallow","message":"failure"}"#),
        },
        text_event(),
    ];
    let mut host = ControlledHost::new(vec![
        Ok(HostReply::ModelChunk(StreamChunk { events, done: true })),
        Ok(HostReply::ModelClosed),
    ]);
    let mut emitted = 0;
    let result = run_model_invocation(
        &mut host,
        &control(),
        &control(),
        &prepared(),
        &hints(),
        "response".into(),
        &mut |_| {
            emitted += 1;
            Ok(())
        },
    );
    assert_eq!(emitted, 3);
    assert_eq!(result.assembled.tool_calls.len(), 1);
    assert!(result.assembled.has_message_end);
    assert!(matches!(
        result.disposition,
        InvocationDisposition::BoundaryError { .. }
    ));
    assert_eq!(result.assembled.finish_reason, FinishReason::Error);
    assert_eq!(
        result.assembled.boundary.value().get("error"),
        Some(snap(r#"{"code":"shallow","message":"failure"}"#).value())
    );
    assert!(result.assembled.error.is_none());
    assert!(!invocation_can_execute_tools(&result));
    assert_eq!(
        *host.calls.lock().unwrap(),
        [("model", "stream_next"), ("model", "close_stream")]
    );
}

#[test]
fn repaired_guard_matches_source_finish_domain_without_deleting_other_paths() {
    use pilotdeck_protocol::model::FinishReason;
    for (finish, may_execute) in [
        (FinishReason::Stop, false),
        (FinishReason::Length, false),
        (FinishReason::ToolCall, false),
        (FinishReason::ContentFilter, true),
    ] {
        let mut assembler = create_model_assembler(None);
        assembler.has_message_end = true;
        assembler.has_repaired_tool_calls = Some(true);
        assembler.finish_reason = Some(finish);
        let report = InvocationReport {
            assembled: Box::new(assemble_assistant_message(&mut assembler)),
            executed: None,
            disposition: InvocationDisposition::Assembled,
        };
        assert_eq!(
            invocation_can_execute_tools(&report),
            may_execute,
            "{finish:?}"
        );
    }
}
