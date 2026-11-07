//! Provider count request projection (no generation-only fields).
use super::CountHttpRequest;
use crate::config::changes::{Operand, get_property};
use crate::host::{HostError, ModuleError};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

static UNDEFINED: JsValue = JsValue::Undefined;
pub(super) use pilotdeck_model_anthropic::blocks::{field,object,array,is,text,truthy,present,put,cache_control,tool_choice,anthropic_block,concatenate,reference_text};

fn reading_error(receiver: &str, request: &JsObject, key: &str) -> HostError {
    // TS keeps the property name's UTF-16 units, including lone surrogates, in
    // the TypeError message. `format!` would replacement-decode them first.
    let mut units = format!("Cannot read properties of {receiver} (reading '")
        .encode_utf16()
        .collect::<Vec<_>>();
    units.extend(
        crate::js_value::to_string_units(request.get(key).unwrap_or(&UNDEFINED))
            .unwrap_or_default(),
    );
    units.extend("')".encode_utf16());
    ModuleError::new(JsString::from_utf16(units)).into()
}

pub(super) fn count_request(
    config: &JsObject,
    request: &JsObject,
) -> Result<Option<CountHttpRequest>, HostError> {
    let Some(providers) = config.get("providers") else {
        return Err(reading_error("undefined", request, "provider"));
    };
    if !present(providers) {
        let receiver = if matches!(providers, JsValue::Null) {
            "null"
        } else {
            "undefined"
        };
        return Err(reading_error(receiver, request, "provider"));
    }
    // The baseline indexes `providers[request.provider]` and then reads
    // `provider?.models[request.model]` on a plain object. Reuse the config
    // diff's JavaScript lookup so `Object.prototype` members are visible here
    // too instead of silently behaving like an own-property miss.
    let Some(provider) = request
        .get("provider")
        .and_then(JsValue::as_string)
        .and_then(|key| get_property(Operand::Value(providers), key))
    else {
        return Ok(None);
    };
    let provider = match provider {
        Operand::Value(JsValue::Undefined | JsValue::Null) => {
            return Ok(None);
        }
        Operand::Value(value @ JsValue::Object(_)) => value,
        // `Object.prototype`/its function members and non-nullish primitives
        // all have an undefined `models`, which the baseline immediately
        // indexes with `request.model` and therefore throws.
        Operand::Value(_)
        | Operand::Number(_)
        | Operand::Function(_)
        | Operand::ObjectPrototype => {
            return Err(reading_error("undefined", request, "model"));
        }
    };
    if !present(field(provider, "models")) {
        let receiver = if matches!(field(provider, "models"), JsValue::Null) {
            "null"
        } else {
            "undefined"
        };
        return Err(reading_error(receiver, request, "model"));
    }
    let Some(model) = request
        .get("model")
        .and_then(JsValue::as_string)
        .and_then(|key| get_property(Operand::Value(field(provider, "models")), key))
    else {
        return Ok(None);
    };
    // An inherited `Object.prototype` member is truthy and reaches the body
    // builder as a model with no own fields. The builder only throws when it
    // reads `model.capabilities` (maxOutputTokens/supportsSpeed), so the
    // protocol and official-URL gate must run first: the baseline returns
    // undefined from countWithProvider before any body is built.
    let prototype_model;
    let (model, prototype) = match model {
        Operand::Value(value) if truthy(value) => (value, false),
        Operand::Value(_) => return Ok(None),
        Operand::Function(_) | Operand::ObjectPrototype | Operand::Number(_) => {
            prototype_model = JsValue::Object(JsObject::new());
            (&prototype_model, true)
        }
    };
    let anthropic = is(field(provider, "protocol"), "anthropic");
    // TS normalizes every non-Anthropic URL before checking the protocol.
    // Anthropic builds its body first, then joinUrl calls base.endsWith().
    // Keep that error precedence for direct calls bypassing config validation.
    let url = if anthropic {
        None
    } else {
        let base = count_base_url(provider, false)?;
        if !is(field(provider, "protocol"), "openai-responses") {
            return Ok(None);
        }
        let Ok(mut parsed) = url::Url::parse(base.trim()) else {
            return Ok(None);
        };
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
        parsed.set_query(None);
        parsed.set_fragment(None);
        let serialized = parsed.as_str();
        let normalized = serialized.strip_suffix('/').unwrap_or(serialized);
        if !matches!(
            normalized,
            "https://api.openai.com" | "https://api.openai.com/v1"
        ) {
            return Ok(None);
        }
        Some(format!(
            "{}/v1/responses/input_tokens",
            parsed.origin().ascii_serialization()
        ))
    };
    let request = JsValue::Object(request.clone());
    let thinking = thinking(&request, provider, model, anthropic)?;
    let body = if anthropic {
        anthropic_body(&request, thinking, prototype)?
    } else {
        responses_body(&request, thinking, prototype)?
    };
    let url = if let Some(url) = url {
        url
    } else {
        let base = count_base_url(provider, true)?;
        format!(
            "{}/v1/messages/count_tokens",
            base.strip_suffix('/').unwrap_or(&base)
        )
    };
    let mut headers = JsObject::new();
    headers.insert("content-type".into(), "application/json".into());
    if let Some(custom) = field(provider, "headers").as_object() {
        for (name, value) in custom.entries() {
            headers.insert(name.clone(), value.clone());
        }
    }
    let key = text(field(provider, "apiKey"));
    let key = crate::metadata::js_trim(&key);
    if anthropic {
        headers.insert("x-api-key".into(), key.into());
        if headers.get("anthropic-version").is_none_or(|v| !present(v)) {
            headers.insert("anthropic-version".into(), "2023-06-01".into());
        }
    } else if headers.get("authorization").is_none_or(|v| !present(v)) {
        headers.insert(
            "authorization".into(),
            format!("Bearer {key}").as_str().into(),
        );
    }
    Ok(Some(CountHttpRequest { url, headers, body }))
}

