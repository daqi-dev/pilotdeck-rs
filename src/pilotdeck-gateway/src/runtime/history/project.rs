use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionError(pub String);
impl fmt::Display for ProjectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ProjectionError {}

/// Context preserved from the original durable row, never the runtime snapshot.
pub struct Context<'a> {
    pub session_key: &'a JsString,
    pub project_key: Option<&'a JsValue>,
    pub created_at: &'a JsValue,
    pub entry_id: Option<&'a JsValue>,
    pub turn_id: Option<&'a JsValue>,
    pub sequence: Option<&'a JsValue>,
    pub message_index: usize,
    pub fork_unsupported: bool,
}
fn field<'a>(v: &'a JsValue, key: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(key).unwrap_or(&U)
}
fn is(v: &JsValue, s: &str) -> bool {
    v.as_string().is_some_and(|v| v.equals_str(s))
}
fn required<'a>(v: &'a JsValue, key: &str) -> Result<&'a JsValue, ProjectionError> {
    let x = field(v, key);
    if x.is_undefined() {
        Err(ProjectionError(format!("History missing {key}")))
    } else {
        Ok(x)
    }
}
fn id(ctx: &Context<'_>, suffix: &str) -> JsString {
    let mut v = ctx.session_key.units().to_vec();
    v.extend(suffix.encode_utf16());
    JsString::from_utf16(v)
}
fn base(ctx: &Context<'_>, suffix: &str, role: &str, kind: &str) -> JsObject {
    let mut o = JsObject::new();
    o.insert("id".into(), JsValue::String(id(ctx, suffix)));
    o.insert(
        "sessionKey".into(),
        JsValue::String(ctx.session_key.clone()),
    );
    o.insert(
        "projectKey".into(),
        ctx.project_key.cloned().unwrap_or(JsValue::Undefined),
    );
    o.insert("createdAt".into(), ctx.created_at.clone());
    o.insert("provider".into(), "pilotdeck".into());
    o.insert("role".into(), role.into());
    o.insert("kind".into(), kind.into());
    o
}
fn finish(mut o: JsObject, ctx: &Context<'_>) -> JsValue {
    o.insert("source".into(), "history".into());
    if let Some(v) = ctx.turn_id {
        o.insert("turnId".into(), v.clone());
    } else {
        o.insert("turnId".into(), JsValue::Undefined);
    }
    if let Some(v) = ctx.sequence {
        o.insert("sequence".into(), v.clone());
    } else {
        o.insert("sequence".into(), JsValue::Undefined);
    }
    JsValue::Object(o)
}
fn append_utf16(target: &mut Vec<u16>, v: &JsValue) -> Result<(), ProjectionError> {
    let s = v
        .as_string()
        .ok_or_else(|| ProjectionError("History text must be a string".into()))?;
    target.extend_from_slice(s.units());
    Ok(())
}
fn truthy_string(v: &JsValue) -> bool {
    v.as_string().is_some_and(|s| !s.units().is_empty())
}

