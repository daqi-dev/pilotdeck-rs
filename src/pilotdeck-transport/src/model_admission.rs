use pilotdeck_agent::ports::{CapturedRequest, PortError, PortResult, RequestProjection, Route};
use pilotdeck_agent::{
    CapturedModelEvent, MessageProjection, ProjectedMessage, ProjectionGap,
    SupplementalToolMessage, ToolContent, ToolExecutionResult, ToolResultDisposition,
};
use pilotdeck_protocol::model::*;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

pub(crate) fn invalid(message: impl Into<String>) -> PortError {
    PortError::InvalidResponse {
        message: message.into().into(),
    }
}
pub(crate) fn object(v: &JsValue) -> PortResult<&JsObject> {
    v.as_object()
        .ok_or_else(|| invalid("Expected an object boundary"))
}
pub(crate) fn field<'a>(v: &'a JsValue, k: &str) -> PortResult<&'a JsValue> {
    v.get(k)
        .ok_or_else(|| invalid(format!("Missing field {k}")))
}
pub(crate) fn string(v: &JsValue) -> PortResult<JsString> {
    v.as_string()
        .cloned()
        .ok_or_else(|| invalid("Expected a UTF16 string"))
}
pub(crate) fn str_field(v: &JsValue, k: &str) -> PortResult<JsString> {
    string(field(v, k)?)
}
pub(crate) fn number(v: &JsValue) -> PortResult<f64> {
    v.as_number().ok_or_else(|| invalid("Expected a Number"))
}
pub(crate) fn num_field(v: &JsValue, k: &str) -> PortResult<f64> {
    number(field(v, k)?)
}
pub(crate) fn boolean(v: &JsValue) -> PortResult<bool> {
    v.as_bool().ok_or_else(|| invalid("Expected a boolean"))
}
pub(crate) fn bool_field(v: &JsValue, k: &str) -> PortResult<bool> {
    boolean(field(v, k)?)
}
pub(crate) fn array(v: &JsValue) -> PortResult<&[JsValue]> {
    if let JsValue::Array(a) = v {
        Ok(a)
    } else {
        Err(invalid("Expected an array"))
    }
}
pub(crate) fn optional<T>(
    v: &JsValue,
    k: &str,
    read: impl FnOnce(&JsValue) -> PortResult<T>,
) -> PortResult<Option<T>> {
    match v.get(k) {
        None | Some(JsValue::Undefined) => Ok(None),
        Some(x) => read(x).map(Some),
    }
}
pub(crate) fn nullable<T>(
    v: &JsValue,
    k: &str,
    read: impl FnOnce(&JsValue) -> PortResult<T>,
) -> PortResult<Option<T>> {
    match v.get(k) {
        None | Some(JsValue::Undefined | JsValue::Null) => Ok(None),
        Some(x) => read(x).map(Some),
    }
}
pub(crate) fn opt_string(v: &JsValue, k: &str) -> PortResult<Option<JsString>> {
    optional(v, k, string)
}
pub(crate) fn opt_number(v: &JsValue, k: &str) -> PortResult<Option<f64>> {
    optional(v, k, number)
}
pub(crate) fn opt_bool(v: &JsValue, k: &str) -> PortResult<Option<bool>> {
    optional(v, k, boolean)
}
pub(crate) fn strings(v: &JsValue) -> PortResult<Vec<JsString>> {
    array(v)?.iter().map(string).collect()
}
pub(crate) fn numbers(v: &JsValue) -> PortResult<Vec<f64>> {
    array(v)?.iter().map(number).collect()
}
pub(crate) fn metadata(v: &JsValue) -> PortResult<JsObject> {
    object(v).cloned()
}
pub(crate) fn protocol(v: &JsValue) -> PortResult<ModelProtocol> {
    let s = string(v)?;
    for (name, p) in [
        ("anthropic", ModelProtocol::Anthropic),
        ("openai", ModelProtocol::OpenAi),
        ("openai-responses", ModelProtocol::OpenAiResponses),
        ("google", ModelProtocol::Google),
    ] {
        if s.equals_str(name) {
            return Ok(p);
        }
    }
    Err(invalid("Unknown model protocol"))
}
fn tag(v: &JsValue, k: &str, choices: &[&str]) -> PortResult<usize> {
    let s = str_field(v, k)?;
    choices
        .iter()
        .position(|t| s.equals_str(t))
        .ok_or_else(|| invalid(format!("Invalid {k} discriminator")))
}
fn echo(v: &JsValue, k: &str) -> Option<ProviderEcho> {
    match v.get(k) {
        None | Some(JsValue::Undefined) => None,
        Some(x) => Some(ProviderEcho(Arc::new(x.clone()))),
    }
}
fn timeline(v: &JsValue) -> PortResult<TimelinePosition> {
    object(v)?;
    if num_field(v, "version")? != 1.0 {
        return Err(invalid("Timeline version must be 1"));
    }
    Ok(TimelinePosition {
        turn_id: str_field(v, "turnId")?,
        id: str_field(v, "id")?,
        previous_id: opt_string(v, "previousId")?,
        order: num_field(v, "order")?,
        revision: num_field(v, "revision")?,
        offset_utf16: opt_number(v, "offset")?,
    })
}
fn source(v: &JsValue) -> PortResult<MediaSource> {
    Ok(if tag(v, "source", &["base64", "url"])? == 0 {
        MediaSource::Base64
    } else {
        MediaSource::Url
    })
}
fn detail(v: &JsValue) -> PortResult<ImageDetail> {
    let s = string(v)?;
    for (name, d) in [
        ("auto", ImageDetail::Auto),
        ("low", ImageDetail::Low),
        ("high", ImageDetail::High),
    ] {
        if s.equals_str(name) {
            return Ok(d);
        }
    }
    Err(invalid("Invalid image detail"))
}
fn text(v: &JsValue) -> PortResult<TextBlock> {
    Ok(TextBlock {
        text: str_field(v, "text")?,
        block_id: opt_string(v, "blockId")?,
    })
}
pub(crate) fn image(v: &JsValue) -> PortResult<ImageBlock> {
    Ok(ImageBlock {
        source: source(v)?,
        data: str_field(v, "data")?,
        mime_type: str_field(v, "mimeType")?,
        bytes: opt_number(v, "bytes")?,
        detail: optional(v, "detail", detail)?,
    })
}
pub(crate) fn pdf(v: &JsValue) -> PortResult<PdfBlock> {
    if !str_field(v, "source")?.equals_str("base64")
        || !str_field(v, "mimeType")?.equals_str("application/pdf")
    {
        return Err(invalid("Invalid PDF source or MIME"));
    }
    Ok(PdfBlock {
        data: str_field(v, "data")?,
        bytes: num_field(v, "bytes")?,
        pages: opt_number(v, "pages")?,
    })
}
pub(crate) fn tool_call(v: &JsValue) -> PortResult<ToolCall> {
    object(v)?;
    Ok(ToolCall {
        timeline: optional(v, "timeline", timeline)?,
        id: str_field(v, "id")?,
        name: str_field(v, "name")?,
        input: v.get("input").cloned().unwrap_or(JsValue::Undefined),
        raw: echo(v, "raw"),
    })
}
fn inner_content(v: &JsValue) -> PortResult<ToolResultItem> {
    Ok(ToolResultItem {
        timeline: optional(v, "timeline", timeline)?,
        body: match tag(v, "type", &["text", "image", "pdf"])? {
            0 => ToolResultContent::Text(text(v)?),
            1 => ToolResultContent::Image(image(v)?),
            _ => ToolResultContent::Pdf(pdf(v)?),
        },
    })
}
fn content(v: &JsValue) -> PortResult<ContentBlock> {
    object(v)?;
    let body = match tag(
        v,
        "type",
        &[
            "text",
            "thinking",
            "image",
            "pdf",
            "audio",
            "tool_call",
            "tool_result",
            "tool_result_reference",
            "media_reference",
        ],
    )? {
        0 => ContentBody::Text(text(v)?),
        1 => ContentBody::Thinking(ThinkingBlock {
            text: str_field(v, "text")?,
            block_id: opt_string(v, "blockId")?,
            signature: opt_string(v, "signature")?,
            reasoning_content: opt_string(v, "reasoningContent")?,
        }),
        2 => ContentBody::Image(image(v)?),
        3 => ContentBody::Pdf(pdf(v)?),
        4 => ContentBody::Audio(AudioBlock {
            source: source(v)?,
            data: str_field(v, "data")?,
            mime_type: str_field(v, "mimeType")?,
            bytes: opt_number(v, "bytes")?,
            duration_seconds: opt_number(v, "durationSeconds")?,
        }),
        5 => ContentBody::ToolCall(tool_call(v)?),
        6 => ContentBody::ToolResult(ToolResult {
            tool_call_id: str_field(v, "toolCallId")?,
            content: array(field(v, "content")?)?
                .iter()
                .map(inner_content)
                .collect::<PortResult<_>>()?,
            is_error: opt_bool(v, "isError")?,
            raw: echo(v, "raw"),
        }),
        7 => ContentBody::ToolResultReference(ToolResultReference {
            tool_call_id: str_field(v, "toolCallId")?,
            is_error: opt_bool(v, "isError")?,
            path: str_field(v, "path")?,
            read_file_path: opt_string(v, "readFilePath")?,
            original_bytes: num_field(v, "originalBytes")?,
            preview: str_field(v, "preview")?,
            has_more: bool_field(v, "hasMore")?,
            mime_type: opt_string(v, "mimeType")?,
            reason: opt_string(v, "reason")?,
        }),
        _ => ContentBody::MediaReference(MediaReference {
            tool_call_id: opt_string(v, "toolCallId")?,
            path: str_field(v, "path")?,
            original_bytes: num_field(v, "originalBytes")?,
            preview: str_field(v, "preview")?,
            has_more: bool_field(v, "hasMore")?,
            mime_type: str_field(v, "mimeType")?,
            media_type: match tag(v, "mediaType", &["image", "pdf", "audio"])? {
                0 => MediaType::Image,
                1 => MediaType::Pdf,
                _ => MediaType::Audio,
            },
            pages: opt_number(v, "pages")?,
            detail: optional(v, "detail", detail)?,
            reason: opt_string(v, "reason")?,
        }),
    };
    Ok(ContentBlock {
        timeline: optional(v, "timeline", timeline)?,
        body,
    })
}
fn message_metadata(v: &JsValue) -> PortResult<MessageMetadata> {
    object(v)?;
    Ok(MessageMetadata {
        module_id: opt_string(v, "moduleId")?,
        model: opt_string(v, "model")?,
        synthetic: opt_bool(v, "synthetic")?,
        transient: opt_bool(v, "transient")?,
        transient_id: opt_string(v, "transientId")?,
        tool_call_id: opt_string(v, "toolCallId")?,
        compact_replacement: opt_bool(v, "compactReplacement")?,
        compact_snapshot_id: opt_string(v, "compactSnapshotId")?,
        purpose: opt_string(v, "purpose")?,
        trusted_context: optional(v, "trustedContext", |x| {
            if !str_field(x, "scope")?.equals_str("turn") {
                return Err(invalid("Trusted context must be turn scoped"));
            }
            Ok(TrustedContext {
                source: str_field(x, "source")?,
                purpose: match tag(
                    x,
                    "purpose",
                    &["material_context", "skill_context", "application_context"],
                )? {
                    0 => TrustedContextPurpose::Material,
                    1 => TrustedContextPurpose::Skill,
                    _ => TrustedContextPurpose::Application,
                },
                authorized_principal: str_field(x, "authorizedPrincipal")?,
            })
        })?,
        queue_item_id: opt_string(v, "queueItemId")?,
        fork_carryover: optional(v, "forkCarryover", |x| {
            Ok(ForkCarryover {
                source_session_id: str_field(x, "sourceSessionId")?,
                source_turn_id: opt_string(x, "sourceTurnId")?,
            })
        })?,
    })
}
fn normalized_message(v: &JsValue) -> PortResult<Message> {
    Ok(Message {
        role: if tag(v, "role", &["user", "assistant"])? == 0 {
            Role::User
        } else {
            Role::Assistant
        },
        content: array(field(v, "content")?)?
            .iter()
            .map(content)
            .collect::<PortResult<_>>()?,
        metadata: nullable(v, "metadata", message_metadata)?,
    })
}
pub(crate) fn message(v: &JsValue) -> PortResult<ProjectedMessage> {
    object(v)?;
    tag(v, "role", &["user", "assistant"])?;
    array(field(v, "content")?)?;
    let normalized = match normalized_message(v) {
        Ok(m) => MessageProjection::Complete(Box::new(m)),
        Err(_) => MessageProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    };
    Ok(ProjectedMessage {
        boundary: BoundarySnapshot::capture(v),
        normalized,
    })
}
pub(crate) fn messages(v: &JsValue) -> PortResult<Vec<ProjectedMessage>> {
    array(v)?.iter().map(message).collect()
}
pub(crate) fn tool_schema(v: &JsValue) -> PortResult<ToolSchema> {
    object(v)?;
    Ok(ToolSchema {
        name: str_field(v, "name")?,
        description: opt_string(v, "description")?,
        input_schema: metadata(field(v, "inputSchema")?)?,
    })
}
pub(crate) fn cache_plan(v: &JsValue) -> PortResult<CachePlan> {
    object(v)?;
    Ok(CachePlan {
        provider: opt_string(v, "provider")?,
        model: opt_string(v, "model")?,
        system: bool_field(v, "system")?,
        tools: bool_field(v, "tools")?,
        messages: numbers(field(v, "messages")?)?,
        fingerprint: str_field(v, "fingerprint")?,
        generation: num_field(v, "generation")?,
    })
}
fn normalized_request(v: &JsValue) -> PortResult<CanonicalModelRequest> {
    Ok(CanonicalModelRequest {
        provider: str_field(v, "provider")?,
        model: str_field(v, "model")?,
        messages: array(field(v, "messages")?)?
            .iter()
            .map(normalized_message)
            .collect::<PortResult<_>>()?,
        system_prompt: opt_string(v, "systemPrompt")?,
        tools: optional(v, "tools", |x| array(x)?.iter().map(tool_schema).collect())?,
        tool_choice: optional(v, "toolChoice", |x| {
            if let Some(s) = x.as_string() {
                for (n, c) in [
                    ("auto", ToolChoice::Auto),
                    ("none", ToolChoice::None),
                    ("required", ToolChoice::Required),
                ] {
                    if s.equals_str(n) {
                        return Ok(c);
                    }
                }
            }
            if str_field(x, "type")?.equals_str("tool") {
                Ok(ToolChoice::Tool {
                    name: str_field(x, "name")?,
                })
            } else {
                Err(invalid("Invalid tool choice"))
            }
        })?,
        max_output_tokens: opt_number(v, "maxOutputTokens")?,
        temperature: opt_number(v, "temperature")?,
        speed: opt_number(v, "speed")?,
        thinking: optional(v, "thinking", |x| {
            Ok(ThinkingConfig {
                enabled: bool_field(x, "enabled")?,
                mode: optional(x, "mode", |m| {
                    let s = string(m)?;
                    for (n, t) in [
                        ("default", ThinkingMode::Default),
                        ("off", ThinkingMode::Off),
                        ("minimal", ThinkingMode::Minimal),
                        ("low", ThinkingMode::Low),
                        ("medium", ThinkingMode::Medium),
                        ("high", ThinkingMode::High),
                        ("xhigh", ThinkingMode::XHigh),
                        ("max", ThinkingMode::Max),
                    ] {
                        if s.equals_str(n) {
                            return Ok(t);
                        }
                    }
                    Err(invalid("Invalid thinking mode"))
                })?,
                budget_tokens: opt_number(x, "budgetTokens")?,
                preserve: opt_bool(x, "preserve")?,
                split_reasoning: opt_bool(x, "splitReasoning")?,
            })
        })?,
        stream: opt_bool(v, "stream")?,
        metadata: nullable(v, "metadata", metadata)?,
        output_schema: optional(v, "outputSchema", |x| {
            Ok(OutputSchema {
                name: str_field(x, "name")?,
                description: opt_string(x, "description")?,
                schema: metadata(field(x, "schema")?)?,
                strict: opt_bool(x, "strict")?,
            })
        })?,
        cache_breakpoints: optional(v, "cacheBreakpoints", numbers)?,
        cache_plan: optional(v, "cachePlan", cache_plan)?,
    })
}
pub(crate) fn request(v: &JsValue) -> PortResult<CapturedRequest> {
    object(v)?;
    let route = Route {
        provider: str_field(v, "provider")?,
        model: str_field(v, "model")?,
    };
    array(field(v, "messages")?)?;
    let normalized = match normalized_request(v) {
        Ok(r) => RequestProjection::Complete(Box::new(r)),
        Err(_) => RequestProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    };
    Ok(CapturedRequest {
        route,
        normalized,
        wire: PreparedWireRequest::capture(v),
    })
}
/// Original prepare/materialize consumers only require a nonnull object
/// request. Routing is bound externally, never inserted into the wire snapshot.
pub(crate) fn bound_request(v: &JsValue, bound_route: &Route) -> PortResult<CapturedRequest> {
    if !matches!(v, JsValue::Object(_) | JsValue::Array(_)) {
        return Err(invalid(
            "Prepared/materialized request must be nonnull object-like",
        ));
    }
    let normalized = match normalized_request(v) {
        Ok(r) => RequestProjection::Complete(Box::new(r)),
        Err(_) => RequestProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    };
    Ok(CapturedRequest {
        route: bound_route.clone(),
        normalized,
        wire: PreparedWireRequest::capture(v),
    })
}
pub(crate) fn usage(v: &JsValue) -> PortResult<Usage> {
    object(v)?;
    Ok(Usage {
        input_tokens: opt_number(v, "inputTokens")?,
        output_tokens: opt_number(v, "outputTokens")?,
        cache_read_tokens: opt_number(v, "cacheReadTokens")?,
        cache_write_tokens: opt_number(v, "cacheWriteTokens")?,
        total_tokens: opt_number(v, "totalTokens")?,
        native_cost: opt_number(v, "nativeCost")?,
    })
}
/// Pure closed normal-turn projection of the real source reducer output.
/// The full source boundary is authoritative; this grants no host operation
/// and must not turn a projection gap into accepted-effect uncertainty.
/// Owning-turn projection preserves open Source errors and exact projected
/// content. Full ModelError/ToolCall admission belongs to later consumers.
pub(crate) fn turn_assembled(v: &JsValue) -> PortResult<pilotdeck_agent::turn::TurnAssembly> {
    object(v)?;
    Ok(pilotdeck_agent::turn::TurnAssembly {
        boundary: BoundarySnapshot::capture(v),
        message: message(field(v, "message")?)?,
        usage: optional(v, "usage", usage)?,
        has_repaired_tool_calls: opt_bool(v, "hasRepairedToolCalls")?,
        has_message_end: bool_field(v, "hasMessageEnd")?,
    })
}

