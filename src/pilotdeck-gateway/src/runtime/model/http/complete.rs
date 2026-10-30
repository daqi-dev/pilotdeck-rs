use pilotdeck_protocol::{JsObject, JsString, JsValue, js_math::min as js_math_min, parse_json};

use super::{
    ErrorInput, ModelHttpTransport, ModelStreamError, StreamOptions, delay, field, is,
    is_premature_stream_close, normalize_model_error, object, protocol, provider_id, providers,
    put, response_text, retry_number, send_provider_request, throw_if_aborted,
};

// ---------------------------------------------------------------------------
// Non-streaming complete path (`complete` / `parseModelResponse` in
// streamModel.ts), moved from http.rs byte for byte (#1111).

/// `parseModelResponse(protocol, raw, providerId)` — converts provider-specific
/// complete response to canonical format. Ports `parseModelResponse.ts` and
/// provider-specific `parse*Response` functions.
fn parse_complete_response(
    protocol: &JsValue,
    raw: &JsValue,
    provider_id: &JsValue,
) -> Result<JsValue, ModelStreamError> {
    let protocol_str = protocol
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_default();

    match protocol_str.as_str() {
        "anthropic" => Ok(parse_anthropic_complete_response(raw)),
        "openai-responses" => parse_openai_responses_complete_response(raw, provider_id),
        _ => parse_openai_complete_response(raw, provider_id), // default: openai
    }
}

/// `asRecord(value)`: a plain object, otherwise `{}`.
fn empty_record() -> &'static JsValue {
    static EMPTY: std::sync::OnceLock<JsValue> = std::sync::OnceLock::new();
    EMPTY.get_or_init(|| JsValue::Object(JsObject::new()))
}

fn as_record(value: &JsValue) -> &JsValue {
    if matches!(value, JsValue::Object(_)) {
        value
    } else {
        empty_record()
    }
}

fn js_text(value: &JsString) -> String {
    match value.to_utf8() {
        Ok(text) => text,
        Err(_) => String::from_utf16_lossy(value.units()),
    }
}

/// `typeof value === "string" ? value : ""`.
fn string_or_empty(value: &JsValue) -> JsValue {
    JsValue::String(value.as_string().cloned().unwrap_or_default())
}

/// `typeof value === "string" && value.length > 0`.
fn non_empty_string(value: &JsValue) -> Option<&JsString> {
    value.as_string().filter(|text| !text.units().is_empty())
}

/// `readNonEmptyString`: non-empty after JS `trim`, returned untrimmed.
fn non_empty_trimmed_string(value: &JsValue) -> Option<&JsString> {
    let text = value.as_string()?;
    let trimmed = match text.to_utf8() {
        Ok(raw) => crate::metadata::js_trim(&raw).to_string(),
        Err(_) => String::from_utf16_lossy(text.units()),
    };
    (!trimmed.is_empty()).then_some(text)
}

/// `safeToolCallIdPart(value)`.
fn safe_tool_call_id_part(value: &JsString) -> JsString {
    let raw = String::from_utf16_lossy(value.units());
    let trimmed = crate::metadata::js_trim(&raw);
    let mut out = String::new();
    let mut replaced = false;
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            out.push(ch);
            replaced = false;
        } else if !replaced {
            out.push('_');
            replaced = true;
        }
    }
    let out = out.trim_matches('_');
    if out.is_empty() {
        "response".into()
    } else {
        out.into()
    }
}

/// `createToolCallIdState` / `createResponseToolCallIdState`.
struct CompleteToolCallIdState {
    base_id: JsString,
    used: Vec<JsString>,
}

impl CompleteToolCallIdState {
    fn new(response: &JsValue) -> Self {
        let raw = non_empty_trimmed_string(field(response, "id"))
            .cloned()
            .unwrap_or_else(|| {
                let uuid = uuid::Uuid::new_v4().to_string();
                format!("response_{}", &uuid[..12]).into()
            });
        Self {
            base_id: safe_tool_call_id_part(&raw),
            used: Vec::new(),
        }
    }

