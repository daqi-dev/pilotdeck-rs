//! `providers/openai-responses/{request,stream}.ts`.
//!
//! Single ports reused here: input items and schema normalization from
//! `count_body.rs` (`responses_input`, `normalize_schema`, also used by the
//! Responses count body); `resolveThinkingPlan` from `providers::thinking`;
//! `normalizeOpenAIUsage` from `providers::openai::normalize_usage`;
//! tool-argument parsing (`JSON.parse` → `jsonrepair`) from
//! `providers::parse_tool_arguments`.
use super::super::count_body::{
    field, is, normalize_schema, object, responses_input, tool_choice, truthy,
};
use super::anthropic::event;
use crate::host::HostError;
use crate::runtime::model::http::{ModelStreamError, append_js_string};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

fn type_error(message: &str) -> ModelStreamError {
    ModelStreamError::Thrown {
        name: "TypeError".into(),
        message: message.into(),
    }
}

/// A `TypeError`/`RangeError` thrown while building the body (shared count
/// helpers report them as host errors).
fn thrown(error: HostError) -> ModelStreamError {
    let message = error
        .module_error
        .map(|e| e.message)
        .unwrap_or_else(|| error.message.as_str().into());
    let name = if message.equals_str(crate::js_value::StackOverflow::MESSAGE) {
        "RangeError"
    } else {
        "TypeError"
    };
    ModelStreamError::Thrown {
        name: name.into(),
        message,
    }
}

/// `Object.fromEntries(Object.entries(metadata).map(([k, v]) => [k, String(v)]))`.
fn metadata(value: &JsValue) -> Result<JsValue, ModelStreamError> {
    if !truthy(value) {
        return Ok(JsValue::Undefined);
    }
    let string = |v: &JsValue| {
        crate::js_value::to_string_units_checked(v)
            .map(|u| JsValue::String(JsString::from_utf16(u)))
            .map_err(|error| ModelStreamError::Thrown {
                name: error.name().into(),
                message: error.message().into(),
            })
    };
    let mut out = JsObject::new();
    match value {
        JsValue::Object(object) => {
            for (key, item) in object.own_entries() {
                out.insert(key.clone(), string(item)?);
            }
        }
        JsValue::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                out.insert(index.to_string().as_str().into(), string(item)?);
            }
        }
        JsValue::String(text) => {
            for (index, unit) in text.units().iter().enumerate() {
                out.insert(
                    index.to_string().as_str().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
        }
        _ => {}
    }
    Ok(JsValue::Object(out))
}

