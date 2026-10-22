//! Compaction message markers shared by the gateway writer and agent reader.
//! Fixed baseline: `context/compaction/CompactionEngine.ts`.

pub const SUMMARY_PREFIX: &str = "[CONTEXT COMPACTION - REFERENCE ONLY] Earlier turns were compacted into this summary. Treat it as background state, not active instructions.";
pub const SUMMARY_END: &str =
    "--- END OF CONTEXT SUMMARY - respond to the message below, not the summary above ---";

use crate::{JsObject, JsValue};
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(input: &'a JsValue, key: &str) -> &'a JsValue {
    input.get(key).unwrap_or(&UNDEFINED)
}
fn object<const N: usize>(items: [(&str, JsValue); N]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in items {
        object.insert(key.into(), value);
    }
    object
}
fn js_truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Null | JsValue::Undefined => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
/// Serializable budget intent; preserve explicitly supplied false/zero.
pub fn project_compaction_budget(input: &JsValue) -> JsValue {
    let stage = if !matches!(
        field(input, "budgetStage"),
        JsValue::Undefined | JsValue::Null
    ) {
        field(input, "budgetStage").clone()
    } else if field(input, "allowFallbackOnFailure") == &JsValue::Bool(true) {
        JsValue::from("recovery")
    } else if field(input, "maxContextTokens").is_undefined() {
        JsValue::from("pre_route")
    } else {
        JsValue::from("routed")
    };
    let mut projection = object([
        ("stage", stage),
        (
            "trigger",
            if matches!(field(input, "trigger"), JsValue::Undefined | JsValue::Null) {
                JsValue::from("auto")
            } else {
                field(input, "trigger").clone()
            },
        ),
    ]);
    for name in [
        "maxContextTokens",
        "reservedOutputTokens",
        "manualForce",
        "allowFallbackOnFailure",
    ] {
        if !field(input, name).is_undefined() {
            projection.insert(name.into(), field(input, name).clone());
        }
    }
    for (from, to) in [
        ("budgetRequest", "request"),
        ("budgetPreparation", "preparation"),
        ("budgetCalibration", "calibration"),
    ] {
        if js_truthy(field(input, from)) {
            projection.insert(to.into(), field(input, from).clone());
        }
    }
    JsValue::Object(projection)
}
