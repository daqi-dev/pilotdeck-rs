//! Provider count request projection (no generation-only fields).
use super::CountHttpRequest;
use crate::config::changes::{Operand, get_property};
use crate::host::{HostError, ModuleError};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

static UNDEFINED: JsValue = JsValue::Undefined;

pub(super) fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
pub(super) fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in entries {
        if !value.is_undefined() {
            result.insert(key.into(), value);
        }
    }
    JsValue::Object(result)
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(values) = value {
        values
    } else {
        &[]
    }
}
fn is(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(text))
}
fn text(value: &JsValue) -> String {
    String::from_utf16_lossy(&crate::js_value::to_string_units(value).unwrap_or_default())
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn present(value: &JsValue) -> bool {
    !matches!(value, JsValue::Undefined | JsValue::Null)
}
fn put(value: &mut JsValue, key: &str, item: JsValue) {
    if let JsValue::Object(object) = value {
        object.insert(key.into(), item);
    }
}
fn cache_control() -> JsValue {
    object([("type", "ephemeral".into()), ("ttl", "5m".into())])
}

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
    let base = text(field(provider, "url"));
    let url = if anthropic {
        format!(
            "{}/v1/messages/count_tokens",
            base.strip_suffix('/').unwrap_or(&base)
        )
    } else {
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
        format!(
            "{}/v1/responses/input_tokens",
            parsed.origin().ascii_serialization()
        )
    };
    let request = JsValue::Object(request.clone());
    let thinking = thinking(&request, model, anthropic)?;
    let body = if anthropic {
        anthropic_body(&request, thinking, prototype)?
    } else {
        responses_body(&request, thinking, prototype)?
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

fn thinking(request: &JsValue, model: &JsValue, anthropic: bool) -> Result<JsValue, HostError> {
    let settings = field(model, "thinking");
    let state = field(settings, "state");
    if !present(state) || is(state, "default") {
        return Ok(JsValue::Undefined);
    }
    let disabled = is(state, "disabled");
    let mode = field(field(request, "thinking"), "mode");
    let effort = if !disabled
        && ["low", "medium", "high", "xhigh", "max"]
            .iter()
            .any(|e| is(mode, e))
    {
        Some(text(mode))
    } else {
        None
    };
    let fail = |message: String| -> HostError {
        // `model.id` may contain lone surrogates; keep its UTF-16 units.
        let mut units = crate::js_value::to_string_units(field(model, "id")).unwrap_or_default();
        units.extend(": ".encode_utf16());
        units.extend(message.encode_utf16());
        ModuleError::new(JsString::from_utf16(units)).into()
    };
    if let Some(effort) = &effort {
        let supported = match field(settings, "efforts") {
            JsValue::Array(efforts) => efforts.iter().any(|e| is(e, effort)),
            JsValue::String(efforts) => {
                let needle: Vec<_> = effort.encode_utf16().collect();
                efforts.units().windows(needle.len()).any(|s| s == needle)
            }
            value @ (JsValue::Null | JsValue::Undefined) => {
                let receiver = if matches!(value, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                };
                return Err(ModuleError::new(format!(
                    "Cannot read properties of {receiver} (reading 'includes')"
                ))
                .into());
            }
            _ => {
                return Err(
                    ModuleError::new("settings?.efforts.includes is not a function").into(),
                );
            }
        };
        if !supported {
            return Err(fail(format!(
                "Reasoning effort '{effort}' is not configured. Select Default or configure the model's supported efforts."
            )));
        }
    }
    if is(field(settings, "format"), "server-default") {
        if disabled || effort.is_some() {
            return Err(fail("This parameter format only supports the server default. Choose a thinking parameter format in model settings.".into()));
        }
        return Ok(JsValue::Undefined);
    }
    if anthropic {
        if disabled {
            return Ok(object([("type", "disabled".into())]));
        }
        let old_model = regex::Regex::new(
            r"claude-(?:3|haiku-3|haiku-4|(?:sonnet|opus)-4(?:[.-][0-5](?:\D|$)|-20|$))",
        )
        .expect("static regex")
        .is_match(&text(field(model, "id")));
        if old_model {
            if effort.is_some() {
                return Err(fail(
                    "This model requires a thinking budget; only Default is supported.".into(),
                ));
            }
            return Ok(JsValue::Undefined);
        }
        Ok(object([("type", "adaptive".into())]))
    } else {
        Ok(if disabled {
            object([("effort", "none".into())])
        } else if let Some(effort) = effort {
            object([("effort", effort.as_str().into())])
        } else {
            JsValue::Undefined
        })
    }
}
fn tool_choice(choice: &JsValue, anthropic: bool) -> JsValue {
    if !truthy(choice) {
        return JsValue::Undefined;
    }
    for name in ["auto", "none", "required"] {
        if is(choice, name) {
            return if anthropic {
                object([("type", if name == "required" { "any" } else { name }.into())])
            } else {
                choice.clone()
            };
        }
    }
    object([
        ("type", if anthropic { "tool" } else { "function" }.into()),
        ("name", field(choice, "name").clone()),
    ])
}
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
fn anthropic_block(block: &JsValue) -> JsValue {
    let tag = text(field(block, "type"));
    match tag.as_str() {
        "text" => object([
            ("type", "text".into()),
            ("text", field(block, "text").clone()),
        ]),
        "thinking" => object([
            ("type", "thinking".into()),
            ("thinking", field(block, "text").clone()),
            (
                "signature",
                if truthy(field(block, "signature")) {
                    field(block, "signature").clone()
                } else {
                    JsValue::Undefined
                },
            ),
        ]),
        "image" | "pdf" | "audio" => {
            let source = if tag != "pdf" && is(field(block, "source"), "url") {
                object([
                    ("type", "url".into()),
                    ("url", field(block, "data").clone()),
                ])
            } else {
                object([
                    ("type", "base64".into()),
                    ("media_type", field(block, "mimeType").clone()),
                    ("data", field(block, "data").clone()),
                ])
            };
            object([
                (
                    "type",
                    if tag == "pdf" {
                        "document"
                    } else {
                        tag.as_str()
                    }
                    .into(),
                ),
                ("source", source),
            ])
        }
        "tool_call" => object([
            ("type", "tool_use".into()),
            ("id", field(block, "id").clone()),
            ("name", field(block, "name").clone()),
            ("input", field(block, "input").clone()),
        ]),
        "tool_result" | "tool_result_reference" => object([
            ("type", "tool_result".into()),
            ("tool_use_id", field(block, "toolCallId").clone()),
            (
                "content",
                JsValue::Array(if tag == "tool_result" {
                    array(field(block, "content"))
                        .iter()
                        .map(|b| {
                            if ["text", "image", "pdf"]
                                .iter()
                                .any(|tag| is(field(b, "type"), tag))
                            {
                                anthropic_block(b)
                            } else {
                                JsValue::Undefined
                            }
                        })
                        .collect()
                } else {
                    vec![object([
                        ("type", "text".into()),
                        ("text", reference_text(block)),
                    ])]
                }),
            ),
            ("is_error", field(block, "isError").clone()),
        ]),
        "media_reference" => object([
            ("type", "text".into()),
            ("text", field(block, "preview").clone()),
        ]),
        _ => JsValue::Undefined,
    }
}
fn concatenate(parts: &[JsValue]) -> JsValue {
    let mut units = Vec::new();
    for part in parts {
        units.extend(crate::js_value::to_string_units(part).unwrap_or_default());
    }
    JsValue::String(JsString::from_utf16(units))
}
fn reference_text(block: &JsValue) -> JsValue {
    if !truthy(field(block, "hasMore")) {
        return field(block, "preview").clone();
    }
    let path = if present(field(block, "readFilePath")) {
        field(block, "readFilePath")
    } else {
        field(block, "path")
    };
    concatenate(&[
        field(block, "preview").clone(), "\n\n[Tool result preview only: original ".into(), field(block, "originalBytes").clone(),
        " bytes. Full output was saved at: ".into(), path.clone(),
        ". To inspect it, first search within the persisted result with grep({ pattern: \"<keyword>\", path: \"".into(), path.clone(),
        "\", output_mode: \"content\", head_limit: 20 }) for relevant names, IDs, errors, URLs, dates, or candidate terms. Then read only the matching neighborhood with read_file({ file_path: \"".into(), path.clone(),
        "\", offset: <line>, limit: 80 }) when surrounding context is needed. Avoid paging through the whole file from offset 1 unless the task truly requires a complete sequential review. If the task depends on complete lists, counts, search candidates, evidence checks, or long page content, use grep/refined searches and targeted reads on the persisted result before concluding.]".into(),
    ])
}
fn responses_body(
    request: &JsValue,
    reasoning: JsValue,
    prototype: bool,
) -> Result<JsValue, HostError> {
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
fn normalize_schema(value: &JsValue) -> Result<JsValue, HostError> {
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