/// `buildOpenAIResponsesRequest(request, model, provider)`.
pub(crate) fn build_request(
    request: &JsValue,
    model: &JsValue,
    provider: &JsValue,
) -> Result<JsValue, ModelStreamError> {
    let plan = super::thinking::resolve(request, provider, model)?;
    super::thinking::throw_if_unsupported(&plan)?;
    let input = responses_input(request).map_err(thrown)?;
    let max_output_tokens = match field(request, "maxOutputTokens") {
        JsValue::Undefined | JsValue::Null => {
            field(field(model, "capabilities"), "maxOutputTokens").clone()
        }
        value => value.clone(),
    };
    let tools = match field(request, "tools") {
        JsValue::Undefined | JsValue::Null => JsValue::Undefined,
        JsValue::Array(tools) => JsValue::Array(
            tools
                .iter()
                .map(|tool| {
                    Ok(object([
                        ("type", "function".into()),
                        ("name", field(tool, "name").clone()),
                        ("description", field(tool, "description").clone()),
                        (
                            "parameters",
                            normalize_schema(field(tool, "inputSchema")).map_err(thrown)?,
                        ),
                        ("strict", JsValue::Bool(true)),
                    ]))
                })
                .collect::<Result<Vec<_>, ModelStreamError>>()?,
        ),
        _ => return Err(type_error("request.tools?.map is not a function")),
    };
    let speed = field(request, "speed");
    let service_tier = if !speed.is_undefined()
        && field(field(model, "capabilities"), "supportsSpeed") == &JsValue::Bool(true)
        && is(field(provider, "speedMapping"), "openai_service_tier")
        && speed.as_number().is_some_and(|s| s >= 0.5)
    {
        "priority".into()
    } else {
        JsValue::Undefined
    };
    let mut body = JsObject::new();
    for (key, value) in [
        ("model", field(request, "model").clone()),
        ("input", JsValue::Array(input)),
        ("instructions", field(request, "systemPrompt").clone()),
        ("max_output_tokens", max_output_tokens),
        ("tools", tools),
        (
            "tool_choice",
            tool_choice(field(request, "toolChoice"), false),
        ),
        ("service_tier", service_tier),
        ("stream", field(request, "stream").clone()),
        ("metadata", metadata(field(request, "metadata"))?),
        ("store", JsValue::Bool(false)),
    ] {
        body.insert(key.into(), value);
    }
    match (
        &plan.effort,
        plan.use_openai_reasoning,
        plan.body_patch.as_ref().and_then(JsValue::as_object),
    ) {
        (Some(effort), true, _) => {
            body.insert(
                "reasoning".into(),
                object([("effort", JsValue::String(effort.clone()))]),
            );
        }
        (_, _, Some(patch)) => {
            for (key, value) in patch.own_entries() {
                body.insert(key.clone(), value.clone());
            }
        }
        _ => {}
    }
    let schema = field(request, "outputSchema");
    if truthy(schema) {
        let strict = match field(schema, "strict") {
            JsValue::Undefined | JsValue::Null => JsValue::Bool(true),
            value => value.clone(),
        };
        body.insert(
            "text".into(),
            object([(
                "format",
                object([
                    ("type", "json_schema".into()),
                    ("name", field(schema, "name").clone()),
                    ("description", field(schema, "description").clone()),
                    (
                        "schema",
                        normalize_schema(field(schema, "schema")).map_err(thrown)?,
                    ),
                    ("strict", strict),
                ]),
            )]),
        );
    }
    Ok(JsValue::Object(body))
}

struct ToolCall {
    key: JsString,
    id: JsString,
    name: JsString,
    arguments: Vec<u16>,
}

/// `OpenAIResponsesStreamState`.
pub(crate) struct StreamState {
    started: bool,
    synthetic_id: String,
    response_id: Option<JsString>,
    tool_calls: Vec<ToolCall>,
    completed: Vec<JsString>,
    used_ids: Vec<JsString>,
    saw_tool_call: bool,
}
impl Default for StreamState {
    fn default() -> Self {
        // `stream_${randomUUID().slice(0, 12)}`
        let uuid = uuid::Uuid::new_v4().to_string();
        Self::with_synthetic_id(format!("stream_{}", &uuid[..12]))
    }
}
impl StreamState {
    pub(crate) fn with_synthetic_id(synthetic_id: String) -> Self {
        Self {
            started: false,
            synthetic_id,
            response_id: None,
            tool_calls: Vec::new(),
            completed: Vec::new(),
            used_ids: Vec::new(),
            saw_tool_call: false,
        }
    }
    fn position(&self, key: &JsString) -> Option<usize> {
        self.tool_calls.iter().position(|c| &c.key == key)
    }
}

/// `readNonEmptyString`: a string whose JS `trim()` is non-empty.
fn non_empty(value: &JsValue) -> Option<&JsString> {
    let text = value.as_string()?;
    let trimmed = String::from_utf16_lossy(text.units());
    (!crate::metadata::js_trim(&trimmed).is_empty()).then_some(text)
}
fn finite(value: &JsValue) -> Option<f64> {
    value.as_number().filter(|n| n.is_finite())
}
fn record(value: &JsValue) -> &JsValue {
    static EMPTY: JsValue = JsValue::Undefined;
    if matches!(value, JsValue::Object(_)) {
        value
    } else {
        &EMPTY
    }
}

