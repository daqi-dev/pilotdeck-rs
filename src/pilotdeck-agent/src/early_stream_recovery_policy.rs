//! Source-derived two-gate policy. Late projections never execute at selection.
use crate::early_stream_recovery::{
    EarlyStreamAction, EarlyStreamInput, EarlyStreamKind, EarlyStreamPlan, EarlyStreamRecovery,
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
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
fn is(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|v| v.equals_str(text))
}
fn exception(value: &JsValue, message: &str) -> PortError {
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
        JsValue::Undefined => Err(exception(
            value,
            &format!("Cannot read properties of undefined (reading '{name}')"),
        )),
        JsValue::Null => Err(exception(
            value,
            &format!("Cannot read properties of null (reading '{name}')"),
        )),
        _ => Ok(field(value, name)),
    }
}
fn interruption(plan: &EarlyStreamPlan) -> &JsValue {
    field(field(plan.assembled.value(), "error"), "streamInterruption")
}
pub(crate) fn select(
    state: &mut EarlyStreamRecovery,
    input: EarlyStreamInput<'_>,
) -> PortResult<EarlyStreamPlan> {
    let error = property(input.assembled.value(), "error")?;
    let stream = field(error, "streamInterruption");
    let (kind, action, structured) = if truthy(stream) {
        if state.interruption_count < 2 {
            state.interruption_count += 1;
            let structured =
                !input.tool_calls.is_empty() || is(field(stream, "phase"), "tool_call");
            (
                Some(EarlyStreamKind::Interruption),
                EarlyStreamAction::Retry {
                    clear_final_message: structured,
                    retain_partial_text: is(field(stream, "phase"), "text") && !structured,
                },
                structured,
            )
        } else {
            (
                Some(EarlyStreamKind::Interruption),
                EarlyStreamAction::Exhausted,
                false,
            )
        }
    } else {
        state.interruption_count = 0;
        if !truthy(error)
            && input.has_message_end
            && is(field(input.assembled.value(), "finishReason"), "unknown")
        {
            if state.unknown_count < 2 {
                state.unknown_count += 1;
                (
                    Some(EarlyStreamKind::UnknownFinish),
                    EarlyStreamAction::Retry {
                        clear_final_message: false,
                        retain_partial_text: input.tool_calls.is_empty(),
                    },
                    false,
                )
            } else {
                (
                    Some(EarlyStreamKind::UnknownFinish),
                    EarlyStreamAction::Exhausted,
                    false,
                )
            }
        } else {
            state.unknown_count = 0;
            (None, EarlyStreamAction::FallThrough, false)
        }
    };
    Ok(EarlyStreamPlan {
        action,
        kind,
        assembled: input.assembled.clone(),
        assistant_message: input.assistant_message.clone(),
        tool_calls: input.tool_calls.to_vec(),
        structured_interruption: structured,
    })
}

