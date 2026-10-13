//! Supplementary controlled port tests; not real formal host/NDJSON acceptance.
use pilotdeck_agent::driver::*;
use pilotdeck_agent::ports::*;
use pilotdeck_protocol::model::{BoundarySnapshot, Usage};
use pilotdeck_protocol::parse_json;
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
