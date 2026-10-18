//! JavaScript value operations shared by configuration validators.
use pilotdeck_protocol::{JsString, JsValue, stringify_json};
pub(super) static UNDEFINED: JsValue = JsValue::Undefined;
pub(super) fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value
        .as_object()
        .and_then(|o| o.get_property(key))
        .unwrap_or(&UNDEFINED)
}
pub(super) fn record(value: &JsValue) -> bool {
    value.as_object().is_some()
}
pub(super) fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}
pub(super) fn text(value: &JsValue) -> Option<String> {
    value.as_string()?.to_utf8().ok()
}
pub(super) fn eq(value: &JsValue, other: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(other))
}
pub(super) fn entries(value: &JsValue) -> Vec<(&JsString, &JsValue)> {
    let Some(obj) = value.as_object() else {
        return vec![];
    };
    let mut keys: Vec<_> = obj.entries().iter().map(|(k, v)| (k, v)).collect();
    fn index(s: &JsString) -> Option<u32> {
        let s = s.to_utf8().ok()?;
        let n = s.parse::<u32>().ok()?;
        (n != u32::MAX && n.to_string() == s).then_some(n)
    }
    keys.sort_by(|a, b| match (index(a.0), index(b.0)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => std::cmp::Ordering::Equal,
    });
    keys
}
pub(super) fn js_string(v: &JsValue) -> String {
    match v {
        JsValue::Undefined => "undefined".into(),
        JsValue::Null => "null".into(),
        JsValue::Bool(v) => v.to_string(),
        JsValue::String(s) => diagnostic_string(s),
        JsValue::Number(v) if v.is_nan() => "NaN".into(),
        JsValue::Number(v) if *v == f64::INFINITY => "Infinity".into(),
        JsValue::Number(v) if *v == f64::NEG_INFINITY => "-Infinity".into(),
        JsValue::Array(a) => a
            .iter()
            .map(|v| {
                if nullish(v) {
                    String::new()
                } else {
                    js_string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        JsValue::Object(o) => format!("[object {}]", o.native_tag().unwrap_or("Object")),
        _ => stringify_json(v).ok().flatten().unwrap_or_default(),
    }
}
pub(super) fn trim(s: &JsString) -> JsString {
    fn white(c: u16) -> bool {
        matches!(c,9..=13|32|160|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff)
    }
    let u = s.units();
    let a = u.iter().position(|c| !white(*c)).unwrap_or(u.len());
    let b = u.iter().rposition(|c| !white(*c)).map_or(a, |i| i + 1);
    JsString::from_utf16(u[a..b].to_vec())
}
pub(super) fn nonempty(v: &JsValue) -> Option<String> {
    let s = text(v)?;
    let s = crate::metadata::js_trim(&s);
    (!s.is_empty()).then(|| s.to_owned())
}
pub(super) fn number(v: &JsValue, positive: bool, integer: bool) -> bool {
    v.as_number().is_some_and(|n| {
        n.is_finite() && if positive { n > 0.0 } else { n >= 0.0 } && (!integer || n.fract() == 0.0)
    })
}
pub(super) fn strings(v: &JsValue) -> bool {
    matches!(v,JsValue::Array(a) if a.iter().all(|v|v.as_string().is_some()))
}
pub(super) fn http_url(v: &str) -> Option<String> {
    let u = url::Url::parse(v).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.to_string())
}
// Rust format! is used for diagnostics, so dynamic JS strings are carried
// through it with an injective UTF-16 escape. Escape the escape character too;
// this is private to diagnostic interpolation and never used in config values.
fn diagnostic_string(s: &JsString) -> String {
    let mut out = String::new();
    for c in char::decode_utf16(s.units().iter().copied()) {
        match c {
            Ok('\u{001f}') => out.push_str("\u{001f}E"),
            Ok(c) => out.push(c),
            Err(e) => out.push_str(&format!("\u{001f}S{:04X}", e.unpaired_surrogate())),
        }
    }
    out
}
pub(super) struct DiagnosticText(pub JsString);
impl From<String> for DiagnosticText {
    fn from(s: String) -> Self {
        let mut chars = s.chars();
        let mut units = vec![];
        while let Some(c) = chars.next() {
            if c == '\u{001f}' {
                match chars.next() {
                    Some('E') => units.push(31),
                    Some('S') => {
                        let hex: String = chars.by_ref().take(4).collect();
                        units.push(
                            u16::from_str_radix(&hex, 16).expect("internal diagnostic escape"),
                        );
                    }
                    _ => unreachable!("unescaped diagnostic marker"),
                }
            } else {
                let mut buf = [0; 2];
                units.extend_from_slice(c.encode_utf16(&mut buf));
            }
        }
        Self(JsString::from_utf16(units))
    }
}
impl From<&str> for DiagnosticText {
    fn from(s: &str) -> Self {
        Self::from(s.to_owned())
    }
}
impl From<JsString> for DiagnosticText {
    fn from(s: JsString) -> Self {
        Self(s)
    }
}
pub(crate) fn builtin_property(s: &JsString) -> bool {
    [
        "__proto__",
        "constructor",
        "toString",
        "toLocaleString",
        "valueOf",
        "hasOwnProperty",
        "isPrototypeOf",
        "propertyIsEnumerable",
        "__defineGetter__",
        "__defineSetter__",
        "__lookupGetter__",
        "__lookupSetter__",
    ]
    .iter()
    .any(|v| s.equals_str(v))
}
pub(crate) fn string_coercible(v: &JsValue) -> bool {
    match v {
        JsValue::Object(o) => o.get_property("toString").is_none(),
        JsValue::Array(a) => a.iter().all(string_coercible),
        _ => true,
    }
}
pub(super) fn primitive_error() -> super::Error {
    let mut e = super::Error::new(
        "gateway_request_failed",
        "Cannot convert object to primitive value",
    );
    e.name = "TypeError";
    e
}
