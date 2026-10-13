//! Source: fixed original model/streaming/assembleModelMessage.ts. No recovery policy.
use crate::{
    AssembledAssistantMessage, AssemblyError, CapturedModelEvent, ModelAssemblerState,
    StreamBlockKind,
};
use pilotdeck_protocol::model::{
    BoundarySnapshot, ContentBlock, ContentBody, FinishReason, InterruptionPhase, Message,
    MessageMetadata, ModelError, ModelEvent, ModelProtocol, Role, TextBlock, ThinkingBlock,
    TimelinePosition, ToolCall, Usage,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashSet;

pub(crate) fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in fields {
        result.insert(key.into(), value);
    }
    JsValue::Object(result)
}
pub(crate) fn optional_string(value: Option<&JsString>) -> JsValue {
    value.map_or(JsValue::Undefined, |v| JsValue::String(v.clone()))
}
fn optional_number(value: Option<f64>) -> JsValue {
    value.map_or(JsValue::Undefined, JsValue::Number)
}
fn optional_bool(value: Option<bool>) -> JsValue {
    value.map_or(JsValue::Undefined, JsValue::Bool)
}

/// Exact captured data comparison, preserving signed zero and non-finite numbers.
/// This deliberately is not stringify equality (which omits Undefined).
pub(crate) fn same_value(a: &JsValue, b: &JsValue) -> bool {
    let mut pending = vec![(a, b)];
    while let Some((a, b)) = pending.pop() {
        match (a, b) {
            (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => (),
            (JsValue::Bool(a), JsValue::Bool(b)) if a == b => (),
            (JsValue::Number(a), JsValue::Number(b))
                if a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()) => {}
            (JsValue::String(a), JsValue::String(b)) if a == b => (),
            (JsValue::Array(a), JsValue::Array(b)) if a.len() == b.len() => {
                pending.extend(a.iter().zip(b))
            }
            (JsValue::Object(a), JsValue::Object(b)) if a.len() == b.len() => {
                for (key, value) in a.entries() {
                    let Some(other) = b.get_key(key) else {
                        return false;
                    };
                    pending.push((value, other));
                }
            }
            _ => return false,
        }
    }
    true
}

/// Check a normalized schema projection without deleting admitted extensions.
pub(crate) fn matches_fields(actual: &JsValue, expected: &JsValue) -> bool {
    let mut pending = vec![(actual, expected)];
    while let Some((actual, expected)) = pending.pop() {
        if let JsValue::Object(fields) = expected {
            let Some(actual) = actual.as_object() else {
                return false;
            };
            for (key, expected) in fields.entries() {
                match actual.get_key(key) {
                    None if expected.is_undefined() => (),
                    Some(value)
                        if key.equals_str("input")
                            || key.equals_str("raw")
                            || key.equals_str("metadata")
                            || key.equals_str("data")
                            || key.equals_str("value") =>
                    {
                        if !same_value(value, expected) {
                            return false;
                        }
                    }
                    Some(value) => pending.push((value, expected)),
                    None => return false,
                }
            }
        } else if !same_value(actual, expected) {
            return false;
        }
    }
    true
}

