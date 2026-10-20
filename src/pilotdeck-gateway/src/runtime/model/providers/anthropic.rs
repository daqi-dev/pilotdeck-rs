//! `providers/anthropic/{request,stream}.ts`, `thinking/registry.ts` (the
//! parts the Anthropic body reads) and `response/normalize{FinishReason,Usage}.ts`.
//!
//! Content blocks, tool choice and cache markers share the count-tokens
//! projection in `count_body.rs`, which ports the same `toAnthropic*` helpers.
use super::super::count_body::{
    anthropic_block, array, cache_control, concatenate, field, is, object, present, put,
    tool_choice, truthy,
};
use crate::runtime::model::http::{ModelStreamError, retry_after_from_message};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};

fn units(value: &JsValue) -> JsString {
    JsString::from_utf16(crate::js_value::to_string_units(value).unwrap_or_default())
}
const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// The fields of `ThinkingPlan` that `buildAnthropicRequest` consumes.
#[derive(Default)]
struct ThinkingPlan {
    thinking_type: Option<&'static str>,
    effort: Option<JsString>,
    anthropic_output_effort: bool,
}

/// `resolveThinkingPlan` + `throwIfUnsupportedThinkingPlan` for an Anthropic
/// provider. Non-Anthropic formats only matter here through their failures.
fn thinking_plan(request: &JsValue, model: &JsValue) -> Result<ThinkingPlan, ModelStreamError> {
    let settings = field(model, "thinking");
    let state = field(settings, "state");
    if !present(state) || is(state, "default") {
        return Ok(ThinkingPlan::default());
    }
    let off = is(state, "disabled");
    let requested = field(field(request, "thinking"), "mode");
    let effort = (!off)
        .then(|| EFFORTS.iter().find(|e| is(requested, e)).copied())
        .flatten();
    let fail = |message: &str| {
        let mut text: Vec<u16> = units(field(model, "id")).units().to_vec();
        text.extend(": ".encode_utf16());
        text.extend(message.encode_utf16());
        Err(ModelStreamError::Request {
            code: "unsupported_thinking".into(),
            message: JsString::from_utf16(text),
        })
    };
    if let Some(effort) = effort {
        let configured = match field(settings, "efforts") {
            JsValue::Array(efforts) => efforts.iter().any(|e| is(e, effort)),
            _ => false,
        };
        if !configured {
            return fail(&format!(
                "Reasoning effort '{effort}' is not configured. Select Default or configure the model's supported efforts."
            ));
        }
    }
    let format = field(settings, "format");
    let format = if !present(format) || is(format, "provider") {
        "anthropic".to_string()
    } else {
        String::from_utf16_lossy(units(format).units())
    };
    let model_id = String::from_utf16_lossy(units(field(model, "id")).units());
    match format.as_str() {
        "server-default" => {
            if off || effort.is_some() {
                return fail(
                    "This parameter format only supports the server default. Choose a thinking parameter format in model settings.",
                );
            }
            Ok(ThinkingPlan::default())
        }
        "openai" => Ok(ThinkingPlan::default()),
        "anthropic" => {
            if off {
                return Ok(ThinkingPlan {
                    thinking_type: Some("disabled"),
                    ..Default::default()
                });
            }
            let budget_only = super::super::count_body::budget_only_model(&model_id);
            if budget_only {
                if effort.is_some() {
                    return fail(
                        "This model requires a thinking budget; only Default is supported.",
                    );
                }
                return Ok(ThinkingPlan::default());
            }
            Ok(ThinkingPlan {
                thinking_type: Some("adaptive"),
                effort: effort.map(JsString::from),
                anthropic_output_effort: true,
            })
        }
        "google" => {
            if off {
                return fail(
                    "Turning off thinking is not supported by this effort-only Gemini adapter. Uncheck both thinking options to use the server default.",
                );
            }
            if !regex::Regex::new("gemini-?3")
                .expect("static regex")
                .is_match(&model_id)
            {
                if effort.is_some() {
                    return fail(
                        "This Gemini model has no supported thinking-level adapter. Select Default.",
                    );
                }
                return Ok(ThinkingPlan::default());
            }
            if let Some(effort) = effort.filter(|e| !["low", "medium", "high"].contains(e)) {
                return fail(&format!(
                    "Gemini thinkingLevel does not support '{effort}'."
                ));
            }
            Ok(ThinkingPlan::default())
        }
        // Other formats only patch OpenAI-style bodies; resolveFormat never
        // yields them from `provider` for an Anthropic provider.
        _ => Ok(ThinkingPlan::default()),
    }
}

