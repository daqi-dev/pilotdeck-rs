//! createLocalGateway.ts:1633-1701 and AgentTranscriptProjections.ts conversation.
use super::*;
use std::collections::HashSet;

fn invalid(message: &str) -> Error {
    gateway("INVALID_SESSION_ARCHIVE", message.to_owned())
}

pub(super) fn validate(archive: &JsValue) -> Result<&[JsValue], Error> {
    if field(archive, "schemaVersion").as_number() != Some(1.0)
        || !is(field(archive, "format"), "portable_text_messages")
    {
        return Err(invalid(
            "Unsupported or malformed session transcript archive.",
        ));
    }
    let JsValue::Array(messages) = field(archive, "messages") else {
        return Err(invalid(
            "Unsupported or malformed session transcript archive.",
        ));
    };
    if messages.is_empty() || messages.len() > 10_000 {
        return Err(invalid(
            "Session transcript archive has an invalid message count.",
        ));
    }
    let title = field(archive, "title");
    if !title.is_undefined() && title.as_string().is_none_or(|s| s.len_utf16() > 512) {
        return Err(invalid("Session transcript archive title is invalid."));
    }
    for message in messages {
        if !(is(field(message, "role"), "user") || is(field(message, "role"), "assistant"))
            || field(message, "text").as_string().is_none()
        {
            return Err(invalid(
                "Session transcript archive contains an invalid message.",
            ));
        }
    }
    let bytes = stringify_json(archive)
        .map_err(|e| plain("TypeError", e.to_string()))?
        .ok_or_else(|| invalid("Unsupported or malformed session transcript archive."))?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(invalid("Session transcript archive is too large."));
    }
    Ok(messages)
}

pub(super) fn project(entries: &[JsValue]) -> Result<JsValue, Error> {
    let (messages, title) = replay(entries)?;
    let mut portable = Vec::new();
    for message in messages {
        if field(field(&message, "metadata"), "synthetic").as_bool() == Some(true) {
            continue;
        }
        let text = portable_text(&message)?;
        if !text.is_empty() {
            portable.push(object([
                ("role", field(&message, "role").clone()),
                ("text", JsValue::String(text)),
            ]));
        }
    }
    let mut archive = fields([
        ("schemaVersion", JsValue::Number(1.0)),
        ("format", "portable_text_messages".into()),
        ("messages", JsValue::Array(portable)),
    ]);
    if let Some(title) = title
        .as_string()
        .filter(|s| !s.is_empty() && s.len_utf16() <= 512)
    {
        archive.insert("title".into(), JsValue::String(title.clone()));
    }
    Ok(JsValue::Object(archive))
}

fn array<'a>(value: &'a JsValue, expression: &str) -> Result<&'a [JsValue], Error> {
    property(value, "map")?;
    match value {
        JsValue::Array(items) => Ok(items),
        _ => Err(plain(
            "TypeError",
            format!("{expression}.map is not a function"),
        )),
    }
}
fn clone_messages(value: &JsValue) -> Result<Vec<JsValue>, Error> {
    array(value, "messages")?
        .iter()
        .map(clone_message)
        .collect()
}
fn clone_message(value: &JsValue) -> Result<JsValue, Error> {
    let content = property(value, "content")?;
    let mut blocks = Vec::new();
    if let JsValue::Array(items) = content {
        for block in items {
            if is(property(block, "type")?, "tool_result") {
                array(field(block, "content"), "tr.content")?;
            }
            blocks.push(block.clone());
        }
    }
    let mut out = value.as_object().cloned().unwrap_or_default();
    out.insert("content".into(), JsValue::Array(blocks));
    Ok(JsValue::Object(out))
}