fn timeline(value: &TimelinePosition) -> JsValue {
    object([
        ("version", JsValue::Number(1.0)),
        ("turnId", JsValue::String(value.turn_id.clone())),
        ("id", JsValue::String(value.id.clone())),
        ("previousId", optional_string(value.previous_id.as_ref())),
        ("order", JsValue::Number(value.order)),
        ("revision", JsValue::Number(value.revision)),
        ("offset", optional_number(value.offset_utf16)),
    ])
}
pub(crate) fn tool_call_projection(value: &ToolCall) -> JsValue {
    object([
        ("id", JsValue::String(value.id.clone())),
        ("name", JsValue::String(value.name.clone())),
        ("input", value.input.clone()),
        (
            "raw",
            value
                .raw
                .as_ref()
                .map_or(JsValue::Undefined, |v| v.0.as_ref().clone()),
        ),
        (
            "timeline",
            value.timeline.as_ref().map_or(JsValue::Undefined, timeline),
        ),
    ])
}
fn usage_projection(value: &Usage) -> JsValue {
    object([
        ("inputTokens", optional_number(value.input_tokens)),
        ("outputTokens", optional_number(value.output_tokens)),
        ("cacheReadTokens", optional_number(value.cache_read_tokens)),
        (
            "cacheWriteTokens",
            optional_number(value.cache_write_tokens),
        ),
        ("totalTokens", optional_number(value.total_tokens)),
        ("nativeCost", optional_number(value.native_cost)),
    ])
}
fn protocol(value: ModelProtocol) -> &'static str {
    match value {
        ModelProtocol::Anthropic => "anthropic",
        ModelProtocol::OpenAi => "openai",
        ModelProtocol::OpenAiResponses => "openai-responses",
        ModelProtocol::Google => "google",
    }
}
fn error_projection(value: &ModelError) -> JsValue {
    let fix = value
        .settings_fix
        .as_ref()
        .map_or(JsValue::Undefined, |fix| {
            object([
                ("description", JsValue::String(fix.description.clone())),
                ("configPath", optional_string(fix.config_path.as_ref())),
                ("command", optional_string(fix.command.as_ref())),
                ("url", optional_string(fix.url.as_ref())),
            ])
        });
    let interruption = value
        .stream_interruption
        .as_ref()
        .map_or(JsValue::Undefined, |v| {
            object([
                (
                    "phase",
                    JsValue::from(match v.phase {
                        InterruptionPhase::Empty => "empty",
                        InterruptionPhase::Text => "text",
                        InterruptionPhase::Reasoning => "reasoning",
                        InterruptionPhase::ToolCall => "tool_call",
                    }),
                ),
                (
                    "activeToolCalls",
                    v.active_tool_calls
                        .as_ref()
                        .map_or(JsValue::Undefined, |calls| {
                            JsValue::Array(
                                calls
                                    .iter()
                                    .map(|call| {
                                        object([
                                            ("id", JsValue::String(call.id.clone())),
                                            ("name", JsValue::String(call.name.clone())),
                                            (
                                                "argumentChars",
                                                JsValue::Number(call.argument_chars_utf16),
                                            ),
                                        ])
                                    })
                                    .collect(),
                            )
                        }),
                ),
            ])
        });
    object([
        ("provider", JsValue::String(value.provider.clone())),
        ("model", optional_string(value.model.as_ref())),
        ("protocol", JsValue::from(protocol(value.protocol))),
        ("code", JsValue::String(value.code.clone())),
        ("status", optional_number(value.status)),
        ("message", JsValue::String(value.message.clone())),
        ("retryable", JsValue::Bool(value.retryable)),
        (
            "raw",
            value
                .raw
                .as_ref()
                .map_or(JsValue::Undefined, |v| v.0.as_ref().clone()),
        ),
        (
            "recoverableViaCompact",
            optional_bool(value.recoverable_via_compact),
        ),
        (
            "recoverableViaImageStrip",
            optional_bool(value.recoverable_via_image_strip),
        ),
        ("retryAfterMs", optional_number(value.retry_after_ms)),
        ("userHint", optional_string(value.user_hint.as_ref())),
        ("settingsFix", fix),
        (
            "maxContextTokens",
            optional_number(value.max_context_tokens),
        ),
        ("maxOutputTokens", optional_number(value.max_output_tokens)),
        (
            "availableOutputTokens",
            optional_number(value.available_output_tokens),
        ),
        ("streamInterruption", interruption),
    ])
}
fn finish(value: FinishReason) -> &'static str {
    match value {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ToolCall => "tool_call",
        FinishReason::ContentFilter => "content_filter",
        FinishReason::Error => "error",
        FinishReason::Unknown => "unknown",
    }
}
fn event_projection(event: &ModelEvent) -> JsValue {
    let mut projected = match event {
        ModelEvent::RequestStarted {
            provider,
            model,
            provider_base_url,
            request_fingerprint,
            metadata,
        } => object([
            ("type", JsValue::from("request_started")),
            ("provider", JsValue::String(provider.clone())),
            ("model", JsValue::String(model.clone())),
            (
                "providerBaseUrl",
                optional_string(provider_base_url.as_ref()),
            ),
            (
                "requestFingerprint",
                optional_string(request_fingerprint.as_ref()),
            ),
            (
                "metadata",
                metadata
                    .as_ref()
                    .map_or(JsValue::Undefined, |v| JsValue::Object(v.clone())),
            ),
        ]),
        ModelEvent::MessageStart { .. } => object([
            ("type", JsValue::from("message_start")),
            ("role", JsValue::from("assistant")),
        ]),
        ModelEvent::TextDelta { text, .. } => object([
            ("type", JsValue::from("text_delta")),
            ("text", JsValue::String(text.clone())),
        ]),
        ModelEvent::ThinkingDelta {
            text,
            signature,
            reasoning_content,
            ..
        } => object([
            ("type", JsValue::from("thinking_delta")),
            ("text", JsValue::String(text.clone())),
            ("signature", optional_string(signature.as_ref())),
            (
                "reasoningContent",
                optional_string(reasoning_content.as_ref()),
            ),
        ]),
        ModelEvent::ToolCallStart { id, name, .. } => object([
            ("type", JsValue::from("tool_call_start")),
            ("id", JsValue::String(id.clone())),
            ("name", JsValue::String(name.clone())),
        ]),
        ModelEvent::ToolCallDelta { id, delta, .. } => object([
            ("type", JsValue::from("tool_call_delta")),
            ("id", JsValue::String(id.clone())),
            ("delta", JsValue::String(delta.clone())),
        ]),
        ModelEvent::ToolCallEnd {
            tool_call,
            was_repaired,
            ..
        } => object([
            ("type", JsValue::from("tool_call_end")),
            ("toolCall", tool_call_projection(tool_call)),
            ("wasRepaired", optional_bool(*was_repaired)),
        ]),
        ModelEvent::MessageEnd { finish_reason, .. } => object([
            ("type", JsValue::from("message_end")),
            ("finishReason", JsValue::from(finish(*finish_reason))),
        ]),
        ModelEvent::Usage { usage, .. } => object([
            ("type", JsValue::from("usage")),
            ("usage", usage_projection(usage)),
        ]),
        ModelEvent::Error(error) => object([
            ("type", JsValue::from("error")),
            ("error", error_projection(error)),
        ]),
    };
    let raw = match event {
        ModelEvent::MessageStart { raw }
        | ModelEvent::TextDelta { raw, .. }
        | ModelEvent::ThinkingDelta { raw, .. }
        | ModelEvent::ToolCallStart { raw, .. }
        | ModelEvent::ToolCallDelta { raw, .. }
        | ModelEvent::ToolCallEnd { raw, .. }
        | ModelEvent::MessageEnd { raw, .. }
        | ModelEvent::Usage { raw, .. } => Some(raw),
        ModelEvent::RequestStarted { .. } | ModelEvent::Error(_) => None,
    };
    if let (Some(raw), JsValue::Object(fields)) = (raw, &mut projected) {
        fields.insert(
            "raw".into(),
            raw.as_ref()
                .map_or(JsValue::Undefined, |v| v.0.as_ref().clone()),
        );
    }
    projected
}

