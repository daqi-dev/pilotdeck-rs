//! Deterministic candidate assembly used only by the injected budget evaluator.
//! TS: modelRequestAssembly, model/protocol/clone, materializeMediaReferences,
//! context/cache/CachePlan. No routing or context-runtime implementation lives here.
use super::{get, is, object};
use crate::host::{HostError, JsObject, JsValue, ModuleError};
use pilotdeck_protocol::JsString;

fn error(message: impl Into<JsString>) -> HostError {
    ModuleError::new(message).into()
}
fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn property<'a>(value: &'a JsValue, key: &str) -> Result<Option<&'a JsValue>, HostError> {
    if matches!(value, JsValue::Undefined | JsValue::Null) {
        let name = if value.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(error(format!(
            "Cannot read properties of {name} (reading '{key}')"
        )));
    }
    Ok(value.get(key))
}
fn value(value: &JsValue, key: &str) -> Result<JsValue, HostError> {
    Ok(property(value, key)?.cloned().unwrap_or(JsValue::Undefined))
}
fn spread(value: &JsValue) -> JsObject {
    match value {
        JsValue::Object(o) => o.clone(),
        JsValue::Array(a) => {
            let mut o = JsObject::new();
            for (i, v) in a.iter().enumerate() {
                o.insert(i.to_string().into(), v.clone());
            }
            o
        }
        JsValue::String(s) => {
            let mut o = JsObject::new();
            for (i, u) in s.units().iter().enumerate() {
                o.insert(
                    i.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*u])),
                );
            }
            o
        }
        _ => JsObject::new(),
    }
}
fn content(message: &JsValue) -> Result<&[JsValue], HostError> {
    Ok(match property(message, "content")? {
        Some(JsValue::Array(a)) => a,
        _ => &[],
    })
}
fn tool_call(message: &JsValue) -> Result<bool, HostError> {
    for block in content(message)? {
        if is(&value(block, "type")?, "tool_call") {
            return Ok(true);
        }
    }
    Ok(false)
}
pub(super) fn normalize(messages: &[JsValue]) -> Result<Vec<JsValue>, HostError> {
    let mut output: Vec<JsValue> = Vec::new();
    for raw in messages {
        let mut message = spread(raw);
        message.insert("content".into(), JsValue::Array(content(raw)?.to_vec()));
        let message = JsValue::Object(message);
        if let Some(last) = output.last()
            && is(&value(last, "role")?, "assistant")
            && is(&value(&message, "role")?, "assistant")
            && !tool_call(last)?
            && !tool_call(&message)?
        {
            let mut blocks = content(last)?.to_vec();
            blocks.extend_from_slice(content(&message)?);
            let first = value(last, "metadata")?;
            let second = value(&message, "metadata")?;
            let metadata = if !truthy(&first) && !truthy(&second) {
                JsValue::Undefined
            } else {
                let mut metadata = spread(&first);
                for (key, v) in spread(&second).entries() {
                    metadata.insert(key.clone(), v.clone());
                }
                JsValue::Object(metadata)
            };
            *output.last_mut().unwrap() = JsValue::Object(object([
                ("role", "assistant".into()),
                ("content", JsValue::Array(blocks)),
                ("metadata", metadata),
            ]));
        } else if !(is(&value(&message, "role")?, "assistant") && content(&message)?.is_empty()) {
            output.push(message);
        }
    }
    Ok(output)
}
fn clone_messages(messages: &JsValue) -> Result<Vec<JsValue>, HostError> {
    let JsValue::Array(messages) = messages else {
        property(messages, "map")?;
        return Err(error("messages.map is not a function"));
    };
    messages
        .iter()
        .map(|message| {
            let mut copy = spread(message);
            let blocks = content(message)?
                .iter()
                .map(|block| {
                    let kind = value(block, "type")?;
                    let mut copy = spread(block);
                    if is(&kind, "tool_result") {
                        let inner = value(block, "content")?;
                        property(&inner, "map")?;
                        let JsValue::Array(ref inner) = inner else {
                            return Err(error("tr.content.map is not a function"));
                        };
                        copy.insert(
                            "content".into(),
                            JsValue::Array(
                                inner.iter().map(|v| JsValue::Object(spread(v))).collect(),
                            ),
                        );
                    } else if is(&kind, "tool_call") {
                        copy.insert("input".into(), value(block, "input")?);
                    }
                    Ok(JsValue::Object(copy))
                })
                .collect::<Result<Vec<_>, HostError>>()?;
            copy.insert("content".into(), JsValue::Array(blocks));
            Ok(JsValue::Object(copy))
        })
        .collect()
}
async fn materialize(messages: JsValue) -> Result<Vec<JsValue>, HostError> {
    let mut messages = clone_messages(&messages)?;
    // File reads run off the executor thread. Like TS, errors become preview
    // text; nested tool-result media references are deliberately left intact.
    for message in &mut messages {
        let mut record = spread(message);
        let mut blocks = Vec::new();
        for block in content(message)? {
            if !is(&value(block, "type")?, "media_reference") {
                blocks.push(block.clone());
                continue;
            }
            let path = value(block, "path")?
                .as_string()
                .map(|s| String::from_utf16_lossy(s.units()));
            let data = if let Some(path) = path {
                tokio::task::spawn_blocking(move || std::fs::read(path))
                    .await
                    .ok()
                    .and_then(Result::ok)
            } else {
                None
            };
            let media = value(block, "mediaType")?;
            let mime = value(block, "mimeType")?;
            if let Some(data) = data.filter(|_| {
                is(&media, "image")
                    || is(&media, "audio")
                    || (is(&media, "pdf") && is(&mime, "application/pdf"))
            }) {
                let mut result = object([
                    ("type", media.clone()),
                    ("source", "base64".into()),
                    (
                        "data",
                        JsValue::String(String::from_utf8_lossy(&data).into_owned().into()),
                    ),
                    ("mimeType", mime),
                    ("bytes", value(block, "originalBytes")?),
                ]);
                let detail = value(block, "detail")?;
                let pages = value(block, "pages")?;
                if is(&media, "image") && truthy(&detail) {
                    result.insert("detail".into(), detail);
                }
                if is(&media, "pdf") && !pages.is_undefined() {
                    result.insert("pages".into(), pages);
                }
                blocks.push(JsValue::Object(result));
            } else {
                blocks.push(JsValue::Object(object([
                    ("type", "text".into()),
                    ("text", value(block, "preview")?),
                ])));
            }
        }
        record.insert("content".into(), JsValue::Array(blocks));
        *message = JsValue::Object(record);
    }
    Ok(messages)
}
fn string_items(s: &JsString) -> Vec<JsValue> {
    let mut values = Vec::new();
    let mut units = s.units().iter().copied().peekable();
    while let Some(first) = units.next() {
        let mut item = vec![first];
        if (0xd800..=0xdbff).contains(&first)
            && units.peek().is_some_and(|v| (0xdc00..=0xdfff).contains(v))
        {
            item.push(units.next().unwrap());
        }
        values.push(JsValue::String(JsString::from_utf16(item)));
    }
    values
}
fn cache_plan(
    request: &JsObject,
    prepared: &JsValue,
    messages: &[JsValue],
) -> Result<JsValue, HostError> {
    let tools = value(prepared, "tools")?;
    let tools = match &tools {
        JsValue::Array(a) => a.clone(),
        JsValue::String(s) => string_items(s),
        _ => return Err(error("input.tools is not iterable")),
    };
    let system = value(prepared, "systemPrompt")?;
    // One fingerprint port for both TS call sites of `CachePlan.buildCachePlan`
    // (this host-side finalization and the runtime's coordinator). The host
    // entry keeps its pre-merge depth behavior: no gate on the stable
    // serialization's explicit stack (the residual TS divergence at
    // ~2100-level objects is registered in A6-JSVALUE-DEPTH-001).
    let identity = crate::runtime::context::cache::cache_plan_identity(
        &crate::project_files::node_locale(|key| std::env::var(key).ok()),
        &get(request, "provider"),
        &get(request, "model"),
        &system,
        &tools,
        messages,
        None,
    )?;
    Ok(JsValue::Object(object([
        ("provider", get(request, "provider")),
        ("model", get(request, "model")),
        ("system", JsValue::Bool(truthy(&system))),
        ("tools", JsValue::Bool(false)),
        (
            "messages",
            JsValue::Array(
                identity
                    .breakpoints
                    .iter()
                    .map(|i| JsValue::Number(*i as f64))
                    .collect(),
            ),
        ),
        ("fingerprint", JsValue::String(identity.fingerprint.into())),
        (
            "generation",
            value(&value(prepared, "cachePlan")?, "generation")?,
        ),
    ])))
}
pub(super) async fn finalize(
    request: &JsObject,
    prepared: JsValue,
    plan: bool,
) -> Result<JsObject, HostError> {
    let mut messages = materialize(value(&prepared, "messages")?).await?;
    if plan {
        messages.push(JsValue::Object(object([("role", "user".into()), ("content", JsValue::Array(vec![JsValue::Object(object([
            ("type", "text".into()), ("text", "Plan mode is active.\nRead first using read-only tools, then write or refine plan markdown only under `.pilotdeck/plans/`.\nDo not make implementation changes while planning.\nWhen the plan is ready for user review, call `exit_plan_mode` with the plan file path.".into())]))])),
            ("metadata", JsValue::Object(object([("synthetic", JsValue::Bool(true)), ("purpose", "plan_mode_reminder".into())])))])));
    }
    let cache = if truthy(&value(&prepared, "cachePlan")?) {
        cache_plan(request, &prepared, &messages)?
    } else {
        JsValue::Undefined
    };
    let mut result = request.clone();
    result.insert("messages".into(), JsValue::Array(messages));
    let system = value(&prepared, "systemPrompt")?;
    result.insert(
        "systemPrompt".into(),
        if matches!(system, JsValue::Undefined | JsValue::Null) {
            get(request, "systemPrompt")
        } else {
            system
        },
    );
    result.insert("tools".into(), value(&prepared, "tools")?);
    result.insert(
        "cacheBreakpoints".into(),
        if !cache.is_undefined() {
            value(&cache, "messages")?
        } else if plan {
            JsValue::Undefined
        } else {
            value(&prepared, "cacheBreakpoints")?
        },
    );
    result.insert("cachePlan".into(), cache);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested(kind: &str, depth: usize) -> JsValue {
        let mut value = JsValue::Number(0.0);
        for _ in 0..depth {
            value = if kind == "obj" {
                JsValue::Object(object([("x", value)]))
            } else {
                JsValue::Array(vec![value])
            };
        }
        value
    }

    /// This host entry serializes the cache identity without a depth gate —
    /// its pre-merge behavior — so deeply nested schemas reach a fingerprint.
    /// The 2398-level nested array also matches real TS (Node v22.22.0 probe,
    /// run twice byte-identical, both real entries succeed there); the
    /// 2100-level nested object succeeds here while TS overflows V8's stack,
    /// a registered divergence (A6-JSVALUE-DEPTH-001, unapproved — the
    /// runtime coordinator instead keeps the 2000-container gate, see
    /// `runtime::context::cache::tests::depth_boundaries_match_pre_pr_rust_behavior`).
    #[test]
    fn deep_input_schema_fingerprints_reach_the_host_entry() {
        for (label, schema, expected) in [
            (
                "2398-level array",
                nested("arr", 2398),
                "b84cd74c717a1f7f357ac36edc241aa758129e0f5a197411305e54e857e87467",
            ),
            (
                "2100-level object",
                nested("obj", 2100),
                "7158368fe2729d2fd5293783b0c178a6707bfa158338809c4e5c6f553bdffbd8",
            ),
        ] {
            let request = object([("provider", "p".into()), ("model", "m".into())]);
            let prepared = JsValue::Object(object([
                (
                    "tools",
                    JsValue::Array(vec![JsValue::Object(object([
                        ("name", "t".into()),
                        ("inputSchema", schema),
                    ]))]),
                ),
                ("systemPrompt", "preview prompt".into()),
                (
                    "cachePlan",
                    JsValue::Object(object([("generation", JsValue::Number(0.0))])),
                ),
            ]));
            let messages = vec![JsValue::Object(object([
                ("role", "user".into()),
                (
                    "content",
                    JsValue::Array(vec![JsValue::Object(object([
                        ("type", "text".into()),
                        ("text", "hello".into()),
                    ]))]),
                ),
            ]))];
            let cache = cache_plan(&request, &prepared, &messages)
                .unwrap_or_else(|e| panic!("{label}: {}", e.message));
            let fingerprint = value(&cache, "fingerprint").unwrap();
            assert_eq!(
                fingerprint.as_string().unwrap().to_utf8().unwrap(),
                expected,
                "{label}"
            );
        }
    }
}