/// Flatten one canonical message in source order; unsupported blocks fail closed.
pub fn flatten_message(
    message: &JsValue,
    ctx: &Context<'_>,
) -> Result<Vec<JsValue>, ProjectionError> {
    let role = if is(field(message, "role"), "user") {
        "user"
    } else {
        "assistant"
    };
    let blocks = match field(message, "content") {
        JsValue::Array(v) => v,
        _ => return Err(ProjectionError("History content must be an array".into())),
    };
    let mut output = Vec::new();
    let mut text = Vec::new();
    let mut block_id = JsValue::Undefined;
    let mut timeline = JsValue::Undefined;
    let flush =
        |out: &mut Vec<JsValue>, text: &mut Vec<u16>, block_id: &JsValue, timeline: &JsValue| {
            if text.is_empty() {
                return;
            }
            let suffix = format!("-msg-{}-{}", ctx.message_index, out.len());
            let mut o = base(ctx, &suffix, role, "text");
            o.insert(
                "text".into(),
                JsValue::String(JsString::from_utf16(std::mem::take(text))),
            );
            if truthy_string(block_id) {
                o.insert("blockId".into(), block_id.clone());
            }
            if !matches!(
                timeline,
                JsValue::Null | JsValue::Undefined | JsValue::Bool(false)
            ) {
                o.insert("timeline".into(), timeline.clone());
            }
            if role == "assistant"
                && let Some(v) = field(message, "metadata")
                    .get("model")
                    .filter(|v| v.as_string().is_some())
            {
                o.insert("model".into(), v.clone());
            }
            if ctx.fork_unsupported {
                let mut p = JsObject::new();
                p.insert("forkUnsupportedContent".into(), JsValue::Bool(true));
                p.insert(
                    "forkUnsupportedReason".into(),
                    "This turn contains attachments or media.".into(),
                );
                o.insert("payload".into(), JsValue::Object(p));
            }
            if let Some(v) = ctx.entry_id.filter(|v| truthy_string(v)) {
                o.insert("entryId".into(), v.clone());
            }
            out.push(finish(o, ctx));
        };
    for block in blocks {
        match field(block, "type")
            .as_string()
            .and_then(|s| s.to_utf8().ok())
            .as_deref()
        {
            Some("text") => {
                let next = field(block, "blockId");
                if &block_id != next {
                    flush(&mut output, &mut text, &block_id, &timeline);
                }
                block_id = next.clone();
                timeline = field(block, "timeline").clone();
                append_utf16(&mut text, required(block, "text")?)?;
            }
            Some("thinking") => {
                flush(&mut output, &mut text, &block_id, &timeline);
                block_id = JsValue::Undefined;
                timeline = JsValue::Undefined;
                let suffix = format!("-thinking-{}-{}", ctx.message_index, output.len());
                let mut o = base(ctx, &suffix, "assistant", "thinking");
                o.insert("text".into(), required(block, "text")?.clone());
                let b = field(block, "blockId");
                if truthy_string(b) {
                    o.insert("blockId".into(), b.clone());
                }
                let t = field(block, "timeline");
                if !matches!(t, JsValue::Null | JsValue::Undefined | JsValue::Bool(false)) {
                    o.insert("timeline".into(), t.clone());
                }
                output.push(finish(o, ctx));
            }
            Some("tool_call") => {
                flush(&mut output, &mut text, &block_id, &timeline);
                block_id = JsValue::Undefined;
                timeline = JsValue::Undefined;
                let call = required(block, "id")?
                    .as_string()
                    .ok_or_else(|| ProjectionError("History tool id must be string".into()))?;
                let mut suffix = format!("-tool-{}-", ctx.message_index)
                    .encode_utf16()
                    .collect::<Vec<_>>();
                suffix.extend_from_slice(call.units());
                let mut o = base(ctx, "", "tool", "tool_use");
                let mut units = ctx.session_key.units().to_vec();
                units.extend(suffix);
                o.insert("id".into(), JsValue::String(JsString::from_utf16(units)));
                o.insert("toolCallId".into(), JsValue::String(call.clone()));
                o.insert("toolName".into(), required(block, "name")?.clone());
                o.insert("payload".into(), field(block, "input").clone());
                let t = field(block, "timeline");
                if !matches!(t, JsValue::Null | JsValue::Undefined | JsValue::Bool(false)) {
                    o.insert("timeline".into(), t.clone());
                }
                output.push(finish(o, ctx));
            }
            Some("tool_result" | "tool_result_reference" | "media_reference") => {
                flush(&mut output, &mut text, &block_id, &timeline);
                block_id = JsValue::Undefined;
                timeline = JsValue::Undefined;
                let kind = field(block, "type");
                let media = is(kind, "media_reference");
                let reference = is(kind, "tool_result_reference");
                let call = field(block, "toolCallId");
                let mut suffix = if media {
                    format!("-media-{}-{}", ctx.message_index, output.len())
                        .encode_utf16()
                        .collect::<Vec<_>>()
                } else {
                    let mut v = format!("-tool-{}-", ctx.message_index)
                        .encode_utf16()
                        .collect::<Vec<_>>();
                    append_utf16(&mut v, call)?;
                    v.extend(if reference { "-result-ref" } else { "-result" }.encode_utf16());
                    v
                };
                let mut units = ctx.session_key.units().to_vec();
                units.append(&mut suffix);
                let mut o = base(ctx, "", "tool", "tool_result");
                o.insert("id".into(), JsValue::String(JsString::from_utf16(units)));
                o.insert("toolCallId".into(), call.clone());
                let raw = field(block, "raw");
                let tool = field(raw, "toolName");
                if !reference && !media && truthy_string(tool) {
                    o.insert("toolName".into(), tool.clone());
                }
                o.insert(
                    "ok".into(),
                    JsValue::Bool(media || field(block, "isError") != &JsValue::Bool(true)),
                );
                if reference || media {
                    o.insert("text".into(), field(block, "preview").clone());
                    if reference {
                        o.insert("resultPath".into(), field(block, "path").clone());
                    }
                    let mut payload = JsObject::new();
                    for k in if media {
                        &[
                            "path",
                            "originalBytes",
                            "hasMore",
                            "mimeType",
                            "mediaType",
                            "pages",
                            "detail",
                            "reason",
                        ][..]
                    } else {
                        &["path", "originalBytes", "hasMore", "mimeType", "reason"][..]
                    } {
                        payload.insert((*k).into(), field(block, k).clone());
                    }
                    o.insert("payload".into(), JsValue::Object(payload));
                } else {
                    let JsValue::Array(parts) = required(block, "content")? else {
                        return Err(ProjectionError(
                            "History tool result content must be an array".into(),
                        ));
                    };
                    let mut joined = Vec::new();
                    for (i, part) in parts.iter().enumerate() {
                        if i > 0 {
                            joined.push(b'\n' as u16);
                        }
                        if is(field(part, "type"), "text") {
                            append_utf16(&mut joined, required(part, "text")?)?;
                        } else {
                            return Err(ProjectionError(
                                "Unsupported tool result content block".into(),
                            ));
                        }
                    }
                    o.insert("text".into(), JsValue::String(JsString::from_utf16(joined)));
                    let code = field(field(raw, "error"), "code");
                    if truthy_string(code) {
                        o.insert("errorCode".into(), code.clone());
                    }
                    let data = field(raw, "data");
                    if field(data, "planFilePath").as_string().is_some() {
                        let mut p = JsObject::new();
                        for k in ["planFilePath", "planTitle", "planSummary"] {
                            p.insert(k.into(), field(data, k).clone());
                        }
                        o.insert("payload".into(), JsValue::Object(p));
                    } else if tool
                        .as_string()
                        .and_then(|s| s.to_utf8().ok())
                        .is_some_and(|s| {
                            s.eq_ignore_ascii_case("grep") || s.eq_ignore_ascii_case("glob")
                        })
                        && matches!(data, JsValue::Object(_) | JsValue::Array(_))
                    {
                        o.insert("payload".into(), data.clone());
                    }
                }
                let t = field(block, "timeline");
                if !matches!(t, JsValue::Null | JsValue::Undefined | JsValue::Bool(false)) {
                    o.insert("timeline".into(), t.clone());
                }
                output.push(finish(o, ctx));
            }
            other => {
                return Err(ProjectionError(format!(
                    "Unsupported history content block: {}",
                    other.unwrap_or("missing")
                )));
            }
        }
    }
    flush(&mut output, &mut text, &block_id, &timeline);
    for v in &mut output {
        if let JsValue::Object(o) = v {
            for field_name in ["queueItemId", "moduleId"] {
                let val = field(field(message, "metadata"), field_name);
                if truthy_string(val) {
                    o.insert(field_name.into(), val.clone());
                }
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    fn value(raw: &str) -> JsValue {
        parse_json(raw).unwrap()
    }
    #[test]
    fn canonical_text_and_tool_preserve_order_and_utf16() {
        let message = value(
            r#"{"role":"assistant","metadata":{"model":"model-a","queueItemId":"q"},"content":[{"type":"text","text":"a\ud800"},{"type":"text","text":"b"},{"type":"thinking","text":"think"},{"type":"tool_call","id":"call","name":"bash","input":{"command":"true"}}]}"#,
        );
        let key: JsString = "s".into();
        let stamp: JsValue = "2026-01-01T00:00:00.000Z".into();
        let ctx = Context {
            session_key: &key,
            project_key: None,
            created_at: &stamp,
            entry_id: None,
            turn_id: None,
            sequence: None,
            message_index: 0,
            fork_unsupported: false,
        };
        let result = flatten_message(&message, &ctx).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].get("text"), Some(&value(r#""a\ud800b""#)));
        assert_eq!(result[1].get("kind"), Some(&JsValue::from("thinking")));
        assert_eq!(result[2].get("kind"), Some(&JsValue::from("tool_use")));
        assert_eq!(result[2].get("toolName"), Some(&JsValue::from("bash")));
        assert!(
            result
                .iter()
                .all(|x| x.get("queueItemId") == Some(&JsValue::from("q")))
        );
    }
    #[test]
    fn unsupported_blocks_fail_instead_of_silent_loss() {
        let key: JsString = "s".into();
        let stamp: JsValue = "now".into();
        let ctx = Context {
            session_key: &key,
            project_key: None,
            created_at: &stamp,
            entry_id: None,
            turn_id: None,
            sequence: None,
            message_index: 0,
            fork_unsupported: false,
        };
        let value = value(
            r#"{"role":"user","content":[{"type":"text","text":"before"},{"type":"unknown"}]}"#,
        );
        assert!(flatten_message(&value, &ctx).is_err());
    }
}