pub(crate) fn new(response_id: Option<JsString>) -> ModelAssemblerState {
    ModelAssemblerState {
        response_id: response_id.filter(|v| !v.is_empty()),
        content: Vec::new(),
        content_boundaries: Vec::new(),
        tool_call_boundaries: Vec::new(),
        error_boundary: None,
        text_buffer: JsString::default(),
        model: None,
        thinking_buffer: JsString::default(),
        thinking_reasoning_content_buffer: JsString::default(),
        thinking_signature: None,
        usage: Usage::default(),
        finish_reason: None,
        has_message_end: false,
        error: None,
        tool_calls: Vec::new(),
        has_repaired_tool_calls: None,
    }
}
fn append(target: &mut JsString, value: &JsString) {
    *target = JsString::from_utf16(
        target
            .units()
            .iter()
            .chain(value.units())
            .copied()
            .collect(),
    );
}
pub(crate) fn block_id(state: &ModelAssemblerState, kind: StreamBlockKind) -> Option<JsString> {
    let response = state.response_id.as_ref().filter(|v| !v.is_empty())?;
    let name = match kind {
        StreamBlockKind::Text => "text",
        StreamBlockKind::Thinking => "thinking",
    };
    let count = state
        .content_boundaries
        .iter()
        .filter(|v| {
            v.value()
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|v| v.equals_str(name))
        })
        .count();
    let mut value = response.clone();
    append(
        &mut value,
        &JsString::from(format!(
            ":{}:{count}",
            match kind {
                StreamBlockKind::Text => "text",
                StreamBlockKind::Thinking => "thinking",
            }
        )),
    );
    Some(value)
}
fn flush(state: &mut ModelAssemblerState) {
    if !state.thinking_buffer.is_empty()
        || !state.thinking_reasoning_content_buffer.is_empty()
        || state.thinking_signature.is_some()
    {
        let block_id = block_id(state, StreamBlockKind::Thinking);
        let reasoning = (!state.thinking_reasoning_content_buffer.is_empty())
            .then(|| state.thinking_reasoning_content_buffer.clone());
        let mut boundary = JsObject::new();
        boundary.insert("type".into(), JsValue::from("thinking"));
        boundary.insert(
            "text".into(),
            JsValue::String(state.thinking_buffer.clone()),
        );
        if let Some(id) = &block_id {
            boundary.insert("blockId".into(), JsValue::String(id.clone()));
        }
        if let Some(value) = &reasoning {
            boundary.insert("reasoningContent".into(), JsValue::String(value.clone()));
        }
        if let Some(value) = &state.thinking_signature {
            boundary.insert("signature".into(), JsValue::String(value.clone()));
        }
        state
            .content_boundaries
            .push(BoundarySnapshot::capture(&JsValue::Object(boundary)));
        state.content.push(ContentBlock {
            timeline: None,
            body: ContentBody::Thinking(ThinkingBlock {
                text: std::mem::take(&mut state.thinking_buffer),
                block_id,
                signature: state.thinking_signature.take(),
                reasoning_content: reasoning,
            }),
        });
        state.thinking_reasoning_content_buffer = JsString::default();
    }
    if !state.text_buffer.is_empty() {
        let block_id = block_id(state, StreamBlockKind::Text);
        let mut boundary = JsObject::new();
        boundary.insert("type".into(), JsValue::from("text"));
        boundary.insert("text".into(), JsValue::String(state.text_buffer.clone()));
        if let Some(id) = &block_id {
            boundary.insert("blockId".into(), JsValue::String(id.clone()));
        }
        state
            .content_boundaries
            .push(BoundarySnapshot::capture(&JsValue::Object(boundary)));
        state.content.push(ContentBlock {
            timeline: None,
            body: ContentBody::Text(TextBlock {
                text: std::mem::take(&mut state.text_buffer),
                block_id,
            }),
        });
    }
}
pub(crate) fn apply(
    state: &mut ModelAssemblerState,
    captured: &CapturedModelEvent,
) -> Result<(), AssemblyError> {
    if !matches_fields(
        captured.boundary.value(),
        &event_projection(&captured.event),
    ) {
        return Err(AssemblyError::InvalidBoundary {
            message: "Typed event contradicts its complete admitted boundary".into(),
        });
    }
    // Validation happens before all mutations, including flushing buffered text.
    match &captured.event {
        ModelEvent::RequestStarted { model, .. } => state.model = Some(model.clone()),
        ModelEvent::MessageStart { .. }
        | ModelEvent::ToolCallStart { .. }
        | ModelEvent::ToolCallDelta { .. } => (),
        ModelEvent::TextDelta { text, .. } => {
            if !state.thinking_buffer.is_empty()
                || !state.thinking_reasoning_content_buffer.is_empty()
                || state.thinking_signature.is_some()
            {
                flush(state);
            }
            append(&mut state.text_buffer, text);
        }
        ModelEvent::ThinkingDelta {
            text,
            reasoning_content,
            signature,
            ..
        } => {
            if !state.text_buffer.is_empty() {
                flush(state);
            }
            append(&mut state.thinking_buffer, text);
            if let Some(value) = reasoning_content {
                append(&mut state.thinking_reasoning_content_buffer, value);
            }
            if let Some(value) = signature.as_ref().filter(|v| !v.is_empty()) {
                state.thinking_signature = Some(value.clone());
            }
        }
        ModelEvent::ToolCallEnd {
            tool_call,
            was_repaired,
            ..
        } => {
            flush(state);
            // Complete call and spread object have different type-property ordering.
            let raw = captured
                .boundary
                .value()
                .get("toolCall")
                .expect("validated call object");
            let mut block = JsObject::new();
            block.insert("type".into(), JsValue::from("tool_call"));
            for (key, value) in raw.as_object().expect("validated call object").entries() {
                block.insert(key.clone(), value.clone());
            }
            state
                .tool_call_boundaries
                .push(BoundarySnapshot::capture(raw));
            state
                .content_boundaries
                .push(BoundarySnapshot::capture(&JsValue::Object(block)));
            state.tool_calls.push(tool_call.clone());
            state.content.push(ContentBlock {
                timeline: tool_call.timeline.clone(),
                body: ContentBody::ToolCall(tool_call.clone()),
            });
            if *was_repaired == Some(true) {
                state.has_repaired_tool_calls = Some(true);
            }
        }
        ModelEvent::MessageEnd { finish_reason, .. } => {
            flush(state);
            state.finish_reason = Some(*finish_reason);
            state.has_message_end = true;
        }
        ModelEvent::Usage { usage, .. } => {
            fn add(a: Option<f64>, b: Option<f64>) -> Option<f64> {
                if a.is_none() && b.is_none() {
                    None
                } else {
                    Some(a.unwrap_or(0.0) + b.unwrap_or(0.0))
                }
            }
            state.usage.input_tokens = add(state.usage.input_tokens, usage.input_tokens);
            state.usage.output_tokens = add(state.usage.output_tokens, usage.output_tokens);
            state.usage.cache_read_tokens =
                add(state.usage.cache_read_tokens, usage.cache_read_tokens);
            state.usage.cache_write_tokens =
                add(state.usage.cache_write_tokens, usage.cache_write_tokens);
            state.usage.total_tokens = add(state.usage.total_tokens, usage.total_tokens);
            state.usage.native_cost = add(state.usage.native_cost, usage.native_cost);
        }
        ModelEvent::Error(error) => {
            flush(state);
            state.error = Some(error.clone());
            state.finish_reason = Some(FinishReason::Error);
            state.error_boundary = Some(BoundarySnapshot::capture(
                captured
                    .boundary
                    .value()
                    .get("error")
                    .expect("validated error"),
            ));
        }
    }
    Ok(())
}

