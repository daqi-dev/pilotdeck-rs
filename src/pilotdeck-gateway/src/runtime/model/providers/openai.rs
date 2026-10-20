//! `providers/openai/{request,stream}.ts`, `response/normalize{FinishReason,
//! Usage}.ts` and the OpenAI half of `request/speedMapping.ts`.
//!
//! The stream normalizer turns chat-completions SSE deltas into canonical
//! events. Unparseable tool-call argument buffers are repaired with the
//! shared `jsonrepair` port (pr5) before being reported as provider errors,
//! matching the baseline.
use super::super::count_body::{array, field, is, normalize_schema, reference_text, text, truthy};
use super::super::http::{ModelStreamError, append_js_string};
use super::thinking;
use pilotdeck_protocol::{JsObject, JsString, JsValue};

fn js_string(value: &JsValue) -> JsString {
    JsString::from_utf16(crate::js_value::to_string_units(value).unwrap_or_default())
}

fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in entries {
        if !value.is_undefined() {
            result.insert(key.into(), value);
        }
    }
    JsValue::Object(result)
}

fn put(value: &mut JsValue, key: &str, item: JsValue) {
    if let JsValue::Object(object) = value {
        object.insert(key.into(), item);
    }
}

fn type_error(message: &str) -> ModelStreamError {
    ModelStreamError::Thrown {
        name: "TypeError".into(),
        message: message.into(),
    }
}
fn range_error() -> ModelStreamError {
    ModelStreamError::Thrown {
        name: "RangeError".into(),
        message: crate::js_value::StackOverflow::MESSAGE.into(),
    }
}

/// `JSON.stringify(value)` with the shared V8 depth bound.
fn stringify(value: &JsValue) -> Result<String, ModelStreamError> {
    crate::js_value::json_stringify(value)
        .map(|value| value.unwrap_or_default())
        .map_err(|_| range_error())
}

/// `String.prototype.trim()` non-empty test.
fn read_non_empty_string(value: &JsValue) -> Option<JsString> {
    value
        .as_string()
        .filter(|text| {
            !crate::metadata::js_trim(&String::from_utf16_lossy(text.units())).is_empty()
        })
        .cloned()
}

/// `const content = messageContent(message)` (a non-array is empty).
fn message_content(message: &JsValue) -> &[JsValue] {
    array(field(message, "content"))
}

/// `isGoogleOpenAICompatibleProvider(provider)`; the Google schema cleaner
/// itself arrives with A6.4-pr8, so schema normalization falls back to the
/// OpenAI projection until then (only reachable for Google-compatible hosts).
fn google_openai_compatible(provider: &JsValue) -> bool {
    if !is(field(provider, "protocol"), "openai") {
        return false;
    }
    if is(field(provider, "id"), "google") {
        return true;
    }
    let raw = text(field(provider, "url"));
    let lowered = raw.to_lowercase();
    match url::Url::parse(crate::metadata::js_trim(&lowered)) {
        Ok(url) => {
            url.host_str() == Some("generativelanguage.googleapis.com")
                && url.path().contains("/openai")
        }
        Err(_) => {
            lowered.contains("generativelanguage.googleapis.com") && lowered.contains("/openai")
        }
    }
}

fn normalize_tool_schema(schema: &JsValue) -> Result<JsValue, ModelStreamError> {
    normalize_schema(schema).map_err(|error| ModelStreamError::Thrown {
        name: "RangeError".into(),
        message: error.message.into(),
    })
}

fn is_record(value: &JsValue) -> bool {
    matches!(value, JsValue::Object(_))
}

/// `toOpenAITool(tool, googleOpenAICompatible)`.
fn to_openai_tool(tool: &JsValue, _google: bool) -> Result<JsValue, ModelStreamError> {
    let schema = normalize_tool_schema(field(tool, "inputSchema"))?;
    Ok(object([
        ("type", "function".into()),
        (
            "function",
            object([
                ("name", field(tool, "name").clone()),
                ("description", field(tool, "description").clone()),
                ("parameters", schema),
            ]),
        ),
    ]))
}

/// `toOpenAIToolChoice(toolChoice)` for chat completions.
fn to_openai_tool_choice(choice: &JsValue) -> JsValue {
    if !truthy(choice) {
        return JsValue::Undefined;
    }
    for name in ["auto", "none", "required"] {
        if is(choice, name) {
            return choice.clone();
        }
    }
    object([
        ("type", "function".into()),
        (
            "function",
            object([("name", field(choice, "name").clone())]),
        ),
    ])
}

fn data_url(block: &JsValue) -> JsValue {
    JsValue::String(JsString::from_utf16({
        let mut units = "data:".encode_utf16().collect::<Vec<_>>();
        units
            .extend(crate::js_value::to_string_units(field(block, "mimeType")).unwrap_or_default());
        units.extend(";base64,".encode_utf16());
        units.extend(crate::js_value::to_string_units(field(block, "data")).unwrap_or_default());
        units
    }))
}

