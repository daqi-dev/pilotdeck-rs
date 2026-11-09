use crate::channel::sms;
use pilotdeck_protocol::{JsString, JsValue};
pub fn render_qq_event(event: &JsValue) -> Option<JsString> {
    if event.get("type") == Some(&JsValue::from("tool_call_finished"))
        && !event.get("ok").is_some_and(sms::truthy)
    {
        let name = event
            .get("toolName")
            .filter(|v| !sms::nullish(v))
            .or_else(|| event.get("toolCallId"))
            .map(sms::js_string_of)
            .unwrap_or_else(|| "undefined".into());
        return Some(sms::concat_units(&[
            &"\n⚠️ ".encode_utf16().collect::<Vec<_>>(),
            name.units(),
            &" 执行失败\n".encode_utf16().collect::<Vec<_>>(),
        ]));
    }
    sms::render::render_sms_event(event)
}
pub fn split_text(text: &JsString, max: usize) -> Vec<JsString> {
    assert!(max > 0);
    if text.units().len() <= max {
        return vec![text.clone()];
    }
    let mut rest = text.units();
    let mut chunks = Vec::new();
    while !rest.is_empty() {
        if rest.len() <= max {
            chunks.push(JsString::from_utf16(rest.to_vec()));
            break;
        }
        let split = rest[..=max]
            .iter()
            .rposition(|u| *u == 10)
            .filter(|at| (*at as f64) >= (max as f64) * 0.3)
            .unwrap_or(max);
        chunks.push(JsString::from_utf16(rest[..split].to_vec()));
        rest = &rest[split..];
        if rest.first() == Some(&10) {
            rest = &rest[1..];
        }
    }
    chunks
}
