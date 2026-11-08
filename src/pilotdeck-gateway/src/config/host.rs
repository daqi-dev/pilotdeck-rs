//! Intrinsic prototype state belongs to the JS host, shared with its other RPCs.
//! A config that can inspect or freeze those intrinsics must remain TS-owned.
use super::value::*;
use pilotdeck_protocol::JsValue;
fn selection(v: &JsValue, providers: &JsValue) -> bool {
    let Some(s) = v.as_string() else {
        return false;
    };
    let s = trim(s);
    let Some(i) = s.units().iter().position(|u| *u == 47) else {
        return false;
    };
    let provider = pilotdeck_protocol::JsString::from_utf16(s.units()[..i].to_vec());
    let model = pilotdeck_protocol::JsString::from_utf16(s.units()[i + 1..].to_vec());
    if !builtin_property(&provider) && !builtin_property(&model) {
        return false;
    }
    // Own, explicitly configured plain definitions never mutate shared
    // intrinsic functions. Missing/inherited definitions remain guarded.
    let Some(config) = providers.as_object().and_then(|p| p.get_key(&provider)) else {
        return true;
    };
    field(config, "models")
        .as_object()
        .and_then(|models| models.get_key(&model))
        .is_none_or(|value| !plain_intrinsic_value(value))
}
// The diagnostic table inherits Object.prototype's methods for these names.
// Plain JSON values do not modify those shared functions. Continue to refuse
// nested intrinsic keys until their cross-request effects are modelled.
fn plain_intrinsic_value(value: &JsValue) -> bool {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            JsValue::Object(object) => {
                if object.prototype().is_some() {
                    return false;
                }
                for (key, value) in object.entries() {
                    if builtin_property(key) {
                        return false;
                    }
                    pending.push(value);
                }
            }
            JsValue::Array(values) => pending.extend(values),
            _ => (),
        }
    }
    true
}
pub(super) fn required(raw: &JsValue) -> bool {
    let providers = field(field(raw, "model"), "providers");
    if entries(providers).iter().any(|(key, p)| {
        (builtin_property(key) && !plain_intrinsic_value(p))
            || entries(field(p, "models"))
                .iter()
                .any(|(key, value)| builtin_property(key) && !plain_intrinsic_value(value))
    }) {
        return true;
    }
    let selection = |value: &JsValue| selection(value, providers);
    let agent = field(raw, "agent");
    if selection(field(agent, "model"))
        || selection(field(field(agent, "subagents"), "default"))
        || selection(field(field(raw, "memory"), "model"))
    {
        return true;
    }
    let router = field(raw, "router");
    if selection(field(field(router, "scenarios"), "default"))
        || selection(field(field(router, "tokenSaver"), "judge"))
    {
        return true;
    }
    for (_, v) in entries(field(router, "fallback")) {
        if let JsValue::Array(a) = v
            && a.iter().any(selection)
        {
            return true;
        }
    }
    if entries(field(field(router, "tokenSaver"), "tiers"))
        .iter()
        .any(|(_, t)| selection(field(t, "model")))
    {
        return true;
    }
    let stats = field(router, "stats");
    if selection(field(stats, "baselineModel"))
        || entries(field(stats, "modelPricing"))
            .iter()
            .any(|(key, _)| selection(&JsValue::String((*key).clone())))
    {
        return true;
    }
    let baseline = field(stats, "baselineModel");
    if field(baseline, "provider")
        .as_string()
        .is_some_and(builtin_property)
        || field(baseline, "model")
            .as_string()
            .is_some_and(builtin_property)
    {
        return true;
    }
    let always = field(raw, "alwaysOn");
    let shared = |key: &pilotdeck_protocol::JsString, value: &JsValue| {
        builtin_property(key) && (key.equals_str("__proto__") || !plain_intrinsic_value(value))
    };
    entries(always)
        .iter()
        .any(|(key, value)| shared(key, value))
        || entries(field(always, "projects"))
            .iter()
            .any(|(_, project)| {
                entries(project)
                    .iter()
                    .any(|(key, value)| shared(key, value))
            })
}