/// `toOpenAIContent(blocks)`.
fn to_openai_content(blocks: &[JsValue]) -> JsValue {
    if blocks.iter().all(|block| is(field(block, "type"), "text")) {
        let mut units = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            if index > 0 {
                units.push(u16::from(b'\n'));
            }
            // `Array#join` renders null/undefined as the empty string.
            if !matches!(field(block, "text"), JsValue::Undefined | JsValue::Null) {
                units.extend(
                    crate::js_value::to_string_units(field(block, "text")).unwrap_or_default(),
                );
            }
        }
        return JsValue::String(JsString::from_utf16(units));
    }
    let parts: Vec<JsValue> = blocks
        .iter()
        .filter_map(|block| {
            let tag = text(field(block, "type"));
            Some(match tag.as_str() {
                "text" => object([
                    ("type", "text".into()),
                    ("text", field(block, "text").clone()),
                ]),
                "thinking" => object([
                    ("type", "text".into()),
                    ("text", field(block, "text").clone()),
                ]),
                "image" => object([
                    ("type", "image_url".into()),
                    (
                        "image_url",
                        object([
                            (
                                "url",
                                if is(field(block, "source"), "url") {
                                    field(block, "data").clone()
                                } else {
                                    data_url(block)
                                },
                            ),
                            ("detail", field(block, "detail").clone()),
                        ]),
                    ),
                ]),
                "audio" => object([
                    ("type", "input_audio".into()),
                    if is(field(block, "source"), "url") {
                        ("audio_url", field(block, "data").clone())
                    } else {
                        (
                            "input_audio",
                            object([
                                ("data", field(block, "data").clone()),
                                ("format", field(block, "mimeType").clone()),
                            ]),
                        )
                    },
                ]),
                "pdf" => object([
                    ("type", "image_url".into()),
                    ("image_url", object([("url", data_url(block))])),
                ]),
                "tool_call" | "tool_result" => return None,
                "tool_result_reference" => object([
                    ("type", "text".into()),
                    ("text", field(block, "preview").clone()),
                ]),
                "media_reference" => object([
                    ("type", "text".into()),
                    ("text", field(block, "preview").clone()),
                ]),
                _ => return None,
            })
        })
        .collect();
    JsValue::Array(parts)
}

/// `toOpenAIReasoningContent(thinkingBlocks)`.
fn reasoning_content(blocks: &[&JsValue]) -> Option<JsValue> {
    let contexts: Vec<JsString> = blocks
        .iter()
        .filter_map(|block| {
            let value = match field(block, "reasoningContent") {
                JsValue::Undefined | JsValue::Null => field(block, "text"),
                value => value,
            };
            value.as_string().filter(|text| !text.is_empty()).cloned()
        })
        .collect();
    if contexts.is_empty() && !blocks.is_empty() {
        return Some("".into());
    }
    if contexts.is_empty() {
        return None;
    }
    let mut units = Vec::new();
    for (index, context) in contexts.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        units.extend_from_slice(context.units());
    }
    Some(JsValue::String(JsString::from_utf16(units)))
}

fn tool_result_message(block: &JsValue) -> JsValue {
    object([
        ("role", "tool".into()),
        ("tool_call_id", field(block, "toolCallId").clone()),
        (
            "content",
            JsValue::String(
                crate::runtime::context::compaction::flatten_tool_result_block_text(block),
            ),
        ),
    ])
}

fn tool_result_reference_message(block: &JsValue) -> JsValue {
    object([
        ("role", "tool".into()),
        ("tool_call_id", field(block, "toolCallId").clone()),
        ("content", reference_text(block)),
    ])
}

fn visual_content(block: &JsValue) -> Vec<JsValue> {
    array(field(block, "content"))
        .iter()
        .filter(|item| is(field(item, "type"), "image") || is(field(item, "type"), "pdf"))
        .cloned()
        .collect()
}

fn tool_result_visual_message(visual: &[JsValue]) -> JsValue {
    let mut blocks = vec![object([
        ("type", "text".into()),
        ("text", "[Visual content from tool result]".into()),
    ])];
    blocks.extend_from_slice(visual);
    object([
        ("role", "user".into()),
        ("content", to_openai_content(&blocks)),
    ])
}

/// `toOpenAIMessages(message, messageIndex)`.
fn to_openai_messages(
    message: &JsValue,
    message_index: usize,
) -> Result<Vec<JsValue>, ModelStreamError> {
    if is(field(message, "role"), "user") {
        return Ok(to_openai_user_messages(message));
    }
    let content = message_content(message);
    let tool_result_blocks: Vec<&JsValue> = content
        .iter()
        .filter(|block| is(field(block, "type"), "tool_result"))
        .collect();
    let tool_messages: Vec<JsValue> = tool_result_blocks
        .iter()
        .map(|block| tool_result_message(block))
        .collect();
    let reference_messages: Vec<JsValue> = content
        .iter()
        .filter(|block| is(field(block, "type"), "tool_result_reference"))
        .map(tool_result_reference_message)
        .collect();
    let visual_messages: Vec<JsValue> = tool_result_blocks
        .iter()
        .filter_map(|block| {
            let visual = visual_content(block);
            (!visual.is_empty()).then(|| tool_result_visual_message(&visual))
        })
        .collect();
    let tool_calls: Vec<JsValue> = content
        .iter()
        .filter(|block| is(field(block, "type"), "tool_call"))
        .map(|block| {
            let args = match field(block, "input") {
                JsValue::Undefined | JsValue::Null => JsValue::Object(JsObject::new()),
                input => input.clone(),
            };
            Ok(object([
                ("id", field(block, "id").clone()),
                ("type", "function".into()),
                (
                    "function",
                    object([
                        ("name", field(block, "name").clone()),
                        (
                            "arguments",
                            JsValue::String(stringify(&args)?.as_str().into()),
                        ),
                    ]),
                ),
            ]))
        })
        .collect::<Result<_, ModelStreamError>>()?;
    let thinking_blocks: Vec<&JsValue> = content
        .iter()
        .filter(|block| is(field(block, "type"), "thinking"))
        .collect();
    let normal_content: Vec<JsValue> = content
        .iter()
        .filter(|block| {
            ![
                "tool_result",
                "tool_result_reference",
                "tool_call",
                "thinking",
            ]
            .iter()
            .any(|tag| is(field(block, "type"), tag))
        })
        .cloned()
        .collect();
    let _ = message_index;
    let mut messages = Vec::new();
    if !normal_content.is_empty() || !tool_calls.is_empty() || !thinking_blocks.is_empty() {
        let content = if !normal_content.is_empty() {
            to_openai_content(&normal_content)
        } else if is(field(message, "role"), "assistant") && !thinking_blocks.is_empty() {
            "".into()
        } else {
            JsValue::Undefined
        };
        let mut msg = object([
            ("role", field(message, "role").clone()),
            ("content", content),
            (
                "tool_calls",
                if tool_calls.is_empty() {
                    JsValue::Undefined
                } else {
                    JsValue::Array(tool_calls.clone())
                },
            ),
        ]);
        if let Some(reasoning) = reasoning_content(&thinking_blocks) {
            put(&mut msg, "reasoning_content", reasoning);
        }
        messages.push(msg);
    }
    messages.extend(tool_messages);
    messages.extend(reference_messages);
    messages.extend(visual_messages);
    Ok(messages)
}

