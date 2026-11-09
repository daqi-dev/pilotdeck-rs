//! Wire text rendering from fixed wecom-render.ts.
use pilotdeck_protocol::{JsString, JsValue};

fn is(event: &JsValue, expected: &str) -> bool {
    event
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(expected))
}
fn append(out: &mut Vec<u16>, value: &str) {
    out.extend(value.encode_utf16());
}
fn value_units(value: Option<&JsValue>) -> Result<Vec<u16>, &'static str> {
    crate::js_value::to_string_units_checked(value.unwrap_or(&JsValue::Undefined))
        .map_err(|_| "WeCom rendering value cannot be stringified")
}
pub(crate) fn render(event: &JsValue) -> Result<Option<JsString>, &'static str> {
    if is(event, "assistant_text_delta") {
        return Ok(event.get("text").and_then(JsValue::as_string).cloned());
    }
    if is(event, "assistant_thinking_delta") || is(event, "tool_call_started") {
        return Ok(Some("".into()));
    }
    if is(event, "tool_call_finished") {
        if event.get("ok") == Some(&JsValue::Bool(true)) {
            return Ok(Some("".into()));
        }
        let mut out = "\n⚠️ ".encode_utf16().collect::<Vec<_>>();
        let name = event
            .get("toolName")
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .or_else(|| event.get("toolCallId"));
        out.extend(value_units(name)?);
        append(&mut out, " failed\n");
        return Ok(Some(JsString::from_utf16(out)));
    }
    if is(event, "error") {
        let mut out = "\n❌ ".encode_utf16().collect::<Vec<_>>();
        out.extend(value_units(event.get("message"))?);
        append(&mut out, "\n");
        return Ok(Some(JsString::from_utf16(out)));
    }
    if is(event, "elicitation_request") {
        let Some(JsValue::Array(questions)) = event.get("questions") else {
            return Err("questions must be an array");
        };
        let mut lines: Vec<Vec<u16>> = Vec::new();
        for question in questions {
            if let Some(header) = question
                .get("header")
                .and_then(JsValue::as_string)
                .filter(|s| !s.units().is_empty())
            {
                let mut line = "**".encode_utf16().collect::<Vec<_>>();
                line.extend_from_slice(header.units());
                append(&mut line, "**");
                lines.push(line);
            }
            if let Some(text) = question
                .get("question")
                .and_then(JsValue::as_string)
                .filter(|s| !s.units().is_empty())
            {
                lines.push(text.units().to_vec());
            }
            let Some(JsValue::Array(options)) = question.get("options") else {
                return Err("options must be an array");
            };
            for (i, option) in options.iter().enumerate() {
                let mut line = format!("{}. ", i + 1).encode_utf16().collect::<Vec<_>>();
                line.extend(value_units(option.get("label"))?);
                lines.push(line);
            }
        }
        if lines.is_empty() {
            return Ok(Some("".into()));
        }
        let mut out = vec!['\n' as u16];
        for (i, line) in lines.iter().enumerate() {
            if i != 0 {
                out.push('\n' as u16);
            }
            out.extend_from_slice(line);
        }
        out.push('\n' as u16);
        return Ok(Some(JsString::from_utf16(out)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn rendering_matches_fixed_typescript_oracle() {
        let oracle = parse_json(include_str!("fixtures/oracle.json")).unwrap();
        let JsValue::Array(events) = oracle.get("events").unwrap() else {
            panic!("events")
        };
        let JsValue::Array(expected) = oracle.get("rendered").unwrap() else {
            panic!("rendered")
        };
        for (event, expected) in events.iter().zip(expected) {
            let actual = render(event)
                .unwrap()
                .map(JsValue::String)
                .unwrap_or(JsValue::Null);
            assert_eq!(&actual, expected);
        }
    }
}
