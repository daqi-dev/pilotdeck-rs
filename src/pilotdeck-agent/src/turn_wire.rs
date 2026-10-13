//! Agent public event/result projection. Module envelopes and delivery are owned
//! by transport; these immutable boundaries carry no executable host authority.
use crate::ports::PermissionMode;
use crate::turn::{
    TurnContinueReason, TurnEvent, TurnInput, TurnResult, TurnResultKind, TurnStopReason,
};
use pilotdeck_protocol::model::{BoundarySnapshot, Usage};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

fn put(o: &mut JsObject, key: &str, value: JsValue) {
    o.insert(key.into(), value);
}
fn text(o: &mut JsObject, key: &str, value: &JsString) {
    put(o, key, JsValue::String(value.clone()));
}
pub fn usage(value: &Usage) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in [
        ("inputTokens", value.input_tokens),
        ("outputTokens", value.output_tokens),
        ("cacheReadTokens", value.cache_read_tokens),
        ("cacheWriteTokens", value.cache_write_tokens),
        ("totalTokens", value.total_tokens),
        ("nativeCost", value.native_cost),
    ] {
        if let Some(value) = value {
            put(&mut out, key, JsValue::Number(value));
        }
    }
    JsValue::Object(out)
}
pub fn result(input: &TurnInput, result: &TurnResult) -> BoundarySnapshot {
    let mut out = JsObject::new();
    put(
        &mut out,
        "type",
        JsValue::from(match result.kind {
            TurnResultKind::Success => "success",
            TurnResultKind::Error => "error",
            TurnResultKind::Aborted => "aborted",
            TurnResultKind::MaxTurns => "max_turns",
        }),
    );
    put(
        &mut out,
        "stopReason",
        JsValue::from(match result.stop_reason {
            TurnStopReason::Completed => "completed",
            TurnStopReason::MaxTurns => "max_turns",
            TurnStopReason::MaxBudget => "max_budget",
            TurnStopReason::TaskBudget => "task_budget",
            TurnStopReason::AbortedStreaming => "aborted_streaming",
            TurnStopReason::AbortedTools => "aborted_tools",
            TurnStopReason::ModelError => "model_error",
            TurnStopReason::PromptTooLong => "prompt_too_long",
            TurnStopReason::ToolError => "tool_error",
            TurnStopReason::UnsupportedRecovery => "unsupported_recovery",
        }),
    );
    put(&mut out, "usage", usage(&result.usage));
    put(
        &mut out,
        "permissionDenials",
        JsValue::Array(
            result
                .permission_denials
                .iter()
                .map(|denial| {
                    let mut out = JsObject::new();
                    text(&mut out, "toolName", &denial.tool_name);
                    text(&mut out, "toolCallId", &denial.tool_call_id);
                    if let Some(code) = &denial.error_code {
                        text(&mut out, "errorCode", code);
                    }
                    JsValue::Object(out)
                })
                .collect(),
        ),
    );
    put(&mut out, "turns", JsValue::Number(result.turns));
    text(&mut out, "startedAt", &result.started_at);
    if let Some(message) = &result.final_message {
        put(&mut out, "finalMessage", message.boundary.value().clone());
    }
    if let Some(value) = &result.structured_output {
        put(&mut out, "structuredOutput", value.value().clone());
    }
    if !result.errors.is_empty() {
        put(
            &mut out,
            "errors",
            JsValue::Array(result.errors.iter().map(|v| v.value().clone()).collect()),
        );
    }
    text(&mut out, "sessionId", &input.session_id);
    text(&mut out, "turnId", &input.turn_id);
    text(&mut out, "completedAt", &result.completed_at);
    if input.max_budget_usd.is_some() || input.task_budget_usd.is_some() {
        let mut budget = JsObject::new();
        put(
            &mut budget,
            "turnSpentUsd",
            JsValue::Number(result.budget.turn_spent_usd),
        );
        if let Some(limit) = input.task_budget_usd {
            put(&mut budget, "taskBudgetUsd", JsValue::Number(limit));
            put(
                &mut budget,
                "taskSpentUsd",
                JsValue::Number(result.budget.task_spent_usd),
            );
        }
        put(&mut out, "budget", JsValue::Object(budget));
    }
    BoundarySnapshot::capture(&JsValue::Object(out))
}