/// `toolCallKey(event, item)`.
fn tool_call_key(event: &JsValue, item: &JsValue) -> JsString {
    non_empty(field(event, "item_id"))
        .or_else(|| non_empty(field(item, "id")))
        .or_else(|| non_empty(field(event, "call_id")))
        .or_else(|| non_empty(field(item, "call_id")))
        .cloned()
        .unwrap_or_else(|| {
            let index = finite(field(event, "output_index")).unwrap_or(0.0);
            format!("index:{}", crate::js_value::number_string(index))
                .as_str()
                .into()
        })
}

/// `safeToolCallIdPart`: trim, runs of `[^A-Za-z0-9_-]` → `_`, strip edge
/// underscores, or `"response"`.
fn safe_id_part(value: &JsString) -> String {
    let space =
        |unit: &u16| char::from_u32(u32::from(*unit)).is_some_and(crate::metadata::js_space);
    let all = value.units();
    let start = all.iter().position(|u| !space(u)).unwrap_or(all.len());
    let end = all.iter().rposition(|u| !space(u)).map_or(start, |i| i + 1);
    // Unit-wise like the non-`u` regex: a surrogate pair is a run of two.
    let mut out = String::new();
    let mut in_run = false;
    for &unit in &all[start..end] {
        let keep =
            unit < 0x80 && ((unit as u8).is_ascii_alphanumeric() || unit == 0x5f || unit == 0x2d);
        if keep {
            out.push(unit as u8 as char);
            in_run = false;
        } else if !in_run {
            out.push('_');
            in_run = true;
        }
    }
    let stripped = out.trim_matches('_');
    if stripped.is_empty() {
        "response".into()
    } else {
        stripped.into()
    }
}

/// `chooseToolCallId(state, incomingId)` + `nextUniqueToolCallId`.
fn choose_tool_call_id(state: &mut StreamState, incoming: Option<&JsString>) -> JsString {
    let candidate = match incoming {
        Some(id) if !state.used_ids.contains(id) => id.clone(),
        _ => {
            let base = match &state.response_id {
                Some(id) => safe_id_part(id),
                None => safe_id_part(&state.synthetic_id.as_str().into()),
            };
            format!("call_{base}_{}", state.used_ids.len())
                .as_str()
                .into()
        }
    };
    let mut id = candidate.clone();
    let mut suffix = 2usize;
    while state.used_ids.contains(&id) {
        let mut units = candidate.units().to_vec();
        units.extend(format!("_{suffix}").encode_utf16());
        id = JsString::from_utf16(units);
        suffix += 1;
    }
    state.used_ids.push(id.clone());
    id
}

fn ensure_started(events: &mut Vec<JsValue>, state: &mut StreamState, raw: &JsValue) {
    if !state.started {
        state.started = true;
        events.push(event([
            ("type", "message_start".into()),
            ("role", "assistant".into()),
            ("raw", raw.clone()),
        ]));
    }
}

/// `ensureToolCall(event, state)`; returns the Map position.
fn ensure_tool_call(event_value: &JsValue, state: &mut StreamState) -> usize {
    let item = record(field(event_value, "item"));
    let key = tool_call_key(event_value, item);
    if let Some(at) = state.position(&key) {
        return at;
    }
    let incoming = non_empty(field(event_value, "call_id"))
        .or_else(|| non_empty(field(item, "call_id")))
        .cloned();
    let id = choose_tool_call_id(state, incoming.as_ref());
    let name = non_empty(field(item, "name")).cloned().unwrap_or_default();
    state.saw_tool_call = true;
    state.tool_calls.push(ToolCall {
        key,
        id,
        name,
        arguments: Vec::new(),
    });
    state.tool_calls.len() - 1
}

