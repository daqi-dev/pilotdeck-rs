//! #1371 controlled AgentLoop: the product-side entry for the five lifecycle
//! cells of the #1242 completion-ordering matrix.
//!
//! Mirrors the substituted loop in `scripts/a5-19/six_cells.mts:236-255`:
//!
//! 1. yield the phase prefix (`model_request_started`, and for `tool` also
//!    `tool_calls_detected` carrying the `controlled_wait` call),
//! 2. `loop.wait`: hold until the collector releases the outcome,
//! 3. `unknown` → a `result_unknown` terminal (never a fabricated success),
//!    `known` → `turn_completed` plus a success terminal,
//! 4. for `known-final-before-timeout-pending`, buffer the completed event
//!    behind a held generator return, exactly like TS `TurnRunner.ts:393-420`
//!    which only emits the buffered completed result after the loop returns.
//!
//! The loop deliberately keeps waiting across a gateway timeout: that is the
//! stimulus ("loop waiting × timeout first"). The wait is still bounded so a
//! lost collector yields a failed turn instead of a hung process.
use crate::loop_stimulus::{LoopStimulus, Order, Phase, Plan};
use crate::server::{ExecutionClock, ExecutionContext, ExecutionTerminal};
use pilotdeck_agent::driver::BudgetLedger;
use pilotdeck_agent::ports::{PortError, PortResult};
use pilotdeck_agent::turn::{TurnInput, TurnResult, TurnResultKind, TurnStopReason};
use pilotdeck_agent::turn_wire;
use pilotdeck_protocol::model::Usage;
use pilotdeck_protocol::module::ModuleOutcome;
use pilotdeck_protocol::{JsObject, JsValue};

/// Run the controlled loop for one admitted turn. Returns the terminal the
/// factory would have produced.
pub(crate) fn run(
    stimulus: &LoopStimulus,
    plan: &Plan,
    input: &TurnInput,
    context: &mut ExecutionContext,
) -> PortResult<ExecutionTerminal> {
    stimulus.publish(
        "loop",
        "input",
        &plan.id,
        // Never reuse `phase`/`key` here: those name the receipt's own fields.
        Some(&fields([
            ("planPhase", JsValue::from(plan.phase.as_str())),
            ("planOrder", JsValue::from(plan.order.as_str())),
        ])),
    );
    emit(
        context,
        input,
        "model_request_started",
        fields([
            ("provider", JsValue::from("fixture")),
            ("model", JsValue::from("controlled")),
        ]),
    )?;
    if plan.phase == Phase::Tool {
        let call = fields([
            ("id", JsValue::from("tool-1")),
            ("name", JsValue::from("controlled_wait")),
            ("input", JsValue::Object(JsObject::new())),
        ]);
        emit(
            context,
            input,
            "tool_calls_detected",
            fields([("calls", JsValue::Array(vec![JsValue::Object(call)]))]),
        )?;
    }
    stimulus.publish(
        "loop",
        "wait",
        &plan.id,
        Some(&fields([("planPhase", JsValue::from(plan.phase.as_str()))])),
    );
    let Some(release) = stimulus.await_release("outcome") else {
        stimulus.publish("loop", "bound", "outcome", None);
        return Err(PortError::InvalidResponse {
            message: "#1371 controlled loop outcome bound expired without a release.".into(),
        });
    };
    if release.outcome.as_deref() == Some("known") {
        run_known(stimulus, plan, input, context)
    } else {
        stimulus.publish(
            "loop",
            "final",
            "result_unknown",
            Some(&fields([("code", JsValue::from("DEADLINE_EXCEEDED"))])),
        );
        Ok(ExecutionTerminal {
            outcome: ModuleOutcome::ResultUnknown,
            payload: JsObject::new(),
            error: Some(fields([
                ("code", JsValue::from("DEADLINE_EXCEEDED")),
                (
                    "message",
                    JsValue::from(
                        "Controlled AgentLoop result is unknown; host reconciliation is required.",
                    ),
                ),
            ])),
        })
    }
}