fn flush_normal(messages: &mut Vec<JsValue>, normal: &mut Vec<JsValue>) {
    if normal.is_empty() {
        return;
    }
    messages.push(object([
        ("role", "user".into()),
        ("content", to_openai_content(normal)),
    ]));
    normal.clear();
}

/// `toOpenAIUserMessages(message)`.
fn to_openai_user_messages(message: &JsValue) -> Vec<JsValue> {
    let content = message_content(message);
    let mut messages = Vec::new();
    let mut normal = Vec::new();
    let mut index = 0;
    while index < content.len() {
        let block = &content[index];
        if is(field(block, "type"), "tool_result") {
            flush_normal(&mut messages, &mut normal);
            let mut visual = Vec::new();
            while index < content.len() {
                let tool_block = &content[index];
                if is(field(tool_block, "type"), "tool_result") {
                    messages.push(tool_result_message(tool_block));
                    visual.extend(visual_content(tool_block));
                    index += 1;
                    continue;
                }
                if is(field(tool_block, "type"), "tool_result_reference") {
                    messages.push(tool_result_reference_message(tool_block));
                    index += 1;
                    continue;
                }
                break;
            }
            index = index.saturating_sub(1);
            if !visual.is_empty() {
                messages.push(tool_result_visual_message(&visual));
            }
            index += 1;
            continue;
        }
        if is(field(block, "type"), "tool_result_reference") {
            flush_normal(&mut messages, &mut normal);
            messages.push(tool_result_reference_message(block));
            index += 1;
            continue;
        }
        normal.push(block.clone());
        index += 1;
    }
    flush_normal(&mut messages, &mut normal);
    messages
}

struct NormalizedToolCall {
    original_id: Option<JsString>,
    tool_call: JsValue,
}