fn count_base_url(provider: &JsValue, anthropic: bool) -> Result<String, HostError> {
    let value = field(provider, "url");
    if value.as_string().is_some() {
        return Ok(text(value));
    }
    let (receiver, method) = if anthropic {
        ("base", "endsWith")
    } else {
        ("raw", "trim")
    };
    let message = match value {
        JsValue::Undefined | JsValue::Null => format!(
            "Cannot read properties of {} (reading '{method}')",
            if value.is_undefined() {
                "undefined"
            } else {
                "null"
            }
        ),
        _ => format!("{receiver}.{method} is not a function"),
    };
    Err(ModuleError::new(message).into())
}

/// The thinking field of the count bodies, through the shared
/// `resolveThinkingPlan` port (`providers::thinking`). `countAnthropic` calls
/// `buildAnthropicRequest` without a provider (TS default
/// `{ id: "anthropic", protocol: "anthropic" }`); the Responses count passes
/// the configured provider and keeps only `reasoning`.
fn thinking(
    request: &JsValue,
    provider: &JsValue,
    model: &JsValue,
    anthropic: bool,
) -> Result<JsValue, HostError> {
    let to_host = |error: super::http::ModelStreamError| -> HostError {
        ModuleError::new(error.message()).into()
    };
    let default_provider = object([("id", "anthropic".into()), ("protocol", "anthropic".into())]);
    let provider = if anthropic {
        &default_provider
    } else {
        provider
    };
    let plan = super::providers::thinking::resolve(request, provider, model).map_err(to_host)?;
    super::providers::thinking::throw_if_unsupported(&plan, request).map_err(to_host)?;
    Ok(if anthropic {
        plan.thinking_type
            .map_or(JsValue::Undefined, |kind| object([("type", kind.into())]))
    } else {
        match (plan.use_openai_reasoning, plan.effort) {
            (true, Some(effort)) => object([("effort", JsValue::String(effort))]),
            _ => JsValue::Undefined,
        }
    })
}
/// `thinking/registry.ts` budget-only Claude models. The TS regex has no `u`
/// flag, so `\D` is "not an ASCII digit": `[^0-9]`, not Rust's Unicode `\D`.
/// Shared by the count and stream request builders.
fn anthropic_body(
    request: &JsValue,
    thinking: JsValue,
    prototype: bool,
) -> Result<JsValue, HostError> {
    // `buildAnthropicRequest` evaluates `request.tools?.map(...)` before the
    // returned object's `max_tokens`/`speed` reads.
    if present(field(request, "tools")) && !matches!(field(request, "tools"), JsValue::Array(_)) {
        return Err(ModuleError::new("request.tools?.map is not a function").into());
    }
    let mut tools: Vec<_> = array(field(request, "tools"))
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
    // `max_tokens: request.maxOutputTokens ?? model.capabilities.maxOutputTokens`
    // is evaluated after the tools were built, before messages.
    if prototype && !present(field(request, "maxOutputTokens")) {
        return Err(ModuleError::new(
            "Cannot read properties of undefined (reading 'maxOutputTokens')",
        )
        .into());
    }
    let plan = field(request, "cachePlan");
    let cache_tools = field(plan, "tools") == &JsValue::Bool(true) && !tools.is_empty();
    if cache_tools {
        put(
            tools.last_mut().expect("nonempty"),
            "cache_control",
            cache_control(),
        );
    }
    let breakpoints = if present(field(plan, "messages")) {
        field(plan, "messages")
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
    let prompt = field(request, "systemPrompt");
    let system = if !truthy(prompt) {
        JsValue::Undefined
    } else if field(plan, "system") == &JsValue::Bool(true)
        || (plan.is_undefined() && !field(request, "cacheBreakpoints").is_undefined())
    {
        JsValue::Array(vec![object([
            ("type", "text".into()),
            ("text", prompt.clone()),
            ("cache_control", cache_control()),
        ])])
    } else {
        prompt.clone()
    };
    // `speed` is the last capabilities read in the returned object literal.
    if prototype && field(request, "speed") != &JsValue::Undefined {
        return Err(ModuleError::new(
            "Cannot read properties of undefined (reading 'supportsSpeed')",
        )
        .into());
    }
    Ok(object([
        ("model", field(request, "model").clone()),
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
        ("thinking", thinking),
    ]))
}
fn responses_body(
    request: &JsValue,
    reasoning: JsValue,
    prototype: bool,
) -> Result<JsValue, HostError> {
    let input = responses_input(request)?;
    // `buildOpenAIResponsesRequest` evaluates `input` (messages) first, then
    // `max_output_tokens`, then `request.tools?.map(...)`, then service_tier.
    if prototype && !present(field(request, "maxOutputTokens")) {
        return Err(ModuleError::new(
            "Cannot read properties of undefined (reading 'maxOutputTokens')",
        )
        .into());
    }
    let tools = field(request, "tools");
    if present(tools) && !matches!(tools, JsValue::Array(_)) {
        return Err(ModuleError::new("request.tools?.map is not a function").into());
    }
    let tools_json = if present(tools) {
        JsValue::Array(
            array(tools)
                .iter()
                .map(|tool| {
                    Ok::<_, HostError>(object([
                        ("type", "function".into()),
                        ("name", field(tool, "name").clone()),
                        ("description", field(tool, "description").clone()),
                        ("parameters", normalize_schema(field(tool, "inputSchema"))?),
                        ("strict", JsValue::Bool(true)),
                    ]))
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
    } else {
        JsValue::Undefined
    };
    let choice = tool_choice(field(request, "toolChoice"), false);
    if prototype && field(request, "speed") != &JsValue::Undefined {
        return Err(ModuleError::new(
            "Cannot read properties of undefined (reading 'supportsSpeed')",
        )
        .into());
    }
    let schema = field(request, "outputSchema");
    Ok(object([
        ("model", field(request, "model").clone()),
        ("input", JsValue::Array(input)),
        ("instructions", field(request, "systemPrompt").clone()),
        ("tools", tools_json),
        ("tool_choice", choice),
        (
            "text",
            if truthy(schema) {
                object([(
                    "format",
                    object([
                        ("type", "json_schema".into()),
                        ("name", field(schema, "name").clone()),
                        ("description", field(schema, "description").clone()),
                        ("schema", normalize_schema(field(schema, "schema"))?),
                        (
                            "strict",
                            if present(field(schema, "strict")) {
                                field(schema, "strict").clone()
                            } else {
                                JsValue::Bool(true)
                            },
                        ),
                    ]),
                )])
            } else {
                JsValue::Undefined
            },
        ),
        ("reasoning", reasoning),
    ]))
}
/// `request.messages.flatMap(toResponsesInputItems)` of
/// `providers/openai-responses/request.ts`; shared by the count projection
/// and the streaming request builder.
pub(super) fn responses_input(request: &JsValue) -> Result<Vec<JsValue>, HostError> {
    let mut input = Vec::new();
    for message in array(field(request, "messages")) {
        let mut content = Vec::new();
        for block in array(field(message, "content")) {
            let tag = text(field(block, "type"));
            if ["tool_call", "tool_result", "tool_result_reference"].contains(&tag.as_str()) {
                flush(&mut input, &mut content, field(message, "role"));
                if tag == "tool_call" {
                    let args = field(block, "input");
                    let args = if present(args) {
                        args.clone()
                    } else {
                        object([])
                    };
                    input.push(object([
                        ("type", "function_call".into()),
                        ("call_id", field(block, "id").clone()),
                        ("name", field(block, "name").clone()),
                        (
                            "arguments",
                            stringify_json(&args)
                                .map_err(|e| ModuleError::new(e.to_string()))?
                                .unwrap_or_default()
                                .as_str()
                                .into(),
                        ),
                    ]));
                } else {
                    let output = if tag == "tool_result_reference" {
                        reference_text(block)
                    } else {
                        crate::host::modules::budget::flatten_result(block)?
                    };
                    input.push(object([
                        ("type", "function_call_output".into()),
                        ("call_id", field(block, "toolCallId").clone()),
                        ("output", output),
                    ]));
                    let visual: Vec<_> = (if tag == "tool_result" {
                        array(field(block, "content"))
                    } else {
                        &[]
                    })
                    .iter()
                    .filter(|b| is(field(b, "type"), "image") || is(field(b, "type"), "pdf"))
                    .map(responses_part)
                    .collect();
                    if !visual.is_empty() {
                        let mut parts = vec![object([
                            ("type", "input_text".into()),
                            ("text", "[Visual content from tool result]".into()),
                        ])];
                        parts.extend(visual);
                        input.push(object([
                            ("role", "user".into()),
                            ("content", JsValue::Array(parts)),
                        ]));
                    }
                }
            } else {
                let part = responses_part(block);
                if !part.is_undefined() {
                    content.push(part);
                }
            }
        }
        flush(&mut input, &mut content, field(message, "role"));
    }
    Ok(input)
}
fn flush(input: &mut Vec<JsValue>, content: &mut Vec<JsValue>, role: &JsValue) {
    if !content.is_empty() {
        input.push(object([
            ("role", role.clone()),
            ("content", JsValue::Array(std::mem::take(content))),
        ]));
    }
}
fn responses_part(block: &JsValue) -> JsValue {
    let tag = text(field(block, "type"));
    match tag.as_str() {
        "text" | "thinking" | "media_reference" => object([
            ("type", "input_text".into()),
            (
                "text",
                field(
                    block,
                    if tag == "media_reference" {
                        "preview"
                    } else {
                        "text"
                    },
                )
                .clone(),
            ),
        ]),
        "image" => object([
            ("type", "input_image".into()),
            (
                "image_url",
                if is(field(block, "source"), "url") {
                    field(block, "data").clone()
                } else {
                    data_url(block)
                },
            ),
            ("detail", field(block, "detail").clone()),
        ]),
        "pdf" => object([
            ("type", "input_file".into()),
            ("filename", "document.pdf".into()),
            ("file_data", data_url(block)),
        ]),
        "audio" => object([
            ("type", "input_text".into()),
            (
                "text",
                if is(field(block, "source"), "url") {
                    concatenate(&[
                        "[Audio URL: ".into(),
                        field(block, "data").clone(),
                        "]".into(),
                    ])
                } else {
                    "[Audio content omitted]".into()
                },
            ),
        ]),
        _ => JsValue::Undefined,
    }
}
fn data_url(block: &JsValue) -> JsValue {
    concatenate(&[
        "data:".into(),
        field(block, "mimeType").clone(),
        ";base64,".into(),
        field(block, "data").clone(),
    ])
}
pub(super) fn normalize_schema(value: &JsValue) -> Result<JsValue, HostError> {
    super::json_walk::transform(value, false, |raw| {
        let mut normalized = JsObject::new();
        for (key, value) in raw.entries() {
            if key.equals_str("__proto__") {
                if matches!(
                    value,
                    JsValue::Object(_) | JsValue::Array(_) | JsValue::Null
                ) {
                    normalized.set_prototype(value.clone());
                }
            } else {
                normalized.insert(key.clone(), value.clone());
            }
        }
        if normalized.get_property("type").is_none() {
            let kind = if let Some(value) = normalized.get_property("const") {
                literal_type(value)
            } else {
                let types: Vec<_> = array(normalized.get_property("enum").unwrap_or(&UNDEFINED))
                    .iter()
                    .map(literal_type)
                    .collect();
                if types.is_empty() || types.iter().any(Option::is_none) {
                    None
                } else if types.iter().all(|t| *t == types[0]) {
                    types[0]
                } else if types
                    .iter()
                    .all(|t| matches!(t, Some("integer" | "number")))
                {
                    Some("number")
                } else {
                    None
                }
            };
            if let Some(kind) = kind {
                normalized.insert("type".into(), kind.into());
            }
        }
        let kind = normalized.get_property("type").unwrap_or(&UNDEFINED);
        if (is(kind, "array") || array(kind).iter().any(|k| is(k, "array")))
            && normalized.get_property("items").is_none()
        {
            normalized.insert("items".into(), object([]));
        }
        normalized
    })
}
fn literal_type(value: &JsValue) -> Option<&'static str> {
    match value {
        JsValue::String(_) => Some("string"),
        JsValue::Bool(_) => Some("boolean"),
        JsValue::Number(n) if n.is_finite() => Some(if n.fract() == 0.0 {
            "integer"
        } else {
            "number"
        }),
        JsValue::Null => Some("null"),
        JsValue::Array(_) => Some("array"),
        JsValue::Object(_) => Some("object"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    /// Includes malformed URL / tools combinations to pin exception precedence.
    #[test]
    fn count_urls_match_real_ts_including_error_precedence() {
        let oracle = crate::test_baseline::oracle(include_str!(
            "../../../tests/fixtures/diff-backfill/oracle.json"
        ));
        let cases = array(field(&oracle, "urlCases"));
        assert_eq!(cases.len(), 48);
        for case in cases {
            let raw = text(field(case, "raw"));
            let url = if raw == "undefined" {
                String::new()
            } else {
                format!("\"url\":{raw},")
            };
            let protocol = text(field(case, "protocol"));
            let config = parse_json(&format!(
                r#"{{"providers":{{"p":{{"id":"p","protocol":"{protocol}",{url}"apiKey":"fixture","models":{{"m":{{"id":"m","capabilities":{{"maxOutputTokens":10}}}}}}}}}}}}"#
            )).unwrap();
            let bad_tools = field(case, "badTools").as_bool().unwrap();
            let request = parse_json(if bad_tools {
                r#"{"provider":"p","model":"m","messages":[],"tools":"x"}"#
            } else {
                r#"{"provider":"p","model":"m","messages":[]}"#
            })
            .unwrap();
            let actual = count_request(config.as_object().unwrap(), request.as_object().unwrap());
            if let Some(expected) = field(case, "error").as_string() {
                assert_eq!(
                    actual.unwrap_err().message,
                    expected.to_utf8().unwrap(),
                    "{case:?}"
                );
            } else {
                let sent: Vec<_> = actual
                    .unwrap()
                    .into_iter()
                    .map(|request| request.url)
                    .collect();
                let expected: Vec<_> = array(field(case, "sent")).iter().map(text).collect();
                assert_eq!(sent, expected, "{case:?}");
            }
        }
    }

    /// node probe (pr7 `r2/probe-r2.out.txt`, `buildOpenAIResponsesRequest`):
    /// the Responses count keeps `reasoning` only for the OpenAI reasoning
    /// format; an effort carried by another format never becomes reasoning.
    #[test]
    fn responses_count_reasoning_follows_the_shared_thinking_plan() {
        let request = parse_json(r#"{"thinking":{"mode":"high"}}"#).unwrap();
        let provider = parse_json(r#"{"id":"p","protocol":"openai-responses"}"#).unwrap();
        let reasoning = |format: &str| {
            let model = parse_json(&format!(
                r#"{{"id":"gpt-x","thinking":{{"state":"enabled","efforts":["high"],"format":"{format}"}}}}"#
            ))
            .unwrap();
            stringify_json(&thinking(&request, &provider, &model, false).unwrap())
                .unwrap()
                .unwrap_or_default()
        };
        assert_eq!(reasoning("anthropic"), "");
        assert_eq!(reasoning("openai"), r#"{"effort":"high"}"#);
        assert_eq!(reasoning("qwen-cloud"), "");
    }
}