    fn choose(&mut self, incoming: Option<JsString>, generated: String) -> JsString {
        let candidate = match incoming {
            Some(id) if !self.used.iter().any(|used| used == &id) => id,
            _ => generated.into(),
        };
        let mut id = candidate;
        if self.used.iter().any(|used| used == &id) {
            let base = js_text(&id);
            let mut suffix = 2u64;
            loop {
                let next: JsString = format!("{base}_{suffix}").into();
                if !self.used.iter().any(|used| used == &next) {
                    id = next;
                    break;
                }
                suffix += 1;
            }
        }
        self.used.push(id.clone());
        id
    }
}

/// `new ModelProviderError({provider, protocol, code, message, retryable, raw})`
/// for `invalid_tool_arguments`.
fn invalid_tool_arguments(
    provider: &JsString,
    protocol: &str,
    message: &str,
    raw: &JsValue,
) -> ModelStreamError {
    let mut error = JsObject::new();
    error.insert("provider".into(), JsValue::String(provider.clone()));
    error.insert("protocol".into(), JsValue::String(protocol.into()));
    error.insert(
        "code".into(),
        JsValue::String("invalid_tool_arguments".into()),
    );
    error.insert("message".into(), JsValue::String(message.into()));
    error.insert("retryable".into(), JsValue::Bool(true));
    error.insert("raw".into(), raw.clone());
    ModelStreamError::Provider(error)
}

fn parse_tool_call_input(raw_arguments: &JsString) -> Option<JsValue> {
    providers::parse_tool_arguments(raw_arguments.units()).map(|(input, _)| input)
}

fn complete_provider_name(provider_id: &JsValue, default: &str) -> JsString {
    provider_id
        .as_string()
        .cloned()
        .unwrap_or_else(|| default.into())
}

/// Parses Anthropic complete response to canonical format.
/// Ports `parseAnthropicResponse` from `anthropic/response.ts`.
fn parse_anthropic_complete_response(raw: &JsValue) -> JsValue {
    let response = as_record(raw);
    let mut content: Vec<JsValue> = Vec::new();
    if let JsValue::Array(blocks) = field(response, "content") {
        for block in blocks {
            let record = as_record(block);
            match field(record, "type").as_string().map(js_text).as_deref() {
                Some("text") => content.push(object([
                    ("type", "text".into()),
                    ("text", string_or_empty(field(record, "text"))),
                ])),
                Some("thinking") => {
                    let text = field(record, "thinking")
                        .as_string()
                        .or_else(|| field(record, "text").as_string())
                        .cloned()
                        .unwrap_or_default();
                    let mut thinking =
                        object([("type", "thinking".into()), ("text", JsValue::String(text))]);
                    if let Some(signature) = non_empty_string(field(record, "signature")) {
                        put(
                            &mut thinking,
                            "signature",
                            JsValue::String(signature.clone()),
                        );
                    }
                    content.push(thinking);
                }
                Some("tool_use") => content.push(object([
                    ("type", "tool_call".into()),
                    ("id", string_or_empty(field(record, "id"))),
                    ("name", string_or_empty(field(record, "name"))),
                    ("input", field(record, "input").clone()),
                    ("raw", block.clone()),
                ])),
                _ => {}
            }
        }
    }

    object([
        ("role", "assistant".into()),
        ("content", JsValue::Array(content)),
        ("usage", normalize_anthropic_usage(field(response, "usage"))),
        (
            "finishReason",
            normalize_anthropic_finish_reason(field(response, "stop_reason")),
        ),
        ("raw", raw.clone()),
    ])
}

