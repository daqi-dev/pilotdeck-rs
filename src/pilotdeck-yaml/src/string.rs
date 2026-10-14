//! Small UTF-16 operations used by the direct JS port; never replacement-decode.
use pilotdeck_protocol::{JsString, JsValue, stringify_json};
pub(crate) fn concat(parts: &[&JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|s| s.units().iter().copied())
            .collect(),
    )
}
pub(crate) fn append(a: &mut JsString, b: &JsString) {
    *a = concat(&[a, b]);
}
pub(crate) fn slice(s: &JsString, start: usize, end: usize) -> JsString {
    JsString::from_utf16(
        s.units()[start.min(s.len_utf16())..end.min(s.len_utf16()).max(start.min(s.len_utf16()))]
            .to_vec(),
    )
}
pub(crate) fn json(s: &JsString) -> JsString {
    stringify_json(&JsValue::String(s.clone()))
        .unwrap()
        .unwrap()
        .into()
}
pub(crate) fn message(prefix: &str, s: &JsString) -> JsString {
    concat(&[&prefix.into(), s])
}
pub(crate) fn is_ws(ch: u16) -> bool {
    matches!(ch, 9..=13 | 32 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
pub(crate) fn trim(s: &JsString) -> JsString {
    let a = s
        .units()
        .iter()
        .position(|c| !is_ws(*c))
        .unwrap_or(s.len_utf16());
    let b = s
        .units()
        .iter()
        .rposition(|c| !is_ws(*c))
        .map_or(a, |i| i + 1);
    slice(s, a, b)
}