/// `buildAnthropicRequest(request, model, provider)`.
pub(crate) fn build_request(
    request: &JsValue,
    model: &JsValue,
    provider: &JsValue,
) -> Result<JsValue, ModelStreamError> {
    let plan = thinking_plan(request, model)?;
    let mut tools: Vec<JsValue> = array(field(request, "tools"))
        .iter()
        .map(|tool| {
            object([
                ("name", field(tool, "name").clone()),
                ("description", field(tool, "description").clone()),
                ("input_schema", field(tool, "inputSchema").clone()),
            ])
        })
        .collect();
    let schema = field(request, "outputSchema");
    let mut choice = tool_choice(field(request, "toolChoice"), true);
    if truthy(schema) {
        let description = field(schema, "description");
        tools.insert(
            0,
            object([
                ("name", "__output__".into()),
                (
                    "description",
                    if present(description) {
                        description.clone()
                    } else {
                        concatenate(&[
                            "Return the final structured payload (schema name: ".into(),
                            field(schema, "name").clone(),
                            "). Always call this tool exactly once.".into(),
                        ])
                    },
                ),
                ("input_schema", field(schema, "schema").clone()),
            ]),
        );
        if field(schema, "strict") != &JsValue::Bool(false) {
            choice = object([("type", "tool".into()), ("name", "__output__".into())]);
        }
    }
    let plan_value = field(request, "cachePlan");
    let cache_tools = field(plan_value, "tools") == &JsValue::Bool(true) && !tools.is_empty();
    if cache_tools {
        put(
            tools.last_mut().expect("nonempty"),
            "cache_control",
            cache_control(),
        );
    }
    let breakpoints = if present(field(plan_value, "messages")) {
        field(plan_value, "messages")
    } else {
        field(request, "cacheBreakpoints")
    };
    let breakpoints = array(breakpoints);
    let keep = if cache_tools { 2 } else { 3 };
    let breakpoints = &breakpoints[breakpoints.len().saturating_sub(keep)..];
    let messages = array(field(request, "messages"))
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let mut content: Vec<_> = array(field(message, "content"))
                .iter()
                .map(anthropic_block)
                .collect();
            if breakpoints
                .iter()
                .any(|b| b.as_number() == Some(index as f64))
                && let Some(block) = content
                    .iter_mut()
                    .rev()
                    .find(|b| b.as_object().is_some() && !is(field(b, "type"), "thinking"))
            {
                put(block, "cache_control", cache_control());
            }
            object([
                ("role", field(message, "role").clone()),
                ("content", JsValue::Array(content)),
            ])
        })
        .collect();
    let max_tokens = match field(request, "maxOutputTokens") {
        JsValue::Undefined | JsValue::Null => {
            field(field(model, "capabilities"), "maxOutputTokens").clone()
        }
        value => value.clone(),
    };
    let prompt = field(request, "systemPrompt");
    let system = if !truthy(prompt) {
        JsValue::Undefined
    } else if field(plan_value, "system") == &JsValue::Bool(true)
        || (plan_value.is_undefined() && !field(request, "cacheBreakpoints").is_undefined())
    {
        JsValue::Array(vec![object([
            ("type", "text".into()),
            ("text", prompt.clone()),
            ("cache_control", cache_control()),
        ])])
    } else {
        prompt.clone()
    };
    let speed = field(request, "speed");
    let speed = if !speed.is_undefined()
        && field(field(model, "capabilities"), "supportsSpeed") == &JsValue::Bool(true)
        && is(field(provider, "speedMapping"), "anthropic_speed")
        && speed.as_number().is_some_and(|s| s >= 0.5)
    {
        "fast".into()
    } else {
        JsValue::Undefined
    };
    let metadata = match field(field(request, "metadata"), "user_id") {
        JsValue::String(id) if !id.is_empty() => object([("user_id", JsValue::String(id.clone()))]),
        _ => JsValue::Undefined,
    };
    Ok(object([
        ("model", field(request, "model").clone()),
        ("max_tokens", max_tokens),
        ("messages", JsValue::Array(messages)),
        ("system", system),
        (
            "tools",
            if tools.is_empty() {
                JsValue::Undefined
            } else {
                JsValue::Array(tools)
            },
        ),
        ("tool_choice", choice),
        ("speed", speed),
        (
            "thinking",
            plan.thinking_type
                .map_or(JsValue::Undefined, |t| object([("type", t.into())])),
        ),
        (
            "output_config",
            match (&plan.effort, plan.anthropic_output_effort) {
                (Some(effort), true) => object([("effort", JsValue::String(effort.clone()))]),
                _ => JsValue::Undefined,
            },
        ),
        ("stream", field(request, "stream").clone()),
        ("metadata", metadata),
    ]))
}

