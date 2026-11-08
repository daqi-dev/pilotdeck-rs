//! Exact wire accounting admission (missing/null and JS coercion preserved).
use pilotdeck_protocol::{JsObject, JsString, JsValue, model::*};
#[derive(Debug, Clone)]
pub struct AccountingError {
    pub message: String,
}
impl AccountingError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
pub fn accounting_request(
    raw: &JsObject,
) -> Result<(CanonicalModelRequest, usize), AccountingError> {
    let system_prompt = raw
        .get("systemPrompt")
        .filter(|value| truthy(value))
        .map(|value| token_text(Some(value)))
        .transpose()?;
    let messages = iterable(raw.get("messages"), "request.messages")?
        .iter()
        .map(|message| {
            let content = iterable(property(Some(message), "content")?, "message.content")?
                .iter()
                .filter_map(|block| accounting_block(block).transpose())
                .collect::<Result<_, _>>()?;
            Ok(Message {
                role: Role::User,
                content,
                metadata: None,
            })
        })
        .collect::<Result<_, AccountingError>>()?;
    let mut extra_tool_tokens = 0;
    let tools = match raw.get("tools") {
        None | Some(JsValue::Null | JsValue::Undefined) => None,
        value if length(value)?.as_number() == Some(0.0) => None,
        value => {
            let mut tools = Vec::new();
            for tool in iterable(value, "tools")?.iter() {
                let name = template_string(property(Some(tool), "name")?)?;
                let description = tool
                    .get("description")
                    .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                    .map(|value| template_string(Some(value)))
                    .transpose()?;
                if let Some(schema) = tool.get("inputSchema").and_then(JsValue::as_object) {
                    tools.push(ToolSchema {
                        name,
                        description,
                        input_schema: schema.clone(),
                    });
                } else {
                    let mut units = name.units().to_vec();
                    if let Some(description) = description {
                        units.extend_from_slice(description.units());
                    }
                    let json = tool
                        .get("inputSchema")
                        .and_then(|schema| {
                            pilotdeck_protocol::stringify_json(schema).ok().flatten()
                        })
                        .unwrap_or_default();
                    units.extend(json.encode_utf16());
                    extra_tool_tokens +=
                        pilotdeck_tokenizer::count_tokens(&JsString::from_utf16(units))
                            .map_err(|error| AccountingError::new(format!("{error:?}")))?;
                }
            }
            Some(tools)
        }
    };
    Ok((
        CanonicalModelRequest {
            // These routing fields do not participate in local token accounting.
            provider: JsString::default(),
            model: JsString::default(),
            messages,
            tools,
            system_prompt,
            tool_choice: None,
            max_output_tokens: None,
            temperature: None,
            speed: None,
            thinking: None,
            stream: None,
            metadata: None,
            output_schema: None,
            cache_breakpoints: None,
            cache_plan: None,
        },
        extra_tool_tokens,
    ))
}
fn accounting_block(block: &JsValue) -> Result<Option<ContentBlock>, AccountingError> {
    let text = |value| -> Result<ContentBody, AccountingError> {
        Ok(ContentBody::Text(TextBlock {
            text: token_text(value)?,
            block_id: None,
        }))
    };
    let body = match property(Some(block), "type")?
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .as_deref()
    {
        Some("text") => text(block.get("text"))?,
        Some("thinking") => text(
            block
                .get("reasoningContent")
                .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                .or_else(|| block.get("text")),
        )?,
        Some("image" | "pdf" | "audio" | "media_reference") => ContentBody::Image(ImageBlock {
            source: MediaSource::Base64,
            data: JsString::default(),
            mime_type: JsString::default(),
            bytes: None,
            detail: None,
        }),
        Some("tool_call") => ContentBody::ToolCall(ToolCall {
            timeline: None,
            id: JsString::default(),
            name: template_string(block.get("name"))?,
            input: block.get("input").cloned().unwrap_or(JsValue::Undefined),
            raw: None,
        }),
        Some("tool_result") => {
            let content = block.get("content");
            property(content, "map")?; // Nullish receivers throw before the call.
            let Some(JsValue::Array(items)) = content else {
                return Err(AccountingError::new("content.map is not a function"));
            };
            // TS completes map before join coerces text values. A later null
            // block must throw before an earlier text's conversion error.
            let mapped = items
                .iter()
                .map(accounting_result)
                .collect::<Result<Vec<_>, _>>()?;
            ContentBody::ToolResult(ToolResult {
                tool_call_id: JsString::default(),
                is_error: None,
                raw: None,
                content: mapped
                    .iter()
                    .map(|value| -> Result<_, AccountingError> {
                        let text = if matches!(value, JsValue::Null | JsValue::Undefined) {
                            JsString::default()
                        } else {
                            template_string(Some(value))?
                        };
                        Ok(ToolResultItem {
                            timeline: None,
                            body: ToolResultContent::Text(TextBlock {
                                text,
                                block_id: None,
                            }),
                        })
                    })
                    .collect::<Result<_, _>>()?,
            })
        }
        Some("tool_result_reference") => text(block.get("preview"))?,
        _ => return Ok(None),
    };
    Ok(Some(ContentBlock {
        timeline: None,
        body,
    }))
}
fn accounting_result(block: &JsValue) -> Result<JsValue, AccountingError> {
    let tag = property(Some(block), "type")?
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok());
    Ok(match tag.as_deref() {
        Some("text") => block.get("text").cloned().unwrap_or(JsValue::Undefined),
        Some("image" | "pdf") => {
            let pdf = tag.as_deref() == Some("pdf");
            let mut units: Vec<u16> = (if pdf { "[PDF: " } else { "[Image: " })
                .encode_utf16()
                .collect();
            units.extend_from_slice(template_string(block.get("mimeType"))?.units());
            units.extend(", ".encode_utf16());
            if !pdf
                && block
                    .get("source")
                    .and_then(JsValue::as_string)
                    .is_some_and(|value| value.equals_str("url"))
            {
                units.extend_from_slice(template_string(block.get("data"))?.units());
            } else {
                units
                    .extend_from_slice(template_string(Some(&length(block.get("data"))?))?.units());
                units.extend(" base64 characters".encode_utf16());
            }
            if pdf && block.get("pages").is_some_and(truthy) {
                units.extend(", ".encode_utf16());
                units.extend_from_slice(template_string(block.get("pages"))?.units());
                units.extend(" pages".encode_utf16());
            }
            units.push(u16::from(b']'));
            JsValue::String(JsString::from_utf16(units))
        }
        _ => JsValue::Undefined,
    })
}
pub fn flatten_result(block: &JsValue) -> Result<JsValue, AccountingError> {
    let content = block.get("content");
    property(content, "map")?; // Nullish receivers throw before the call.
    let Some(JsValue::Array(items)) = content else {
        return Err(AccountingError::new("content.map is not a function"));
    };
    let mapped = items
        .iter()
        .map(accounting_result)
        .collect::<Result<Vec<_>, _>>()?;
    let mut units = Vec::new();
    for (index, value) in mapped.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        if !matches!(value, JsValue::Null | JsValue::Undefined) {
            units.extend_from_slice(template_string(Some(value))?.units());
        }
    }
    Ok(JsValue::String(JsString::from_utf16(units)))
}
fn template_string(value: Option<&JsValue>) -> Result<JsString, AccountingError> {
    let undefined = JsValue::Undefined;
    let value = value.unwrap_or(&undefined);
    check_primitive_conversion(value)?;
    crate::coercion::to_string_units(value)
        .map(JsString::from_utf16)
        .map_err(|_| conversion_error())
}
// Wire JSON cannot contain callable properties. An own toString shadows the
// inherited conversion method; valueOf cannot supply a primitive either.
// Array#join visits elements, but ordinary object properties are not visited.
fn check_primitive_conversion(value: &JsValue) -> Result<(), AccountingError> {
    let mut frames = vec![std::slice::from_ref(value).iter()];
    while let Some(frame) = frames.last_mut() {
        match frame.next() {
            Some(JsValue::Object(object)) if object.get("toString").is_some() => {
                return Err(AccountingError::new(
                    "Cannot convert object to primitive value",
                ));
            }
            Some(JsValue::Array(items)) => {
                if frames.len() > crate::coercion::MAX_ARRAY_DEPTH {
                    return Err(conversion_error());
                }
                frames.push(items.iter());
            }
            Some(_) => {}
            None => {
                frames.pop();
            }
        }
    }
    Ok(())
}
fn conversion_error() -> AccountingError {
    AccountingError::new(crate::coercion::StackOverflow::MESSAGE)
}
fn property<'a>(
    value: Option<&'a JsValue>,
    field: &str,
) -> Result<Option<&'a JsValue>, AccountingError> {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => {
            let receiver = if matches!(value, Some(JsValue::Null)) {
                "null"
            } else {
                "undefined"
            };
            Err(AccountingError::new(format!(
                "Cannot read properties of {receiver} (reading '{field}')"
            )))
        }
        Some(value) => Ok(value.get(field)),
    }
}
fn length(value: Option<&JsValue>) -> Result<JsValue, AccountingError> {
    Ok(match value {
        Some(JsValue::String(text)) => JsValue::Number(text.len_utf16() as f64),
        Some(JsValue::Array(items)) => JsValue::Number(items.len() as f64),
        _ => property(value, "length")?
            .cloned()
            .unwrap_or(JsValue::Undefined),
    })
}
fn token_text(value: Option<&JsValue>) -> Result<JsString, AccountingError> {
    // countTokens first reads length, then calls js-tiktoken's substring.
    if length(value)?.as_number() == Some(0.0) {
        return Ok(JsString::default());
    }
    match value {
        Some(JsValue::String(text)) => Ok(text.clone()),
        _ => Err(AccountingError::new("text.substring is not a function")),
    }
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
fn iterable<'a>(
    value: Option<&'a JsValue>,
    name: &str,
) -> Result<std::borrow::Cow<'a, [JsValue]>, AccountingError> {
    match value {
        Some(JsValue::Array(items)) => Ok(std::borrow::Cow::Borrowed(items)),
        Some(JsValue::String(text)) => Ok(std::borrow::Cow::Owned(
            char::decode_utf16(text.units().iter().copied())
                .map(|ch| {
                    JsValue::String(match ch {
                        Ok(ch) => JsString::from(ch.to_string()),
                        Err(ch) => JsString::from_utf16(vec![ch.unpaired_surrogate()]),
                    })
                })
                .collect(),
        )),
        _ => Err(AccountingError::new(format!("{name} is not iterable"))),
    }
}