/// `finishToolCall(toolCall, raw)`.
fn finish_tool_call(call: &ToolCall, raw: &JsValue) -> Result<JsValue, ModelStreamError> {
    // `JSON.parse(rawArguments || "{}")`, then jsonrepair.
    let parsed = if call.arguments.is_empty() {
        Some((JsValue::Object(JsObject::new()), false))
    } else {
        super::parse_tool_arguments(&call.arguments)
    };
    let Some((input, was_repaired)) = parsed else {
        let mut error = JsObject::new();
        error.insert("provider".into(), "openai-responses".into());
        error.insert("protocol".into(), "openai-responses".into());
        error.insert("code".into(), "invalid_tool_arguments".into());
        error.insert(
            "message".into(),
            "OpenAI Responses stream tool call arguments are not valid JSON.".into(),
        );
        error.insert("retryable".into(), JsValue::Bool(true));
        error.insert("raw".into(), raw.clone());
        return Err(ModelStreamError::Provider(error));
    };
    Ok(event([
        ("type", "tool_call_end".into()),
        (
            "toolCall",
            event([
                ("id", JsValue::String(call.id.clone())),
                ("name", JsValue::String(call.name.clone())),
                ("input", input),
                ("raw", raw.clone()),
            ]),
        ),
        ("wasRepaired", JsValue::Bool(was_repaired)),
        ("raw", raw.clone()),
    ]))
}

/// The `function_call_arguments.done` / `output_item.done` tail.
fn complete_tool_call(
    event_value: &JsValue,
    arguments: Option<&JsString>,
    state: &mut StreamState,
    raw: &JsValue,
    events: &mut Vec<JsValue>,
) -> Result<(), ModelStreamError> {
    let item = record(field(event_value, "item"));
    let key = tool_call_key(event_value, item);
    if state.completed.contains(&key) {
        return Ok(());
    }
    let at = ensure_tool_call(event_value, state);
    if let Some(arguments) = arguments {
        state.tool_calls[at].arguments = arguments.units().to_vec();
    }
    events.push(finish_tool_call(&state.tool_calls[at], raw)?);
    state.completed.push(key.clone());
    if let Some(at) = state.position(&key) {
        state.tool_calls.remove(at);
    }
    Ok(())
}