/// Parses OpenAI complete response to canonical format.
/// Ports `parseOpenAIResponse` from `openai/response.ts`.
fn parse_openai_complete_response(
    raw: &JsValue,
    provider_id: &JsValue,
) -> Result<JsValue, ModelStreamError> {
    let response = as_record(raw);
    let provider = complete_provider_name(provider_id, "openai");
    let choices = match field(response, "choices") {
        JsValue::Array(choices) => choices.as_slice(),
        _ => &[],
    };
    let first_choice = match choices.first() {
        Some(choice) => as_record(choice),
        None => empty_record(),
    };
    let choice_index = field(first_choice, "index").as_number().unwrap_or(0.0);
    let message = as_record(field(first_choice, "message"));
    let mut content: Vec<JsValue> = Vec::new();
    let mut id_state = CompleteToolCallIdState::new(response);

    if let Some(reasoning) = read_reasoning_text(field(message, "reasoning_content"))
        .or_else(|| read_reasoning_text(field(message, "reasoning")))
    {
        content.push(object([
            ("type", "thinking".into()),
            ("text", JsValue::String(reasoning.clone())),
            ("reasoningContent", JsValue::String(reasoning.clone())),
        ]));
    }
    if let JsValue::Array(details) = field(message, "reasoning_details") {
        let mut text: Vec<u16> = Vec::new();
        for part in details {
            if let Some(detail) = read_reasoning_detail_text(as_record(part)) {
                if !text.is_empty() {
                    text.push('\n' as u16);
                }
                text.extend(detail.units());
            }
        }
        if !text.is_empty() {
            let text = JsString::from_utf16(text);
            content.push(object([
                ("type", "thinking".into()),
                ("text", JsValue::String(text.clone())),
                ("reasoningContent", JsValue::String(text)),
            ]));
        }
    }

    match field(message, "content") {
        JsValue::String(text) if !text.units().is_empty() => content.push(object([
            ("type", "text".into()),
            ("text", JsValue::String(text.clone())),
        ])),
        JsValue::Array(parts) => {
            for part in parts {
                let record = as_record(part);
                if field(record, "type")
                    .as_string()
                    .is_some_and(|kind| kind.equals_str("text"))
                    && let Some(text) = non_empty_string(field(record, "text"))
                {
                    content.push(object([
                        ("type", "text".into()),
                        ("text", JsValue::String(text.clone())),
                    ]));
                }
            }
        }
        _ => {}
    }

    if let JsValue::Array(tool_calls) = field(message, "tool_calls") {
        for (tool_index, tool_call) in tool_calls.iter().enumerate() {
            let record = as_record(tool_call);
            let function = as_record(field(record, "function"));
            let raw_arguments = field(function, "arguments")
                .as_string()
                .cloned()
                .unwrap_or_else(|| "{}".into());
            let Some(input) = parse_tool_call_input(&raw_arguments) else {
                return Err(invalid_tool_arguments(
                    &provider,
                    "openai",
                    "OpenAI tool call arguments are not valid JSON.",
                    tool_call,
                ));
            };
            let generated = format!(
                "call_{}_{}_{}",
                js_text(&id_state.base_id),
                pilotdeck_protocol::number::number_to_string(choice_index),
                tool_index
            );
            // TS `readNonEmptyString(record.id)`: trim only decides emptiness;
            // a non-blank id is returned untrimmed (streamModel's sibling and
            // the Responses parser already use this helper).
            let id = id_state.choose(
                non_empty_trimmed_string(field(record, "id")).cloned(),
                generated,
            );
            content.push(object([
                ("type", "tool_call".into()),
                ("id", JsValue::String(id)),
                ("name", string_or_empty(field(function, "name"))),
                ("input", input),
                ("raw", tool_call.clone()),
            ]));
        }
    }

    Ok(object([
        ("role", "assistant".into()),
        ("content", JsValue::Array(content)),
        (
            "usage",
            providers::openai::normalize_usage(field(response, "usage"))
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "finishReason",
            normalize_openai_finish_reason(field(first_choice, "finish_reason")),
        ),
        ("raw", raw.clone()),
    ]))
}

/// `readReasoningText(value)`: a non-empty string, else the detail fields of
/// an object.
fn read_reasoning_text(value: &JsValue) -> Option<&JsString> {
    non_empty_string(value).or_else(|| read_reasoning_detail_text(as_record(value)))
}

/// `readReasoningDetailText(part)`.
fn read_reasoning_detail_text(part: &JsValue) -> Option<&JsString> {
    ["text", "content", "reasoning", "summary"]
        .iter()
        .find_map(|key| non_empty_string(field(part, key)))
}

