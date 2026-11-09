//! Telegram and SMS event fragments share the same fixed-baseline projection.
use pilotdeck_protocol::{JsString, JsValue};
pub fn render_telegram_event(event: &JsValue) -> Option<JsString> {
    crate::channel::sms::render::render_sms_event(event)
}
/// JS string length/slice are measured in UTF-16 units, including a split surrogate.
pub fn chunk_text(content: &JsString, max: usize) -> Vec<JsString> {
    assert!(max > 0);
    let mut rest = content.units();
    if rest.len() <= max {
        return vec![content.clone()];
    }
    let mut chunks = Vec::new();
    while rest.len() > max {
        let mut split = rest[..=max].iter().rposition(|u| *u == 10).unwrap_or(0);
        if split * 2 < max {
            split = rest[..=max].iter().rposition(|u| *u == 32).unwrap_or(0);
        }
        if split * 2 < max {
            split = max;
        }
        chunks.push(JsString::from_utf16(rest[..split].to_vec()));
        rest = &rest[split..];
        while rest.first() == Some(&10) {
            rest = &rest[1..];
        }
    }
    if !rest.is_empty() {
        chunks.push(JsString::from_utf16(rest.to_vec()));
    }
    chunks
}
