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
    keys.sort_by(|a, b| match (a.0.array_index(), b.0.array_index()) {
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
        // Iterative (explicit stack): a deep array used to recurse one Rust
        // frame per level here as well, aborting the process (#1550 修 r4).
        // The joined units then take the same injective escape as the scalar
        // branch: every diagnostic is decoded by `DiagnosticText`, which panics
        // on a raw U+001F and would silently drop a raw escape tail
        // (#1550 修 r5).
        JsValue::Array(_) => match crate::js_value::to_string_units(v) {
            Ok(units) => diagnostic_string(&JsString::from_utf16(units)),
            // A caller that skipped `string_coercible` gets an empty message
            // rather than the TS RangeError; every config call site that can
            // see an array checks the bound first (#1550 修 r5).
            Err(crate::js_value::StackOverflow) => String::new(),
        },
        JsValue::Object(o) => format!("[object {}]", crate::js_value::object_tag(o)),
        _ => stringify_json(v).ok().flatten().unwrap_or_default(),
    }
}
pub(super) fn trim(s: &JsString) -> JsString {
    JsString::from_utf16(crate::runtime::memory::js_trim_units(s.units()).to_vec())
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
/// `Object.prototype`'s own property names under V8 (Node v22.22.0). Named so
/// a test can assert set equality in both directions instead of only probing
/// membership: adding a name here 200s a route TS answers 404 for, and no
/// membership probe would notice (#2183 评审 r2 P1).
pub(crate) const BUILTIN_PROPERTY_NAMES: [&str; 12] = [
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
];
pub(crate) fn builtin_property(s: &JsString) -> bool {
    BUILTIN_PROPERTY_NAMES.iter().any(|v| s.equals_str(v))
}
/// Would `String(v)` throw, and how? Delegates to the shared bounded walk:
/// config files and binding rows are parsed JSON, and a deep array used to
/// recurse one Rust frame per level until the process aborted (#1550 修 r4).
/// The error kind is kept (not collapsed to a flag) because TS reports
/// `RangeError` past the depth bound and `TypeError` for an own `toString`
/// (#1550 修 r5).
pub(crate) fn string_coercible(v: &JsValue) -> Result<(), crate::js_value::ToStringError> {
    crate::js_value::string_coercible_checked(v)
}
/// The error TS raises for a failed `String(v)`, with the class V8 picks
/// (`RangeError` past the shared depth bound, `TypeError` otherwise).
pub(super) fn coercion_error(kind: crate::js_value::ToStringError) -> super::Error {
    let mut e = super::Error::new("gateway_request_failed", kind.message());
    e.name = kind.name();
    e
}
/// The one check-and-map every config read face that coerces with
/// `String(value)` goes through (`schemaVersion`, `alwaysOn.language`,
/// `encodeURIComponent(agentId)`): the walk's error kind maps to the JS error
/// class, never a collapsed one. A single helper keeps the three call sites
/// from re-collapsing the class on their own (#1833 B1 — each site used to be
/// free to hardcode the kind, and only `NotPrimitive` was reachable through a
/// real YAML load, so a collapsed mapping stayed green).
pub(super) fn require_string_coercible(v: &JsValue) -> Result<(), super::Error> {
    string_coercible(v).map_err(coercion_error)
}