/// `readTextPart(part)` for a Responses message/reasoning summary part.
fn read_text_part(part: &JsValue) -> Option<&JsString> {
    non_empty_string(field(part, "text")).or_else(|| non_empty_string(field(part, "output_text")))
}

fn text_block(text: &JsString) -> JsValue {
    object([
        ("type", "text".into()),
        ("text", JsValue::String(text.clone())),
    ])
}

/// Parses OpenAI-responses complete response to canonical format.
/// Ports `parseOpenAIResponsesResponse` from `openai-responses/response.ts`.
fn parse_openai_responses_complete_response(
    raw: &JsValue,
    provider_id: &JsValue,
) -> Result<JsValue, ModelStreamError> {
    let response = as_record(raw);
    let provider = complete_provider_name(provider_id, "openai-responses");
    let output_text = field(response, "output_text");
    let mut content: Vec<JsValue> = Vec::new();
    let mut id_state = CompleteToolCallIdState::new(response);

    if let Some(text) = non_empty_string(output_text) {
        content.push(text_block(text));
    }

    let output = match field(response, "output") {
        JsValue::Array(output) => output.as_slice(),
        _ => &[],
    };
    for (index, item) in output.iter().enumerate() {
        let record = as_record(item);
        match field(record, "type").as_string().map(js_text).as_deref() {
            Some("message") => {
                if let JsValue::Array(parts) = field(record, "content") {
                    for part in parts {
                        if let Some(text) = read_text_part(as_record(part)) {
                            content.push(text_block(text));
                        }
                    }
                }
            }
            Some("function_call") => {
                let raw_arguments = field(record, "arguments")
                    .as_string()
                    .cloned()
                    .unwrap_or_else(|| "{}".into());
                let Some(input) = parse_tool_call_input(&raw_arguments) else {
                    return Err(invalid_tool_arguments(
                        &provider,
                        "openai-responses",
                        "OpenAI Responses tool call arguments are not valid JSON.",
                        item,
                    ));
                };
                let incoming = non_empty_trimmed_string(field(record, "call_id"))
                    .or_else(|| non_empty_trimmed_string(field(record, "id")))
                    .cloned();
                let generated = format!("call_{}_{}", js_text(&id_state.base_id), index);
                let id = id_state.choose(incoming, generated);
                content.push(object([
                    ("type", "tool_call".into()),
                    ("id", JsValue::String(id)),
                    ("name", string_or_empty(field(record, "name"))),
                    ("input", input),
                    ("raw", item.clone()),
                ]));
            }
            Some("reasoning") => {
                if let JsValue::Array(summary) = field(record, "summary") {
                    let mut text: Vec<u16> = Vec::new();
                    for part in summary {
                        if let Some(detail) = read_text_part(as_record(part)) {
                            if !text.is_empty() {
                                text.push('\n' as u16);
                            }
                            text.extend(detail.units());
                        }
                    }
                    if !text.is_empty() {
                        content.push(object([
                            ("type", "thinking".into()),
                            ("text", JsValue::String(JsString::from_utf16(text))),
                        ]));
                    }
                }
            }
            _ => {}
        }
    }

    dedupe_initial_output_text(&mut content, output_text);

    Ok(object([
        ("role", "assistant".into()),
        ("content", JsValue::Array(content)),
        (
            "usage",
            providers::openai::normalize_usage(field(response, "usage"))
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "finishReason",
            normalize_responses_finish_reason(response, output),
        ),
        ("raw", raw.clone()),
    ]))
}

/// `dedupeInitialOutputText`: when all remaining text blocks joined by `\n`
/// repeat `output_text`, drop the leading `output_text` block.
fn dedupe_initial_output_text(content: &mut Vec<JsValue>, output_text: &JsValue) {
    let Some(output_text) = non_empty_string(output_text) else {
        return;
    };
    let Some(first) = content.first() else {
        return;
    };
    if !field(first, "type")
        .as_string()
        .is_some_and(|kind| kind.equals_str("text"))
    {
        return;
    }
    let mut rest: Vec<u16> = Vec::new();
    for block in &content[1..] {
        if field(block, "type")
            .as_string()
            .is_some_and(|kind| kind.equals_str("text"))
            && let Some(text) = field(block, "text").as_string()
        {
            if !rest.is_empty() {
                rest.push('\n' as u16);
            }
            rest.extend(text.units());
        }
    }
    if JsString::from_utf16(rest) == *output_text {
        content.remove(0);
    }
}

