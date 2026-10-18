//! Deterministic candidate assembly used only by the injected budget evaluator.
//! TS: modelRequestAssembly, model/protocol/clone, materializeMediaReferences,
//! context/cache/CachePlan. No routing or context-runtime implementation lives here.
use super::{get, is, object};
use crate::host::{HostError, JsObject, JsValue, ModuleError};
use pilotdeck_protocol::{JsString, stringify_json};
use sha2::{Digest, Sha256};

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
fn stable(value: &JsValue) -> Result<Option<String>, HostError> {
    enum Part<'a> {
        Value(&'a JsValue),
        Text(String),
    }
    if value.is_undefined() {
        return Ok(None);
    }
    let mut output = String::new();
    let mut stack = vec![Part::Value(value)];
    while let Some(part) = stack.pop() {
        match part {
            Part::Text(s) => output.push_str(&s),
            Part::Value(JsValue::Array(a)) => {
                output.push('[');
                stack.push(Part::Text("]".into()));
                for (i, v) in a.iter().enumerate().rev() {
                    if !v.is_undefined() {
                        stack.push(Part::Value(v));
                    }
                    if i > 0 {
                        stack.push(Part::Text(",".into()));
                    }
                }
            }
            Part::Value(JsValue::Object(o)) => {
                output.push('{');
                stack.push(Part::Text("}".into()));
                let mut fields = o.entries().iter().collect::<Vec<_>>();
                fields.sort_by(|a, b| a.0.units().cmp(b.0.units()));
                for (i, (k, v)) in fields.into_iter().enumerate().rev() {
                    if v.is_undefined() {
                        stack.push(Part::Text("undefined".into()));
                    } else {
                        stack.push(Part::Value(v));
                    }
                    let key = stringify_json(&JsValue::String(k.clone()))
                        .map_err(|e| error(e.to_string()))?
                        .unwrap();
                    stack.push(Part::Text(format!("{key}:")));
                    if i > 0 {
                        stack.push(Part::Text(",".into()));
                    }
                }
            }
            Part::Value(v) => output.push_str(
                &stringify_json(v)
                    .map_err(|e| error(e.to_string()))?
                    .unwrap(),
            ),
        }
    }
    Ok(Some(output))
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
    let selected = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            !is(
                &m.get("role").cloned().unwrap_or(JsValue::Undefined),
                "system",
            )
        })
        .map(|(i, _)| i)
        .collect::<Vec<_>>();
    let selected = &selected[selected.len().saturating_sub(3)..];
    let tools = value(prepared, "tools")?;
    let mut tools = match &tools {
        JsValue::Array(a) => a.clone(),
        JsValue::String(s) => string_items(s),
        _ => return Err(error("input.tools is not iterable")),
    };
    if tools.len() > 1 {
        let locale = crate::project_files::node_locale(|key| std::env::var(key).ok());
        let collator = crate::project_files::node_collator(&locale).map_err(|(_, m)| error(m))?;
        let compare = |left: &JsValue, right: &JsValue| -> Result<std::cmp::Ordering, HostError> {
            let name = value(left, "name")?;
            let left_name = name.as_string().ok_or_else(|| {
                error(if name.is_undefined() {
                    "Cannot read properties of undefined (reading 'localeCompare')"
                } else if matches!(name, JsValue::Null) {
                    "Cannot read properties of null (reading 'localeCompare')"
                } else {
                    "left.name.localeCompare is not a function"
                })
            })?;
            let right_name = crate::js_value::to_string_units(&value(right, "name")?)
                .map_err(|_| error(crate::js_value::StackOverflow::MESSAGE))?;
            let order = collator.compare_utf16(left_name.units(), &right_name);
            if order != std::cmp::Ordering::Equal {
                return Ok(order);
            }
            let left = JsString::from(stable(left)?.unwrap_or_default());
            let right = JsString::from(stable(right)?.unwrap_or_default());
            Ok(collator.compare_utf16(left.units(), right.units()))
        };
        let mut failure = None;
        tools.sort_by(|a, b| {
            if failure.is_some() {
                return std::cmp::Ordering::Equal;
            }
            match compare(a, b) {
                Ok(order) => order,
                Err(e) => {
                    failure = Some(e);
                    std::cmp::Ordering::Equal
                }
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
    }

    let system = value(prepared, "systemPrompt")?;
    let nullish = |v: JsValue| {
        if matches!(v, JsValue::Null | JsValue::Undefined) {
            "".into()
        } else {
            v
        }
    };
    let fingerprint = JsValue::Object(object([
        ("provider", nullish(get(request, "provider"))),
        ("model", nullish(get(request, "model"))),
        ("system", nullish(system.clone())),
        ("tools", JsValue::Array(tools)),
        (
            "messages",
            JsValue::Array(selected.iter().map(|i| messages[*i].clone()).collect()),
        ),
    ]));
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(stable(&fingerprint)?.unwrap().as_bytes())
    );
    Ok(JsValue::Object(object([
        ("provider", get(request, "provider")),
        ("model", get(request, "model")),
        ("system", JsValue::Bool(truthy(&system))),
        ("tools", JsValue::Bool(false)),
        (
            "messages",
            JsValue::Array(
                selected
                    .iter()
                    .map(|i| JsValue::Number(*i as f64))
                    .collect(),
            ),
        ),
        ("fingerprint", JsValue::String(fingerprint.into())),
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
