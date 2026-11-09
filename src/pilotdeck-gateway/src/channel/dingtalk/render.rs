use pilotdeck_protocol::{JsString, JsValue};
fn field(event: &JsValue, key: &str) -> JsString {
    super::super::sms::js_string_of(event.get(key).unwrap_or(&JsValue::Undefined))
}
pub fn render_dingtalk_event(event: &JsValue) -> Option<JsString> {
    let kind = event.get("type")?.as_string()?;
    if kind.equals_str("assistant_text_delta") {
        return event.get("text").and_then(JsValue::as_string).cloned();
    }
    if kind.equals_str("assistant_thinking_delta") || kind.equals_str("tool_call_started") {
        return Some("".into());
    }
    if kind.equals_str("tool_call_finished") {
        if event.get("ok") == Some(&JsValue::Bool(true)) {
            return Some("".into());
        }
        let undefined = JsValue::Undefined;
        let name = event
            .get("toolName")
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .unwrap_or_else(|| event.get("toolCallId").unwrap_or(&undefined));
        let mut u: Vec<u16> = "\n⚠️ ".encode_utf16().collect();
        u.extend_from_slice(super::super::sms::js_string_of(name).units());
        u.extend(" 执行失败\n".encode_utf16());
        return Some(JsString::from_utf16(u));
    }
    if kind.equals_str("error") {
        let mut u: Vec<u16> = "\n❌ ".encode_utf16().collect();
        u.extend_from_slice(field(event, "message").units());
        u.extend("\n".encode_utf16());
        return Some(JsString::from_utf16(u));
    }
    if kind.equals_str("elicitation_request") {
        let mut lines: Vec<JsString> = Vec::new();
        if let Some(JsValue::Array(questions)) = event.get("questions") {
            for q in questions {
                if let Some(h) = q
                    .get("header")
                    .and_then(JsValue::as_string)
                    .filter(|s| !s.is_empty())
                {
                    let mut u: Vec<u16> = "**".encode_utf16().collect();
                    u.extend_from_slice(h.units());
                    u.extend("**".encode_utf16());
                    lines.push(JsString::from_utf16(u));
                }
                if let Some(q) = q
                    .get("question")
                    .and_then(JsValue::as_string)
                    .filter(|s| !s.is_empty())
                {
                    lines.push(q.clone());
                }
                if let Some(JsValue::Array(options)) = q.get("options") {
                    for (i, o) in options.iter().enumerate() {
                        let mut u: Vec<u16> = format!("{}. ", i + 1).encode_utf16().collect();
                        u.extend_from_slice(field(o, "label").units());
                        lines.push(JsString::from_utf16(u));
                    }
                }
            }
        }
        if lines.is_empty() {
            return Some("".into());
        }
        let mut u = vec![10];
        for (i, l) in lines.iter().enumerate() {
            if i > 0 {
                u.push(10);
            }
            u.extend_from_slice(l.units());
        }
        u.push(10);
        return Some(JsString::from_utf16(u));
    }
    None
}
