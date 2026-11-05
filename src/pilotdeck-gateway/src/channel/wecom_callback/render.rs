//! `wecom-callback-render.ts` — one gateway event to a reply fragment.

use pilotdeck_protocol::JsValue;

pub(crate) fn js_truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) | Some(JsValue::Bool(false)) => false,
        Some(JsValue::Bool(true)) | Some(JsValue::Object(_)) | Some(JsValue::Array(_)) => true,
        Some(JsValue::Number(number)) => *number != 0.0 && !number.is_nan(),
        Some(JsValue::String(text)) => !text.is_empty(),
    }
}

fn text(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
}

/// JavaScript `String(value)` for the interpolation sites in the TS renderer.
fn display(value: Option<&JsValue>) -> String {
    match value {
        None | Some(JsValue::Undefined) => "undefined".into(),
        Some(JsValue::Null) => "null".into(),
        Some(JsValue::Bool(value)) => value.to_string(),
        Some(JsValue::Number(value)) => crate::js_value::number_string(*value),
        Some(JsValue::String(value)) => String::from_utf16_lossy(value.units()),
        Some(JsValue::Array(_)) | Some(JsValue::Object(_)) => value
            .and_then(|value| crate::js_value::display(value).ok())
            .unwrap_or_else(|| "undefined".into()),
    }
}

fn items(value: Option<&JsValue>) -> &[JsValue] {
    match value {
        Some(JsValue::Array(items)) => items,
        _ => &[],
    }
}

/// `renderWeComCallbackEvent`: `Some("")` for events that yield an empty
/// fragment, `None` for events the channel ignores.
pub(crate) fn render_event(event: &JsValue) -> Option<String> {
    let kind = text(event.get("type"))?;
    match kind.as_str() {
        "assistant_text_delta" => text(event.get("text")),
        "assistant_thinking_delta" | "tool_call_started" => Some(String::new()),
        "tool_call_finished" => {
            if js_truthy(event.get("ok")) {
                return Some(String::new());
            }
            let name = event
                .get("toolName")
                .filter(|value| !value.is_undefined() && !matches!(value, JsValue::Null))
                .or_else(|| event.get("toolCallId"));
            Some(format!("\n⚠️ {} failed\n", display(name)))
        }
        "elicitation_request" => {
            let mut lines = Vec::new();
            for question in items(event.get("questions")) {
                let header = text(question.get("header")).unwrap_or_default();
                if !header.is_empty() {
                    lines.push(format!("**{header}**"));
                }
                let prompt = text(question.get("question")).unwrap_or_default();
                if !prompt.is_empty() {
                    lines.push(prompt);
                }
                for (index, option) in items(question.get("options")).iter().enumerate() {
                    let label = display(option.get("label"));
                    lines.push(format!("{}. {label}", index + 1));
                }
            }
            if lines.is_empty() {
                Some(String::new())
            } else {
                Some(format!("\n{}\n", lines.join("\n")))
            }
        }
        "error" => Some(format!("\n❌ {}\n", display(event.get("message")))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;

    fn event(raw: &str) -> JsValue {
        parse_json(raw).unwrap()
    }

    #[test]
    fn renders_the_ts_fragment_shapes() {
        assert_eq!(
            render_event(&event(r#"{"type":"assistant_text_delta","text":"hi"}"#)),
            Some("hi".into())
        );
        assert_eq!(
            render_event(&event(r#"{"type":"assistant_thinking_delta","text":"x"}"#)),
            Some("".into())
        );
        assert_eq!(
            render_event(&event(
                r#"{"type":"tool_call_finished","ok":false,"toolName":"bash"}"#
            )),
            Some("\n⚠️ bash failed\n".into())
        );
        assert_eq!(
            render_event(&event(
                r#"{"type":"tool_call_finished","ok":false,"toolCallId":"t1"}"#
            )),
            Some("\n⚠️ t1 failed\n".into())
        );
        assert_eq!(
            render_event(&event(r#"{"type":"error","message":"boom"}"#)),
            Some("\n❌ boom\n".into())
        );
        assert_eq!(render_event(&event(r#"{"type":"unknown"}"#)), None);
        assert_eq!(
            render_event(&event(
                r#"{"type":"elicitation_request","questions":[{"header":"H","question":"Q","options":[{"label":"A"},{"label":"B"}]}]}"#
            )),
            Some("\n**H**\nQ\n1. A\n2. B\n".into())
        );
    }
}