/// `normalizeAnthropicFinishReason`.
fn finish_reason(reason: &JsValue) -> &'static str {
    match reason.as_string().and_then(|s| s.to_utf8().ok()).as_deref() {
        Some("end_turn" | "stop_sequence") => "stop",
        Some("max_tokens") => "length",
        Some("tool_use") => "tool_call",
        Some("refusal") => "content_filter",
        _ => "unknown",
    }
}

/// `normalizeAnthropicUsage`.
fn usage(raw: &JsValue) -> Option<JsValue> {
    let JsValue::Object(_) = raw else {
        return None;
    };
    let number = |key: &str| match field(raw, key) {
        JsValue::Number(n) if n.is_finite() => Some(*n),
        _ => None,
    };
    let values = [
        number("input_tokens"),
        number("output_tokens"),
        number("cache_read_input_tokens"),
        number("cache_creation_input_tokens"),
    ];
    let total = values.iter().flatten().copied().reduce(|a, b| a + b);
    if values.iter().all(Option::is_none) && total.is_none() {
        return None;
    }
    let mut out = JsObject::new();
    for (key, value) in [
        "inputTokens",
        "outputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
        "totalTokens",
    ]
    .iter()
    .zip(values.iter().copied().chain([total]))
    {
        out.insert(
            (*key).into(),
            value.map_or(JsValue::Undefined, JsValue::Number),
        );
    }
    Some(JsValue::Object(out))
}

/// UTF-16 JSON text as a Rust string for `parse_json`. A lone surrogate is
/// only legal inside a JSON string, where `\uXXXX` denotes the same unit; one
/// that would follow an escaping backslash stays invalid (U+FFFD), and
/// anywhere else both forms are a syntax error, as in `JSON.parse`.
fn json_text(units: &[u16]) -> String {
    let mut out = String::with_capacity(units.len());
    let mut backslashes = 0usize;
    for item in char::decode_utf16(units.iter().copied()) {
        match item {
            Ok(c) => {
                out.push(c);
                backslashes = if c == '\\' { backslashes + 1 } else { 0 };
            }
            Err(error) => {
                if backslashes % 2 == 1 {
                    out.push('\u{FFFD}');
                } else {
                    out.push_str(&format!("\\u{:04x}", error.unpaired_surrogate()));
                }
                backslashes = 0;
            }
        }
    }
    out
}

/// JS `Map` keys compare with SameValueZero.
fn same_key(a: f64, b: f64) -> bool {
    a == b || (a.is_nan() && b.is_nan())
}

struct ToolCall {
    index: f64,
    id: JsValue,
    name: JsValue,
    input_buffer: Vec<u16>,
}

/// `AnthropicStreamState`.
#[derive(Default)]
pub(crate) struct StreamState {
    tool_calls: Vec<ToolCall>,
    /// Deferred unparseable tool inputs (only their count is observable).
    failed: usize,
}
impl StreamState {
    fn get(&self, index: f64) -> Option<&ToolCall> {
        self.tool_calls.iter().find(|c| same_key(c.index, index))
    }
    fn set(&mut self, call: ToolCall) {
        match self
            .tool_calls
            .iter_mut()
            .find(|c| same_key(c.index, call.index))
        {
            Some(slot) => *slot = call,
            None => self.tool_calls.push(call),
        }
    }
    fn delete(&mut self, index: f64) -> Option<ToolCall> {
        let at = self
            .tool_calls
            .iter()
            .position(|c| same_key(c.index, index))?;
        Some(self.tool_calls.remove(at))
    }
}