/// `normalizeAnthropicUsage`.
fn normalize_anthropic_usage(raw: &JsValue) -> JsValue {
    if !matches!(raw, JsValue::Object(_)) {
        return JsValue::Undefined;
    }
    let read = |key: &str| {
        field(raw, key)
            .as_number()
            .filter(|value| value.is_finite())
    };
    let input = read("input_tokens");
    let output = read("output_tokens");
    let cache_read = read("cache_read_input_tokens");
    let cache_write = read("cache_creation_input_tokens");
    let defined: Vec<f64> = [input, output, cache_read, cache_write]
        .into_iter()
        .flatten()
        .collect();
    if defined.is_empty() {
        return JsValue::Undefined;
    }
    let total = defined.iter().sum::<f64>();
    let mut usage = JsObject::new();
    for (key, value) in [
        ("inputTokens", input),
        ("outputTokens", output),
        ("cacheReadTokens", cache_read),
        ("cacheWriteTokens", cache_write),
        ("totalTokens", Some(total)),
    ] {
        usage.insert(
            key.into(),
            value.map_or(JsValue::Undefined, JsValue::Number),
        );
    }
    JsValue::Object(usage)
}

/// Normalizes Anthropic finish reason to canonical format.
fn normalize_anthropic_finish_reason(stop_reason: &JsValue) -> JsValue {
    match stop_reason.as_string().map(js_text).as_deref() {
        Some("end_turn" | "stop_sequence") => "stop".into(),
        Some("max_tokens") => "length".into(),
        Some("tool_use") => "tool_call".into(),
        Some("refusal") => "content_filter".into(),
        _ => "unknown".into(),
    }
}

/// Normalizes OpenAI finish reason to canonical format.
fn normalize_openai_finish_reason(finish_reason: &JsValue) -> JsValue {
    match finish_reason.as_string().map(js_text).as_deref() {
        Some("stop") => "stop".into(),
        Some("length") => "length".into(),
        Some("tool_calls" | "function_call") => "tool_call".into(),
        Some("content_filter" | "content_filter_results") => "content_filter".into(),
        _ => "unknown".into(),
    }
}

/// `normalizeResponsesFinishReason`.
fn normalize_responses_finish_reason(response: &JsValue, output: &[JsValue]) -> JsValue {
    if output.iter().any(|item| {
        field(as_record(item), "type")
            .as_string()
            .is_some_and(|t| t.equals_str("function_call"))
    }) {
        return "tool_call".into();
    }
    match field(response, "status")
        .as_string()
        .map(js_text)
        .as_deref()
    {
        Some("completed") => "stop".into(),
        Some("incomplete") => "length".into(),
        Some("failed" | "cancelled") => "error".into(),
        _ => "unknown".into(),
    }
}

/// `createNativeRetryPolicy({defaultMaxRetries: 2, defaultJitter: 0})` for
/// `kind: "request"` (complete uses a linear, unjittered request retry).
pub(super) fn retry_decide_request(provider: &JsValue, attempt: f64, random: f64) -> (f64, f64) {
    let max_retries = retry_number(provider, "requestMaxRetries").unwrap_or(2.0);
    let max_delay = retry_number(provider, "maxDelayMs").unwrap_or(8_000.0);
    let base = retry_number(provider, "baseDelayMs").unwrap_or(500.0);
    let jitter = retry_number(provider, "jitter").unwrap_or(0.0);
    let deterministic = base * (attempt + 1.0);
    (
        max_retries,
        js_math_min(deterministic + deterministic * jitter * random, max_delay),
    )
}

