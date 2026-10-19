//! Original repeated annotations and invalid-input circuit, with no effects.
use crate::ports::{PortError, PortResult};
use crate::tool_failures::{InvalidToolAction, InvalidToolPlan, ToolFailureRecovery};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::collections::HashSet;
fn undefined() -> &'static JsValue {
    static V: JsValue = JsValue::Undefined;
    &V
}
fn property<'a>(v: &'a JsValue, k: &str) -> PortResult<&'a JsValue> {
    if matches!(v, JsValue::Null | JsValue::Undefined) {
        return Err(exception(
            v,
            &format!(
                "Cannot read properties of {} (reading '{k}')",
                if v.is_undefined() {
                    "undefined"
                } else {
                    "null"
                }
            ),
        ));
    }
    Ok(v.get(k).unwrap_or(undefined()))
}
fn exception(v: &JsValue, message: &str) -> PortError {
    PortError::SourceException {
        message: message.into(),
        boundary: BoundarySnapshot::capture(v),
    }
}
fn invalid(message: &str) -> PortError {
    PortError::InvalidResponse {
        message: message.into(),
    }
}
fn is(v: &JsValue, s: &str) -> bool {
    v.as_string().is_some_and(|v| v.equals_str(s))
}
fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn nullish(v: &JsValue) -> bool {
    matches!(v, JsValue::Null | JsValue::Undefined)
}
/// ECMAScript template conversion on ordinary JSON/WTF16 values. Native
/// functions/Symbols/alias graphs are outside these captured boundary values.
fn string(v: &JsValue) -> PortResult<JsString> {
    enum Part<'a> {
        Value(&'a JsValue, bool),
        Comma,
    }
    let mut todo = vec![Part::Value(v, false)];
    let mut out = Vec::new();
    while let Some(part) = todo.pop() {
        match part {
            Part::Comma => out.push(44),
            Part::Value(v, joined) => match v {
                JsValue::Undefined => {
                    if !joined {
                        out.extend("undefined".encode_utf16());
                    }
                }
                JsValue::Null => {
                    if !joined {
                        out.extend("null".encode_utf16());
                    }
                }
                JsValue::Bool(v) => out.extend(if *v { "true" } else { "false" }.encode_utf16()),
                JsValue::String(s) => out.extend_from_slice(s.units()),
                JsValue::Number(n) => {
                    let s = if n.is_nan() {
                        "NaN".to_owned()
                    } else if *n == f64::INFINITY {
                        "Infinity".to_owned()
                    } else if *n == f64::NEG_INFINITY {
                        "-Infinity".to_owned()
                    } else {
                        stringify_json(v)
                            .map_err(|e| exception(v, &e.message))?
                            .ok_or_else(|| invalid("Number template conversion missing"))?
                    };
                    out.extend(s.encode_utf16());
                }
                JsValue::Array(items) => {
                    for i in (0..items.len()).rev() {
                        todo.push(Part::Value(&items[i], true));
                        if i > 0 {
                            todo.push(Part::Comma);
                        }
                    }
                }
                JsValue::Object(o) => {
                    if o.get("toString").is_some() {
                        return Err(exception(v, "Cannot convert object to primitive value"));
                    }
                    out.extend("[object Object]".encode_utf16());
                }
            },
        }
    }
    Ok(JsString::from_utf16(out))
}
fn joined(parts: &[JsString], separator: &str) -> JsString {
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.extend(separator.encode_utf16());
        }
        out.extend_from_slice(p.units());
    }
    JsString::from_utf16(out)
}
fn recovery(v: &JsValue) -> PortResult<Option<&JsObject>> {
    let metadata = property(v, "metadata")?;
    let r = if nullish(metadata) {
        undefined()
    } else {
        property(metadata, "recovery")?
    };
    Ok(r.as_object())
}
fn failure_key(v: &JsValue, r: Option<&JsObject>) -> PortResult<JsString> {
    // Template substitutions evaluate in order, including ToPrimitive errors.
    let name = string(property(v, "toolName")?)?;
    let code = string(property(property(v, "error")?, "code")?)?;
    let class = r
        .and_then(|r| r.get("failureClass"))
        .filter(|v| !nullish(v));
    let class = match class {
        Some(v) => string(v)?,
        None => "unknown".into(),
    };
    Ok(joined(&[name, code, class], "::"))
}
fn error_entries(results: &[BoundarySnapshot]) -> PortResult<Vec<&JsValue>> {
    let mut entries = Vec::new();
    for r in results {
        if is(property(r.value(), "type")?, "error") {
            entries.push(r.value());
        }
    }
    Ok(entries)
}
fn spread(v: &JsValue) -> JsObject {
    match v {
        JsValue::Object(o) => {
            let mut indices = Vec::new();
            let mut others = Vec::new();
            for entry in o.entries() {
                match entry.0.array_index() {
                    Some(index) => indices.push((index, entry)),
                    None => others.push(entry),
                }
            }
            indices.sort_unstable_by_key(|(index, _)| *index);
            let mut output = JsObject::new();
            for (_, (k, v)) in indices {
                output.insert(k.clone(), v.clone());
            }
            for (k, v) in others {
                output.insert(k.clone(), v.clone());
            }
            output
        }
        JsValue::Array(items) => {
            let mut o = JsObject::new();
            for (i, v) in items.iter().enumerate() {
                o.insert(i.to_string().as_str().into(), v.clone());
            }
            o
        }
        JsValue::String(s) => {
            let mut o = JsObject::new();
            for (i, u) in s.units().iter().enumerate() {
                o.insert(
                    i.to_string().as_str().into(),
                    JsValue::String(JsString::from_utf16(vec![*u])),
                );
            }
            o
        }
        _ => JsObject::new(),
    }
}
fn text_block(text: JsString) -> JsValue {
    let mut o = JsObject::new();
    o.insert("type".into(), "text".into());
    o.insert("text".into(), JsValue::String(text));
    JsValue::Object(o)
}
fn content_items(v: &JsValue) -> PortResult<Vec<JsValue>> {
    match v {
        JsValue::Array(items) => Ok(items.clone()),
        JsValue::String(s) => {
            let mut items = Vec::new();
            let mut i = 0;
            while i < s.units().len() {
                let u = s.units()[i];
                let pair = (0xd800..=0xdbff).contains(&u)
                    && s.units()
                        .get(i + 1)
                        .is_some_and(|v| (0xdc00..=0xdfff).contains(v));
                let end = i + if pair { 2 } else { 1 };
                items.push(JsValue::String(JsString::from_utf16(
                    s.units()[i..end].to_vec(),
                )));
                i = end;
            }
            Ok(items)
        }
        _ => Err(exception(v, "content is not iterable")),
    }
}
fn append_content(content: &JsValue, suffix: &JsString) -> PortResult<JsValue> {
    let mut items = content_items(content)?;
    let first = items.first().unwrap_or(undefined());
    // Suffix is constructed below with exactly two leading LF; Source trimStart
    // removes those before the fixed non-whitespace "Repeated" prefix.
    let trimmed = JsString::from_utf16(suffix.units()[2..].to_vec());
    if !truthy(first) {
        return Ok(JsValue::Array(vec![text_block(trimmed)]));
    }
    if !is(property(first, "type")?, "text") {
        items.insert(0, text_block(trimmed));
        return Ok(JsValue::Array(items));
    }
    let before = string(property(first, "text")?)?;
    let text = joined(&[before, suffix.clone()], "");
    let mut first = spread(first);
    first.insert("text".into(), JsValue::String(text));
    items[0] = JsValue::Object(first);
    Ok(JsValue::Array(items))
}
pub(crate) fn annotate_results(
    state: &mut ToolFailureRecovery,
    results: &[BoundarySnapshot],
) -> PortResult<Vec<BoundarySnapshot>> {
    let entries = error_entries(results)?;
    let mut keys = Vec::with_capacity(entries.len());
    for entry in entries {
        keys.push(failure_key(entry, recovery(entry)?)?);
    }
    keys.sort_by(|a, b| a.units().cmp(b.units()));
    let fingerprint = if keys.is_empty() {
        None
    } else {
        Some(joined(&keys, "\n"))
    };
    let mut seen = HashSet::new();
    let mut repeated = HashSet::new();
    for k in &keys {
        if !seen.insert(k.clone()) {
            repeated.insert(k.clone());
        }
    }
    if fingerprint
        .as_ref()
        .is_some_and(|f| Some(f) == state.last_tool_failure_fingerprint.as_ref())
    {
        repeated.extend(keys);
    }
    let mut output = Vec::with_capacity(results.len());
    for result in results {
        let v = result.value();
        if repeated.is_empty() || !is(property(v, "type")?, "error") {
            output.push(result.clone());
            continue;
        }
        let r = recovery(v)?;
        if !repeated.contains(&failure_key(v, r)?) {
            output.push(result.clone());
            continue;
        }
        let reason=r.and_then(|r|r.get("avoidRetryReason")).and_then(JsValue::as_string).cloned().unwrap_or_else(||"The same tool, error code, and recovery class repeated. Retrying unchanged is likely to fail again.".into());
        let suffix=joined(&["\n\nRepeated failure: ".into(),reason.clone(),"\nChange at least one of the tool, parameters, path, scope, permission path, or explain the blocker in text.".into()],"");
        let content = append_content(property(v, "content")?, &suffix)?;
        let mut annotation = spread(v);
        annotation.insert("content".into(), content);
        let mut metadata = spread(property(v, "metadata")?);
        let mut rec = r
            .map(|r| spread(&JsValue::Object(r.clone())))
            .unwrap_or_default();
        rec.insert("avoidRetryReason".into(), JsValue::String(reason));
        rec.insert("repeatedFailure".into(), JsValue::Bool(true));
        metadata.insert("recovery".into(), JsValue::Object(rec));
        annotation.insert("metadata".into(), JsValue::Object(metadata));
        output.push(BoundarySnapshot::capture(&JsValue::Object(annotation)));
    }
    // Source detect result does not commit until every annotation has completed.
    state.last_tool_failure_fingerprint = fingerprint;
    Ok(output)
}
pub(crate) fn all_invalid(results: &[BoundarySnapshot]) -> PortResult<bool> {
    if results.is_empty() {
        return Ok(false);
    }
    for r in results {
        let v = r.value();
        if !is(property(v, "type")?, "error")
            || !is(
                property(property(v, "error")?, "code")?,
                "invalid_tool_input",
            )
        {
            return Ok(false);
        }
    }
    Ok(true)
}
fn invalid_fingerprint(results: &[BoundarySnapshot]) -> PortResult<JsString> {
    let mut selected = Vec::new();
    for r in results {
        let v = r.value();
        if is(property(v, "type")?, "error")
            && is(
                property(property(v, "error")?, "code")?,
                "invalid_tool_input",
            )
        {
            selected.push(v);
        }
    }
    let mut keys = Vec::new();
    for v in selected {
        let name = string(property(v, "toolName")?)?;
        let message = string(property(property(v, "error")?, "message")?)?;
        keys.push(joined(&[name, message], "::"));
    }
    keys.sort_by(|a, b| a.units().cmp(b.units()));
    Ok(joined(&keys, "\n"))
}
pub(crate) fn select_invalid(
    state: &mut ToolFailureRecovery,
    results: &[BoundarySnapshot],
) -> PortResult<InvalidToolPlan> {
    if all_invalid(results)? {
        let f = invalid_fingerprint(results)?;
        if state.last_invalid_fingerprint.as_ref() == Some(&f) {
            state.same_invalid_count += 1;
        } else {
            state.same_invalid_count = 1;
            state.last_invalid_fingerprint = Some(f);
            state.used_invalid_grace = false;
        }
        if state.same_invalid_count >= 3 {
            if !state.used_invalid_grace {
                state.used_invalid_grace = true;
                return Ok(InvalidToolPlan{action:InvalidToolAction::Continue{prompt:"Your last several tool calls all failed input validation with the same error. This may indicate a tool-side issue rather than a problem with your approach. Options: (1) try a different tool or different parameters, (2) explain the situation in text without calling tools, (3) if you believe the tool should work, try once more with corrected input.".into(),purpose:"circuit_breaker_grace".into()}});
            }
            return Ok(InvalidToolPlan {
                action: InvalidToolAction::Stop {
                    repeated_failures: state.same_invalid_count,
                },
            });
        }
        return Ok(InvalidToolPlan {
            action: InvalidToolAction::FallThrough {
                reset_output_recovery: false,
            },
        });
    }
    state.same_invalid_count = 0;
    state.last_invalid_fingerprint = None;
    state.used_invalid_grace = false;
    let mut any_error = false;
    for r in results {
        if is(property(r.value(), "type")?, "error") {
            any_error = true;
            break;
        }
    }
    if !any_error {
        state.last_tool_failure_fingerprint = None;
    }
    Ok(InvalidToolPlan {
        action: InvalidToolAction::FallThrough {
            reset_output_recovery: true,
        },
    })
}
pub(crate) fn terminal_error(plan: &InvalidToolPlan) -> PortResult<BoundarySnapshot> {
    let InvalidToolAction::Stop { repeated_failures } = plan.action else {
        return Err(invalid("Invalid-tool terminal error requires Stop."));
    };
    let mut e = JsObject::new();
    e.insert("code".into(), "agent_tool_error_loop".into());
    e.insert("message".into(),format!("Terminated: {repeated_failures} consecutive turns with identical tool input validation failures (same tool + same error). The model appears stuck in a loop.").as_str().into());
    e.insert("details".into(), JsValue::Undefined);
    e.insert("userHint".into(),"The model is repeatedly producing invalid tool calls. Consider switching to a more capable model via settings.".into());
    Ok(BoundarySnapshot::capture(&JsValue::Object(e)))
}
pub(crate) fn terminal_status(
    plan: &InvalidToolPlan,
    error: &BoundarySnapshot,
) -> PortResult<BoundarySnapshot> {
    let InvalidToolAction::Stop { repeated_failures } = plan.action else {
        return Err(invalid("Invalid-tool terminal status requires Stop."));
    };
    let message = property(error.value(), "message")?;
    let text = if truthy(message) {
        message.clone()
    } else {
        "The agent repeatedly hit the same tool error, so this turn has stopped.".into()
    };
    let code = property(error.value(), "code")?;
    let hint = property(error.value(), "userHint")?;
    let hint = if nullish(hint) {
        "Change the request to avoid repeating the same failing tool call, grant any required permission, or switch to a model with stronger tool-calling support in Settings → Model Provider.".into()
    } else {
        hint.clone()
    };
    let mut detail = JsObject::new();
    detail.insert("message".into(), text.clone());
    if !code.is_undefined() {
        detail.insert("code".into(), code.clone());
    }
    detail.insert("severity".into(), "error".into());
    detail.insert("visible".into(), JsValue::Bool(true));
    detail.insert("userHint".into(), hint);
    detail.insert("scope".into(), "turn".into());
    detail.insert("source".into(), "agent".into());
    detail.insert(
        "repeatedFailures".into(),
        JsValue::Number(f64::from(repeated_failures)),
    );
    let mut o = JsObject::new();
    o.insert("event".into(), "tool_error_loop".into());
    o.insert("kind".into(), "error".into());
    o.insert("text".into(), text);
    o.insert("detail".into(), JsValue::Object(detail));
    Ok(BoundarySnapshot::capture(&JsValue::Object(o)))
}
