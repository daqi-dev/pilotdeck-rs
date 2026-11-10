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
use crate::server::{ExecuteRequest, ExecutionClock, ExecutionContext, ExecutionTerminal};
use pilotdeck_agent::driver::{BudgetLedger, InvocationDisposition};
use pilotdeck_agent::ports::{
    CallControl, ExecutionHints, ExecutionIdentity, HostCall, HostPort, HostReply, ModelHints,
    PortError, PortResult, PreparedRequestHandle, Route,
};
use pilotdeck_agent::source_invocation::run_source_model_invocation;
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
    provider: Option<&mut dyn FnMut() -> PortResult<()>>,
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
    if plan.require_provider {
        let consume =
            provider.ok_or_else(|| invalid("Controlled model invocation was not composed."))?;
        consume()?;
        stimulus.publish("model", "completed", &plan.id, None);
    }
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

/// Consume the actual model iterator for this operation using the same host
/// prepare, pull/eager, error reducer and close operations as an ordinary turn.
/// The fixture owns the visible lifecycle events; a missing, empty or failed
/// provider response can never advance its outcome barrier.
pub(crate) fn consume_provider(
    host: &mut dyn HostPort,
    control: &CallControl,
    input: &TurnInput,
    route: &Route,
    identity: &ExecutionIdentity,
    request: &ExecuteRequest,
) -> PortResult<()> {
    let request_value = JsValue::Object(fields([
        ("provider", JsValue::String(route.provider.clone())),
        ("model", JsValue::String(route.model.clone())),
        (
            "messages",
            JsValue::Array(
                input
                    .messages
                    .iter()
                    .map(|message| message.value().clone())
                    .collect(),
            ),
        ),
        ("stream", JsValue::Bool(true)),
        ("maxOutputTokens", JsValue::Number(32.0)),
    ]));
    let canonical = crate::model_admission::request(&request_value)?;
    let hints = ModelHints {
        session_id: identity.session_id.clone(),
        turn_id: identity.turn_id.clone(),
        execution: ExecutionHints {
            run_id: identity.run_id.clone(),
            operation_id: identity.operation_id.clone(),
            idempotency_key: identity.idempotency_key.clone(),
            operation_deadline: request
                .boundary
                .get("operationDeadline")
                .and_then(JsValue::as_string)
                .cloned(),
        },
        workspace_id: None,
        storage_config_version: None,
        metadata: None,
        model_override: input
            .model_override
            .as_ref()
            .map(|model| model.route.clone()),
    };
    let id = uuid::Uuid::new_v4().to_string();
    let HostReply::ModelPrepared(prepared) = host.call(
        control,
        HostCall::ModelPrepare {
            handle: PreparedRequestHandle(id.clone().into()),
            request: Box::new(canonical),
            hints: hints.clone(),
        },
    )?
    else {
        return Err(invalid(
            "Controlled model preparation returned another operation.",
        ));
    };
    let cleanup = CallControl {
        cancellation: control.cancellation.cleanup_scope(),
        // The existing invocation owner gives close its own bounded budget.
        deadline: None,
    };
    let mut text = false;
    let mut ended = false;
    let invocation = run_source_model_invocation(
        host,
        control,
        &cleanup,
        &prepared,
        &hints,
        id.into(),
        &mut |event| {
            let value = event.boundary.value();
            let kind = value.get("type").and_then(JsValue::as_string);
            ended |= kind.is_some_and(|kind| kind.equals_str("message_end"));
            text |= kind.is_some_and(|kind| kind.equals_str("text_delta"))
                && value
                    .get("text")
                    .and_then(JsValue::as_string)
                    .is_some_and(|delta| !delta.units().is_empty());
            Ok(())
        },
    );
    match invocation.disposition {
        InvocationDisposition::PortFailure(error) => Err(*error),
        InvocationDisposition::Cancelled(cause) => Err(PortError::Cancelled { cause }),
        InvocationDisposition::Assembled
            if invocation.stream_completed && invocation.assembled.is_ok() && text && ended =>
        {
            Ok(())
        }
        _ => Err(invalid(
            "Controlled model invocation did not consume a complete nonempty response.",
        )),
    }
}

fn invalid(message: &'static str) -> PortError {
    PortError::InvalidResponse {
        message: message.into(),
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
        // The completion receipt follows the delivery it describes: a
        // collector reading `final/completed` must never see it for a turn
        // whose completion the transport then suppressed.
        stimulus.publish("loop", "final", "completed", None);
        return Ok(ExecutionTerminal {
            outcome: ModuleOutcome::Completed,
            payload,
            error: None,
        });
    }
    let result = success_result(input);
    if emit(context, input, "turn_completed", result_event(&result)).is_err() {
        // The turn was already cancelled (timeout/abort) before the release.
        stimulus.publish("loop", "final", "completed-suppressed", None);
        return Ok(ExecutionTerminal {
            outcome: ModuleOutcome::Cancelled,
            payload: JsObject::new(),
            error: None,
        });
    }
    stimulus.publish("loop", "final", "completed", None);
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