fn next_unique_tool_call_id(id: &JsString, used: &[JsString]) -> JsString {
    if !used.contains(id) {
        return id.clone();
    }
    let mut suffix = 2;
    loop {
        let candidate = JsString::from_utf16({
            let mut units = id.units().to_vec();
            units.push(u16::from(b'_'));
            units.extend(suffix.to_string().encode_utf16());
            units
        });
        if !used.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn normalize_tool_call(
    tool_call: &JsValue,
    message_index: usize,
    tool_call_index: usize,
) -> Result<JsValue, ModelStreamError> {
    static UNDEFINED: JsValue = JsValue::Undefined;
    let record = is_record(tool_call).then_some(tool_call);
    let record = record.unwrap_or(&UNDEFINED);
    let function = is_record(field(record, "function")).then(|| field(record, "function"));
    let function = function.unwrap_or(&UNDEFINED);
    let id = match field(record, "id") {
        JsValue::String(id)
            if !crate::metadata::js_trim(&String::from_utf16_lossy(id.units())).is_empty() =>
        {
            id.clone()
        }
        _ => format!("call_{message_index}_{tool_call_index}")
            .as_str()
            .into(),
    };
    let name = match field(function, "name") {
        JsValue::String(name) => name.clone(),
        _ => "".into(),
    };
    let arguments = match field(function, "arguments") {
        JsValue::String(arguments) => arguments.clone(),
        arguments => {
            let arguments = match arguments {
                JsValue::Undefined | JsValue::Null => JsValue::Object(JsObject::new()),
                value => value.clone(),
            };
            stringify(&arguments)?.as_str().into()
        }
    };
    Ok(object([
        ("id", JsValue::String(id)),
        ("type", "function".into()),
        (
            "function",
            object([
                ("name", JsValue::String(name)),
                ("arguments", JsValue::String(arguments)),
            ]),
        ),
    ]))
}

fn normalize_tool_calls(
    tool_calls: &[JsValue],
    message_index: usize,
) -> Result<Vec<NormalizedToolCall>, ModelStreamError> {
    let mut used: Vec<JsString> = Vec::new();
    let mut entries = Vec::new();
    for (tool_call_index, tool_call) in tool_calls.iter().enumerate() {
        let record = is_record(tool_call).then_some(tool_call);
        let original_id = record
            .and_then(|record| field(record, "id").as_string())
            .map(|id| {
                JsString::from_utf16({
                    let text = String::from_utf16_lossy(id.units());
                    let trimmed = crate::metadata::js_trim(&text);
                    trimmed.encode_utf16().collect::<Vec<_>>()
                })
            });
        let normalized = normalize_tool_call(tool_call, message_index, tool_call_index)?;
        let normalized_id = field(&normalized, "id")
            .as_string()
            .cloned()
            .unwrap_or_default();
        let id = next_unique_tool_call_id(&normalized_id, &used);
        used.push(id.clone());
        let tool_call = if id == normalized_id {
            normalized
        } else {
            let mut tool_call = normalized;
            put(&mut tool_call, "id", JsValue::String(id));
            tool_call
        };
        entries.push(NormalizedToolCall {
            original_id,
            tool_call,
        });
    }
    Ok(entries)
}

fn take_expected_tool_call(
    expected: &[NormalizedToolCall],
    matched: &[bool],
    tool_call_id: &JsValue,
) -> Option<usize> {
    let id = tool_call_id.as_string()?;
    let id_text = String::from_utf16_lossy(id.units());
    let id = crate::metadata::js_trim(&id_text);
    expected.iter().enumerate().position(|(index, entry)| {
        !matched[index]
            && (entry.original_id.as_ref().is_some_and(|original| {
                crate::metadata::js_trim(&String::from_utf16_lossy(original.units())) == id
            }) || entry
                .tool_call
                .get("id")
                .and_then(JsValue::as_string)
                .is_some_and(|call_id| {
                    crate::metadata::js_trim(&String::from_utf16_lossy(call_id.units())) == id
                }))
    })
}

/// `repairOpenAIToolPairing(messages)`.
fn repair_tool_pairing(messages: Vec<JsValue>) -> Result<Vec<JsValue>, ModelStreamError> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < messages.len() {
        let message = &messages[index];
        let tool_calls = field(message, "tool_calls");
        let has_calls = matches!(tool_calls, JsValue::Array(calls) if !calls.is_empty());
        if !is(field(message, "role"), "assistant") || !has_calls {
            if !is(field(message, "role"), "tool") {
                out.push(message.clone());
            }
            index += 1;
            continue;
        }
        let expected = normalize_tool_calls(array(tool_calls), index)?;
        let mut replaced = message.clone();
        put(
            &mut replaced,
            "tool_calls",
            JsValue::Array(
                expected
                    .iter()
                    .map(|entry| entry.tool_call.clone())
                    .collect(),
            ),
        );
        out.push(replaced);
        let mut matched = vec![false; expected.len()];
        let mut next = index + 1;
        while next < messages.len() && is(field(&messages[next], "role"), "tool") {
            if let Some(position) =
                take_expected_tool_call(&expected, &matched, field(&messages[next], "tool_call_id"))
            {
                let mut tool_message = messages[next].clone();
                put(
                    &mut tool_message,
                    "tool_call_id",
                    expected[position]
                        .tool_call
                        .get("id")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
                out.push(tool_message);
                matched[position] = true;
            }
            next += 1;
        }
        for (position, entry) in expected.iter().enumerate() {
            if matched[position] {
                continue;
            }
            out.push(object([
                ("role", "tool".into()),
                (
                    "tool_call_id",
                    entry
                        .tool_call
                        .get("id")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                ("content", "[result truncated]".into()),
            ]));
        }
        index = next;
    }
    Ok(out)
}

/// `buildOpenAIRequest(request, model, provider)`.
pub(crate) fn build_request(
    request: &JsValue,
    model: &JsValue,
    provider: &JsValue,
) -> Result<JsValue, ModelStreamError> {
    let google = google_openai_compatible(provider);
    let plan = thinking::resolve(request, provider, model)?;
    thinking::throw_if_unsupported(&plan)?;
    let mut messages = Vec::new();
    for (index, message) in array(field(request, "messages")).iter().enumerate() {
        messages.extend(to_openai_messages(message, index)?);
    }
    let mut messages = repair_tool_pairing(messages)?;
    if truthy(field(request, "systemPrompt")) {
        messages.insert(
            0,
            object([
                ("role", "system".into()),
                ("content", field(request, "systemPrompt").clone()),
            ]),
        );
    }
    let tools = match field(request, "tools") {
        JsValue::Undefined | JsValue::Null => JsValue::Undefined,
        JsValue::Array(tools) => JsValue::Array(
            tools
                .iter()
                .map(|tool| to_openai_tool(tool, google))
                .collect::<Result<_, _>>()?,
        ),
        _ => return Err(type_error("request.tools?.map is not a function")),
    };
    let service_tier = if !field(request, "speed").is_undefined()
        && field(field(model, "capabilities"), "supportsSpeed") == &JsValue::Bool(true)
        && is(field(provider, "speedMapping"), "openai_service_tier")
    {
        match field(request, "speed").as_number() {
            Some(speed) if speed >= 0.5 => "priority".into(),
            _ => JsValue::Undefined,
        }
    } else {
        JsValue::Undefined
    };
    let metadata = if truthy(field(request, "metadata")) {
        let mut metadata = JsObject::new();
        if let Some(entries) = field(request, "metadata").as_object() {
            for (key, value) in entries.own_entries() {
                metadata.insert(key.clone(), JsValue::String(js_string(value)));
            }
        }
        JsValue::Object(metadata)
    } else {
        JsValue::Undefined
    };
    let mut body = JsObject::new();
    body.insert("model".into(), field(request, "model").clone());
    body.insert("messages".into(), JsValue::Array(messages));
    body.insert(
        "max_tokens".into(),
        match field(request, "maxOutputTokens") {
            JsValue::Undefined | JsValue::Null => {
                field(field(model, "capabilities"), "maxOutputTokens").clone()
            }
            value => value.clone(),
        },
    );
    body.insert("tools".into(), tools);
    body.insert(
        "tool_choice".into(),
        to_openai_tool_choice(field(request, "toolChoice")),
    );
    body.insert("service_tier".into(), service_tier);
    body.insert("stream".into(), field(request, "stream").clone());
    body.insert("metadata".into(), metadata);
    if truthy(field(request, "outputSchema")) {
        let schema = field(request, "outputSchema");
        body.insert(
            "response_format".into(),
            object([
                ("type", "json_schema".into()),
                (
                    "json_schema",
                    object([
                        ("name", field(schema, "name").clone()),
                        ("description", field(schema, "description").clone()),
                        ("schema", normalize_tool_schema(field(schema, "schema"))?),
                        (
                            "strict",
                            match field(schema, "strict") {
                                JsValue::Undefined | JsValue::Null => JsValue::Bool(true),
                                value => value.clone(),
                            },
                        ),
                    ]),
                ),
            ]),
        );
    }
    if plan.use_openai_reasoning && plan.effort.is_some() {
        body.insert(
            "reasoning_effort".into(),
            JsValue::String(plan.effort.clone().unwrap_or_default()),
        );
    } else if let Some(patch) = &plan.body_patch
        && let Some(patch) = patch.as_object()
    {
        for (key, value) in patch.entries() {
            body.insert(key.clone(), value.clone());
        }
    }
    Ok(JsValue::Object(body))
}

// ---------------------------------------------------------------------------
// Stream normalization

fn normalize_finish_reason(reason: &JsValue) -> &'static str {
    match reason
        .as_string()
        .and_then(|text| text.to_utf8().ok())
        .as_deref()
    {
        Some("stop") => "stop",
        Some("length") => "length",
        Some("tool_calls" | "function_call") => "tool_call",
        Some("content_filter" | "content_filter_results") => "content_filter",
        _ => "unknown",
    }
}

fn read_number(value: &JsValue) -> Option<f64> {
    value.as_number().filter(|number| number.is_finite())
}

fn sum_defined(values: &[Option<f64>]) -> Option<f64> {
    let defined: Vec<f64> = values.iter().flatten().copied().collect();
    (!defined.is_empty()).then(|| defined.iter().sum())
}

/// `normalizeOpenAIUsage(raw)`.
/// `normalizeOpenAIUsage`; also used by the Responses stream.
pub(crate) fn normalize_usage(raw: &JsValue) -> Option<JsValue> {
    if !is_record(raw) {
        return None;
    }
    let prompt = read_number(field(raw, "prompt_tokens"))
        .or_else(|| read_number(field(raw, "input_tokens")));
    let output = read_number(field(raw, "completion_tokens"))
        .or_else(|| read_number(field(raw, "output_tokens")));
    let native_cost = read_number(field(raw, "cost"))
        .or_else(|| read_number(field(raw, "total_cost")))
        .or_else(|| read_number(field(raw, "estimated_cost")));
    let details = if is_record(field(raw, "prompt_tokens_details")) {
        Some(field(raw, "prompt_tokens_details"))
    } else if is_record(field(raw, "input_tokens_details")) {
        Some(field(raw, "input_tokens_details"))
    } else {
        None
    };
    let cache_read = details
        .and_then(|details| read_number(field(details, "cached_tokens")))
        .or_else(|| read_number(field(raw, "cache_read_input_tokens")));
    let cache_write = details
        .and_then(|details| read_number(field(details, "cache_write_tokens")))
        .or_else(|| read_number(field(raw, "cache_creation_input_tokens")));
    let input =
        prompt.map(|prompt| prompt - cache_read.unwrap_or(0.0) - cache_write.unwrap_or(0.0));
    let total = read_number(field(raw, "total_tokens")).or_else(|| sum_defined(&[prompt, output]));
    let values = [input, output, cache_read, cache_write, total, native_cost];
    if values.iter().all(Option::is_none) {
        return None;
    }
    let mut usage = JsObject::new();
    for (key, value) in [
        "inputTokens",
        "outputTokens",
        "cacheReadTokens",
        "cacheWriteTokens",
        "totalTokens",
        "nativeCost",
    ]
    .iter()
    .zip(values)
    {
        usage.insert(
            (*key).into(),
            value.map_or(JsValue::Undefined, JsValue::Number),
        );
    }
    Some(JsValue::Object(usage))
}

fn event<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    // Canonical events keep `undefined` members (dropped only by JSON).
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}

#[derive(Clone)]
struct ToolCall {
    id: Option<JsString>,
    name: Option<JsString>,
    /// `argumentsBuffer` is absent until the first string fragment arrives;
    /// `finishToolCalls` then parses `"{}"` (`?? "{}"`), unlike `""`.
    arguments_buffer: Option<JsString>,
    choice_index: f64,
    tool_index: f64,
}

pub(crate) struct StreamState {
    started: bool,
    tool_calls: Vec<(JsString, ToolCall)>,
    used_tool_call_ids: Vec<JsString>,
    stream_synthetic_id: JsString,
    stream_response_id: Option<JsString>,
    tool_call_base_id: Option<JsString>,
    reasoning_snapshot: Vec<u16>,
}

impl Default for StreamState {
    fn default() -> Self {
        // `stream_${randomUUID().slice(0, 12)}`: first 8 hex digits, a dash,
        // then three more. No fixture relies on it (every stream has an id).
        let raw = uuid::Uuid::new_v4().simple().to_string();
        let synthetic: JsString = format!("stream_{}-{}", &raw[..8], &raw[8..11])
            .as_str()
            .into();
        Self {
            started: false,
            tool_calls: Vec::new(),
            used_tool_call_ids: Vec::new(),
            stream_synthetic_id: synthetic,
            stream_response_id: None,
            tool_call_base_id: None,
            reasoning_snapshot: Vec::new(),
        }
    }
}

fn as_record(value: &JsValue) -> &JsValue {
    static EMPTY: JsValue = JsValue::Undefined;
    if is_record(value) { value } else { &EMPTY }
}

fn same_id(left: &JsString, right: &JsString) -> bool {
    left == right
}

impl StreamState {
    fn get(&self, key: &JsString) -> Option<&ToolCall> {
        self.tool_calls
            .iter()
            .find(|(k, _)| same_id(k, key))
            .map(|(_, call)| call)
    }
    fn set(&mut self, key: JsString, call: ToolCall) {
        match self.tool_calls.iter_mut().find(|(k, _)| same_id(k, &key)) {
            Some(slot) => slot.1 = call,
            None => self.tool_calls.push((key, call)),
        }
    }
    fn delete(&mut self, key: &JsString) -> Option<ToolCall> {
        let position = self.tool_calls.iter().position(|(k, _)| same_id(k, key))?;
        Some(self.tool_calls.remove(position).1)
    }
    fn base_id(&mut self) -> JsString {
        if self.tool_call_base_id.is_none() {
            let source = self
                .stream_response_id
                .clone()
                .unwrap_or_else(|| self.stream_synthetic_id.clone());
            let text = String::from_utf16_lossy(source.units());
            self.tool_call_base_id = Some(safe_tool_call_id_part(&text).as_str().into());
        }
        self.tool_call_base_id.clone().unwrap_or_default()
    }
    fn generate_id(&mut self, choice_index: f64, tool_index: f64) -> JsString {
        let base = self.base_id();
        format!(
            "call_{}_{}_{}",
            String::from_utf16_lossy(base.units()),
            crate::js_value::number_string(choice_index),
            crate::js_value::number_string(tool_index)
        )
        .as_str()
        .into()
    }
    fn choose_id(
        &mut self,
        incoming: Option<&JsString>,
        choice_index: f64,
        tool_index: f64,
    ) -> JsString {
        let candidate = match incoming {
            Some(id) if !self.used_tool_call_ids.contains(id) => id.clone(),
            _ => self.generate_id(choice_index, tool_index),
        };
        let id = self.next_unique(&candidate);
        self.used_tool_call_ids.push(id.clone());
        id
    }
    fn next_unique(&self, id: &JsString) -> JsString {
        if !self.used_tool_call_ids.contains(id) {
            return id.clone();
        }
        let mut suffix = 2;
        loop {
            let candidate = JsString::from_utf16({
                let mut units = id.units().to_vec();
                units.push(u16::from(b'_'));
                units.extend(suffix.to_string().encode_utf16());
                units
            });
            if !self.used_tool_call_ids.contains(&candidate) {
                return candidate;
            }
            suffix += 1;
        }
    }
}

fn safe_tool_call_id_part(value: &str) -> String {
    let mut out = String::new();
    let mut in_run = false;
    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            out.push(character);
            in_run = false;
        } else if !in_run {
            out.push('_');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "stream".into()
    } else {
        trimmed.to_string()
    }
}