fn number_key(index: f64) -> JsValue {
    JsValue::String(crate::js_value::number_string(index).as_str().into())
}
fn event<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    // Event objects keep `undefined` members (dropped only by JSON).
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}
fn read_string(value: &JsValue) -> Option<&JsString> {
    value.as_string()
}
fn read_number(value: &JsValue) -> Option<f64> {
    value.as_number()
}

/// `normalizeAnthropicStreamEvent(raw, state)`.
pub(crate) fn normalize_stream_event(raw: &JsValue, state: &mut StreamState) -> Vec<JsValue> {
    static EMPTY: JsValue = JsValue::Undefined;
    let record = if matches!(raw, JsValue::Object(_)) {
        raw
    } else {
        &EMPTY
    };
    let tag = field(record, "type");
    let tag = tag
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_default();
    match tag.as_str() {
        "message_start" => vec![event([
            ("type", "message_start".into()),
            ("role", "assistant".into()),
            ("raw", raw.clone()),
        ])],
        "content_block_start" => {
            let block = field(record, "content_block");
            if !matches!(block, JsValue::Object(_)) || !is(field(block, "type"), "tool_use") {
                return Vec::new();
            }
            let index =
                read_number(field(record, "index")).unwrap_or(state.tool_calls.len() as f64);
            let id = read_string(field(block, "id"))
                .map_or_else(|| number_key(index), |s| JsValue::String(s.clone()));
            let name = read_string(field(block, "name"))
                .map_or_else(|| "".into(), |s| JsValue::String(s.clone()));
            state.set(ToolCall {
                index,
                id: id.clone(),
                name: name.clone(),
                input_buffer: Vec::new(),
            });
            vec![event([
                ("type", "tool_call_start".into()),
                ("id", id),
                ("name", name),
                ("raw", raw.clone()),
            ])]
        }
        "content_block_delta" => {
            let delta = field(record, "delta");
            let delta = if matches!(delta, JsValue::Object(_)) {
                delta
            } else {
                &EMPTY
            };
            let index = read_number(field(record, "index"));
            let text_of = |key: &str| {
                read_string(field(delta, key))
                    .map_or_else(|| "".into(), |s| JsValue::String(s.clone()))
            };
            let kind = field(delta, "type")
                .as_string()
                .and_then(|s| s.to_utf8().ok())
                .unwrap_or_default();
            match kind.as_str() {
                "text_delta" => vec![event([
                    ("type", "text_delta".into()),
                    ("text", text_of("text")),
                    ("raw", raw.clone()),
                ])],
                "thinking_delta" => vec![event([
                    ("type", "thinking_delta".into()),
                    ("text", text_of("thinking")),
                    ("raw", raw.clone()),
                ])],
                "signature_delta" => vec![event([
                    ("type", "thinking_delta".into()),
                    ("text", "".into()),
                    ("signature", text_of("signature")),
                    ("raw", raw.clone()),
                ])],
                "input_json_delta" => {
                    let partial = text_of("partial_json");
                    if let Some(index) = index {
                        let mut current = state.take_or_default(index);
                        if let JsValue::String(p) = &partial {
                            current.input_buffer.extend_from_slice(p.units());
                        }
                        state.set(current);
                    }
                    let id = match index {
                        None => "".into(),
                        Some(index) => state
                            .get(index)
                            .map_or_else(|| number_key(index), |c| c.id.clone()),
                    };
                    vec![event([
                        ("type", "tool_call_delta".into()),
                        ("id", id),
                        ("delta", partial),
                        ("raw", raw.clone()),
                    ])]
                }
                _ => Vec::new(),
            }
        }
        "content_block_stop" => {
            let Some(index) = read_number(field(record, "index")) else {
                return Vec::new();
            };
            let Some(call) = state.get(index) else {
                return Vec::new();
            };
            // `JSON.parse(rawInput)`, then `JSON.parse(jsonrepair(rawInput))`.
            let parse = |units: &[u16]| parse_json(&json_text(units)).ok();
            let (input, was_repaired) = if call.input_buffer.is_empty() {
                (Some(JsValue::Object(JsObject::new())), false)
            } else if let Some(input) = parse(&call.input_buffer) {
                (Some(input), false)
            } else {
                match crate::runtime::model::jsonrepair::jsonrepair(&call.input_buffer) {
                    Ok(repaired) => match parse(&repaired) {
                        Some(input) => (Some(input), true),
                        None => (None, false),
                    },
                    Err(_) => (None, false),
                }
            };
            let call = state.delete(index).expect("present");
            match input {
                Some(input) => vec![event([
                    ("type", "tool_call_end".into()),
                    (
                        "toolCall",
                        event([
                            ("id", call.id),
                            ("name", call.name),
                            ("input", input),
                            ("raw", raw.clone()),
                        ]),
                    ),
                    ("wasRepaired", JsValue::Bool(was_repaired)),
                    ("raw", raw.clone()),
                ])],
                None => {
                    state.failed += 1;
                    Vec::new()
                }
            }
        }
        "message_delta" => {
            let delta = field(record, "delta");
            let mut events = Vec::new();
            if let Some(usage) = usage(field(record, "usage")) {
                events.push(event([
                    ("type", "usage".into()),
                    ("usage", usage),
                    ("raw", raw.clone()),
                ]));
            }
            let stop = field(delta, "stop_reason");
            if truthy(stop) {
                let reason = finish_reason(stop);
                events.extend(flush_failed(state, reason, raw));
                events.push(event([
                    ("type", "message_end".into()),
                    ("finishReason", reason.into()),
                    ("raw", raw.clone()),
                ]));
            }
            events
        }
        "message_stop" => flush_failed(state, "unknown", raw),
        "error" => {
            let error = field(record, "error");
            let error = if matches!(error, JsValue::Object(_)) {
                error
            } else {
                &EMPTY
            };
            let code = read_string(field(error, "type"))
                .cloned()
                .unwrap_or_else(|| "provider_error".into());
            let message = read_string(field(error, "message"))
                .cloned()
                .unwrap_or_else(|| "Anthropic stream error.".into());
            let retryable = [
                "overloaded_error",
                "rate_limit_error",
                "api_error",
                "timeout_error",
            ]
            .iter()
            .any(|t| code.equals_str(t));
            let mut canonical = JsObject::new();
            canonical.insert("provider".into(), "anthropic".into());
            canonical.insert("protocol".into(), "anthropic".into());
            canonical.insert("code".into(), JsValue::String(code));
            canonical.insert("message".into(), JsValue::String(message.clone()));
            canonical.insert("retryable".into(), JsValue::Bool(retryable));
            if let Some(ms) = retry_after_from_message(&String::from_utf16_lossy(message.units())) {
                canonical.insert("retryAfterMs".into(), JsValue::Number(ms));
            }
            canonical.insert("raw".into(), raw.clone());
            vec![event([
                ("type", "error".into()),
                ("error", JsValue::Object(canonical)),
            ])]
        }
        _ => Vec::new(),
    }
}

