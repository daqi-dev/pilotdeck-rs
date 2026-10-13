//! Apply already executed tools through the advertised host context capability.
//! No tool execution, filesystem, permissions or transcript authority lives here.
use crate::ports::{
    ApplyToolResults, HostCall, HostPort, HostReply, PortError, PortResult, SupplementalMessage,
};
use crate::tool_context::{ContextMessageCollection, ToolContextInput, ToolContextOutcome};
use crate::{MessageProjection, ProjectedMessage, ProjectionGap, ToolExecutionResult};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsString, JsValue};

fn bind_supplementals(
    results: &[ToolExecutionResult],
    supplemental: &[ProjectedMessage],
) -> Vec<SupplementalMessage> {
    let mut bound = vec![];
    let mut index = 0;
    for result in results {
        let count = result.supplemental_messages.as_ref().map_or(0, Vec::len);
        for _ in 0..count {
            let Some(message) = supplemental.get(index) else {
                break;
            };
            bound.push(SupplementalMessage {
                tool_call_id: result.tool_call_id.clone(),
                message: message.clone(),
            });
            index += 1;
        }
    }
    bound
}

fn source_error(message: &str, value: &JsValue) -> PortError {
    PortError::SourceException {
        message: message.into(),
        boundary: BoundarySnapshot::capture(value),
    }
}
fn property(value: &JsValue, key: &str) -> PortResult<JsValue> {
    match value {
        JsValue::Null => Err(source_error(
            &format!("Cannot read properties of null (reading '{key}')"),
            value,
        )),
        JsValue::Undefined => Err(source_error(
            &format!("Cannot read properties of undefined (reading '{key}')"),
            value,
        )),
        _ => Ok(value.get(key).cloned().unwrap_or(JsValue::Undefined)),
    }
}
fn length(value: &JsValue) -> PortResult<JsValue> {
    match value {
        JsValue::Array(values) => Ok(JsValue::Number(values.len() as f64)),
        JsValue::String(value) => Ok(JsValue::Number(value.len_utf16() as f64)),
        _ => property(value, "length"),
    }
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Null | JsValue::Undefined => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
fn project(value: &JsValue) -> ProjectedMessage {
    ProjectedMessage {
        boundary: BoundarySnapshot::capture(value),
        normalized: MessageProjection::BoundaryOnly(vec![ProjectionGap::OtherBoundaryShape]),
    }
}
fn collection(value: &JsValue) -> ContextMessageCollection {
    match value {
        JsValue::Array(values) => {
            ContextMessageCollection::Items(values.iter().map(project).collect())
        }
        _ => ContextMessageCollection::BoundaryOnly(BoundarySnapshot::capture(value)),
    }
}
fn fallback(
    messages: ContextMessageCollection,
    appended_messages: ContextMessageCollection,
    projected: &[ProjectedMessage],
    retains_projected_appended_identity: bool,
) -> PortResult<ToolContextOutcome> {
    match messages {
        ContextMessageCollection::Items(mut messages) => {
            messages.extend_from_slice(projected);
            Ok(ToolContextOutcome {
                messages: ContextMessageCollection::Items(messages),
                appended_messages,
                reattach_tail: false,
                fallback_projection_tail: retains_projected_appended_identity,
            })
        }
        ContextMessageCollection::BoundaryOnly(value) => {
            let message = match value.value() {
                JsValue::Null => "Cannot read properties of null (reading 'push')",
                JsValue::Undefined => "Cannot read properties of undefined (reading 'push')",
                _ => "messages.push is not a function",
            };
            Err(source_error(message, value.value()))
        }
    }
}
fn primitive(value: &JsValue, depth: usize) -> PortResult<JsValue> {
    match value {
        JsValue::Object(object) => {
            // For an admitted plain JSON object, non-callable own valueOf is
            // skipped; default toString works only when not shadowed. Native
            // callable/accessor/prototype graphs are not represented by JsValue.
            if object.contains_key("toString") {
                Err(source_error(
                    "Cannot convert object to primitive value",
                    value,
                ))
            } else {
                Ok(JsValue::from("[object Object]"))
            }
        }
        JsValue::Array(values) => {
            if depth >= 1024 {
                return Err(PortError::HostFailure { code: "TOOL_CONTEXT_NATIVE_COERCION_DEPTH_PROFILE".into(), message: "Native Array.toString recursion limits require an independent engine profile; no source RangeError was invented.".into(), boundary: BoundarySnapshot::capture(value) });
            }
            let mut units = vec![];
            for (index, item) in values.iter().enumerate() {
                if index > 0 {
                    units.push(b',' as u16);
                }
                if matches!(item, JsValue::Null | JsValue::Undefined) {
                    continue;
                }
                let item = primitive(item, depth + 1)?;
                match &item {
                    JsValue::String(text) => units.extend_from_slice(text.units()),
                    JsValue::Bool(value) => {
                        units.extend(if *value { "true" } else { "false" }.encode_utf16())
                    }
                    JsValue::Number(value) => {
                        let text = if value.is_nan() {
                            "NaN".into()
                        } else if *value == f64::INFINITY {
                            "Infinity".into()
                        } else if *value == f64::NEG_INFINITY {
                            "-Infinity".into()
                        } else {
                            pilotdeck_protocol::stringify_json(&item)
                                .map_err(|error| source_error(&error.to_string(), &item))?
                                .expect("number has JSON text")
                        };
                        units.extend(text.encode_utf16());
                    }
                    _ => unreachable!("primitive conversion returns a primitive"),
                }
            }
            Ok(JsValue::String(JsString::from_utf16(units)))
        }
        _ => Ok(value.clone()),
    }
}
fn js_space(character: char) -> bool {
    matches!(character as u32, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn radix_number(text: &str, radix: u32) -> f64 {
    let mut bits = vec![];
    let width = radix.trailing_zeros();
    for character in text.chars() {
        let Some(digit) = character.to_digit(radix) else {
            return f64::NAN;
        };
        for shift in (0..width).rev() {
            bits.push(digit & (1 << shift) != 0);
        }
    }
    if bits.is_empty() {
        return f64::NAN;
    }
    let first = bits.iter().position(|bit| *bit).unwrap_or(bits.len());
    let bits = &bits[first..];
    if bits.len() > 1024 {
        return f64::INFINITY;
    }
    let mut significand = 0u64;
    for bit in bits.iter().take(53) {
        significand = (significand << 1) | u64::from(*bit);
    }
    if bits.len() <= 53 {
        return significand as f64;
    }
    if bits[53] && (bits[54..].iter().any(|bit| *bit) || significand & 1 != 0) {
        significand += 1;
    }
    (significand as f64) * 2f64.powi((bits.len() - 53) as i32)
}
fn number(value: &JsValue) -> f64 {
    match value {
        JsValue::Null => 0.0,
        JsValue::Undefined => f64::NAN,
        JsValue::Bool(value) => f64::from(u8::from(*value)),
        JsValue::Number(value) => *value,
        JsValue::String(string) => {
            let Ok(string) = string.to_utf8() else {
                return f64::NAN;
            };
            let text = string.trim_matches(js_space);
            if text.is_empty() {
                return 0.0;
            }
            match text {
                "Infinity" | "+Infinity" => return f64::INFINITY,
                "-Infinity" => return f64::NEG_INFINITY,
                _ => (),
            }
            for (prefix, radix) in [
                ("0x", 16),
                ("0X", 16),
                ("0b", 2),
                ("0B", 2),
                ("0o", 8),
                ("0O", 8),
            ] {
                if let Some(digits) = text.strip_prefix(prefix) {
                    return radix_number(digits, radix);
                }
            }
            if text
                .chars()
                .any(|c| !matches!(c, '0'..='9' | '+' | '-' | '.' | 'e' | 'E'))
            {
                return f64::NAN;
            }
            text.parse().unwrap_or(f64::NAN)
        }
        _ => unreachable!("only after primitive conversion"),
    }
}
fn fits(appended: &JsValue, messages: &JsValue) -> PortResult<bool> {
    // Property reads precede conversion; even an undefined left length does
    // not suppress the source exception from null messages.length.
    let left_length = length(appended)?;
    let right_length = length(messages)?;
    let left = primitive(&left_length, 0)?;
    let right = primitive(&right_length, 0)?;
    match (&left, &right) {
        (JsValue::String(left), JsValue::String(right)) => Ok(left <= right),
        _ => Ok(number(&left) <= number(&right)),
    }
}
fn string_items(string: &JsString) -> Vec<ProjectedMessage> {
    let mut out = vec![];
    let mut at = 0;
    while at < string.units().len() {
        let unit = string.units()[at];
        let paired = (0xd800..=0xdbff).contains(&unit)
            && string
                .units()
                .get(at + 1)
                .is_some_and(|next| (0xdc00..=0xdfff).contains(next));
        let width = if paired { 2 } else { 1 };
        out.push(project(&JsValue::String(JsString::from_utf16(
            string.units()[at..at + width].to_vec(),
        ))));
        at += width;
    }
    out
}
pub fn apply(
    host: &mut dyn HostPort,
    input: ToolContextInput<'_>,
) -> PortResult<ToolContextOutcome> {
    input.control.check()?;
    let Some((main, supplements)) = input.projected.split_first() else {
        return Err(PortError::InvalidResponse { message: "Tool projection must contain the original aggregate message, even for zero results.".into() });
    };
    let supplemental_messages = bind_supplementals(input.results, supplements);
    let mut messages = ContextMessageCollection::Items(input.messages.to_vec());
    let mut appended_messages = ContextMessageCollection::Items(input.projected.to_vec());
    if !host.advertised("context", "apply_tool_results") {
        return fallback(messages, appended_messages, input.projected, true);
    }
    let reply = host.call(
        input.control,
        HostCall::ApplyToolResults(ApplyToolResults {
            session_id: input.input.session_id.clone(),
            turn_id: input.input.turn_id.clone(),
            tool_result_message: main.clone(),
            supplemental_messages: Some(supplemental_messages),
            messages: input.messages.to_vec(),
        }),
    );
    let applied = match reply {
        Ok(HostReply::ToolResultsApplied(value)) => value.boundary,
        Ok(HostReply::ToolResultsAppliedBoundaryOnly(value)) => value,
        Ok(_) => {
            return Err(PortError::InvalidResponse {
                message: "Context apply received an unrelated host reply tag.".into(),
            });
        }
        Err(error @ (PortError::AcceptedUnknown { .. } | PortError::Cancelled { .. })) => {
            return Err(error);
        }
        Err(_) => return fallback(messages, appended_messages, input.projected, true),
    };
    let raw_messages = match property(applied.value(), "messages") {
        Ok(value) => value,
        Err(_) => return fallback(messages, appended_messages, input.projected, true),
    };
    messages = collection(&raw_messages);
    let supplied = property(applied.value(), "appendedMessages")?;
    if !matches!(supplied, JsValue::Null | JsValue::Undefined) {
        appended_messages = collection(&supplied);
    }
    if !truthy(&supplied) {
        return Ok(ToolContextOutcome {
            messages,
            appended_messages,
            reattach_tail: false,
            fallback_projection_tail: false,
        });
    }
    let fits = match fits(&supplied, &raw_messages) {
        Ok(value) => value,
        Err(error @ PortError::HostFailure { .. }) => return Err(error),
        Err(_) => return fallback(messages, appended_messages, input.projected, false),
    };
    if !fits {
        return Ok(ToolContextOutcome {
            messages,
            appended_messages,
            reattach_tail: false,
            fallback_projection_tail: false,
        });
    }
    if !matches!(raw_messages, JsValue::Array(_) | JsValue::String(_))
        || !matches!(supplied, JsValue::Array(_) | JsValue::String(_))
    {
        // The source slice/spread exception is caught after assignment. The
        // catch then pushes onto that assigned returned message container.
        return fallback(messages, appended_messages, input.projected, false);
    }
    if let JsValue::String(string) = &raw_messages {
        let count = match &supplied {
            JsValue::Array(values) => values.len(),
            JsValue::String(value) => value.len_utf16(),
            _ => unreachable!(),
        };
        let end = string.len_utf16() - count;
        let mut prefix = string_items(&JsString::from_utf16(string.units()[..end].to_vec()));
        match &supplied {
            JsValue::Array(values) => prefix.extend(values.iter().map(project)),
            JsValue::String(value) => prefix.extend(string_items(value)),
            _ => unreachable!(),
        }
        messages = ContextMessageCollection::Items(prefix);
    }
    // The turn owner replaces the normal array tail after annotating canonical
    // appended values. No post-ACK check discards the acknowledged state here.
    Ok(ToolContextOutcome {
        messages,
        appended_messages,
        reattach_tail: true,
        fallback_projection_tail: false,
    })
}