fn tool_call_key(choice_index: f64, tool_index: f64) -> JsString {
    format!(
        "{}:{}",
        crate::js_value::number_string(choice_index),
        crate::js_value::number_string(tool_index)
    )
    .as_str()
    .into()
}

fn tool_call_events(
    deltas: &[JsValue],
    state: &mut StreamState,
    raw: &JsValue,
    choice_index: f64,
) -> Result<Vec<JsValue>, ModelStreamError> {
    let mut events = Vec::new();
    for delta in deltas {
        let record = as_record(delta);
        let tool_index = field(record, "index").as_number().unwrap_or(0.0);
        let key = tool_call_key(choice_index, tool_index);
        let function = as_record(field(record, "function"));
        let has_existing = state.get(&key).is_some();
        let mut current = state.get(&key).cloned().unwrap_or(ToolCall {
            id: None,
            name: None,
            arguments_buffer: None,
            choice_index,
            tool_index,
        });
        let incoming_id = read_non_empty_string(field(record, "id"));
        if let Some(name) = read_non_empty_string(field(function, "name")) {
            current.name = Some(name);
        }
        if !has_existing {
            current.id = Some(state.choose_id(incoming_id.as_ref(), choice_index, tool_index));
            state.set(key.clone(), current.clone());
            events.push(event([
                ("type", "tool_call_start".into()),
                (
                    "id",
                    JsValue::String(current.id.clone().unwrap_or_default()),
                ),
                (
                    "name",
                    JsValue::String(current.name.clone().unwrap_or_default()),
                ),
                ("raw", raw.clone()),
            ]));
        }
        if let JsValue::String(arguments) = field(function, "arguments") {
            let current_id = match current.id.clone() {
                Some(id) => id,
                None => state.choose_id(None, choice_index, tool_index),
            };
            current.id = Some(current_id.clone());
            // `${argumentsBuffer ?? ""}${fn.arguments}`: past V8's limit the
            // template literal throws before the assignment, leaving the
            // buffer unchanged (the shared Map object still keeps name/id).
            let mut units = current
                .arguments_buffer
                .as_ref()
                .map(|buffer| buffer.units().to_vec())
                .unwrap_or_default();
            let appended = append_js_string(&mut units, arguments.units());
            if appended.is_ok() {
                current.arguments_buffer = Some(JsString::from_utf16(units));
            }
            state.set(key.clone(), current);
            appended?;
            events.push(event([
                ("type", "tool_call_delta".into()),
                ("id", JsValue::String(current_id)),
                ("delta", JsValue::String(arguments.clone())),
                ("raw", raw.clone()),
            ]));
        } else {
            state.set(key, current);
        }
    }
    Ok(events)
}

