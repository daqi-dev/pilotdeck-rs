//! Source-derived repaired-call and empty gates. No Host or LargeFileRepair effects.
use crate::LargeFileRepairDecision;
use crate::output_recovery::{
    OutputRecovery, OutputRecoveryAction, OutputRecoveryInput, OutputRecoveryPlan,
    OutputRecoverySelection, RecoveryCapChange, RecoveryCapEvent, RepairedGateContinuation,
};
use crate::ports::{PortError, PortResult};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
fn undefined() -> &'static JsValue {
    static VALUE: JsValue = JsValue::Undefined;
    &VALUE
}
fn field<'a>(value: &'a JsValue, name: &str) -> &'a JsValue {
    value.get(name).unwrap_or(undefined())
}
fn source_error(value: &JsValue, message: &str) -> PortError {
    PortError::SourceException {
        message: message.into(),
        boundary: BoundarySnapshot::capture(value),
    }
}
fn invalid(message: &str) -> PortError {
    PortError::InvalidResponse {
        message: message.into(),
    }
}
fn property<'a>(value: &'a JsValue, name: &str) -> PortResult<&'a JsValue> {
    match value {
        JsValue::Null => Err(source_error(
            value,
            &format!("Cannot read properties of null (reading '{name}')"),
        )),
        JsValue::Undefined => Err(source_error(
            value,
            &format!("Cannot read properties of undefined (reading '{name}')"),
        )),
        _ => Ok(field(value, name)),
    }
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Null | JsValue::Undefined => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
fn is(value: &JsValue, label: &str) -> bool {
    value.as_string().is_some_and(|v| v.equals_str(label))
}
fn to_string(value: &JsValue) -> PortResult<JsString> {
    enum Part<'a> {
        Value(&'a JsValue, bool),
        Comma,
    }
    let mut stack = vec![Part::Value(value, false)];
    let mut units = Vec::new();
    while let Some(part) = stack.pop() {
        match part {
            Part::Comma => units.push(b',' as u16),
            Part::Value(v, array_item) => match v {
                JsValue::Null | JsValue::Undefined if array_item => {}
                JsValue::Null => units.extend("null".encode_utf16()),
                JsValue::Undefined => units.extend("undefined".encode_utf16()),
                JsValue::Bool(v) => units.extend(if *v { "true" } else { "false" }.encode_utf16()),
                JsValue::String(v) => units.extend_from_slice(v.units()),
                JsValue::Number(v) => {
                    let n = if v.is_nan() {
                        "NaN".to_owned()
                    } else if *v == f64::INFINITY {
                        "Infinity".to_owned()
                    } else if *v == f64::NEG_INFINITY {
                        "-Infinity".to_owned()
                    } else {
                        stringify_json(&JsValue::Number(*v))
                            .map_err(|_| invalid("Protocol number formatting failed."))?
                            .ok_or_else(|| invalid("Protocol number has no text."))?
                    };
                    units.extend(n.encode_utf16());
                }
                JsValue::Array(values) => {
                    for (i, v) in values.iter().enumerate().rev() {
                        stack.push(Part::Value(v, true));
                        if i > 0 {
                            stack.push(Part::Comma);
                        }
                    }
                }
                JsValue::Object(o) => {
                    if o.contains_key("toString") {
                        return Err(source_error(v, "Cannot convert object to primitive value"));
                    }
                    units.extend("[object Object]".encode_utf16());
                }
            },
        }
    }
    Ok(JsString::from_utf16(units))
}
fn empty_text(message: &BoundarySnapshot) -> PortResult<bool> {
    let content = property(message.value(), "content")?;
    let JsValue::Array(blocks) = content else {
        return Err(match content {
            JsValue::Null | JsValue::Undefined => property(content, "filter").unwrap_err(),
            _ => source_error(content, "message.content.filter is not a function"),
        });
    };
    let mut text_blocks = Vec::new();
    // Source filter examines every type before any .map/.join text conversion.
    for block in blocks {
        if is(property(block, "type")?, "text") {
            text_blocks.push(block);
        }
    }
    let mut texts = Vec::new();
    for block in text_blocks {
        texts.push(property(block, "text")?);
    }
    let mut length = 0usize;
    for (index, value) in texts.iter().enumerate() {
        if index > 0 {
            length += 1;
        }
        if !matches!(value, JsValue::Null | JsValue::Undefined) {
            length += to_string(value)?.len_utf16();
        }
    }
    Ok(length == 0)
}
fn plan(
    state: &OutputRecovery,
    input: OutputRecoveryInput<'_>,
    action: OutputRecoveryAction,
) -> OutputRecoveryPlan {
    OutputRecoveryPlan {
        action,
        original_request: input.original_request.clone(),
        routed_route: input.routed_route.clone(),
        assembled: input.assembled.clone(),
        output_recovery_count: state.output_recovery_count,
    }
}
pub(crate) fn select<'a>(
    state: &'a mut OutputRecovery,
    input: OutputRecoveryInput<'a>,
) -> PortResult<OutputRecoverySelection<'a>> {
    let _ = property(input.assembled.value(), "hasRepairedToolCalls")?;
    if input.has_repaired_tool_calls
        && ["length", "tool_call", "stop"]
            .iter()
            .any(|label| is(field(input.assembled.value(), "finishReason"), label))
    {
        return Ok(OutputRecoverySelection::NeedsLargeFileRepair(
            RepairedGateContinuation { state, input },
        ));
    }
    let error = property(input.assembled.value(), "error")?;
    if !truthy(error) && input.tool_calls.is_empty() && empty_text(input.assistant_message)? {
        let action = if state.output_recovery_count > 0 {
            state.consecutive_empty_count += 1;
            if state.consecutive_empty_count < 3 && state.output_recovery_count < 50 {
                state.output_recovery_count += 1;
                continuation(input, false)
            } else {
                OutputRecoveryAction::EmptyExhausted {
                    attempts: state.consecutive_empty_count,
                }
            }
        } else if !state.attempted_empty_retry {
            state.attempted_empty_retry = true;
            state.output_recovery_count += 1;
            continuation(input, true)
        } else {
            OutputRecoveryAction::EmptyExhausted { attempts: 2 }
        };
        return Ok(OutputRecoverySelection::Plan(plan(state, input, action)));
    }
    Ok(OutputRecoverySelection::Plan(plan(
        state,
        input,
        OutputRecoveryAction::FallThrough,
    )))
}
fn bump(current: Option<f64>, model: Option<f64>) -> Option<f64> {
    let current = current?;
    if !current.is_finite() || current <= 0.0 {
        return None;
    }
    if let Some(model) = model
        && (!model.is_finite() || model <= 0.0 || current >= model)
    {
        return None;
    }
    let doubled = current * 2.0;
    if !doubled.is_finite() || doubled <= current {
        return None;
    }
    Some(model.map_or(doubled, |cap| doubled.min(cap)))
}
fn empty_cap(current: Option<f64>, model: Option<f64>) -> Option<f64> {
    let doubled = current.unwrap_or(0.0) * 2.0;
    if doubled.is_nan() {
        return None;
    }
    let requested = doubled.max(4096.0);
    if !requested.is_finite() || requested <= 0.0 {
        return None;
    }
    let next = requested.floor();
    Some(match model {
        Some(cap) if cap.is_finite() && cap > 0.0 => next.min(cap.floor()),
        _ => next,
    })
}
fn continuation(input: OutputRecoveryInput<'_>, first: bool) -> OutputRecoveryAction {
    let previous = input.current_max_output_tokens;
    let cap_change = if is(field(input.assembled.value(), "finishReason"), "length") {
        empty_cap(previous, input.routed_model_max_output_tokens)
            .filter(|next| Some(*next) != previous)
            .map(|next| RecoveryCapChange {
                event: RecoveryCapEvent::EmptyOutputRecovery,
                previous,
                next,
            })
    } else {
        None
    };
    let (prompt, purpose) = if first {
        (
            "Your previous response was empty (thinking only, no visible text). Please provide your answer as visible text output.",
            "empty_response_retry",
        )
    } else {
        (
            "Output token limit hit. Resume directly - no apology, no recap of what you were doing. Pick up mid-sentence if that is where the cut happened.",
            "max_output_recovery",
        )
    };
    OutputRecoveryAction::Continue {
        cap_change,
        prompt: Some((prompt.into(), purpose.into())),
    }
}
pub(crate) fn resolve(
    token: RepairedGateContinuation<'_>,
    decision: Option<LargeFileRepairDecision>,
) -> PortResult<OutputRecoveryPlan> {
    let state = token.state;
    let input = token.input;
    if let Some(decision) = decision {
        return Ok(plan(
            state,
            input,
            OutputRecoveryAction::UseLargeFileDecision {
                decision,
                strip_current_assistant: false,
            },
        ));
    }
    if !state.attempted_output_retry {
        state.attempted_output_retry = true;
        if let Some(next) = bump(
            input.current_max_output_tokens,
            input.routed_model_max_output_tokens,
        ) {
            return Ok(plan(
                state,
                input,
                OutputRecoveryAction::Continue {
                    cap_change: Some(RecoveryCapChange {
                        event: RecoveryCapEvent::TokenCapAdjusted,
                        previous: input.current_max_output_tokens,
                        next,
                    }),
                    prompt: None,
                },
            ));
        }
    }
    let action = if state.output_recovery_count < 50 {
        state.output_recovery_count += 1;
        OutputRecoveryAction::Continue {cap_change:None,prompt:Some(("Output token limit hit. Resume directly - no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces.".into(),"max_output_recovery".into()))}
    } else {
        OutputRecoveryAction::RepairedExhausted
    };
    Ok(plan(state, input, action))
}
/// Root calls this only in the residual assembled-error branch, after the
/// earlier reasoning/JSON/context recovery owners have declined.
pub(crate) fn select_residual_max_output(
    state: &mut OutputRecovery,
    input: OutputRecoveryInput<'_>,
) -> PortResult<OutputRecoveryPlan> {
    let error = property(input.assembled.value(), "error")?;
    if !is(property(error, "code")?, "max_output_reached") {
        return Ok(plan(state, input, OutputRecoveryAction::FallThrough));
    }
    if !state.attempted_output_retry {
        // A failed bump consumes the same guard used by reactive/repaired retry.
        state.attempted_output_retry = true;
        if let Some(next) = bump(
            input.current_max_output_tokens,
            input.routed_model_max_output_tokens,
        ) {
            return Ok(plan(
                state,
                input,
                OutputRecoveryAction::StripErrorPairThenContinue {
                    cap_change: RecoveryCapChange {
                        event: RecoveryCapEvent::TokenCapAdjusted,
                        previous: input.current_max_output_tokens,
                        next,
                    },
                },
            ));
        }
    }
    let action = if state.output_recovery_count < 50 {
        state.output_recovery_count += 1;
        OutputRecoveryAction::Continue {
            cap_change: None,
            prompt: Some((
                "Output token limit hit. Resume directly - no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces.".into(),
                "max_output_recovery".into(),
            )),
        }
    } else {
        // The ordinary model-error owner, rather than repaired exhaustion,
        // classifies the genuine provider error and dispatches StopFailure.
        OutputRecoveryAction::FallThrough
    };
    Ok(plan(state, input, action))
}
/// Root guarantees that valid visible text is already durable and that there
/// are no tools/error. This phase does not re-read or discard that assistant.
pub(crate) fn select_visible_length(
    state: &mut OutputRecovery,
    input: OutputRecoveryInput<'_>,
) -> PortResult<OutputRecoveryPlan> {
    if !is(property(input.assembled.value(), "finishReason")?, "length") {
        return Ok(plan(state, input, OutputRecoveryAction::FallThrough));
    }
    state.consecutive_empty_count = 0;
    let action = if state.output_recovery_count < 50 {
        state.output_recovery_count += 1;
        OutputRecoveryAction::Continue {
            cap_change: None,
            prompt: Some((
                "Output token limit hit. Resume directly - no apology, no recap of what you were doing. Pick up mid-sentence if that is where the cut happened.".into(),
                "max_output_recovery".into(),
            )),
        }
    } else {
        OutputRecoveryAction::VisibleLengthExhausted
    };
    Ok(plan(state, input, action))
}
pub(crate) fn terminal_error(plan: &OutputRecoveryPlan) -> PortResult<BoundarySnapshot> {
    if plan.action != OutputRecoveryAction::RepairedExhausted {
        return Err(invalid(
            "Output terminal error requires repaired exhaustion.",
        ));
    }
    let mut e = JsObject::new();
    e.insert("code".into(), "agent_model_error".into());
    e.insert("message".into(),"Recovered tool call still looked repaired/truncated after max-output recovery was exhausted.".into());
    e.insert("details".into(), JsValue::Undefined);
    Ok(BoundarySnapshot::capture(&JsValue::Object(e)))
}
fn descriptor(key: &str, params: Option<JsObject>) -> JsValue {
    let mut o = JsObject::new();
    o.insert("key".into(), key.into());
    if let Some(params) = params {
        o.insert("params".into(), JsValue::Object(params));
    }
    JsValue::Object(o)
}
fn status(
    event: &str,
    text: JsValue,
    code: JsValue,
    hint: JsValue,
    mi: JsValue,
    hi: JsValue,
    extra: JsObject,
) -> BoundarySnapshot {
    let mut detail = JsObject::new();
    detail.insert("message".into(), text.clone());
    detail.insert("messageI18n".into(), mi);
    if !matches!(code, JsValue::Undefined) {
        detail.insert("code".into(), code);
    }
    detail.insert("severity".into(), "error".into());
    detail.insert("visible".into(), JsValue::Bool(true));
    detail.insert("userHint".into(), hint);
    detail.insert("userHintI18n".into(), hi);
    detail.insert("scope".into(), "turn".into());
    detail.insert("source".into(), "agent".into());
    for (k, v) in extra.entries() {
        if !matches!(v, JsValue::Undefined) {
            detail.insert(k.clone(), v.clone());
        }
    }
    let mut out = JsObject::new();
    out.insert("event".into(), event.into());
    out.insert("kind".into(), "error".into());
    out.insert("text".into(), text);
    out.insert("detail".into(), JsValue::Object(detail));
    BoundarySnapshot::capture(&JsValue::Object(out))
}
pub(crate) fn terminal_status(
    plan: &OutputRecoveryPlan,
    error: Option<&BoundarySnapshot>,
) -> PortResult<BoundarySnapshot> {
    match plan.action {
        OutputRecoveryAction::VisibleLengthExhausted => {
            let text = "Output token recovery was exhausted, so the visible response may be incomplete. Increase max output tokens or split the task into smaller steps and try again.";
            let mut detail = JsObject::new();
            detail.insert("message".into(), text.into());
            detail.insert(
                "messageI18n".into(),
                descriptor("chat:agentStatus.maxOutputRecoveryExhausted.message", None),
            );
            detail.insert("code".into(), "max_output_recovery_exhausted".into());
            detail.insert("severity".into(), "warning".into());
            detail.insert("visible".into(), JsValue::Bool(true));
            detail.insert("userHint".into(), "Increase max output tokens in Settings → Model Provider, or ask the agent to split the answer into smaller parts.".into());
            detail.insert(
                "userHintI18n".into(),
                descriptor("chat:agentStatus.maxOutputRecoveryExhausted.hint", None),
            );
            detail.insert("scope".into(), "turn".into());
            detail.insert("source".into(), "agent".into());
            detail.insert(
                "attempts".into(),
                JsValue::Number(f64::from(plan.output_recovery_count)),
            );
            let mut out = JsObject::new();
            out.insert("event".into(), "max_output_recovery_exhausted".into());
            out.insert("kind".into(), "error".into());
            out.insert("text".into(), text.into());
            out.insert("detail".into(), JsValue::Object(detail));
            Ok(BoundarySnapshot::capture(&JsValue::Object(out)))
        }
        OutputRecoveryAction::RepairedExhausted => {
            let error = error.ok_or_else(|| {
                invalid("Repaired status requires the already constructed error.")
            })?;
            let message = property(error.value(), "message")?;
            let text = if truthy(message) {
                message.clone()
            } else {
                "Tool-call recovery was exhausted, so this turn has stopped.".into()
            };
            let mut params = JsObject::new();
            params.insert("message".into(), text.clone());
            let code = property(error.value(), "code")?.clone();
            let hint = property(error.value(), "userHint")?;
            let hint = if matches!(hint, JsValue::Null | JsValue::Undefined) {
                "Retry with a shorter prompt, ask the agent to split large tool inputs into smaller steps, or switch to a model with stronger tool-calling support in Settings → Model Provider.".into()
            } else {
                hint.clone()
            };
            let mut extra = JsObject::new();
            extra.insert(
                "attempts".into(),
                JsValue::Number(f64::from(plan.output_recovery_count)),
            );
            extra.insert("reason".into(), "repaired_truncated_tool_calls".into());
            Ok(status(
                "tool_call_recovery_exhausted",
                text,
                code,
                hint,
                descriptor(
                    "chat:agentStatus.toolCallRecoveryExhausted.message",
                    Some(params),
                ),
                descriptor("chat:agentStatus.toolCallRecoveryExhausted.hint", None),
                extra,
            ))
        }
        OutputRecoveryAction::EmptyExhausted { attempts } => {
            let mut extra = JsObject::new();
            extra.insert(
                "provider".into(),
                property(plan.original_request.value(), "provider")?.clone(),
            );
            extra.insert(
                "model".into(),
                property(plan.original_request.value(), "model")?.clone(),
            );
            extra.insert("attempts".into(), JsValue::Number(f64::from(attempts)));
            Ok(status("model_empty_response_exhausted","The model returned empty content repeatedly, so this turn has stopped. Try again later or increase max output tokens.".into(),"model_empty_response_exhausted".into(),"Increase max output tokens in Settings → Model Provider, retry with a shorter prompt, or check whether this provider/model supports the requested output format.".into(),descriptor("chat:agentStatus.emptyResponse.message",None),descriptor("chat:agentStatus.emptyResponse.hint",None),extra))
        }
        _ => Err(invalid("Output status requested outside exhaustion phase.")),
    }
}