fn run_known(
    stimulus: &LoopStimulus,
    plan: &Plan,
    input: &TurnInput,
    context: &mut ExecutionContext,
) -> PortResult<ExecutionTerminal> {
    if plan.order == Order::KnownFinalBeforeTimeoutPending {
        // The generator yielded completed but its return is still held. TS
        // buffers that event; nothing reaches the gateway until the return
        // releases, so the client cannot observe completed before timeout.
        // The `final/completed` receipt therefore follows the release: it
        // marks the point where the buffered result is actually delivered.
        stimulus.publish("loop", "return", "block", None);
        if stimulus.await_release("return").is_none() {
            stimulus.publish("loop", "bound", "return", None);
            return Err(PortError::InvalidResponse {
                message: "#1371 controlled loop return bound expired without a release.".into(),
            });
        }
        stimulus.publish("loop", "return", "release", None);
        stimulus.publish("loop", "final", "completed", None);
        let result = success_result(input);
        let payload = terminal_payload(input, &result);
        // Already-accepted provider data keeps its bounded delivery budget.
        if emit_accepted(context, input, "turn_completed", result_event(&result)).is_err() {
            stimulus.publish("loop", "final", "completed-suppressed", None);
            return Ok(ExecutionTerminal {
                outcome: ModuleOutcome::Cancelled,
                payload: JsObject::new(),
                error: None,
            });
        }
        return Ok(ExecutionTerminal {
            outcome: ModuleOutcome::Completed,
            payload,
            error: None,
        });
    }
    let result = success_result(input);
    stimulus.publish("loop", "final", "completed", None);
    if emit(context, input, "turn_completed", result_event(&result)).is_err() {
        // The turn was already cancelled (timeout/abort) before the release.
        stimulus.publish("loop", "final", "completed-suppressed", None);
        return Ok(ExecutionTerminal {
            outcome: ModuleOutcome::Cancelled,
            payload: JsObject::new(),
            error: None,
        });
    }
    Ok(ExecutionTerminal {
        outcome: ModuleOutcome::Completed,
        payload: terminal_payload(input, &result),
        error: None,
    })
}

fn success_result(input: &TurnInput) -> TurnResult {
    let now = ExecutionClock::now_iso(&crate::system_clock::SystemExecutionClock);
    TurnResult {
        kind: TurnResultKind::Success,
        stop_reason: TurnStopReason::Completed,
        final_message: None,
        usage: Usage::default(),
        permission_denials: Vec::new(),
        turns: 1.0,
        started_at: now.clone(),
        completed_at: now,
        structured_output: None,
        errors: Vec::new(),
        budget: BudgetLedger::new(input.max_budget_usd, input.task_budget_usd, 0.0),
    }
}

fn result_event(result: &TurnResult) -> JsObject {
    let started = result.started_at.to_utf8().unwrap_or_default();
    let completed = result.completed_at.to_utf8().unwrap_or_default();
    let mut event = JsObject::new();
    event.insert(
        "result".into(),
        JsValue::Object(fields([
            ("type", JsValue::from("success")),
            ("stopReason", JsValue::from("completed")),
            ("usage", JsValue::Object(JsObject::new())),
            ("permissionDenials", JsValue::Array(Vec::new())),
            ("turns", JsValue::Number(1.0)),
            ("startedAt", JsValue::from(started.as_str())),
            ("completedAt", JsValue::from(completed.as_str())),
        ])),
    );
    event
}

fn terminal_payload(input: &TurnInput, result: &TurnResult) -> JsObject {
    let mut payload = JsObject::new();
    payload.insert(
        "result".into(),
        turn_wire::result(input, result).value().clone(),
    );
    payload.insert(
        "messages".into(),
        JsValue::Array(
            input
                .messages
                .iter()
                .map(|message| message.value().clone())
                .collect(),
        ),
    );
    payload
}

fn emit(
    context: &mut ExecutionContext,
    input: &TurnInput,
    kind: &str,
    extra: JsObject,
) -> PortResult<()> {
    context.emit(name(kind), event_payload(input, kind, extra))
}

fn emit_accepted(
    context: &mut ExecutionContext,
    input: &TurnInput,
    kind: &str,
    extra: JsObject,
) -> PortResult<()> {
    context.emit_accepted(name(kind), event_payload(input, kind, extra))
}

fn name(kind: &str) -> pilotdeck_protocol::JsString {
    let mut name = String::from("agent.");
    name.push_str(kind);
    name.into()
}

fn event_payload(input: &TurnInput, kind: &str, extra: JsObject) -> JsObject {
    let mut payload = fields([
        ("type", JsValue::from(kind)),
        ("sessionId", JsValue::String(input.session_id.clone())),
        ("turnId", JsValue::String(input.turn_id.clone())),
    ]);
    for (name, value) in extra.entries() {
        payload.insert(name.clone(), value.clone());
    }
    payload
}

fn fields<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut object = JsObject::new();
    for (name, value) in entries {
        object.insert(name.into(), value);
    }
    object
}
