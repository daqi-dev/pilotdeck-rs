//! Original canonical Anthropic blocks and shared request helpers; no IO.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
static UNDEFINED: JsValue = JsValue::Undefined;
pub fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}
pub fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsValue {
    let mut result = JsObject::new();
    for (key, value) in entries {
        if !value.is_undefined() {
            result.insert(key.into(), value);
        }
    }
    JsValue::Object(result)
}
pub fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(values) = value {
        values
    } else {
        &[]
    }
}
pub fn is(value: &JsValue, text: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(text))
}
pub fn text(value: &JsValue) -> String {
    String::from_utf16_lossy(
        &pilotdeck_protocol::coercion::to_string_units(value).unwrap_or_default(),
    )
}
pub fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        _ => true,
    }
}
pub fn present(value: &JsValue) -> bool {
    !matches!(value, JsValue::Undefined | JsValue::Null)
}
pub fn put(value: &mut JsValue, key: &str, item: JsValue) {
    if let JsValue::Object(object) = value {
        object.insert(key.into(), item);
    }
}
pub fn cache_control() -> JsValue {
    object([("type", "ephemeral".into()), ("ttl", "5m".into())])
}

pub fn budget_only_model(model_id: &str) -> bool {
    static PATTERN: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    PATTERN
        .get_or_init(|| {
            regex::Regex::new(
                r"claude-(?:3|haiku-3|haiku-4|(?:sonnet|opus)-4(?:[.-][0-5](?:[^0-9]|$)|-20|$))",
            )
            .expect("static regex")
        })
        .is_match(model_id)
}
pub fn tool_choice(choice: &JsValue, anthropic: bool) -> JsValue {
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
pub fn anthropic_block(block: &JsValue) -> JsValue {
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
pub fn concatenate(parts: &[JsValue]) -> JsValue {
    let mut units = Vec::new();
    for part in parts {
        units.extend(pilotdeck_protocol::coercion::to_string_units(part).unwrap_or_default());
    }
    JsValue::String(JsString::from_utf16(units))
}
pub fn reference_text(block: &JsValue) -> JsValue {
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
