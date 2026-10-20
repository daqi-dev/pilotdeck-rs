//! Fixed original assembleModelMessage.ts, open immutable JSON boundary profile.
//! This state never grants tool/permission authority. Native callable/prototype/
//! cyclic graph behavior is outside JsValue, rather than guessed from JSON data.
use crate::source_model::{
    SourceAssemblerState, SourceAssemblyError, SourceField, SourceStateKey, SourceUsage,
};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashSet;

type Result<T> = std::result::Result<T, SourceAssemblyError>;
fn present(value: &JsValue) -> SourceField {
    SourceField::Present(BoundarySnapshot::capture(value))
}
fn value(field: &SourceField) -> JsValue {
    match field {
        SourceField::Missing => JsValue::Undefined,
        SourceField::Present(v) => v.value().clone(),
    }
}
fn touch(state: &mut SourceAssemblerState, key: SourceStateKey) {
    if !state.property_order.contains(&key) {
        state.property_order.push(key);
    }
}
fn error(message: impl Into<JsString>) -> SourceAssemblyError {
    SourceAssemblyError::TypeError {
        message: message.into(),
    }
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
    matches!(v, JsValue::Undefined | JsValue::Null)
}
fn get(v: &JsValue, key: &str) -> Result<JsValue> {
    match v {
        JsValue::Null => Err(error(format!(
            "Cannot read properties of null (reading '{key}')"
        ))),
        JsValue::Undefined => Err(error(format!(
            "Cannot read properties of undefined (reading '{key}')"
        ))),
        JsValue::Object(o) => Ok(o.get(key).cloned().unwrap_or(JsValue::Undefined)),
        JsValue::Array(a) if key == "length" => Ok(JsValue::Number(a.len() as f64)),
        JsValue::String(s) if key == "length" => Ok(JsValue::Number(s.len_utf16() as f64)),
        _ => Ok(JsValue::Undefined),
    }
}
fn append(to: &mut JsString, from: &JsString) {
    let mut units = to.units().to_vec();
    units.extend_from_slice(from.units());
    *to = JsString::from_utf16(units);
}
fn primitive(v: &JsValue) -> Result<JsValue> {
    match v {
        JsValue::Object(o) if o.contains_key("toString") => {
            Err(error("Cannot convert object to primitive value"))
        }
        JsValue::Object(_) => Ok(JsValue::from("[object Object]")),
        JsValue::Array(_) => Ok(JsValue::String(string(v)?)),
        _ => Ok(v.clone()),
    }
}
/// Iterative Array.prototype.join/default ToPrimitive; UTF16 is never lossy.
fn string(v: &JsValue) -> Result<JsString> {
    let mut out = Vec::new();
    let mut tasks = vec![Some((v, false))];
    while let Some(task) = tasks.pop() {
        let Some((v, array_item)) = task else {
            out.push(44);
            continue;
        };
        match v {
            JsValue::Undefined | JsValue::Null if array_item => {}
            JsValue::Undefined => out.extend("undefined".encode_utf16()),
            JsValue::Null => out.extend("null".encode_utf16()),
            JsValue::Bool(v) => out.extend(if *v { "true" } else { "false" }.encode_utf16()),
            JsValue::String(v) => out.extend_from_slice(v.units()),
            JsValue::Number(v) => {
                let text = if v.is_nan() {
                    "NaN".into()
                } else if *v == f64::INFINITY {
                    "Infinity".into()
                } else if *v == f64::NEG_INFINITY {
                    "-Infinity".into()
                } else {
                    // Protocol codec's finite f64 stringify contract always returns
                    // Some exact ryu-js text, including JS -0 -> "0".
                    pilotdeck_protocol::stringify_json(&JsValue::Number(*v))
                        .expect("finite number codec invariant")
                        .expect("number is never undefined")
                };
                out.extend(text.encode_utf16());
            }
            JsValue::Object(o) => {
                if o.contains_key("toString") {
                    return Err(error("Cannot convert object to primitive value"));
                }
                out.extend("[object Object]".encode_utf16());
            }
            JsValue::Array(items) => {
                for (i, v) in items.iter().enumerate().rev() {
                    tasks.push(Some((v, true)));
                    if i > 0 {
                        tasks.push(None);
                    }
                }
            }
        }
    }
    Ok(JsString::from_utf16(out))
}
fn whitespace(c: u16) -> bool {
    matches!(c,0x0009..=0x000d|0x0020|0x00a0|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
}
fn trim(v: &JsString) -> JsString {
    let u = v.units();
    let start = u.iter().position(|c| !whitespace(*c)).unwrap_or(u.len());
    let end = u
        .iter()
        .rposition(|c| !whitespace(*c))
        .map_or(start, |i| i + 1);
    JsString::from_utf16(u[start..end].to_vec())
}
fn radix_number(text: &str, radix: u32) -> f64 {
    let mut bits = 0usize;
    let mut head = 0u64;
    let mut guard = false;
    let mut sticky = false;
    let width = match radix {
        2 => 1,
        8 => 3,
        _ => 4,
    };
    for c in text.bytes() {
        let Some(d) = (c as char).to_digit(radix) else {
            return f64::NAN;
        };
        for bit in (0..width).rev() {
            let one = d & (1 << bit) != 0;
            if bits == 0 && !one {
                continue;
            }
            bits += 1;
            if bits <= 53 {
                head = (head << 1) | u64::from(one);
            } else if bits == 54 {
                guard = one;
            } else {
                sticky |= one;
            }
        }
    }
    if bits == 0 {
        return 0.0;
    }
    if bits <= 53 {
        return head as f64;
    }
    if bits > 1024 {
        return f64::INFINITY;
    }
    if guard && (sticky || head & 1 != 0) {
        head += 1;
    }
    (head as f64) * 2.0f64.powi((bits - 53) as i32)
}
fn decimal_syntax(v: &[u8]) -> bool {
    let mut i = usize::from(matches!(v.first(), Some(b'+' | b'-')));
    let mut digits = 0;
    while v.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
        digits += 1;
    }
    if v.get(i) == Some(&b'.') {
        i += 1;
        while v.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return false;
    }
    if matches!(v.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(v.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        let start = i;
        while v.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == start {
            return false;
        }
    }
    i == v.len()
}
fn number(v: &JsValue) -> Result<f64> {
    let v = primitive(v)?;
    Ok(match &v {
        JsValue::Undefined => f64::NAN,
        JsValue::Null => 0.0,
        JsValue::Bool(v) => {
            if *v {
                1.0
            } else {
                0.0
            }
        }
        JsValue::Number(v) => *v,
        JsValue::String(s) => {
            let t = trim(s);
            let Ok(t) = t.to_utf8() else {
                return Ok(f64::NAN);
            };
            if t.is_empty() {
                0.0
            } else if t == "Infinity" || t == "+Infinity" {
                f64::INFINITY
            } else if t == "-Infinity" {
                f64::NEG_INFINITY
            } else if t.starts_with("0x") || t.starts_with("0X") {
                if t.len() == 2 {
                    f64::NAN
                } else {
                    radix_number(&t[2..], 16)
                }
            } else if t.starts_with("0b") || t.starts_with("0B") {
                if t.len() == 2 {
                    f64::NAN
                } else {
                    radix_number(&t[2..], 2)
                }
            } else if t.starts_with("0o") || t.starts_with("0O") {
                if t.len() == 2 {
                    f64::NAN
                } else {
                    radix_number(&t[2..], 8)
                }
            } else if decimal_syntax(t.as_bytes()) {
                t.parse::<f64>().unwrap_or(f64::NAN)
            } else {
                f64::NAN
            }
        }
        _ => unreachable!("primitive produces only primitives"),
    })
}
fn plus(first: &JsValue, second: &JsValue) -> Result<JsValue> {
    let a = primitive(first)?;
    let b = primitive(second)?;
    if matches!(a, JsValue::String(_)) || matches!(b, JsValue::String(_)) {
        let mut a = string(&a)?;
        append(&mut a, &string(&b)?);
        Ok(JsValue::String(a))
    } else {
        Ok(JsValue::Number(number(&a)? + number(&b)?))
    }
}
fn add(first: &SourceField, second: &JsValue) -> Result<SourceField> {
    let a = value(first);
    if a.is_undefined() && second.is_undefined() {
        return Ok(present(&JsValue::Undefined));
    }
    let zero = JsValue::Number(0.0);
    let a = if nullish(&a) { &zero } else { &a };
    let b = if nullish(second) { &zero } else { second };
    Ok(present(&plus(a, b)?))
}
fn index(key: &JsString) -> Option<u32> {
    // Canonical array-index key, shared with every other JS-order seam (#330).
    key.array_index()
}
fn spread(to: &mut JsObject, from: &JsValue) {
    match from {
        JsValue::Object(o) => {
            let mut entries = o.entries().iter().collect::<Vec<_>>();
            entries.sort_by_key(|(k, _)| index(k).map_or((1, 0), |n| (0, n)));
            for (k, v) in entries {
                to.insert(k.clone(), v.clone());
            }
        }
        JsValue::Array(v) => {
            for (i, v) in v.iter().enumerate() {
                to.insert(i.to_string().into(), v.clone());
            }
        }
        JsValue::String(v) => {
            for (i, u) in v.units().iter().enumerate() {
                to.insert(
                    i.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*u])),
                );
            }
        }
        _ => {}
    }
}
fn capture_object(o: JsObject) -> BoundarySnapshot {
    let mut ordered = JsObject::new();
    spread(&mut ordered, &JsValue::Object(o));
    BoundarySnapshot::capture(&JsValue::Object(ordered))
}
fn object(fields: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn captures(values: &[BoundarySnapshot]) -> JsValue {
    JsValue::Array(values.iter().map(|v| v.value().clone()).collect())
}
fn usage_object(u: &SourceUsage) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in [
        ("inputTokens", &u.input_tokens),
        ("outputTokens", &u.output_tokens),
        ("cacheReadTokens", &u.cache_read_tokens),
        ("cacheWriteTokens", &u.cache_write_tokens),
        ("totalTokens", &u.total_tokens),
        ("nativeCost", &u.native_cost),
    ] {
        if let SourceField::Present(v) = v {
            o.insert(k.into(), v.value().clone());
        }
    }
    JsValue::Object(o)
}