pub(crate) fn assembled(v: &JsValue) -> PortResult<pilotdeck_agent::AssembledAssistantMessage> {
    object(v)?;
    let boundary = BoundarySnapshot::capture(v);
    let projected = message(field(v, "message")?)?;
    let MessageProjection::Complete(message) = projected.normalized else {
        return Err(invalid(
            "Unimplemented normal-turn projection: assembled message is BoundaryOnly",
        ));
    };
    let finish = str_field(v, "finishReason")?;
    let finish_reason = if finish.equals_str("stop") {
        FinishReason::Stop
    } else if finish.equals_str("length") {
        FinishReason::Length
    } else if finish.equals_str("tool_call") {
        FinishReason::ToolCall
    } else if finish.equals_str("content_filter") {
        FinishReason::ContentFilter
    } else if finish.equals_str("error") {
        FinishReason::Error
    } else {
        FinishReason::Unknown
    };
    Ok(pilotdeck_agent::AssembledAssistantMessage {
        boundary,
        message: *message,
        finish_reason,
        usage: optional(v, "usage", usage)?,
        tool_calls: array(field(v, "toolCalls")?)?
            .iter()
            .map(tool_call)
            .collect::<PortResult<_>>()?,
        error: optional(v, "error", |value| model_error(value).map(Box::new))?,
        has_repaired_tool_calls: opt_bool(v, "hasRepairedToolCalls")?,
        has_message_end: bool_field(v, "hasMessageEnd")?,
    })
}