fn provider_error(code: &str, message: &str, raw: &JsValue) -> ModelStreamError {
    let mut canonical = JsObject::new();
    canonical.insert("provider".into(), "openai".into());
    canonical.insert("protocol".into(), "openai".into());
    canonical.insert("code".into(), code.into());
    canonical.insert("message".into(), message.into());
    canonical.insert("retryable".into(), JsValue::Bool(true));
    canonical.insert("raw".into(), raw.clone());
    ModelStreamError::Provider(canonical)
}

enum ParsedArguments {
    Parsed(JsValue),
    Repaired(JsValue),
    Invalid,
}

/// `JSON.parse(rawArguments)`, then `JSON.parse(jsonrepair(rawArguments))`.
fn parse_tool_arguments(raw_arguments: &JsString) -> ParsedArguments {
    match super::parse_tool_arguments(raw_arguments.units()) {
        Some((input, false)) => ParsedArguments::Parsed(input),
        Some((input, true)) => ParsedArguments::Repaired(input),
        None => ParsedArguments::Invalid,
    }
}

fn finish_tool_calls(
    state: &mut StreamState,
    raw: &JsValue,
    finish_reason: &str,
    choice_index: f64,
) -> Result<Vec<JsValue>, ModelStreamError> {
    let truncation = finish_reason == "length";
    let keys: Vec<JsString> = state
        .tool_calls
        .iter()
        .filter(|(_, call)| call.choice_index == choice_index)
        .map(|(key, _)| key.clone())
        .collect();
    let mut events = Vec::new();
    for key in keys {
        let Some(call) = state.get(&key).cloned() else {
            continue;
        };
        // `toolCall.argumentsBuffer ?? "{}"`: absent means no fragment at all.
        let raw_arguments = call.arguments_buffer.clone().unwrap_or_else(|| "{}".into());
        let (input, was_repaired) = match parse_tool_arguments(&raw_arguments) {
            ParsedArguments::Parsed(input) => (input, false),
            ParsedArguments::Repaired(input) => {
                // `jsonrepair` may silently truncate under max_tokens; the
                // baseline retries instead of accepting a partial value.
                if truncation {
                    return Err(provider_error(
                        "max_output_reached",
                        "Output token limit reached — repaired tool call arguments are likely incomplete.",
                        raw,
                    ));
                }
                (input, true)
            }
            ParsedArguments::Invalid => {
                let (code, message) = if truncation {
                    (
                        "max_output_reached",
                        "Output token limit reached — tool call arguments were truncated.",
                    )
                } else {
                    (
                        "invalid_tool_arguments",
                        "OpenAI stream tool call arguments are not valid JSON.",
                    )
                };
                return Err(provider_error(code, message, raw));
            }
        };
        let id = match call.id.clone() {
            Some(id) => id,
            None => state.choose_id(None, call.choice_index, call.tool_index),
        };
        state.delete(&key);
        events.push(event([
            ("type", "tool_call_end".into()),
            (
                "toolCall",
                event([
                    ("id", JsValue::String(id)),
                    (
                        "name",
                        JsValue::String(call.name.clone().unwrap_or_default()),
                    ),
                    ("input", input),
                    ("raw", raw.clone()),
                ]),
            ),
            ("wasRepaired", JsValue::Bool(was_repaired)),
            ("raw", raw.clone()),
        ]));
    }
    Ok(events)
}

