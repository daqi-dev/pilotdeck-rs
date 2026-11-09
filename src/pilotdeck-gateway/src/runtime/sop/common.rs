use crate::host::{HostError, ModuleError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
pub(super) fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&U)
}
pub(super) fn text(v: &JsValue) -> Option<JsString> {
    v.as_string().and_then(|s| {
        let units = crate::runtime::memory::js_trim_units(s.units());
        (!units.is_empty()).then(|| JsString::from_utf16(units.to_vec()))
    })
}
pub(super) fn key<'a>(v: &'a JsValue, k: &JsString) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.as_object().and_then(|o| o.get_key(k)).unwrap_or(&U)
}
pub(super) fn display(v: &JsValue) -> String {
    v.as_string()
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_else(|| crate::js_value::display(v).unwrap_or_else(|_| "[value]".into()))
}
pub(super) fn strings(v: &JsValue) -> Vec<JsString> {
    if let JsValue::Array(a) = v {
        a.iter().filter_map(|v| v.as_string().cloned()).collect()
    } else {
        vec![]
    }
}
pub(super) fn object(v: &JsValue) -> JsObject {
    v.as_object().cloned().unwrap_or_default()
}
pub(super) fn set(v: &mut JsValue, k: &str, x: JsValue) {
    if let JsValue::Object(o) = v {
        o.insert(k.into(), x);
    }
}
pub(super) fn without(v: &JsValue, keys: &[&str]) -> JsValue {
    let mut out = JsObject::new();
    if let Some(o) = v.as_object() {
        for (k, v) in o.own_entries() {
            if !keys.iter().any(|x| k.equals_str(x)) {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    JsValue::Object(out)
}
pub(super) fn error(code: &str, message: impl Into<JsString>) -> HostError {
    let mut e = ModuleError::new(message);
    e.code = Some(code.into());
    e.into()
}
pub(super) fn json(v: &JsValue) -> String {
    pilotdeck_protocol::stringify_json(v)
        .ok()
        .flatten()
        .unwrap_or_else(|| "null".into())
}
pub(super) fn clock() -> JsString {
    use pilotdeck_transport::server::ExecutionClock;
    pilotdeck_transport::system_clock::SystemExecutionClock.now_iso()
}
pub(super) fn active(v: &JsValue) -> bool {
    field(v, "status") == &JsValue::from("active")
}
pub(super) fn cancelled() -> HostError {
    error(
        "SOP_RUNTIME_CANCELLED",
        "StaffDeck SOP runtime request was cancelled: This operation was aborted",
    )
}
pub(super) fn store_error(e: crate::module_host::approvals::disk::SopFailure) -> HostError {
    error(&e.code, e.message.as_str())
}
pub(super) fn client_error(e: super::client::ClientError) -> HostError {
    let mut out = error(&String::from_utf16_lossy(e.code.units()), e.message);
    if let Some(module) = &mut out.module_error {
        module.canonical = e.details.and_then(|v| v.as_object().cloned());
    }
    out
}