pub(crate) fn model_error(v: &JsValue) -> PortResult<ModelError> {
    object(v)?;
    Ok(ModelError {
        provider: str_field(v, "provider")?,
        model: opt_string(v, "model")?,
        protocol: protocol(field(v, "protocol")?)?,
        code: str_field(v, "code")?,
        status: opt_number(v, "status")?,
        message: str_field(v, "message")?,
        retryable: bool_field(v, "retryable")?,
        raw: echo(v, "raw"),
        recoverable_via_compact: opt_bool(v, "recoverableViaCompact")?,
        recoverable_via_image_strip: opt_bool(v, "recoverableViaImageStrip")?,
        retry_after_ms: opt_number(v, "retryAfterMs")?,
        user_hint: opt_string(v, "userHint")?,
        settings_fix: optional(v, "settingsFix", |x| {
            Ok(SettingsFix {
                description: str_field(x, "description")?,
                config_path: opt_string(x, "configPath")?,
                command: opt_string(x, "command")?,
                url: opt_string(x, "url")?,
            })
        })?,
        max_context_tokens: opt_number(v, "maxContextTokens")?,
        max_output_tokens: opt_number(v, "maxOutputTokens")?,
        available_output_tokens: opt_number(v, "availableOutputTokens")?,
        stream_interruption: optional(v, "streamInterruption", |x| {
            Ok(StreamInterruption {
                phase: match tag(x, "phase", &["empty", "text", "reasoning", "tool_call"])? {
                    0 => InterruptionPhase::Empty,
                    1 => InterruptionPhase::Text,
                    2 => InterruptionPhase::Reasoning,
                    _ => InterruptionPhase::ToolCall,
                },
                active_tool_calls: optional(x, "activeToolCalls", |a| {
                    array(a)?
                        .iter()
                        .map(|t| {
                            Ok(InterruptedTool {
                                id: str_field(t, "id")?,
                                name: str_field(t, "name")?,
                                argument_chars_utf16: num_field(t, "argumentChars")?,
                            })
                        })
                        .collect()
                })?,
            })
        })?,
    })
}
pub(crate) fn event(v: &JsValue) -> PortResult<CapturedModelEvent> {
    object(v)?;
    let raw = echo(v, "raw");
    let event = match tag(
        v,
        "type",
        &[
            "request_started",
            "message_start",
            "text_delta",
            "thinking_delta",
            "tool_call_start",
            "tool_call_delta",
            "tool_call_end",
            "message_end",
            "usage",
            "error",
        ],
    )? {
        0 => ModelEvent::RequestStarted {
            provider: str_field(v, "provider")?,
            model: str_field(v, "model")?,
            provider_base_url: opt_string(v, "providerBaseUrl")?,
            request_fingerprint: opt_string(v, "requestFingerprint")?,
            metadata: optional(v, "metadata", metadata)?,
        },
        1 => {
            if !str_field(v, "role")?.equals_str("assistant") {
                return Err(invalid("Message start role must be assistant"));
            }
            ModelEvent::MessageStart { raw }
        }
        2 => ModelEvent::TextDelta {
            text: str_field(v, "text")?,
            raw,
        },
        3 => ModelEvent::ThinkingDelta {
            text: str_field(v, "text")?,
            signature: opt_string(v, "signature")?,
            reasoning_content: opt_string(v, "reasoningContent")?,
            raw,
        },
        4 => ModelEvent::ToolCallStart {
            id: str_field(v, "id")?,
            name: str_field(v, "name")?,
            raw,
        },
        5 => ModelEvent::ToolCallDelta {
            id: str_field(v, "id")?,
            delta: str_field(v, "delta")?,
            raw,
        },
        6 => ModelEvent::ToolCallEnd {
            tool_call: tool_call(field(v, "toolCall")?)?,
            was_repaired: opt_bool(v, "wasRepaired")?,
            raw,
        },
        7 => ModelEvent::MessageEnd {
            finish_reason: match tag(
                v,
                "finishReason",
                &[
                    "stop",
                    "length",
                    "tool_call",
                    "content_filter",
                    "error",
                    "unknown",
                ],
            )? {
                0 => FinishReason::Stop,
                1 => FinishReason::Length,
                2 => FinishReason::ToolCall,
                3 => FinishReason::ContentFilter,
                4 => FinishReason::Error,
                _ => FinishReason::Unknown,
            },
            raw,
        },
        8 => ModelEvent::Usage {
            usage: usage(field(v, "usage")?)?,
            raw,
        },
        _ => ModelEvent::Error(Box::new(model_error(field(v, "error")?)?)),
    };
    Ok(CapturedModelEvent {
        event,
        boundary: BoundarySnapshot::capture(v),
    })
}
fn tool_content(v: &JsValue) -> ToolContent {
    let boundary = BoundarySnapshot::capture(v);
    let read = || -> PortResult<ToolContent> {
        Ok(
            match tag(v, "type", &["text", "json", "image", "pdf", "file"])? {
                0 => ToolContent::Text {
                    text: str_field(v, "text")?,
                    boundary: boundary.clone(),
                },
                1 => ToolContent::Json {
                    value: v.get("value").cloned().unwrap_or(JsValue::Undefined),
                    boundary: boundary.clone(),
                },
                2 => ToolContent::Image {
                    image: image(v)?,
                    boundary: boundary.clone(),
                },
                3 => ToolContent::Pdf {
                    pdf: pdf(v)?,
                    boundary: boundary.clone(),
                },
                _ => ToolContent::File {
                    path: str_field(v, "path")?,
                    mime_type: opt_string(v, "mimeType")?,
                    description: opt_string(v, "description")?,
                    boundary: boundary.clone(),
                },
            },
        )
    };
    read().unwrap_or(ToolContent::BoundaryOnly { boundary })
}
pub(crate) fn tool_result(v: &JsValue) -> PortResult<ToolExecutionResult> {
    object(v)?;
    let disposition = match tag(v, "type", &["success", "error"])? {
        0 => ToolResultDisposition::Success {
            data: match v.get("data") {
                None | Some(JsValue::Undefined) => None,
                Some(x) => Some(x.clone()),
            },
        },
        _ => {
            let e = field(v, "error")?;
            ToolResultDisposition::Error {
                code: str_field(e, "code")?,
                message: str_field(e, "message")?,
                fields: metadata(e)?,
            }
        }
    };
    Ok(ToolExecutionResult {
        tool_call_id: str_field(v, "toolCallId")?,
        tool_name: str_field(v, "toolName")?,
        disposition,
        content: array(field(v, "content")?)?
            .iter()
            .map(tool_content)
            .collect(),
        supplemental_messages: nullable(v, "supplementalMessages", |a| {
            array(a)?
                .iter()
                .map(|m| {
                    Ok(SupplementalToolMessage {
                        content: array(field(m, "content")?)?
                            .iter()
                            .map(tool_content)
                            .collect(),
                        boundary: BoundarySnapshot::capture(m),
                    })
                })
                .collect()
        })?,
        metadata: nullable(v, "metadata", metadata)?,
        started_at: str_field(v, "startedAt")?,
        completed_at: str_field(v, "completedAt")?,
        boundary: BoundarySnapshot::capture(v),
    })
}
