//! Replay the observable read/clone order of the four native projections.
//! Values projected only for history are retained here because malformed content
//! must fail before metadata/config restoration, even for a selection-only RPC.
use super::{Error, JsValue, error, field, is, object, property, truthy};
use std::collections::HashSet;

fn map<'a>(v: &'a JsValue, expression: &str) -> Result<&'a [JsValue], Error> {
    property(v, "map")?;
    match v {
        JsValue::Array(a) => Ok(a),
        _ => Err(error(
            "gateway_request_failed",
            format!("{expression}.map is not a function"),
        )),
    }
}
fn clone_messages(v: &JsValue) -> Result<Vec<JsValue>, Error> {
    map(v, "messages")?.iter().map(clone_message).collect()
}
fn clone_message(v: &JsValue) -> Result<JsValue, Error> {
    let content = property(v, "content")?;
    let mut blocks = vec![];
    if let JsValue::Array(a) = content {
        for b in a {
            if is(property(b, "type")?, "tool_result") {
                map(field(b, "content"), "tr.content")?;
            }
            blocks.push(b.clone());
        }
    }
    // JS spreads primitives and null blocks, but accesses .type beforehand.
    let mut result = v
        .as_object()
        .cloned()
        .map(JsValue::Object)
        .unwrap_or_else(|| object([]));
    if let JsValue::Object(o) = &mut result {
        o.insert("content".into(), JsValue::Array(blocks));
    }
    Ok(result)
}
fn snapshot(boundary: &JsValue) -> Result<Option<&JsValue>, Error> {
    let s = field(boundary, "snapshot");
    if s.as_object().is_none() || field(s, "version").as_number() != Some(1.0) {
        return Ok(None);
    }
    let JsValue::Array(messages) = field(s, "messages") else {
        return Ok(None);
    };
    if messages.is_empty() {
        return Ok(None);
    }
    for message in messages {
        if !valid_message(message)? {
            return Ok(None);
        }
    }
    Ok(Some(field(s, "messages")))
}
fn valid_message(m: &JsValue) -> Result<bool, Error> {
    if m.as_object().is_none()
        || !(is(field(m, "role"), "user") || is(field(m, "role"), "assistant"))
        || !(field(m, "metadata").is_undefined() || field(m, "metadata").as_object().is_some())
    {
        return Ok(false);
    }
    let JsValue::Array(a) = field(m, "content") else {
        return Ok(false);
    };
    for block in a {
        if !valid_block(block)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn checked_string(v: &JsValue) -> Result<(), Error> {
    if crate::config::string_coercible(v) {
        Ok(())
    } else {
        Err(error(
            "gateway_request_failed",
            "Cannot convert object to primitive value",
        ))
    }
}
fn valid_block(b: &JsValue) -> Result<bool, Error> {
    if b.as_object().is_none() {
        return Ok(false);
    }
    let text = |key| field(b, key).as_string().is_some();
    Ok(
        match field(b, "type")
            .as_string()
            .and_then(|s| s.to_utf8().ok())
            .as_deref()
        {
            Some("text" | "thinking") => text("text"),
            Some("image" | "audio") => {
                (is(field(b, "source"), "base64") || is(field(b, "source"), "url"))
                    && text("data")
                    && text("mimeType")
            }
            Some("pdf") => {
                is(field(b, "source"), "base64")
                    && text("data")
                    && is(field(b, "mimeType"), "application/pdf")
                    && field(b, "bytes").as_number().is_some()
            }
            Some("tool_call") => text("id") && text("name"),
            Some("tool_result") => {
                if !text("toolCallId") {
                    return Ok(false);
                }
                let JsValue::Array(a) = field(b, "content") else {
                    return Ok(false);
                };
                for block in a {
                    if block.as_object().is_none() {
                        return Ok(false);
                    }
                    checked_string(field(block, "type"))?;
                    if !["text", "image", "pdf"]
                        .iter()
                        .any(|t| is(field(block, "type"), t))
                        || !valid_block(block)?
                    {
                        return Ok(false);
                    }
                }
                true
            }
            Some("tool_result_reference" | "media_reference") => {
                if !(text("path")
                    && field(b, "originalBytes").as_number().is_some()
                    && text("preview")
                    && field(b, "hasMore").as_bool().is_some())
                {
                    return Ok(false);
                }
                if is(field(b, "type"), "tool_result_reference") {
                    text("toolCallId")
                } else if text("mimeType") {
                    checked_string(field(b, "mediaType"))?;
                    ["image", "pdf", "audio"]
                        .iter()
                        .any(|t| is(field(b, "mediaType"), t))
                } else {
                    false
                }
            }
            _ => false,
        },
    )
}
fn compact(b: &JsValue) -> Result<bool, Error> {
    Ok(is(property(b, "kind")?, "compact") && is(field(b, "subtype"), "compact_boundary"))
}
fn clone_result(r: &JsValue) -> Result<(), Error> {
    property(r, "usage")?;
    map(field(r, "permissionDenials"), "result.permissionDenials")?;
    let errors = field(r, "errors");
    if !matches!(errors, JsValue::Null | JsValue::Undefined) && !matches!(errors, JsValue::Array(_))
    {
        return Err(error(
            "gateway_request_failed",
            "result.errors?.map is not a function",
        ));
    }
    Ok(())
}
fn user_request(m: &JsValue) -> Result<bool, Error> {
    if !is(field(m, "role"), "user")
        || field(field(m, "metadata"), "synthetic").as_bool() == Some(true)
    {
        return Ok(false);
    }
    let JsValue::Array(blocks) = field(m, "content") else {
        return Ok(false);
    };
    for b in blocks {
        let t = field(b, "type");
        if is(t, "tool_result")
            || is(t, "tool_result_reference")
            || (is(t, "media_reference") && field(b, "toolCallId").as_string().is_some())
        {
            continue;
        }
        if !is(t, "text") {
            return Ok(true);
        }
        let text = property(field(b, "text"), "trim")?;
        let _ = text;
        let Some(text) = field(b, "text").as_string() else {
            return Err(error(
                "gateway_request_failed",
                "block.text.trim is not a function",
            ));
        };
        let text = String::from_utf16_lossy(text.units());
        let text = crate::metadata::js_trim(&text);
        if !text.is_empty()
            && text
                != "[system: the conversation above has been compacted. please continue with the current task.]"
            && ![
                "<compact-boundary",
                "<snip-boundary",
                "<memory-context>",
                "<internal-compaction-control",
                "<hook_context",
            ]
            .iter()
            .any(|p| text.starts_with(p))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(super) fn selection(entries: &[JsValue]) -> Result<JsValue, Error> {
    let mut included = vec![];
    let mut completed = HashSet::new();
    let mut boundary_index = None;
    let mut replacement: Option<(usize, Vec<JsValue>, bool)> = None;
    // Conversation reduce completes before its finalize and before other projections.
    for (i, e) in entries.iter().enumerate() {
        let t = field(e, "type");
        if [
            "accepted_input",
            "assistant_message",
            "tool_result_message",
            "durable_message",
            "turn_result",
        ]
        .iter()
        .any(|s| is(t, s))
        {
            included.push(i);
            if is(t, "turn_result") {
                completed.insert(field(e, "turnId").as_string().unwrap().clone());
            }
        } else if is(t, "context_snapshot") {
            let m = field(e, "runtimeContextMessages");
            let length = match m {
                JsValue::Array(a) => a.len() as f64,
                JsValue::String(s) => s.len_utf16() as f64,
                _ => field(m, "length").as_number().unwrap_or(0.0),
            };
            if truthy(m) && length > 0.0 {
                included.push(i);
            }
        } else if is(t, "control_boundary") {
            let b = field(e, "boundary");
            if compact(b)? {
                let snapshot = snapshot(b)?;
                let messages = snapshot.unwrap_or(field(b, "replacementMessages"));
                if truthy(messages) {
                    replacement = Some((i, clone_messages(messages)?, snapshot.is_some()));
                    boundary_index = Some(i);
                    included.push(i);
                } else if b.as_object().is_some_and(|o| o.contains_key("snapshot")) {
                    included.push(i);
                }
            }
        }
    }
    let mut messages = vec![];
    for i in included {
        let e = &entries[i];
        let t = field(e, "type");
        let before = boundary_index.is_some_and(|b| i < b);
        let done = completed.contains(field(e, "turnId").as_string().unwrap());
        if is(t, "accepted_input") {
            if !before {
                messages.extend(clone_messages(field(e, "messages"))?);
            }
        } else if [
            "assistant_message",
            "tool_result_message",
            "durable_message",
        ]
        .iter()
        .any(|s| is(t, s))
        {
            let m = field(e, "message");
            let metadata = property(m, "metadata")?;
            if field(metadata, "compactReplacement").as_bool() == Some(true) {
                continue;
            }
            if done && !before {
                messages.push(clone_message(m)?);
                property(m, "role")?;
            }
        } else if is(t, "context_snapshot") {
            if !before && done && truthy(field(e, "runtimeContextMessages")) {
                let additions = clone_messages(field(e, "runtimeContextMessages"))?;
                if !additions.is_empty() {
                    let mut index = messages.len();
                    for (i, m) in messages.iter().enumerate().rev() {
                        if user_request(m)? {
                            index = i;
                            break;
                        }
                    }
                    messages.splice(index..index, additions);
                }
            }
        } else if is(t, "turn_result") {
            if !before {
                clone_result(field(e, "result"))?;
            }
        } else if is(t, "control_boundary")
            && let Some((b, m, replay)) = &replacement
            && *b == i
            && (*replay || done)
        {
            for m in m {
                messages.push(clone_message(m)?);
                property(m, "role")?;
            }
        }
    }
    // Turn-summary reduce still visits records hidden by a compact boundary.
    for e in entries
        .iter()
        .filter(|e| is(field(e, "type"), "turn_result"))
    {
        let result = field(e, "result");
        let usage = property(result, "usage")?;
        for key in [
            "inputTokens",
            "outputTokens",
            "cacheReadTokens",
            "cacheWriteTokens",
            "totalTokens",
        ] {
            property(usage, key)?;
        }
        let denials = field(result, "permissionDenials");
        if !matches!(denials, JsValue::Array(_) | JsValue::String(_)) {
            return Err(error(
                "gateway_request_failed",
                "entry.result.permissionDenials is not iterable",
            ));
        }
    }
    let mut saved = JsValue::Undefined;
    for e in entries {
        if is(field(e, "type"), "session_metadata") {
            let metadata = field(e, "metadata");
            property(metadata, "title")?;
            property(metadata, "linkedPullRequest")?;
            if !field(metadata, "modelSelection").is_undefined() {
                saved = field(metadata, "modelSelection").clone();
            }
        } else if is(field(e, "type"), "accepted_input") {
            let selection = field(field(e, "metadata"), "modelSelection");
            if truthy(selection)
                && (is(field(selection, "mode"), "auto")
                    || (is(field(selection, "mode"), "model")
                        && field(selection, "provider").as_string().is_some()
                        && field(selection, "model").as_string().is_some()))
            {
                saved = selection.clone();
            }
        }
    }
    Ok(saved)
}