/// `isRetryableRequestError` (`streamModel.ts:565`): `isAbortError` is the
/// first test and wins over a premature-close message. It matches either
/// `name === "AbortError"` or a message containing `aborted`, so a
/// `ModelProviderError` (retryable: true) with "request aborted" is not
/// retried. A provider error retries on its own flag or a premature-close
/// message; other errors retry on a network-ish message.
pub(super) fn is_retryable_request_error(error: &ModelStreamError) -> bool {
    if error.is_abort() {
        return false;
    }
    let message = String::from_utf16_lossy(error.message().units()).to_ascii_lowercase();
    if is_premature_stream_close(&message) {
        return true;
    }
    match error {
        ModelStreamError::Provider(canonical) => {
            canonical.get("retryable") == Some(&JsValue::Bool(true))
        }
        _ => [
            "network",
            "econnreset",
            "socket hang up",
            "fetch failed",
            "timeout",
            "etimedout",
            "epipe",
            "econnrefused",
        ]
        .iter()
        .any(|needle| message.contains(needle)),
    }
}

/// `complete(request, config, options)` — non-streaming HTTP path.
/// `streamModel.ts:76`: sends `stream: false`, reads the full body, returns the
/// parsed response object. HTTP status errors throw immediately (the baseline
/// does not retry a non-2xx complete); only transport failures use the linear
/// request retry policy.
pub async fn complete_model(
    transport: &dyn ModelHttpTransport,
    request: &JsValue,
    config: &JsValue,
    options: &StreamOptions,
) -> Result<JsValue, ModelStreamError> {
    let mut current = request.clone();
    put(&mut current, "stream", JsValue::Bool(false));
    let (provider, _) = providers::validate_model_request(&current, config)?;

    // P1-2: Google protocol not supported in complete path
    if is(field(&provider, "protocol"), "google") {
        return Err(ModelStreamError::thrown(
            "UnsupportedProtocol",
            "complete(): Google protocol is not supported in non-streaming mode",
        ));
    }

    let (max_retries, _) = retry_decide_request(&provider, 0.0, 0.0);
    let mut attempt = 0.0;
    // `streamModel.ts:92`: `for (attempt = 0; attempt <= maxRetries; attempt++)`.
    // A fractional limit such as 1.5 therefore runs attempts 0 and 1 only and
    // falls out of the loop into the exhausted error below.
    while attempt <= max_retries {
        throw_if_aborted(options)?;
        let (provider_now, model_now) = providers::validate_model_request(&current, config)?;
        let body = providers::build_model_request(&current, &provider_now, &model_now)?;
        let response =
            match send_provider_request(transport, &provider, &body, options, false).await {
                Ok(response) => response,
                Err(error) => {
                    if attempt < max_retries && is_retryable_request_error(&error) {
                        let (_, delay_ms) =
                            retry_decide_request(&provider, attempt, (options.random)());
                        let _ = delay(delay_ms, options).await?;
                        attempt += 1.0;
                        continue;
                    }
                    return Err(error);
                }
            };
        let status = response.status().as_u16();
        let (mut body_stream, before_abort) = response.into_body();
        // TS `response.text()` is outside the send try/catch: a mid-body
        // failure is not a retryable send error and must not re-enter the loop.
        let text = response_text(&mut body_stream, options, before_abort).await?;
        // `parseRawJson`: invalid/empty success bodies stay as the raw text.
        let raw = parse_json(&text).unwrap_or_else(|_| JsValue::String(text.as_str().into()));

        if !(200..300).contains(&status) {
            let error = normalize_model_error(
                &provider_id(&provider),
                &protocol(&provider),
                ErrorInput::Value(&raw),
                Some(f64::from(status)),
            );
            return Err(ModelStreamError::Provider(error));
        }

        let protocol_value = field(&provider, "protocol").clone();
        let provider_id_value = field(&provider, "id").clone();
        return parse_complete_response(&protocol_value, &raw, &provider_id_value);
    }

    // `streamModel.ts:154`: only a fractional/invalid limit that lets attempts
    // run without the final failure being rethrown reaches this branch.
    Err(ModelStreamError::thrown(
        "Error",
        "complete() exhausted all retry attempts without a result.",
    ))
}