/// `normalizeOpenAIStreamEvent(raw, state)`.
///
/// Fallible because `finishToolCalls` throws on an unparseable tool buffer
/// (including a repaired-but-truncated one) and a cross-event string
/// accumulator past V8's limit throws `RangeError`; the whole chunk's events
/// are then discarded, as in the baseline.
pub(crate) fn normalize_stream_event(
    raw: &JsValue,
    state: &mut StreamState,
) -> Result<Vec<JsValue>, ModelStreamError> {
    let chunk = as_record(raw);
    // Baseline cc7c4e6: `const error = asRecord(chunk.error);
    // if (Object.keys(error).length > 0)`. This is deliberately not
    // `if (chunk.error)`: `{}`, `"boom"`, `[]` and `null` are swallowed by TS
    // too (node probe `fix1-probe.mts`; fixtures `stream-error-shapes`).
    let error = as_record(field(chunk, "error"));
    if error.as_object().is_some_and(|error| !error.is_empty()) {
        // No `retryAfterMs`: only anthropic/stream.ts parses a hint out of the
        // message. openai/stream.ts does not, so the retry loop uses the
        // header/default backoff (`stream-error-rate-limit-hint` pins it).
        let code = read_non_empty_string(field(error, "type"))
            .or_else(|| read_non_empty_string(field(error, "code")))
            .unwrap_or_else(|| "provider_error".into());
        let message = read_non_empty_string(field(error, "message"))
            .unwrap_or_else(|| "OpenAI stream request failed.".into());
        let code_text = String::from_utf16_lossy(code.units());
        let retryable = [
            "rate_limit_error",
            "overloaded_error",
            "server_error",
            "timeout_error",
        ]
        .iter()
        .any(|kind| code_text == *kind);
        let mut canonical = JsObject::new();
        canonical.insert("provider".into(), "openai".into());
        canonical.insert("protocol".into(), "openai".into());
        canonical.insert("code".into(), JsValue::String(code));
        canonical.insert("message".into(), JsValue::String(message));
        canonical.insert("retryable".into(), JsValue::Bool(retryable));
        canonical.insert("raw".into(), raw.clone());
        return Ok(vec![event([
            ("type", "error".into()),
            ("error", JsValue::Object(canonical)),
        ])]);
    }
    if let Some(response_id) = read_non_empty_string(field(chunk, "id"))
        && state.stream_response_id.is_none()
    {
        state.stream_response_id = Some(response_id);
    }
    let mut events = Vec::new();
    if !state.started {
        state.started = true;
        events.push(event([
            ("type", "message_start".into()),
            ("role", "assistant".into()),
            ("raw", raw.clone()),
        ]));
    }
    if let Some(usage) = normalize_usage(field(chunk, "usage")) {
        events.push(event([
            ("type", "usage".into()),
            ("usage", usage),
            ("raw", raw.clone()),
        ]));
    }
    let choices = array(field(chunk, "choices"));
    for (position, choice) in choices.iter().enumerate() {
        let choice_record = as_record(choice);
        let choice_index = field(choice_record, "index")
            .as_number()
            .unwrap_or(position as f64);
        let delta = as_record(field(choice_record, "delta"));
        let reasoning = match field(delta, "reasoning_content") {
            JsValue::Undefined | JsValue::Null => field(delta, "reasoning"),
            value => value,
        };
        if let JsValue::String(reasoning) = reasoning
            && !reasoning.is_empty()
        {
            let previous = state.reasoning_snapshot.clone();
            let units = reasoning.units();
            let (emit, snapshot) = if units.starts_with(&previous) {
                (
                    JsString::from_utf16(units[previous.len()..].to_vec()),
                    units.to_vec(),
                )
            } else {
                // `state.reasoningSnapshot = prev + reasoning`: past V8's
                // limit the template/`+` throws and the snapshot is unchanged.
                let mut snapshot = previous.clone();
                append_js_string(&mut snapshot, units)?;
                (reasoning.clone(), snapshot)
            };
            state.reasoning_snapshot = snapshot;
            if !emit.is_empty() {
                events.push(event([
                    ("type", "thinking_delta".into()),
                    ("text", JsValue::String(emit.clone())),
                    ("reasoningContent", JsValue::String(emit)),
                    ("raw", raw.clone()),
                ]));
            }
        }
        if let JsValue::String(content) = field(delta, "content")
            && !content.is_empty()
        {
            events.push(event([
                ("type", "text_delta".into()),
                ("text", JsValue::String(content.clone())),
                ("raw", raw.clone()),
            ]));
        }
        if let JsValue::Array(tool_calls) = field(delta, "tool_calls") {
            events.extend(tool_call_events(tool_calls, state, raw, choice_index)?);
        }
        let finish = field(choice_record, "finish_reason");
        if truthy(finish) {
            let reason = normalize_finish_reason(finish);
            events.extend(finish_tool_calls(state, raw, reason, choice_index)?);
            events.push(event([
                ("type", "message_end".into()),
                ("finishReason", reason.into()),
                ("raw", raw.clone()),
            ]));
        }
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    fn chunk(value: &str) -> JsValue {
        parse_json(value).unwrap()
    }
    /// Restores the previous `STRING_LIMIT` on drop, including on panic: a
    /// leaked tiny limit would make later tests in this thread fail spuriously.
    struct LimitGuard(usize);
    impl LimitGuard {
        fn set(limit: usize) -> Self {
            let previous = crate::runtime::model::http::STRING_LIMIT.with(|cell| cell.get());
            crate::runtime::model::http::STRING_LIMIT.with(|cell| cell.set(limit));
            Self(previous)
        }
    }
    impl Drop for LimitGuard {
        fn drop(&mut self) {
            crate::runtime::model::http::STRING_LIMIT.with(|cell| cell.set(self.0));
        }
    }
    fn tool_key() -> JsString {
        tool_call_key(0.0, 0.0)
    }

    /// node probe (r3): `${argumentsBuffer}${arguments}` past V8's limit
    /// throws `RangeError: Invalid string length`; the buffer keeps its old
    /// value but the shared call object keeps the name/id updates.
    #[test]
    fn tool_argument_accumulator_over_the_v8_limit_throws_range_error() {
        let mut state = StreamState::default();
        let first = chunk(
            r#"{"id":"c","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"lookup","arguments":"abc"}}]}}]}"#,
        );
        let second = chunk(
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"lookup2","arguments":"d"}}]}}]}"#,
        );
        let _limit = LimitGuard::set(3);
        normalize_stream_event(&first, &mut state).unwrap();
        let error = normalize_stream_event(&second, &mut state).unwrap_err();

        assert!(error.name().equals_str("RangeError"), "{error:?}");
        assert!(error.message().equals_str("Invalid string length"));
        let call = state.get(&tool_key()).expect("call survives the throw");
        assert_eq!(
            call.name.as_ref().unwrap().units(),
            "lookup2".encode_utf16().collect::<Vec<_>>()
        );
        assert_eq!(
            call.arguments_buffer.as_ref().unwrap().units(),
            "abc".encode_utf16().collect::<Vec<_>>(),
            "the overflowing fragment is not appended"
        );
    }

    /// `state.reasoningSnapshot = prev + reasoning` on a non-prefix chunk.
    #[test]
    fn reasoning_snapshot_over_the_v8_limit_throws_range_error() {
        let mut state = StreamState::default();
        let first = chunk(r#"{"choices":[{"index":0,"delta":{"reasoning_content":"abc"}}]}"#);
        let second = chunk(r#"{"choices":[{"index":0,"delta":{"reasoning_content":"d"}}]}"#);
        let _limit = LimitGuard::set(3);
        normalize_stream_event(&first, &mut state).unwrap();
        let error = normalize_stream_event(&second, &mut state).unwrap_err();

        assert!(error.name().equals_str("RangeError"), "{error:?}");
        assert!(error.message().equals_str("Invalid string length"));
        assert_eq!(
            state.reasoning_snapshot,
            "abc".encode_utf16().collect::<Vec<_>>()
        );
    }

    #[test]
    fn reparsed_tool_arguments_set_was_repaired() {
        let mut state = StreamState::default();
        normalize_stream_event(
            &chunk(
                r#"{"id":"c","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_r","function":{"name":"lookup","arguments":"{\"q\":\"x\",}"}}]}}]}"#,
            ),
            &mut state,
        )
        .unwrap();
        let events = normalize_stream_event(
            &chunk(r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#),
            &mut state,
        )
        .unwrap();
        let end = events
            .iter()
            .find(|event| {
                field(event, "type")
                    .as_string()
                    .is_some_and(|tag| tag.equals_str("tool_call_end"))
            })
            .expect("tool_call_end");
        assert_eq!(field(end, "wasRepaired").as_bool(), Some(true));
        assert_eq!(
            field(field(end, "toolCall"), "input")
                .get("q")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
            "x"
        );
    }

    /// `argumentsBuffer ?? "{}"`: no argument fragment at all parses as `{}`.
    #[test]
    fn absent_tool_arguments_default_to_an_empty_object() {
        let mut state = StreamState::default();
        normalize_stream_event(
            &chunk(
                r#"{"id":"c","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_e","function":{"name":"lookup"}}]}}]}"#,
            ),
            &mut state,
        )
        .unwrap();
        let events = normalize_stream_event(
            &chunk(r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#),
            &mut state,
        )
        .unwrap();
        let end = events
            .iter()
            .find(|event| {
                field(event, "type")
                    .as_string()
                    .is_some_and(|tag| tag.equals_str("tool_call_end"))
            })
            .expect("tool_call_end");
        assert_eq!(field(end, "wasRepaired").as_bool(), Some(false));
        assert!(
            field(field(end, "toolCall"), "input")
                .as_object()
                .unwrap()
                .is_empty()
        );
    }
}