impl StreamState {
    /// `state.toolCalls.get(index) ?? { id: String(index), name: "", inputBuffer: "" }`;
    /// the buffer is moved out and `set` writes it back at the same Map slot.
    fn take_or_default(&mut self, index: f64) -> ToolCall {
        match self
            .tool_calls
            .iter_mut()
            .find(|c| same_key(c.index, index))
        {
            Some(slot) => ToolCall {
                index,
                id: slot.id.clone(),
                name: slot.name.clone(),
                input_buffer: std::mem::take(&mut slot.input_buffer),
            },
            None => ToolCall {
                index,
                id: number_key(index),
                name: "".into(),
                input_buffer: Vec::new(),
            },
        }
    }
}

fn flush_failed(state: &mut StreamState, reason: &str, raw: &JsValue) -> Vec<JsValue> {
    if state.failed == 0 {
        return Vec::new();
    }
    state.failed = 0;
    let truncation = reason == "length";
    let mut canonical = JsObject::new();
    canonical.insert("provider".into(), "anthropic".into());
    canonical.insert("protocol".into(), "anthropic".into());
    canonical.insert(
        "code".into(),
        if truncation {
            "max_output_reached"
        } else {
            "invalid_tool_arguments"
        }
        .into(),
    );
    canonical.insert(
        "message".into(),
        if truncation {
            "Output token limit reached — tool call arguments were truncated."
        } else {
            "Anthropic stream tool call arguments are not valid JSON."
        }
        .into(),
    );
    canonical.insert("retryable".into(), JsValue::Bool(true));
    canonical.insert("raw".into(), raw.clone());
    vec![event([
        ("type", "error".into()),
        ("error", JsValue::Object(canonical)),
    ])]
}