pub fn create(response_id: &JsValue) -> SourceAssemblerState {
    let mut order = Vec::new();
    let response_id = if truthy(response_id) {
        order.push(SourceStateKey::ResponseId);
        present(response_id)
    } else {
        SourceField::Missing
    };
    order.extend([
        SourceStateKey::Content,
        SourceStateKey::TextBuffer,
        SourceStateKey::ThinkingBuffer,
        SourceStateKey::ThinkingReasoningContentBuffer,
        SourceStateKey::Usage,
        SourceStateKey::HasMessageEnd,
        SourceStateKey::ToolCalls,
    ]);
    SourceAssemblerState {
        property_order: order,
        response_id,
        content: vec![],
        text_buffer: "".into(),
        model: SourceField::Missing,
        thinking_buffer: "".into(),
        thinking_reasoning_content_buffer: "".into(),
        thinking_signature: SourceField::Missing,
        usage: SourceUsage {
            input_tokens: SourceField::Missing,
            output_tokens: SourceField::Missing,
            cache_read_tokens: SourceField::Missing,
            cache_write_tokens: SourceField::Missing,
            total_tokens: SourceField::Missing,
            native_cost: SourceField::Missing,
        },
        finish_reason: SourceField::Missing,
        has_message_end: false,
        error: SourceField::Missing,
        tool_calls: vec![],
        has_repaired_tool_calls: SourceField::Missing,
    }
}
pub fn block_id(state: &SourceAssemblerState, kind: &JsString) -> Result<Option<JsString>> {
    let id = value(&state.response_id);
    if !truthy(&id) {
        return Ok(None);
    }
    // JS evaluates the template substitutions from left to right.
    let mut id = string(&id)?;
    append(&mut id, &":".into());
    append(&mut id, kind);
    append(&mut id, &":".into());
    let mut count = 0usize;
    for b in &state.content {
        if get(b.value(), "type")?.as_string() == Some(kind) {
            count += 1;
        }
    }
    append(&mut id, &count.to_string().into());
    Ok(Some(id))
}
fn flush(state: &mut SourceAssemblerState) -> Result<()> {
    if !state.thinking_buffer.is_empty()
        || !state.thinking_reasoning_content_buffer.is_empty()
        || !value(&state.thinking_signature).is_undefined()
    {
        let mut o = JsObject::new();
        o.insert("type".into(), "thinking".into());
        o.insert(
            "text".into(),
            JsValue::String(state.thinking_buffer.clone()),
        );
        if truthy(&value(&state.response_id)) {
            o.insert(
                "blockId".into(),
                block_id(state, &"thinking".into())?.map_or(JsValue::Undefined, JsValue::String),
            );
        }
        if !state.thinking_reasoning_content_buffer.is_empty() {
            o.insert(
                "reasoningContent".into(),
                JsValue::String(state.thinking_reasoning_content_buffer.clone()),
            );
        }
        let signature = value(&state.thinking_signature);
        if !signature.is_undefined() {
            o.insert("signature".into(), signature);
        }
        state.content.push(capture_object(o));
        state.thinking_buffer = "".into();
        state.thinking_reasoning_content_buffer = "".into();
        state.thinking_signature = present(&JsValue::Undefined);
        touch(state, SourceStateKey::ThinkingSignature);
    }
    if !state.text_buffer.is_empty() {
        let mut o = JsObject::new();
        o.insert("type".into(), "text".into());
        o.insert("text".into(), JsValue::String(state.text_buffer.clone()));
        if truthy(&value(&state.response_id)) {
            o.insert(
                "blockId".into(),
                block_id(state, &"text".into())?.map_or(JsValue::Undefined, JsValue::String),
            );
        }
        state.content.push(capture_object(o));
        state.text_buffer = "".into();
    }
    Ok(())
}
pub fn apply(state: &mut SourceAssemblerState, event: &BoundarySnapshot) -> Result<()> {
    let event = event.value();
    let tag = get(event, "type")?;
    let Some(tag) = tag.as_string() else {
        return Ok(());
    };
    if tag.equals_str("request_started") {
        state.model = present(&get(event, "model")?);
        touch(state, SourceStateKey::Model);
    } else if tag.equals_str("text_delta") {
        if !state.thinking_buffer.is_empty()
            || !state.thinking_reasoning_content_buffer.is_empty()
            || !value(&state.thinking_signature).is_undefined()
        {
            flush(state)?;
        }
        let text = string(&get(event, "text")?)?;
        append(&mut state.text_buffer, &text);
    } else if tag.equals_str("thinking_delta") {
        if !state.text_buffer.is_empty() {
            flush(state)?;
        }
        let text = string(&get(event, "text")?)?;
        append(&mut state.thinking_buffer, &text);
        let reasoning = get(event, "reasoningContent")?;
        if !reasoning.is_undefined() {
            let text = string(&reasoning)?;
            append(&mut state.thinking_reasoning_content_buffer, &text);
        }
        let signature = get(event, "signature")?;
        if !signature.is_undefined() && number(&get(&signature, "length")?)? > 0.0 {
            state.thinking_signature = present(&signature);
            touch(state, SourceStateKey::ThinkingSignature);
        }
    } else if tag.equals_str("tool_call_end") {
        flush(state)?;
        let call = get(event, "toolCall")?;
        state.tool_calls.push(BoundarySnapshot::capture(&call));
        let mut o = JsObject::new();
        o.insert("type".into(), "tool_call".into());
        spread(&mut o, &call);
        state.content.push(capture_object(o));
        if truthy(&get(event, "wasRepaired")?) {
            state.has_repaired_tool_calls = present(&JsValue::Bool(true));
            touch(state, SourceStateKey::HasRepairedToolCalls);
        }
    } else if tag.equals_str("message_end") {
        flush(state)?;
        state.finish_reason = present(&get(event, "finishReason")?);
        touch(state, SourceStateKey::FinishReason);
        state.has_message_end = true;
    } else if tag.equals_str("usage") {
        let second = get(event, "usage")?;
        let usage = SourceUsage {
            input_tokens: add(&state.usage.input_tokens, &get(&second, "inputTokens")?)?,
            output_tokens: add(&state.usage.output_tokens, &get(&second, "outputTokens")?)?,
            cache_read_tokens: add(
                &state.usage.cache_read_tokens,
                &get(&second, "cacheReadTokens")?,
            )?,
            cache_write_tokens: add(
                &state.usage.cache_write_tokens,
                &get(&second, "cacheWriteTokens")?,
            )?,
            total_tokens: add(&state.usage.total_tokens, &get(&second, "totalTokens")?)?,
            native_cost: add(&state.usage.native_cost, &get(&second, "nativeCost")?)?,
        };
        state.usage = usage;
    } else if tag.equals_str("error") {
        flush(state)?;
        state.error = present(&get(event, "error")?);
        touch(state, SourceStateKey::Error);
        state.finish_reason = present(&"error".into());
        touch(state, SourceStateKey::FinishReason);
    }
    // All remaining nonnull primitive/object and unknown-tag events are source no-ops.
    Ok(())
}
fn next_id(raw: &JsValue, i: usize, used: &HashSet<JsString>) -> Result<JsString> {
    let base = if truthy(raw) {
        let JsValue::String(raw) = raw else {
            return Err(error("rawId.trim is not a function"));
        };
        let trimmed = trim(raw);
        if trimmed.is_empty() {
            format!("call_{i}").into()
        } else {
            trimmed
        }
    } else {
        format!("call_{i}").into()
    };
    if !used.contains(&base) {
        return Ok(base);
    }
    let mut suffix = 2u64;
    loop {
        let mut id = base.clone();
        append(&mut id, &format!("_{suffix}").into());
        if !used.contains(&id) {
            return Ok(id);
        }
        suffix += 1;
    }
}
fn normalize_calls(state: &mut SourceAssemblerState) -> Result<()> {
    if state.tool_calls.is_empty() {
        return Ok(());
    }
    let mut used = HashSet::new();
    let mut calls = Vec::new();
    for (i, call) in state.tool_calls.iter().enumerate() {
        let raw = get(call.value(), "id")?;
        let id = next_id(&raw, i, &used)?;
        used.insert(id.clone());
        if raw.as_string() == Some(&id) {
            calls.push(call.clone());
        } else {
            let mut o = JsObject::new();
            spread(&mut o, call.value());
            o.insert("id".into(), JsValue::String(id));
            calls.push(capture_object(o));
        }
    }
    let mut index = 0;
    let mut content = Vec::new();
    for block in &state.content {
        if get(block.value(), "type")?
            .as_string()
            .is_some_and(|s| s.equals_str("tool_call"))
        {
            let call = calls.get(index);
            index += 1;
            if let Some(call) = call {
                let mut o = JsObject::new();
                spread(&mut o, block.value());
                o.insert("id".into(), get(call.value(), "id")?);
                content.push(capture_object(o));
            } else {
                content.push(block.clone());
            }
        } else {
            content.push(block.clone());
        }
    }
    state.content = content;
    state.tool_calls = calls;
    Ok(())
}
pub fn assemble(state: &mut SourceAssemblerState) -> Result<BoundarySnapshot> {
    flush(state)?;
    normalize_calls(state)?;
    let model = value(&state.model);
    let mut message = JsObject::new();
    message.insert("role".into(), "assistant".into());
    message.insert("content".into(), captures(&state.content));
    if truthy(&model) {
        message.insert("metadata".into(), object([("model", model)]));
    }
    let finish = value(&state.finish_reason);
    let finish = if nullish(&finish) {
        if truthy(&value(&state.error)) {
            "error".into()
        } else {
            "unknown".into()
        }
    } else {
        finish
    };
    let usage = usage_object(&state.usage);
    let has_usage = usage
        .as_object()
        .is_some_and(|o| o.entries().iter().any(|(_, v)| !v.is_undefined()));
    Ok(BoundarySnapshot::capture(&object([
        ("message", JsValue::Object(message)),
        ("finishReason", finish),
        ("hasMessageEnd", JsValue::Bool(state.has_message_end)),
        ("usage", if has_usage { usage } else { JsValue::Undefined }),
        ("toolCalls", captures(&state.tool_calls)),
        ("error", value(&state.error)),
        (
            "hasRepairedToolCalls",
            value(&state.has_repaired_tool_calls),
        ),
    ])))
}
pub fn snapshot(state: &SourceAssemblerState) -> JsObject {
    let mut o = JsObject::new();
    for key in &state.property_order {
        let (name, v) = match key {
            SourceStateKey::ResponseId => ("responseId", value(&state.response_id)),
            SourceStateKey::Content => ("content", captures(&state.content)),
            SourceStateKey::TextBuffer => {
                ("textBuffer", JsValue::String(state.text_buffer.clone()))
            }
            SourceStateKey::Model => ("model", value(&state.model)),
            SourceStateKey::ThinkingBuffer => (
                "thinkingBuffer",
                JsValue::String(state.thinking_buffer.clone()),
            ),
            SourceStateKey::ThinkingReasoningContentBuffer => (
                "thinkingReasoningContentBuffer",
                JsValue::String(state.thinking_reasoning_content_buffer.clone()),
            ),
            SourceStateKey::ThinkingSignature => {
                ("thinkingSignature", value(&state.thinking_signature))
            }
            SourceStateKey::Usage => ("usage", usage_object(&state.usage)),
            SourceStateKey::FinishReason => ("finishReason", value(&state.finish_reason)),
            SourceStateKey::HasMessageEnd => {
                ("hasMessageEnd", JsValue::Bool(state.has_message_end))
            }
            SourceStateKey::Error => ("error", value(&state.error)),
            SourceStateKey::ToolCalls => ("toolCalls", captures(&state.tool_calls)),
            SourceStateKey::HasRepairedToolCalls => (
                "hasRepairedToolCalls",
                value(&state.has_repaired_tool_calls),
            ),
        };
        o.insert(name.into(), v);
    }
    o
}