/// Replay deliberately dereferences malformed durable messages even when their
/// turn is incomplete or they precede a compact boundary, as TS does.
pub(super) fn replay(entries: &[JsValue]) -> Result<(Vec<JsValue>, JsValue), Error> {
    let completed: HashSet<JsString> = entries
        .iter()
        .filter(|e| is(field(e, "type"), "turn_result"))
        .filter_map(|e| field(e, "turnId").as_string().cloned())
        .collect();
    let mut replacement = None;
    for (index, entry) in entries.iter().enumerate() {
        if !is(field(entry, "type"), "control_boundary") {
            continue;
        }
        let boundary = field(entry, "boundary");
        if is(property(boundary, "kind")?, "compact")
            && is(field(boundary, "subtype"), "compact_boundary")
        {
            let snapshot = field(boundary, "snapshot");
            let snapshot_messages = match field(snapshot, "messages") {
                JsValue::Array(messages)
                    if snapshot.as_object().is_some()
                        && field(snapshot, "version").as_number() == Some(1.0)
                        && !messages.is_empty()
                        && messages.iter().all(valid_message) =>
                {
                    Some(messages.as_slice())
                }
                _ => None,
            };
            if let Some(messages) = snapshot_messages {
                replacement = Some((index, messages.to_vec(), true));
            } else if truthy(field(boundary, "replacementMessages")) {
                replacement = Some((
                    index,
                    clone_messages(field(boundary, "replacementMessages"))?,
                    false,
                ));
            }
        }
    }
    let mut messages = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let before = replacement.as_ref().is_some_and(|(at, _, _)| index < *at);
        let complete = field(entry, "turnId")
            .as_string()
            .is_some_and(|id| completed.contains(id));
        if is(field(entry, "type"), "accepted_input") {
            if !before {
                messages.extend(clone_messages(field(entry, "messages"))?);
            }
        } else if [
            "assistant_message",
            "tool_result_message",
            "durable_message",
        ]
        .iter()
        .any(|s| is(field(entry, "type"), s))
        {
            let message = field(entry, "message");
            let metadata = property(message, "metadata")?;
            if field(metadata, "compactReplacement").as_bool() != Some(true) && complete && !before
            {
                messages.push(clone_message(message)?);
            }
        } else if is(field(entry, "type"), "context_snapshot") && complete && !before {
            let additions = field(entry, "runtimeContextMessages");
            if truthy(additions) {
                let additions = clone_messages(additions)?;
                let index = messages
                    .iter()
                    .rposition(real_user_request)
                    .unwrap_or(messages.len());
                messages.splice(index..index, additions);
            }
        } else if is(field(entry, "type"), "turn_result") && !before {
            let result = field(entry, "result");
            array(
                property(result, "permissionDenials")?,
                "result.permissionDenials",
            )?;
            let errors = field(result, "errors");
            if !matches!(errors, JsValue::Null | JsValue::Undefined) {
                array(errors, "result.errors")?;
            }
        } else if replacement
            .as_ref()
            .is_some_and(|(at, _, early)| index == *at && (*early || complete))
        {
            messages.extend(
                replacement
                    .as_ref()
                    .unwrap()
                    .1
                    .iter()
                    .map(clone_message)
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
    }
    // TS evaluates all projections, including summary and metadata, even for
    // portable export. Do not let malformed non-message payloads disappear.
    let mut title = JsValue::Undefined;
    let mut ai_title = JsValue::Undefined;
    for entry in entries {
        if is(field(entry, "type"), "turn_result") {
            let usage = property(field(entry, "result"), "usage")?;
            property(usage, "inputTokens")?;
        }
        if is(field(entry, "type"), "session_metadata") {
            let metadata = field(entry, "metadata");
            let next = property(metadata, "title")?;
            if !matches!(next, JsValue::Null | JsValue::Undefined) {
                title = next.clone();
            }
            if !field(metadata, "aiTitle").is_undefined() {
                ai_title = field(metadata, "aiTitle").clone();
            }
        }
    }
    Ok((
        messages,
        if matches!(title, JsValue::Null | JsValue::Undefined) {
            ai_title
        } else {
            title
        },
    ))
}

fn valid_message(message: &JsValue) -> bool {
    message.as_object().is_some()
        && (is(field(message, "role"), "user") || is(field(message, "role"), "assistant"))
        && (field(message, "metadata").is_undefined()
            || field(message, "metadata").as_object().is_some())
        && matches!(field(message, "content"), JsValue::Array(items) if items.iter().all(valid_block))
}
fn valid_block(block: &JsValue) -> bool {
    if block.as_object().is_none() {
        return false;
    }
    let string = |key| field(block, key).as_string().is_some();
    match field(block, "type")
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .as_deref()
    {
        Some("text" | "thinking") => string("text"),
        Some("image" | "audio") => {
            (is(field(block, "source"), "base64") || is(field(block, "source"), "url"))
                && string("data")
                && string("mimeType")
        }
        Some("pdf") => {
            is(field(block, "source"), "base64")
                && string("data")
                && is(field(block, "mimeType"), "application/pdf")
                && field(block, "bytes").as_number().is_some()
        }
        Some("tool_call") => string("id") && string("name"),
        Some("tool_result") => {
            string("toolCallId")
                && matches!(field(block, "content"), JsValue::Array(items)
            if items.iter().all(|b| ["text", "image", "pdf"].iter().any(|s| is(field(b,"type"),s)) && valid_block(b)))
        }
        Some("tool_result_reference" | "media_reference") => {
            string("path")
                && string("preview")
                && field(block, "originalBytes").as_number().is_some()
                && field(block, "hasMore").as_bool().is_some()
                && if is(field(block, "type"), "tool_result_reference") {
                    string("toolCallId")
                } else {
                    string("mimeType")
                        && ["image", "pdf", "audio"]
                            .iter()
                            .any(|s| is(field(block, "mediaType"), s))
                }
        }
        _ => false,
    }
}
fn real_user_request(message: &JsValue) -> bool {
    if !is(field(message, "role"), "user")
        || field(field(message, "metadata"), "synthetic").as_bool() == Some(true)
    {
        return false;
    }
    let JsValue::Array(blocks) = field(message, "content") else {
        return false;
    };
    blocks.iter().any(|block| {
        if ["tool_result", "tool_result_reference"].iter().any(|s| is(field(block,"type"),s))
            || (is(field(block,"type"),"media_reference") && field(block,"toolCallId").as_string().is_some()) { return false; }
        if !is(field(block,"type"),"text") { return true; }
        let Some(value) = field(block,"text").as_string() else { return false; };
        let value = text(&trim(value)).to_lowercase();
        !value.is_empty() && value != "[system: the conversation above has been compacted. please continue with the current task.]"
            && !["<compact-boundary", "<snip-boundary", "<memory-context>", "<internal-compaction-control", "<hook_context"].iter().any(|s| value.starts_with(s))
    })
}
fn display(value: &JsValue) -> Result<JsString, Error> {
    crate::js_value::display(value)
        .map(JsString::from)
        .map_err(|_| plain("RangeError", "Maximum call stack size exceeded"))
}
fn join(parts: &[JsString], separator: &str) -> JsString {
    let mut units = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            units.extend(separator.encode_utf16());
        }
        units.extend_from_slice(part.units());
    }
    JsString::from_utf16(units)
}
fn portable_text(message: &JsValue) -> Result<JsString, Error> {
    let mut parts = Vec::new();
    let JsValue::Array(blocks) = field(message, "content") else {
        return Ok("".into());
    };
    for block in blocks {
        let kind = property(block, "type")?;
        if is(kind, "text") && truthy(field(block, "text")) {
            parts.push(display(field(block, "text"))?);
        } else if is(kind, "tool_call") {
            let input = field(block, "input");
            let value = match input.as_string() {
                Some(s) => s.clone(),
                None => {
                    let json =
                        stringify_json(input).map_err(|e| plain("TypeError", e.to_string()))?;
                    json.map_or_else(|| "undefined".into(), |s| pretty(&s).into())
                }
            };
            parts.push(join(
                &[
                    "[Tool call: ".into(),
                    display(field(block, "name"))?,
                    "]\n".into(),
                    value,
                ],
                "",
            ));
        } else if is(kind, "tool_result") {
            let contents = array(field(block, "content"), "content")?;
            let mut rendered = Vec::new();
            for content in contents {
                let kind = property(content, "type")?;
                if is(kind, "text") {
                    let text = field(content, "text");
                    rendered.push(if matches!(text, JsValue::Null | JsValue::Undefined) {
                        "".into()
                    } else {
                        display(text)?
                    });
                } else if is(kind, "image") || is(kind, "pdf") {
                    let mime = display(field(content, "mimeType"))?;
                    let data = field(content, "data");
                    let representation = if is(kind, "image") && is(field(content, "source"), "url")
                    {
                        display(data)?
                    } else {
                        format!(
                            "{} base64 characters",
                            property(data, "length")?.as_number().map_or_else(
                                || data
                                    .as_string()
                                    .map_or("undefined".into(), |s| s.len_utf16().to_string()),
                                |n| n.to_string()
                            )
                        )
                        .into()
                    };
                    let pages = field(content, "pages");
                    let suffix = if is(kind, "pdf") && truthy(pages) {
                        join(&[", ".into(), display(pages)?, " pages".into()], "")
                    } else {
                        "".into()
                    };
                    rendered.push(join(
                        &[
                            if is(kind, "pdf") {
                                "[PDF: ".into()
                            } else {
                                "[Image: ".into()
                            },
                            mime,
                            ", ".into(),
                            representation,
                            suffix,
                            "]".into(),
                        ],
                        "",
                    ));
                } else {
                    rendered.push("".into());
                }
            }
            let text = join(&rendered, "\n");
            parts.push(if text.is_empty() {
                "[Tool result]".into()
            } else {
                join(&["[Tool result]\n".into(), text], "")
            });
        } else if is(kind, "tool_result_reference") {
            parts.push(join(
                &[
                    "[Tool result reference: ".into(),
                    display(field(block, "toolCallId"))?,
                    "]".into(),
                ],
                "",
            ));
        } else if is(kind, "image") {
            parts.push("[Image]".into());
        }
    }
    Ok(trim(&join(&parts, "\n\n")))
}

/// Indent the protocol codec's JSON without decoding its UTF-16 escapes.
fn pretty(json: &str) -> String {
    let mut out = String::new();
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    let mut chars = json.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => {
                quoted = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                if chars.peek() == Some(&if c == '{' { '}' } else { ']' }) {
                    out.push(chars.next().unwrap());
                } else {
                    depth += 1;
                    out.push('\n');
                    out.push_str(&"  ".repeat(depth));
                }
            }
            '}' | ']' => {
                depth -= 1;
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(c);
            }
            ',' => {
                out.push_str(",\n");
                out.push_str(&"  ".repeat(depth));
            }
            ':' => out.push_str(": "),
            _ => out.push(c),
        }
    }
    out
}
