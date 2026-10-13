//! Pure fixed-Source request transforms; no ports, handles or mutable owners.
//! b243 AgentLoop.ts 2473–2526 and 2707–2728.
use crate::ports::{PortError, PortResult};
use crate::request_materialization::{BudgetCandidateInput, PreparedWindowInput, RequestCapsInput};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsObject, JsString, JsValue};

fn array_index(key: &JsString) -> Option<u32> {
    let text = key.to_utf8().ok()?;
    let index = text.parse::<u32>().ok()?;
    (index != u32::MAX && index.to_string() == text).then_some(index)
}
fn spread(value: &JsValue) -> JsObject {
    let mut out = JsObject::new();
    match value {
        JsValue::Object(object) => {
            let mut entries: Vec<_> = object.entries().iter().collect();
            entries.sort_by_key(|(key, _)| array_index(key).map_or((1, 0), |index| (0, index)));
            for (key, value) in entries {
                out.insert(key.clone(), value.clone());
            }
        }
        JsValue::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                out.insert(index.to_string().into(), value.clone());
            }
        }
        JsValue::String(value) => {
            for (index, unit) in value.units().iter().enumerate() {
                out.insert(
                    index.to_string().into(),
                    JsValue::String(JsString::from_utf16(vec![*unit])),
                );
            }
        }
        // Source object spread skips nullish and primitive wrappers without own
        // enumerable keys. Native getters/prototypes/functions are not JsValue.
        _ => (),
    }
    out
}
fn property(value: &JsValue, key: &str) -> PortResult<JsValue> {
    match value {
        JsValue::Null | JsValue::Undefined => Err(PortError::SourceException {
            message: format!(
                "Cannot read properties of {} (reading '{key}')",
                if matches!(value, JsValue::Null) {
                    "null"
                } else {
                    "undefined"
                }
            )
            .into(),
            boundary: BoundarySnapshot::capture(value),
        }),
        // No selected Source field is a primitive wrapper's built-in property.
        _ => Ok(value.get(key).cloned().unwrap_or(JsValue::Undefined)),
    }
}
fn capture(object: JsObject) -> BoundarySnapshot {
    BoundarySnapshot::capture(&JsValue::Object(object))
}

pub fn budget_candidate(input: BudgetCandidateInput<'_>) -> PortResult<BoundarySnapshot> {
    let mut candidate = spread(input.base_request.value());
    // Spread base first, then read/assign only the five preview-owned fields.
    // Absent preview fields become OWN Undefined, not retained base values.
    for key in [
        "messages",
        "systemPrompt",
        "tools",
        "cacheBreakpoints",
        "cachePlan",
    ] {
        candidate.insert(key.into(), property(input.preview_request.value(), key)?);
    }
    Ok(capture(candidate))
}

pub fn default_prepared_window(input: PreparedWindowInput<'_>) -> PortResult<BoundarySnapshot> {
    // Source first spreads candidate into routedCandidate. Nullish candidates
    // therefore legitimately become an object, not a property-read TypeError.
    let mut routed = spread(input.candidate_request.value());
    routed.insert(
        "provider".into(),
        JsValue::String(input.route.provider.clone()),
    );
    routed.insert("model".into(), JsValue::String(input.route.model.clone()));
    let mut prepared = spread(input.prepared_request.value());
    for key in ["messages", "cacheBreakpoints", "cachePlan"] {
        prepared.insert(
            key.into(),
            routed.get(key).cloned().unwrap_or(JsValue::Undefined),
        );
    }
    prepared.insert(
        "provider".into(),
        JsValue::String(input.route.provider.clone()),
    );
    prepared.insert("model".into(), JsValue::String(input.route.model.clone()));
    // Prepared systemPrompt/tools/cap/cache/provider extensions remain intact;
    // the Source replaces cachePlan itself but retains unrelated cache controls.
    Ok(capture(prepared))
}
fn js_min(left: f64, right: f64) -> f64 {
    // Rust f64::min skips NaN, unlike Math.min. Its signed-zero decision also
    // cannot be delegated to argument order. Source normal current caps are
    // positive finite floors, but the transform never silently alters numbers.
    if left.is_nan() || right.is_nan() {
        f64::NAN
    } else if left == 0.0 && right == 0.0 {
        if left.is_sign_negative() || right.is_sign_negative() {
            -0.0
        } else {
            0.0
        }
    } else {
        left.min(right)
    }
}
pub fn apply_caps(input: RequestCapsInput<'_>) -> PortResult<BoundarySnapshot> {
    // Source reads request.maxOutputTokens before spreading; nullish request
    // fails here, unlike nullish prepared.request/candidate spreads above.
    let prepared_cap = property(input.request.value(), "maxOutputTokens")?;
    let effective = match (&prepared_cap, input.current_max_output_tokens) {
        (JsValue::Number(prepared), Some(current))
            if !input.attempt_override_present && prepared.is_finite() && *prepared > 0.0 =>
        {
            JsValue::Number(js_min(current, prepared.floor()))
        }
        (_, Some(current)) => JsValue::Number(current),
        (_, None) => prepared_cap,
    };
    let mut request = spread(input.request.value());
    request.insert(
        "provider".into(),
        JsValue::String(input.route.provider.clone()),
    );
    request.insert("model".into(), JsValue::String(input.route.model.clone()));
    // Omitted assignment does not delete an existing OWN Undefined cap from
    // spread(request); only JSON.stringify later omits that key.
    if !effective.is_undefined() {
        request.insert("maxOutputTokens".into(), effective);
    }
    Ok(capture(request))
}