#[cfg(test)]
mod language_regressions {
    use super::*;
    #[test]
    fn numeric_string_grammar_and_radix_rounding_do_not_use_parsefloat() {
        for (s, n) in [
            ("", 0.0),
            ("\u{feff}01\u{feff}", 1.0),
            ("+.5", 0.5),
            ("1.", 1.0),
            ("-0", -0.0),
            ("1e400", f64::INFINITY),
            ("+Infinity", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
            ("0x20000000000001", 9007199254740992.0),
            ("0x20000000000003", 9007199254740996.0),
            ("0o17", 15.0),
            ("0b101", 5.0),
        ] {
            assert_eq!(
                number(&JsValue::from(s)).unwrap().to_bits(),
                n.to_bits(),
                "{s}"
            );
        }
        for s in [
            "+0x1", "-0b1", "0x", "0b2", "1_000", "1n", "1junk", "inf", ".e2", "1e+", "\u{85}1",
            "0x1x",
        ] {
            assert!(number(&JsValue::from(s)).unwrap().is_nan(), "{s}");
        }
        assert!(
            number(&JsValue::String(JsString::from_utf16(vec![0xd800])))
                .unwrap()
                .is_nan()
        );
        assert_eq!(
            number(&JsValue::String(format!("0x1{}", "0".repeat(255)).into())).unwrap(),
            2.0f64.powi(1020)
        );
        assert_eq!(
            number(&JsValue::String(format!("0x{}", "f".repeat(256)).into())).unwrap(),
            f64::INFINITY
        );
        assert!(matches!(
            number(&object([("toString", JsValue::Null)])),
            Err(SourceAssemblyError::TypeError { .. })
        ));
    }
    #[test]
    fn finite_and_nonfinite_js_number_strings_preserve_source_form() {
        for (n, s) in [
            (-0.0, "0"),
            (1e21, "1e+21"),
            (1e-6, "0.000001"),
            (1e-7, "1e-7"),
            (f64::INFINITY, "Infinity"),
            (f64::NEG_INFINITY, "-Infinity"),
            (f64::NAN, "NaN"),
        ] {
            assert!(string(&JsValue::Number(n)).unwrap().equals_str(s));
        }
        let got = plus(&JsValue::Number(-0.0), &JsValue::Number(-0.0)).unwrap();
        assert_eq!(got.as_number().unwrap().to_bits(), (-0.0f64).to_bits());
    }
}
