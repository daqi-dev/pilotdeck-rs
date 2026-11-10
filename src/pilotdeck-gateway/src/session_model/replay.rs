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
/// The `String(...)` coercion inside `isContentBlock` (CompactSnapshot.ts:16-47)
/// throws for an object carrying a `toString` property and throws `RangeError`
/// once the array nesting passes the shared 2000-container bound; the same
/// transcript line can reach this port through `session_model_get`, so the
/// recursive truthiness helper would abort the process here exactly as it did
/// in `storage::transcript_adapter` (#1550 P0). Both throw surfaces collapse
/// into the RPC envelope TS builds at `GatewayWsConnection.ts:251` (code
/// `gateway_request_failed`, `error.message` verbatim, no name field).
fn checked_string(v: &JsValue) -> Result<(), Error> {
    match crate::js_value::string_coercible_checked(v) {
        Ok(()) => Ok(()),
        Err(kind) => Err(error("gateway_request_failed", kind.message())),
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
/// One replay over the agent-transcript projections, the shared fold behind
/// both `session_model_get` (the saved model selection) and the portable
/// transcript archive export (the conversation messages plus the merged
/// title). #297 P2#3: the two halves read one fold, so a port change cannot
/// drift between them.
pub(crate) struct Replay {
    /// Conversation finalize output (the replayed `messages`).
    pub(crate) messages: Vec<JsValue>,
    /// `mergeMetadata`'s `title` (`second.title ?? first.title`, so nullish
    /// values keep the previous one) and `aiTitle` (object spread: a present
    /// key overrides with any value, an absent one keeps the old). The
    /// archive's title chain is `title ?? aiTitle` over the merged values.
    pub(crate) title: Option<JsValue>,
    pub(crate) ai_title: Option<JsValue>,
    /// The saved model selection this module was written for.
    pub(crate) model_selection: JsValue,
}

pub(super) fn selection(entries: &[JsValue]) -> Result<JsValue, Error> {
    Ok(replay(entries)?.model_selection)
}

pub(super) fn replay(entries: &[JsValue]) -> Result<Replay, Error> {
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
    let mut title: Option<JsValue> = None;
    let mut ai_title: Option<JsValue> = None;
    for e in entries {
        if is(field(e, "type"), "session_metadata") {
            let metadata = field(e, "metadata");
            property(metadata, "title")?;
            property(metadata, "linkedPullRequest")?;
            // mergeMetadata's title keeps the previous value while the new one
            // is nullish; aiTitle follows the spread (a present key wins, any
            // value, so only a present key replaces it).
            if !matches!(field(metadata, "title"), JsValue::Undefined | JsValue::Null) {
                title = Some(field(metadata, "title").clone());
            }
            if let JsValue::Object(object) = metadata
                && let Some(value) = object.get("aiTitle")
            {
                ai_title = Some(value.clone());
            }
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
    Ok(Replay {
        messages,
        title,
        ai_title,
        model_selection: saved,
    })
}

#[cfg(test)]
mod coercion_depth_tests {
    use super::*;

    /// A `tool_result` block whose inner block `type` is `depth` arrays deep.
    /// `isContentBlock` runs `String(block.type)` on inner blocks
    /// (`CompactSnapshot.ts:41-43`), so this is the shape that reaches
    /// `checked_string` through `snapshot` → `valid_message` → `valid_block`.
    fn nested(depth: usize) -> JsValue {
        let mut value = JsValue::from("text");
        for _ in 0..depth {
            value = JsValue::Array(vec![value]);
        }
        value
    }

    fn tool_result_with_deep_type(depth: usize) -> JsValue {
        let inner = object([("type", nested(depth)), ("text", JsValue::from("x"))]);
        object([
            ("type", JsValue::from("tool_result")),
            ("toolCallId", JsValue::from("t")),
            ("content", JsValue::Array(vec![inner])),
        ])
    }

    /// `session_model_get` reads the same untrusted `chats/<key>.jsonl` as the
    /// storage adapter, so this goes through the real entry (`valid_block`),
    /// not the private helper: a deeply nested `block.type` must surface as the
    /// RPC error TS builds for a caught `RangeError` (code
    /// `gateway_request_failed`, message verbatim, no name field) instead of
    /// aborting the process.
    #[test]
    fn deep_block_type_is_a_gateway_error_not_a_stack_overflow() {
        let failed = valid_block(&tool_result_with_deep_type(
            crate::js_value::MAX_ARRAY_DEPTH + 1,
        ))
        .expect_err("past the shared bound the coercion throws");
        assert_eq!(failed.code, "gateway_request_failed");
        assert_eq!(
            failed.message.to_utf8().expect("utf-8 message"),
            crate::js_value::StackOverflow::MESSAGE
        );
        // At the bound TS `String()` still succeeds, so the port must too; the
        // joined text `"text"` is not one of the accepted inner kinds.
        assert!(
            !valid_block(&tool_result_with_deep_type(
                crate::js_value::MAX_ARRAY_DEPTH
            ))
            .expect("at the shared bound the coercion succeeds")
        );
    }

    /// The other `String()` call site in `isContentBlock`: `value.mediaType`
    /// on a `media_reference` block (`CompactSnapshot.ts:44-50`).
    #[test]
    fn deep_media_type_is_a_gateway_error_not_a_stack_overflow() {
        let block = |depth: usize| {
            object([
                ("type", JsValue::from("media_reference")),
                ("path", JsValue::from("media/p.png")),
                ("originalBytes", JsValue::Number(1.0)),
                ("preview", JsValue::from("preview")),
                ("hasMore", JsValue::Bool(false)),
                ("mimeType", JsValue::from("image/png")),
                ("mediaType", nested(depth)),
            ])
        };
        let failed = valid_block(&block(crate::js_value::MAX_ARRAY_DEPTH + 1))
            .expect_err("past the shared bound the coercion throws");
        assert_eq!(failed.code, "gateway_request_failed");
        assert_eq!(
            failed.message.to_utf8().expect("utf-8 message"),
            crate::js_value::StackOverflow::MESSAGE
        );
        assert!(
            !valid_block(&block(crate::js_value::MAX_ARRAY_DEPTH))
                .expect("at the shared bound the coercion succeeds")
        );
    }
}
