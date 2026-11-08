//! Session metadata when the catalog has no matching row.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&U)
}
fn is(v: &JsValue, s: &str) -> bool {
    v.as_string().is_some_and(|x| x.equals_str(s))
}
fn trim(v: &JsValue) -> Option<JsValue> {
    let units = crate::runtime::memory::js_trim_units(v.as_string()?.units());
    (!units.is_empty()).then(|| JsValue::String(JsString::from_utf16(units.to_vec())))
}
fn date(v: &JsValue) -> Option<f64> {
    let s = v.as_string()?.to_utf8().ok()?;
    chrono::DateTime::parse_from_rfc3339(&s)
        .ok()
        .map(|d| d.timestamp_millis() as f64)
}
fn nullish(v: &JsValue) -> bool {
    matches!(v, JsValue::Undefined | JsValue::Null)
}
/// An empty transcript has no synthesized metadata and falls back to the request key.
pub fn fallback(entries: &[JsValue], key: &JsString, project: &JsValue) -> JsValue {
    let mut attrs = JsObject::new();
    let mut first = JsValue::Undefined;
    let mut last = JsValue::Undefined;
    for entry in entries {
        if is(field(entry, "type"), "accepted_input") {
            if let JsValue::Array(messages) = field(entry, "messages") {
                for message in messages {
                    if let JsValue::Array(content) = field(message, "content") {
                        if let Some(v) = content
                            .iter()
                            .find(|b| is(field(b, "type"), "text"))
                            .and_then(|b| trim(field(b, "text")))
                        {
                            if first.is_undefined() {
                                first = v.clone();
                            }
                            last = v;
                        }
                    }
                }
            }
        } else if is(field(entry, "type"), "session_metadata") {
            let metadata = field(entry, "metadata");
            for (src, dst) in [
                ("title", "customTitle"),
                ("aiTitle", "aiTitle"),
                ("tag", "tag"),
                ("parentSessionId", "parentSessionId"),
                ("forkedFromTurnId", "forkedFromTurnId"),
            ] {
                let value = field(metadata, src);
                if !nullish(value) {
                    attrs.insert(dst.into(), value.clone());
                }
            }
            let value = field(metadata, "firstPrompt");
            if !nullish(value) {
                first = value.clone();
            }
            let value = field(metadata, "lastPrompt");
            if !nullish(value) {
                last = value.clone();
            }
        }
    }
    let mut session = JsObject::new();
    session.insert("sessionId".into(), JsValue::String(key.clone()));
    session.insert("sessionKey".into(), JsValue::String(key.clone()));
    let summary = [
        attrs.get("customTitle"),
        attrs.get("aiTitle"),
        Some(&last),
        Some(&first),
    ]
    .into_iter()
    .flatten()
    .find(|v| !nullish(v))
    .cloned()
    .unwrap_or_else(|| JsValue::String(key.clone()));
    session.insert("summary".into(), summary);
    session.insert(
        "lastModified".into(),
        JsValue::Number(
            entries
                .last()
                .and_then(|v| date(field(v, "createdAt")))
                .unwrap_or(0.0),
        ),
    );
    for k in ["customTitle", "aiTitle"] {
        if let Some(v) = attrs.get(k) {
            session.insert(k.into(), v.clone());
        }
    }
    if !first.is_undefined() {
        session.insert("firstPrompt".into(), first);
    }
    if !entries.is_empty() {
        session.insert("cwd".into(), project.clone());
    }
    if let Some(v) = attrs.get("tag") {
        session.insert("tag".into(), v.clone());
    }
    if let Some(time) = entries.first().and_then(|v| date(field(v, "createdAt"))) {
        session.insert("createdAt".into(), JsValue::Number(time));
    }
    for k in ["parentSessionId", "forkedFromTurnId"] {
        if let Some(v) = attrs.get(k) {
            session.insert(k.into(), v.clone());
        }
    }
    JsValue::Object(session)
}