pub fn event(input: &TurnInput, event: &TurnEvent) -> BoundarySnapshot {
    let mut out = JsObject::new();
    let kind = match event {
        TurnEvent::ContextBudget { .. } => "context_budget",
        TurnEvent::RecoveryCap { change, .. } => match change.event {
            crate::output_recovery::RecoveryCapEvent::TokenCapAdjusted => "token_cap_adjusted",
            crate::output_recovery::RecoveryCapEvent::EmptyOutputRecovery => {
                "empty_output_recovery"
            }
        },
        TurnEvent::TokenCapAdjusted { .. } => "token_cap_adjusted",
        TurnEvent::ModelRequestStarted { .. } => "model_request_started",
        TurnEvent::Model { .. } => "model_event",
        TurnEvent::AssistantMessage { .. } => "assistant_message",
        TurnEvent::ToolCallsDetected { .. } => "tool_calls_detected",
        TurnEvent::ToolResult { .. } => "tool_result",
        TurnEvent::ToolResultsProjected { .. } => "tool_results_projected",
        TurnEvent::ModeChangeRequested { .. } => "mode_change_requested",
        TurnEvent::SteerApplied { .. } => "steer_applied",
        TurnEvent::Continued { .. } => "turn_continued",
        TurnEvent::StopRequested => "stop_requested",
        TurnEvent::StopFailure { .. } | TurnEvent::StopFailureBoundary { .. } => "stop_failure",
        TurnEvent::AgentStatus { .. } | TurnEvent::AgentStatusText { .. } => "agent_status",
        TurnEvent::Failed { .. } => "turn_failed",
        TurnEvent::Completed { .. } => "turn_completed",
    };
    put(&mut out, "type", JsValue::from(kind));
    text(&mut out, "sessionId", &input.session_id);
    text(&mut out, "turnId", &input.turn_id);
    match event {
        TurnEvent::RecoveryCap {
            route,
            change,
            finish_reason,
        } => {
            text(&mut out, "provider", &route.provider);
            text(&mut out, "model", &route.model);
            match change.event {
                crate::output_recovery::RecoveryCapEvent::TokenCapAdjusted => {
                    put(&mut out, "cap", "output".into());
                    if let Some(previous) = change.previous {
                        put(&mut out, "previous", JsValue::Number(previous));
                    }
                    put(&mut out, "next", JsValue::Number(change.next));
                    put(&mut out, "reason", "max-output-retry-bump".into());
                }
                crate::output_recovery::RecoveryCapEvent::EmptyOutputRecovery => {
                    if let Some(reason) = finish_reason {
                        put(&mut out, "finishReason", reason.value().clone());
                    }
                    if let Some(previous) = change.previous {
                        put(
                            &mut out,
                            "previousMaxOutputTokens",
                            JsValue::Number(previous),
                        );
                    }
                    put(
                        &mut out,
                        "nextMaxOutputTokens",
                        JsValue::Number(change.next),
                    );
                }
            }
        }
        TurnEvent::TokenCapAdjusted {
            route,
            cap,
            previous,
            next,
            reason,
        } => {
            // events.ts:81-91; `previous` is omitted when Source would yield
            // `undefined` (JSON.stringify drops it), as for the residual bump.
            text(&mut out, "provider", &route.provider);
            text(&mut out, "model", &route.model);
            put(
                &mut out,
                "cap",
                match cap {
                    crate::turn::TokenCapKind::Output => "output",
                    crate::turn::TokenCapKind::Context => "context",
                }
                .into(),
            );
            if let Some(previous) = previous {
                put(&mut out, "previous", JsValue::Number(*previous));
            }
            put(&mut out, "next", JsValue::Number(*next));
            text(&mut out, "reason", reason);
        }
        TurnEvent::ContextBudget { snapshot } => {
            put(&mut out, "snapshot", snapshot.value().clone());
        }
        TurnEvent::Failed { error } => {
            put(&mut out, "error", error.value().clone());
        }
        TurnEvent::StopFailureBoundary { error } => {
            put(&mut out, "error", error.value().clone());
        }
        TurnEvent::StopFailure { error } => {
            text(&mut out, "error", error);
        }
        TurnEvent::AgentStatus { event, detail } => {
            text(&mut out, "event", event);
            put(&mut out, "detail", detail.value().clone());
        }
        TurnEvent::AgentStatusText {
            event,
            text: status_text,
            detail,
        } => {
            text(&mut out, "event", event);
            text(&mut out, "text", status_text);
            put(&mut out, "detail", detail.value().clone());
        }
        TurnEvent::ModelRequestStarted { route } => {
            text(&mut out, "model", &route.model);
            text(&mut out, "provider", &route.provider);
        }
        TurnEvent::Model { live } => {
            put(&mut out, "event", live.boundary.value().clone());
            if let Some(id) = &live.block_id
                && !id.is_empty()
            {
                text(&mut out, "blockId", id);
            }
        }
        TurnEvent::AssistantMessage { message } | TurnEvent::ToolResultsProjected { message } => {
            put(&mut out, "message", message.boundary.value().clone());
        }
        TurnEvent::ToolCallsDetected { boundaries, .. } => {
            put(
                &mut out,
                "calls",
                JsValue::Array(boundaries.iter().map(|v| v.value().clone()).collect()),
            );
        }
        TurnEvent::ToolResult { result } => {
            put(&mut out, "result", result.boundary.value().clone());
        }
        TurnEvent::ModeChangeRequested { mode } => {
            put(
                &mut out,
                "mode",
                JsValue::from(match mode {
                    PermissionMode::Default => "default",
                    PermissionMode::Plan => "plan",
                    PermissionMode::BypassPermissions => "bypassPermissions",
                }),
            );
        }
        TurnEvent::SteerApplied { item_id, message } => {
            text(&mut out, "itemId", item_id);
            put(&mut out, "message", message.boundary.value().clone());
        }
        TurnEvent::Continued { reason } => {
            put(
                &mut out,
                "reason",
                JsValue::from(match reason {
                    TurnContinueReason::ToolResults => "next_turn",
                    TurnContinueReason::UserSteer => "user_steer",
                    TurnContinueReason::AutoCompact => "auto_compact",
                    TurnContinueReason::ModelError => "model_error",
                }),
            );
        }
        TurnEvent::StopRequested => {}
        TurnEvent::Completed { result: value } => {
            put(&mut out, "result", result(input, value).value().clone());
        }
    }
    BoundarySnapshot::capture(&JsValue::Object(out))
}
