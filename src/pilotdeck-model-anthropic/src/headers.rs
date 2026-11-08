//! Original provider header projection; credentials are supplied by the caller.
use crate::blocks::{field, is, present};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
fn units(v: &JsValue) -> Vec<u16> {
    pilotdeck_protocol::coercion::to_string_units(v).unwrap_or_default()
}
fn js_trim(v: &str) -> &str {
    v.trim_matches(|c:char|matches!(c,'\u{0009}'..='\u{000d}'|' '|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}'))
}
pub fn provider_headers(provider: &JsValue, body: &JsValue) -> Vec<(JsString, JsValue)> {
    let mut headers = JsObject::new();
    headers.insert("content-type".into(), "application/json".into());
    if let Some(custom) = field(provider, "headers").as_object() {
        for (name, value) in custom.own_entries() {
            headers.insert(name.clone(), value.clone());
        }
    }
    let key = String::from_utf16_lossy(&units(field(provider, "apiKey")));
    let key = js_trim(&key);
    if is(field(provider, "protocol"), "anthropic") {
        headers.insert("x-api-key".into(), key.into());
        if !present(
            headers
                .get("anthropic-version")
                .unwrap_or(&JsValue::Undefined),
        ) {
            headers.insert("anthropic-version".into(), "2023-06-01".into());
        }
        if is(field(provider, "speedMapping"), "anthropic_speed")
            && is(field(body, "speed"), "fast")
        {
            append_anthropic_beta(&mut headers, "fast-mode-2026-02-01");
        }
    } else if !present(headers.get("authorization").unwrap_or(&JsValue::Undefined)) {
        headers.insert(
            "authorization".into(),
            format!("Bearer {key}").as_str().into(),
        );
    }
    headers
        .own_entries()
        .into_iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn append_anthropic_beta(headers: &mut JsObject, beta: &str) {
    let key = headers
        .own_entries()
        .into_iter()
        .map(|(k, _)| k.clone())
        .find(|k| String::from_utf16_lossy(k.units()).to_lowercase() == "anthropic-beta")
        .unwrap_or_else(|| "anthropic-beta".into());
    let current = headers
        .get_key(&key)
        .map(|v| String::from_utf16_lossy(&units(v)))
        .unwrap_or_default();
    let mut values: Vec<String> = current
        .split(',')
        .map(|v| js_trim(v).to_string())
        .filter(|v| !v.is_empty())
        .collect();
    if !values.iter().any(|v| v == beta) {
        values.push(beta.to_string());
    }
    headers.insert(key, values.join(", ").as_str().into());
}