/// Ordinary dense array join and primitive conversion preserve raw UTF-16.
fn string(value: &JsValue) -> PortResult<JsString> {
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
                JsValue::Undefined | JsValue::Null if array_item => {}
                JsValue::Undefined => units.extend("undefined".encode_utf16()),
                JsValue::Null => units.extend("null".encode_utf16()),
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
                            .map_err(|_| invalid("Protocol number formatter failed."))?
                            .ok_or_else(|| invalid("Protocol number formatter returned no text."))?
                    };
                    units.extend(n.encode_utf16());
                }
                JsValue::Array(values) => {
                    for (index, item) in values.iter().enumerate().rev() {
                        stack.push(Part::Value(item, true));
                        if index > 0 {
                            stack.push(Part::Comma);
                        }
                    }
                }
                JsValue::Object(object) => {
                    if object.contains_key("toString") {
                        return Err(exception(v, "Cannot convert object to primitive value"));
                    }
                    units.extend("[object Object]".encode_utf16());
                }
            },
        }
    }
    Ok(JsString::from_utf16(units))
}
fn concat(parts: &[&JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|s| s.units().iter().copied())
            .collect(),
    )
}
fn whitespace(v: u16) -> bool {
    matches!(v, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028..=0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(v: &JsString) -> JsString {
    let units = v.units();
    let first = units
        .iter()
        .position(|v| !whitespace(*v))
        .unwrap_or(units.len());
    let last = units
        .iter()
        .rposition(|v| !whitespace(*v))
        .map_or(first, |i| i + 1);
    JsString::from_utf16(units[first..last].to_vec())
}
fn without_thinking(message: &BoundarySnapshot) -> PortResult<BoundarySnapshot> {
    let value = message.value();
    let content = property(value, "content")?;
    let mut kept = Vec::new();
    if let JsValue::Array(blocks) = content {
        for block in blocks {
            if !is(property(block, "type")?, "thinking") {
                kept.push(block.clone());
            }
        }
    }
    let mut result = value.as_object().cloned().unwrap_or_default();
    result.insert("content".into(), JsValue::Array(kept));
    Ok(BoundarySnapshot::capture(&JsValue::Object(result)))
}
fn nonempty_text(message: BoundarySnapshot) -> PortResult<Option<BoundarySnapshot>> {
    let JsValue::Array(content) = field(message.value(), "content") else {
        return Err(invalid(
            "Internal without-thinking projection has no content array.",
        ));
    };
    let mut join = Vec::new();
    let mut first = true;
    for block in content {
        if is(property(block, "type")?, "text") {
            if !first {
                join.push(b'\n' as u16);
            }
            first = false;
            let text = property(block, "text")?;
            if !matches!(text, JsValue::Undefined | JsValue::Null) {
                join.extend_from_slice(string(text)?.units());
            }
        }
    }
    Ok((!trim(&JsString::from_utf16(join)).is_empty()).then_some(message))
}
pub(crate) fn partial_text_message(plan: &EarlyStreamPlan) -> PortResult<Option<BoundarySnapshot>> {
    match plan.action {
        EarlyStreamAction::Retry { .. } if plan.kind == Some(EarlyStreamKind::UnknownFinish) => {
            // Source constructs this even when the structured-call gate forbids
            // retention; only textFromMessage is short-circuited by calls.length.
            let partial = without_thinking(&plan.assistant_message)?;
            if plan.tool_calls.is_empty() {
                nonempty_text(partial)
            } else {
                Ok(None)
            }
        }
        EarlyStreamAction::Retry {
            retain_partial_text: false,
            ..
        } => Ok(None),
        EarlyStreamAction::Retry {
            retain_partial_text: true,
            ..
        } => nonempty_text(without_thinking(&plan.assistant_message)?),
        EarlyStreamAction::Exhausted if !plan.tool_calls.is_empty() => Ok(None),
        EarlyStreamAction::Exhausted => nonempty_text(without_thinking(&plan.assistant_message)?),
        EarlyStreamAction::FallThrough => {
            Err(invalid("Fall-through has no partial projection phase."))
        }
    }
}
pub(crate) fn retry_prompt(plan: &EarlyStreamPlan) -> PortResult<(JsString, JsString)> {
    if !matches!(plan.action, EarlyStreamAction::Retry { .. }) {
        return Err(invalid("Retry prompt requested outside retry phase."));
    }
    match plan.kind {
        Some(EarlyStreamKind::UnknownFinish) => Ok((
            if plan.tool_calls.is_empty() {
                "The previous response ended without a recognized finish reason. Continue exactly where the visible response ended; do not repeat prior text or recap.".into()
            } else {
                "The previous response ended without a recognized finish reason after generating tool calls. No tool call was executed.\nContinue the original task from the current workspace state. Inspect relevant files before acting.\nDo not repeat the same large atomic write. Use small focused write_file or edit_file calls.".into()
            },
            "unknown_finish_recovery".into(),
        )),
        Some(EarlyStreamKind::Interruption) => {
            let stream = interruption(plan);
            let phase = field(stream, "phase");
            let prompt = if plan.structured_interruption || is(phase, "tool_call") {
                let calls = field(stream, "activeToolCalls");
                let mut names = Vec::new();
                if !matches!(calls, JsValue::Undefined | JsValue::Null) {
                    let JsValue::Array(calls) = calls else {
                        return Err(exception(
                            calls,
                            "interruption.activeToolCalls?.map is not a function",
                        ));
                    };
                    // Map every call before Source slices the first three.
                    for call in calls {
                        let name = property(call, "name")?;
                        names.push(if truthy(name) { name } else { undefined() });
                    }
                }
                let mut label = Vec::new();
                if !names.is_empty() {
                    label.extend(" (".encode_utf16());
                    for (index, name) in names.iter().take(3).enumerate() {
                        if index > 0 {
                            label.extend(", ".encode_utf16());
                        }
                        let name = if matches!(name, JsValue::Undefined) {
                            "unknown".into()
                        } else {
                            string(name)?
                        };
                        label.extend_from_slice(name.units());
                    }
                    label.push(b')' as u16);
                }
                concat(&[&"The previous model stream disconnected while generating a tool call".into(),&JsString::from_utf16(label),&". No incomplete tool call was executed.\nContinue the original task from the current workspace state. Inspect relevant files before writing.\nDo not retry the same large atomic write. Create or extend the artifact through small focused write_file or edit_file calls, keeping each tool call well under 8K output tokens.".into()])
            } else if is(phase, "reasoning") {
                "The previous model stream disconnected during reasoning. Continue the original task directly from the current workspace state; do not repeat analysis or recap.".into()
            } else if is(phase, "text") {
                "The previous model stream disconnected mid-response. Continue exactly where the visible response ended; do not repeat prior text or recap.".into()
            } else {
                "The previous model stream disconnected before producing a response. Continue the original task directly from the current workspace state.".into()
            };
            Ok((prompt, "stream_interruption_recovery".into()))
        }
        None => Err(invalid("Retry kind missing.")),
    }
}
pub(crate) fn terminal_error(plan: &EarlyStreamPlan) -> PortResult<BoundarySnapshot> {
    if plan.action != EarlyStreamAction::Exhausted {
        return Err(invalid(
            "Terminal error requested outside exhaustion phase.",
        ));
    }
    let (message, details, hint) = match plan.kind {
        Some(EarlyStreamKind::Interruption) => (
            concat(&[
                &"Stream interruption recovery exhausted after 2 attempts (".into(),
                &string(field(interruption(plan), "phase"))?,
                &").".into(),
            ]),
            field(plan.assembled.value(), "error").clone(),
            "The model stream repeatedly disconnected. Retry the turn or switch providers.",
        ),
        Some(EarlyStreamKind::UnknownFinish) => (
            "Unknown finish reason recovery exhausted after 2 attempts.".into(),
            JsValue::Undefined,
            "The provider repeatedly ended the stream without a recognized finish reason. Retry the turn or switch providers.",
        ),
        None => return Err(invalid("Exhausted kind missing.")),
    };
    let mut error = JsObject::new();
    error.insert("code".into(), "agent_model_error".into());
    error.insert("message".into(), JsValue::String(message));
    error.insert("details".into(), details);
    error.insert("userHint".into(), hint.into());
    Ok(BoundarySnapshot::capture(&JsValue::Object(error)))
}
struct FailureAction {
    hint: JsString,
    target: &'static str,
    key: &'static str,
    params: JsObject,
}
fn failure_action(error: &JsValue) -> PortResult<FailureAction> {
    if !truthy(error) {
        return Ok(FailureAction { hint:"Check Settings → Model Provider and verify the selected provider, base URL, API key, model name, and timeoutMs. If the provider is slow or the network is unstable, increase timeoutMs or check provider status.".into(),target:"settings",key:"settingsDefault",params:JsObject::new() });
    }
    // Both labels are evaluated before selecting an error/status branch.
    let provider = field(error, "provider");
    let model = field(error, "model");
    let provider_label = if truthy(provider) {
        concat(&[&" provider \"".into(), &string(provider)?, &"\"".into()])
    } else {
        " provider".into()
    };
    let model_label = if truthy(model) {
        concat(&[&" model \"".into(), &string(model)?, &"\"".into()])
    } else {
        " selected model".into()
    };
    let code = field(error, "code");
    let status = field(error, "status");
    let mut params = JsObject::new();
    let provider_param = if matches!(provider, JsValue::Undefined | JsValue::Null) {
        "the provider".into()
    } else {
        provider.clone()
    };
    let label_hint = |a: &str, label: &JsString, b: &str| concat(&[&a.into(), label, &b.into()]);
    let (hint, target, key) = if status.as_number().is_some_and(|n| n == 401.0 || n == 403.0)
        || is(code, "auth_error")
    {
        params.insert("provider".into(), provider_param);
        (
            label_hint(
                "Update the API key or access permissions for",
                &provider_label,
                " in Settings → Model Provider, or run pilotdeck setup.",
            ),
            "settings",
            "auth",
        )
    } else if is(code, "model_not_found") {
        params.insert("provider".into(), provider_param);
        params.insert("model".into(), model.clone());
        (concat(&[&"Choose a valid".into(),&model_label,&" for".into(),&provider_label,&" in Settings → Model Provider, or add it under model.providers.<id>.models in pilotdeck.yaml.".into()]),"settings","modelNotFound")
    } else if is(code, "timeout") {
        params.insert("provider".into(), provider_param);
        if is(
            field(field(error, "settingsFix"), "configPath"),
            "model.providers.<id>.retry.streamIdleTimeoutMs",
        ) {
            (
                label_hint(
                    "Increase streamIdleTimeoutMs for",
                    &provider_label,
                    " in Settings → Advanced, or check local network/proxy and provider status.",
                ),
                "network",
                "streamIdleTimeout",
            )
        } else {
            (
                label_hint(
                    "Increase timeoutMs for",
                    &provider_label,
                    " in Settings → Model Provider → Advanced, or check local network/proxy and provider status.",
                ),
                "network",
                "timeout",
            )
        }
    } else if status.as_number() == Some(429.0) || is(code, "rate_limit_error") {
        ("Wait for the provider rate limit to reset, reduce concurrency, or switch to another provider/model in Settings.".into(),"provider","rateLimit")
    } else if is(code, "billing") {
        ("Top up billing/quota on the provider API side, or switch to another provider/model in Settings.".into(),"provider","billing")
    } else if [
        "prompt_too_long",
        "context_overflow",
        "context_overflow_after_emergency_compaction",
    ]
    .iter()
    .any(|c| is(code, c))
    {
        ("Run /compact, start a new session, remove large attachments, or switch to a larger-context model in Settings.".into(),"prompt","contextOverflow")
    } else if is(code, "payload_too_large") || is(code, "request_too_large") {
        ("Reduce attachments/context size, run /compact, or start a new session before retrying.".into(),"prompt","payloadTooLarge")
    } else if is(code, "max_output_reached") {
        ("Increase max output tokens in Settings → Model Provider, or ask the agent to split the answer into smaller parts.".into(),"settings","maxOutput")
    } else if is(code, "image_too_large") {
        (
            "Resize or remove large images, then retry.".into(),
            "prompt",
            "imageTooLarge",
        )
    } else if truthy(field(error, "retryable"))
        || is(code, "server_error")
        || is(code, "overloaded_error")
    {
        ("Retry later, check provider API status, or switch to another provider/model in Settings if it repeats.".into(),"provider","providerRetry")
    } else {
        ("Check Settings → Model Provider for base URL/API key/model and timeoutMs. If settings look correct, check local network/proxy and provider API status/logs.".into(),"settings","settingsDefault")
    };
    Ok(FailureAction {
        hint,
        target,
        key,
        params,
    })
}
pub(crate) fn terminal_status(
    plan: &EarlyStreamPlan,
    error: &BoundarySnapshot,
) -> PortResult<BoundarySnapshot> {
    if plan.action != EarlyStreamAction::Exhausted {
        return Err(invalid(
            "Terminal status requested outside exhaustion phase.",
        ));
    }
    let model_error = if plan.kind == Some(EarlyStreamKind::Interruption) {
        field(plan.assembled.value(), "error")
    } else {
        undefined()
    };
    create_model_request_failure_status(error, &BoundarySnapshot::capture(model_error))
}

/// Source createModelRequestFailedStatus for ordinary and recovery failures.
/// The caller supplies the classified error and the original raw model error;
/// no early-stream exhaustion plan or recovery decision is fabricated.
pub fn create_model_request_failure_status(
    error: &BoundarySnapshot,
    model_error: &BoundarySnapshot,
) -> PortResult<BoundarySnapshot> {
    let model_error = model_error.value();
    let message = property(error.value(), "message")?;
    let provider_message = if truthy(message) {
        message
    } else if truthy(field(model_error, "message")) {
        field(model_error, "message")
    } else {
        undefined()
    };
    let fallback = JsValue::from("The model request failed, so this turn has stopped.");
    let provider_message = if matches!(provider_message, JsValue::Undefined) {
        &fallback
    } else {
        provider_message
    };
    let Some(message) = provider_message.as_string() else {
        return Err(exception(
            provider_message,
            "providerMessage.trim is not a function",
        ));
    };
    let clean = trim(message);
    let clean = if clean.is_empty() {
        "The model request failed.".into()
    } else {
        clean
    };
    let action = failure_action(model_error)?;
    let text = concat(&[&clean, &"\n\nAction: ".into(), &action.hint]);
    let mut message_params = JsObject::new();
    message_params.insert("providerMessage".into(), provider_message.clone());
    let mut mi = JsObject::new();
    mi.insert(
        "key".into(),
        "chat:agentStatus.modelRequestFailed.message".into(),
    );
    mi.insert("params".into(), JsValue::Object(message_params));
    let mut hi = JsObject::new();
    hi.insert(
        "key".into(),
        JsValue::String(
            format!("chat:agentStatus.modelRequestFailed.actions.{}", action.key).into(),
        ),
    );
    hi.insert("params".into(), JsValue::Object(action.params));
    let mut detail = JsObject::new();
    detail.insert("message".into(), JsValue::String(text.clone()));
    detail.insert("messageI18n".into(), JsValue::Object(mi));
    let code = property(error.value(), "code")?;
    if !matches!(code, JsValue::Undefined) {
        detail.insert("code".into(), code.clone());
    }
    detail.insert("severity".into(), "error".into());
    detail.insert("visible".into(), JsValue::Bool(true));
    detail.insert("userHint".into(), JsValue::String(action.hint));
    detail.insert("userHintI18n".into(), JsValue::Object(hi));
    detail.insert("scope".into(), "turn".into());
    detail.insert("source".into(), "agent".into());
    for (out, source) in [
        ("provider", "provider"),
        ("protocol", "protocol"),
        ("status", "status"),
        ("modelErrorCode", "code"),
        ("retryable", "retryable"),
    ] {
        let v = field(model_error, source);
        if !matches!(v, JsValue::Undefined) {
            detail.insert(out.into(), v.clone());
        }
    }
    detail.insert("providerMessage".into(), provider_message.clone());
    let fix = field(model_error, "settingsFix");
    if !matches!(fix, JsValue::Undefined) {
        detail.insert("settingsFix".into(), fix.clone());
    }
    detail.insert("fixTarget".into(), action.target.into());
    let mut result = JsObject::new();
    result.insert("event".into(), "model_request_failed".into());
    result.insert("kind".into(), "error".into());
    result.insert("text".into(), JsValue::String(text));
    result.insert("detail".into(), JsValue::Object(detail));
    Ok(BoundarySnapshot::capture(&JsValue::Object(result)))
}
