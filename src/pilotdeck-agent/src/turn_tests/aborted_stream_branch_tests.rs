//! Source-derived regression for the two Original aborted-stream branches
//! (AgentLoop.ts:859-885 vs :916-946), confirmed by the genuine Original
//! probe original-cancel-branch-probe-v1: during_stream_throw -> 0 status,
//! during_stream_eos -> 1 status, quiet reason -> 0 status.
use super::*;
use crate::ports::Cancellation;
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
struct Preparation;
impl TurnPreparationPort for Preparation {
    fn finish_pending_effects(&mut self) -> PortResult<()> {
        panic!("aborted stream path has no pending fence")
    }
    fn prepare(
        &mut self,
        _: &mut dyn HostPort,
        _: &CallControl,
        _: &TurnRuntimeConfig,
        _: &TurnInput,
        _: &[ProjectedMessage],
    ) -> PortResult<(CapturedRequest, ModelHints)> {
        panic!("no preparation after abort")
    }
    fn project_assembly(&self, _: &BoundarySnapshot) -> PortResult<TurnAssembly> {
        panic!("pending tool calls forbid a partial message")
    }
}
struct Host(Log);
impl HostPort for Host {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        module == "context" && operation == "capture_turn"
    }
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
        assert!(control.deadline.is_some());
        let HostCall::CaptureTurn(capture) = call else {
            panic!("no new model/tool effect")
        };
        assert!(!capture.errored);
        self.0.lock().unwrap().push("capture_false");
        Ok(HostReply::CapturedTurn)
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
/// A pending tool call makes Source `safeFinalTextMessage` return nothing,
/// so only the status/capture/terminal sequence is observed.
fn assembled_with_pending_tool_call() -> BoundarySnapshot {
    let mut call = JsObject::new();
    call.insert("id".into(), "call-1".into());
    call.insert("name".into(), "lookup".into());
    let mut message = JsObject::new();
    message.insert("role".into(), "assistant".into());
    message.insert("content".into(), JsValue::Array(vec![]));
    let mut value = JsObject::new();
    value.insert(
        "toolCalls".into(),
        JsValue::Array(vec![JsValue::Object(call)]),
    );
    value.insert("message".into(), JsValue::Object(message));
    BoundarySnapshot::capture(&JsValue::Object(value))
}
fn run(branch: AbortedStreamBranch, reason: &str) -> Vec<&'static str> {
    let log = Arc::new(Mutex::new(vec![]));
    let state = TurnExecutionState {
        messages: vec![message("prior"), message("prompt")],
        turn_count: 1.0,
        usage: empty_usage(),
        permission_denials: vec![],
        final_message: None,
        structured_output: None,
        budget: BudgetLedger::new(None, None, 0.0),
        timeline: TurnTimeline::new("turn".into()),
        transient_prompt_ids: vec![],
    };
    let mut host = Host(log.clone());
    let preparation = Preparation;
    let mut events = Events(log.clone());
    let clock = Clock(log.clone());
    let control = CallControl {
        cancellation: Cancellation::default(),
        deadline: None,
    };
    let turn_input = input();
    let result = finish_aborted_stream(
        state,
        "started".into(),
        assembled_with_pending_tool_call(),
        CancelCause::Requested(reason.into()),
        branch,
        AbortedTurnPorts {
            host: &mut host,
            preparation: &preparation,
            events: &mut events,
            clock: &clock,
            input: &turn_input,
            control: &control,
        },
    );
    assert!(
        result.is_ok(),
        "aborted stream must complete as known cancellation"
    );
    let observed = log.lock().unwrap();
    observed.clone()
}
#[test]
fn stream_threw_under_parity_cancel_omits_status() {
    assert_eq!(
        run(AbortedStreamBranch::StreamThrew, "parity_cancel"),
        vec!["capture_false", "result_clock", "completed"]
    );
}
#[test]
fn stream_completed_then_parity_cancel_emits_status() {
    assert_eq!(
        run(AbortedStreamBranch::StreamCompleted, "parity_cancel"),
        vec!["status", "capture_false", "result_clock", "completed"]
    );
}
#[test]
fn stream_completed_quiet_reason_omits_status() {
    assert_eq!(
        run(AbortedStreamBranch::StreamCompleted, "user_stop"),
        vec!["capture_false", "result_clock", "completed"]
    );
}
#[test]
fn stream_threw_timeout_reason_omits_status() {
    assert_eq!(
        run(AbortedStreamBranch::StreamThrew, "timeout:run-1"),
        vec!["capture_false", "result_clock", "completed"]
    );
}