fn is_js_space(unit: u16) -> bool {
    matches!(unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(value: &JsString) -> JsString {
    let units = value.units();
    let start = units
        .iter()
        .position(|v| !is_js_space(*v))
        .unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|v| !is_js_space(*v))
        .map_or(start, |i| i + 1);
    JsString::from_utf16(units[start..end].to_vec())
}
fn replace_id(snapshot: &BoundarySnapshot, id: &JsString) -> BoundarySnapshot {
    let mut object = snapshot
        .value()
        .as_object()
        .expect("admitted call boundary")
        .clone();
    object.insert("id".into(), JsValue::String(id.clone()));
    BoundarySnapshot::capture(&JsValue::Object(object))
}
fn normalize_ids(state: &mut ModelAssemblerState) {
    let mut used = HashSet::new();
    for (index, call) in state.tool_calls.iter_mut().enumerate() {
        let trimmed = trim(&call.id);
        let base = if trimmed.is_empty() {
            JsString::from(format!("call_{index}"))
        } else {
            trimmed
        };
        let mut id = base.clone();
        let mut suffix = 2;
        while used.contains(&id) {
            id = base.clone();
            append(&mut id, &JsString::from(format!("_{suffix}")));
            suffix += 1;
        }
        used.insert(id.clone());
        call.id = id.clone();
        state.tool_call_boundaries[index] = replace_id(&state.tool_call_boundaries[index], &id);
    }
    let mut index = 0;
    for (block, boundary) in state.content.iter_mut().zip(&mut state.content_boundaries) {
        // The source tests block.type, not merely its typed projection; a lax
        // toolCall own type can override the source's spread tag.
        if !boundary
            .value()
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|v| v.equals_str("tool_call"))
        {
            continue;
        }
        if let Some(call) = state.tool_calls.get(index) {
            if let ContentBody::ToolCall(block_call) = &mut block.body {
                block_call.id = call.id.clone();
            }
            *boundary = replace_id(boundary, &call.id);
        }
        index += 1;
    }
}
pub(crate) fn assemble(state: &mut ModelAssemblerState) -> AssembledAssistantMessage {
    flush(state);
    normalize_ids(state);
    let reason = state.finish_reason.unwrap_or(if state.error.is_some() {
        FinishReason::Error
    } else {
        FinishReason::Unknown
    });
    let has_usage = [
        state.usage.input_tokens,
        state.usage.output_tokens,
        state.usage.cache_read_tokens,
        state.usage.cache_write_tokens,
        state.usage.total_tokens,
        state.usage.native_cost,
    ]
    .iter()
    .any(Option::is_some);
    let model = state.model.as_ref().filter(|v| !v.is_empty());
    let mut message = JsObject::new();
    message.insert("role".into(), JsValue::from("assistant"));
    message.insert(
        "content".into(),
        JsValue::Array(
            state
                .content_boundaries
                .iter()
                .map(|v| v.value().clone())
                .collect(),
        ),
    );
    if let Some(model) = model {
        message.insert(
            "metadata".into(),
            object([("model", JsValue::String(model.clone()))]),
        );
    }
    let boundary = object([
        ("message", JsValue::Object(message)),
        ("finishReason", JsValue::from(finish(reason))),
        ("hasMessageEnd", JsValue::Bool(state.has_message_end)),
        (
            "usage",
            if has_usage {
                usage_projection(&state.usage)
            } else {
                JsValue::Undefined
            },
        ),
        (
            "toolCalls",
            JsValue::Array(
                state
                    .tool_call_boundaries
                    .iter()
                    .map(|v| v.value().clone())
                    .collect(),
            ),
        ),
        (
            "error",
            state
                .error_boundary
                .as_ref()
                .map_or(JsValue::Undefined, |v| v.value().clone()),
        ),
        (
            "hasRepairedToolCalls",
            optional_bool(state.has_repaired_tool_calls),
        ),
    ]);
    AssembledAssistantMessage {
        boundary: BoundarySnapshot::capture(&boundary),
        message: Message {
            role: Role::Assistant,
            content: state.content.clone(),
            metadata: model.map(|v| MessageMetadata {
                model: Some(v.clone()),
                ..MessageMetadata::default()
            }),
        },
        finish_reason: reason,
        usage: has_usage.then(|| state.usage.clone()),
        tool_calls: state.tool_calls.clone(),
        error: state.error.clone(),
        has_repaired_tool_calls: state.has_repaired_tool_calls,
        has_message_end: state.has_message_end,
    }
}