/// `normalizeOpenAIResponsesStreamEvent(raw, state)`.
pub(crate) fn normalize_stream_event(
    raw: &JsValue,
    state: &mut StreamState,
) -> Result<Vec<JsValue>, ModelStreamError> {
    let event_value = record(raw);
    let kind = field(event_value, "type")
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_default();
    let mut events = Vec::new();
    let response = record(field(event_value, "response"));
    let response_id = non_empty(field(response, "id"))
        .or_else(|| non_empty(field(event_value, "response_id")))
        .cloned();
    if state.response_id.is_none() {
        state.response_id = response_id;
    }
    let delta = field(event_value, "delta");
    match kind.as_str() {
        "response.created" | "response.in_progress" => ensure_started(&mut events, state, raw),
        "response.output_text.delta"
        | "response.reasoning_summary_text.delta"
        | "response.reasoning_text.delta"
        | "response.reasoning.delta" => {
            if let JsValue::String(text) = delta
                && !text.is_empty()
            {
                ensure_started(&mut events, state, raw);
                let tag = if kind == "response.output_text.delta" {
                    "text_delta"
                } else {
                    "thinking_delta"
                };
                events.push(event([
                    ("type", tag.into()),
                    ("text", JsValue::String(text.clone())),
                    ("raw", raw.clone()),
                ]));
            }
        }
        "response.output_item.added" => {
            let item = record(field(event_value, "item"));
            if is(field(item, "type"), "function_call") {
                ensure_started(&mut events, state, raw);
                let key = tool_call_key(event_value, item);
                if state.position(&key).is_none() {
                    let incoming = non_empty(field(item, "call_id"))
                        .or_else(|| non_empty(field(item, "id")))
                        .cloned();
                    let id = choose_tool_call_id(state, incoming.as_ref());
                    let name = non_empty(field(item, "name")).cloned().unwrap_or_default();
                    let arguments = match field(item, "arguments") {
                        JsValue::String(arguments) => arguments.units().to_vec(),
                        _ => Vec::new(),
                    };
                    state.saw_tool_call = true;
                    state.tool_calls.push(ToolCall {
                        key,
                        id: id.clone(),
                        name: name.clone(),
                        arguments,
                    });
                    events.push(event([
                        ("type", "tool_call_start".into()),
                        ("id", JsValue::String(id)),
                        ("name", JsValue::String(name)),
                        ("raw", raw.clone()),
                    ]));
                }
            }
        }
        "response.function_call_arguments.delta" => {
            if let JsValue::String(text) = delta {
                ensure_started(&mut events, state, raw);
                let at = ensure_tool_call(event_value, state);
                // `${argumentsBuffer}${delta}` throws past V8's string limit.
                append_js_string(&mut state.tool_calls[at].arguments, text.units())?;
                events.push(event([
                    ("type", "tool_call_delta".into()),
                    ("id", JsValue::String(state.tool_calls[at].id.clone())),
                    ("delta", JsValue::String(text.clone())),
                    ("raw", raw.clone()),
                ]));
            }
        }
        "response.function_call_arguments.done" => {
            ensure_started(&mut events, state, raw);
            let arguments = field(event_value, "arguments").as_string().cloned();
            complete_tool_call(event_value, arguments.as_ref(), state, raw, &mut events)?;
        }
        "response.output_item.done" => {
            let item = record(field(event_value, "item"));
            if is(field(item, "type"), "function_call") {
                ensure_started(&mut events, state, raw);
                let arguments = field(item, "arguments").as_string().cloned();
                complete_tool_call(event_value, arguments.as_ref(), state, raw, &mut events)?;
            }
        }
        "response.completed" => {
            ensure_started(&mut events, state, raw);
            if let Some(usage) = super::openai::normalize_usage(field(response, "usage")) {
                events.push(event([
                    ("type", "usage".into()),
                    ("usage", usage),
                    ("raw", raw.clone()),
                ]));
            }
            while !state.tool_calls.is_empty() {
                events.push(finish_tool_call(&state.tool_calls[0], raw)?);
                state.tool_calls.remove(0);
            }
            let reason = if state.saw_tool_call {
                "tool_call"
            } else {
                "stop"
            };
            events.push(event([
                ("type", "message_end".into()),
                ("finishReason", reason.into()),
                ("raw", raw.clone()),
            ]));
        }
        "response.incomplete" => {
            ensure_started(&mut events, state, raw);
            events.push(event([
                ("type", "message_end".into()),
                ("finishReason", "length".into()),
                ("raw", raw.clone()),
            ]));
        }
        "response.failed" | "error" => {
            let failed = kind == "response.failed";
            if failed {
                ensure_started(&mut events, state, raw);
            }
            let response_error = field(response, "error");
            let error = match response_error {
                JsValue::Object(o) if !o.own_entries().is_empty() => response_error,
                _ => record(field(event_value, "error")),
            };
            let mut canonical = JsObject::new();
            canonical.insert("provider".into(), "openai-responses".into());
            canonical.insert("protocol".into(), "openai-responses".into());
            canonical.insert(
                "code".into(),
                JsValue::String(
                    non_empty(field(error, "code"))
                        .cloned()
                        .unwrap_or_else(|| "provider_error".into()),
                ),
            );
            canonical.insert(
                "message".into(),
                JsValue::String(
                    non_empty(field(error, "message"))
                        .cloned()
                        .unwrap_or_else(|| "OpenAI Responses request failed.".into()),
                ),
            );
            canonical.insert("retryable".into(), JsValue::Bool(false));
            canonical.insert("raw".into(), raw.clone());
            events.push(event([
                ("type", "error".into()),
                ("error", JsValue::Object(canonical)),
            ]));
            if failed {
                events.push(event([
                    ("type", "message_end".into()),
                    ("finishReason", "error".into()),
                    ("raw", raw.clone()),
                ]));
            }
        }
        _ => {}
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_id_part_collapses_runs_and_strips_edges() {
        assert_eq!(safe_id_part(&" resp.é😀id ".into()), "resp_id");
        assert_eq!(safe_id_part(&"__".into()), "response");
        assert_eq!(safe_id_part(&"a-b_c".into()), "a-b_c");
    }

    #[test]
    fn duplicate_and_missing_call_ids_get_unique_synthetic_ids() {
        let mut state = StreamState::with_synthetic_id("stream_abc".into());
        let a = choose_tool_call_id(&mut state, Some(&"x".into()));
        let b = choose_tool_call_id(&mut state, Some(&"x".into()));
        let c = choose_tool_call_id(&mut state, None);
        assert_eq!(a, "x".into());
        assert_eq!(b, "call_stream_abc_1".into());
        assert_eq!(c, "call_stream_abc_2".into());
        state.used_ids.push("call_stream_abc_4".into());
        let d = choose_tool_call_id(&mut state, None);
        assert_eq!(d, "call_stream_abc_4_2".into());
    }

    fn feed(state: &mut StreamState, json: &str) -> Result<Vec<String>, ModelStreamError> {
        let raw = pilotdeck_protocol::parse_json(json).unwrap();
        normalize_stream_event(&raw, state).map(|events| {
            events
                .iter()
                .map(|e| {
                    let tag = field(e, "type").as_string().unwrap().to_utf8().unwrap();
                    match field(e, "id").as_string() {
                        Some(id) => format!("{tag}:{}", id.to_utf8().unwrap()),
                        None => tag,
                    }
                })
                .collect()
        })
    }

    /// node probe (`_tmp/20261020-a64-pr7/probe.out.txt`): the
    /// `${argumentsBuffer}${delta}` concat throws `RangeError: Invalid string
    /// length` past V8's limit; exactly the limit is allowed.
    #[test]
    fn function_arguments_over_the_v8_limit_throw_range_error() {
        use crate::runtime::model::http::STRING_LIMIT;
        STRING_LIMIT.with(|cell| cell.set(8));
        let mut state = StreamState::with_synthetic_id("stream_t".into());
        feed(&mut state, r#"{"type":"response.output_item.added","item":{"type":"function_call","id":"fc","call_id":"c","name":"n"}}"#).unwrap();
        assert_eq!(
            feed(&mut state, r#"{"type":"response.function_call_arguments.delta","item_id":"fc","delta":"{\"a\":12}"}"#).unwrap(),
            ["tool_call_delta:c"]
        );
        let error = feed(
            &mut state,
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc","delta":"x"}"#,
        )
        .unwrap_err();
        STRING_LIMIT.with(|cell| cell.set(crate::runtime::model::count_http::MAX_STRING_LENGTH));
        assert!(
            error.name().equals_str("RangeError")
                && error.message().equals_str("Invalid string length")
        );
    }

    /// node probe: without any id the key is `index:<output_index ?? 0>` and
    /// the synthetic call id uses the stream id; a later `output_item.added`
    /// for the same key emits nothing.
    #[test]
    fn idless_events_share_the_index_key() {
        let mut state = StreamState::with_synthetic_id("stream_0a1b2c3d-4e5".into());
        assert_eq!(
            feed(
                &mut state,
                r#"{"type":"response.function_call_arguments.delta","delta":"{}"}"#
            )
            .unwrap(),
            [
                "message_start",
                "tool_call_delta:call_stream_0a1b2c3d-4e5_0"
            ]
        );
        assert!(feed(&mut state, r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","name":"n"}}"#).unwrap().is_empty());
    }
}
