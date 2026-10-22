use super::binding::WsBinding;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use serde_json::Value;
use std::sync::Arc;

pub(super) fn js_value(value: &Value) -> JsValue {
    parse_json(&value.to_string()).expect("serde JSON is valid JS JSON")
}

// JSON scalars use JS SameValueZero; arrays/objects retain request identity.
// Cloning a key for cleanup/close must preserve that identity.
#[derive(Clone, Debug)]
pub(super) struct SessionKey(Arc<JsValue>);
impl SessionKey {
    pub fn new(value: JsValue) -> Self {
        Self(Arc::new(value))
    }
    pub fn value(&self) -> &JsValue {
        &self.0
    }
}
impl PartialEq for SessionKey {
    fn eq(&self, other: &Self) -> bool {
        match (self.value(), other.value()) {
            (JsValue::Array(_) | JsValue::Object(_), _) => Arc::ptr_eq(&self.0, &other.0),
            // SameValueZero: NaN equals NaN, +0 equals -0.
            (JsValue::Number(a), JsValue::Number(b)) => a == b || (a.is_nan() && b.is_nan()),
            (JsValue::String(a), JsValue::String(b)) => a == b,
            (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
            (JsValue::Null, JsValue::Null) => true,
            (JsValue::Undefined, JsValue::Undefined) => true,
            _ => false,
        }
    }
}
pub(super) fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}
#[derive(Default)]
pub(super) struct State {
    pub authed: bool,
    // JS strings are UTF-16 code units; a lone surrogate clientName must remain
    // comparable with "sdk" without a lossy UTF-8 conversion.
    pub client_name: Option<JsString>,
    // Insertion-ordered session sets, matching TS Set (including its
    // unconditional session deletion when any concurrent submit completes).
    pub in_flight: Vec<SessionKey>,
    pub interactions: Vec<SessionKey>,
}
impl State {
    pub fn track_in_flight(&mut self, session: &SessionKey) {
        if !self.in_flight.iter().any(|s| s == session) {
            self.in_flight.push(session.clone());
        }
    }
    pub fn track_interaction(&mut self, session: &SessionKey) {
        if truthy(session.value()) && !self.interactions.iter().any(|s| s == session) {
            self.interactions.push(session.clone());
        }
    }
    pub fn inject(&mut self, method: &JsString, params: JsValue, binding: &WsBinding) -> JsValue {
        if ![
            "submit_turn",
            "elicitation_respond",
            "permission_decide",
            "reconnect_interaction",
        ]
        .iter()
        .any(|name| method.equals_str(name))
        {
            return params;
        }
        let session_value = params.get("sessionKey").cloned().unwrap_or(JsValue::Null);
        let session = SessionKey::new(session_value);
        // JS object spread enumerates UTF-16 code units, including surrogate
        // halves. Keep those in JsValue all the way to the gateway dispatch.
        let mut fields = match &params {
            JsValue::Object(object) => object.clone(),
            JsValue::String(text) => {
                let mut fields = JsObject::new();
                for (index, unit) in text.units().iter().copied().enumerate() {
                    fields.insert(
                        index.to_string().into(),
                        JsValue::String(JsString::from_utf16(vec![unit])),
                    );
                }
                fields
            }
            JsValue::Array(items) => {
                let mut fields = JsObject::new();
                for (index, item) in items.iter().enumerate() {
                    fields.insert(index.to_string().into(), item.clone());
                }
                fields
            }
            _ => JsObject::new(),
        };
        if method.equals_str("submit_turn") || method.equals_str("elicitation_respond") {
            fields.insert("interactionBinding".into(), js_value(&binding.to_json()));
        } else if method.equals_str("permission_decide") {
            let sdk_control = self
                .client_name
                .as_ref()
                .is_some_and(|name| name.equals_str("sdk"))
                && (session.value().as_string().is_none_or(JsString::is_empty)
                    || !self.interactions.contains(&session));
            if !sdk_control {
                fields.insert("interactionBinding".into(), js_value(&binding.to_json()));
            }
        } else {
            self.track_interaction(&session);
            // TS forwards `params.sessionKey ?? ""`: null and undefined become
            // the empty string; any other value, including a lone surrogate,
            // passes through untouched.
            let forwarded = match session.value() {
                JsValue::Null | JsValue::Undefined => JsValue::from(""),
                value => value.clone(),
            };
            let mut next = JsObject::new();
            next.insert("sessionKey".into(), forwarded);
            next.insert("nextBinding".into(), js_value(&binding.to_json()));
            // Forward previousBinding when the key is present, even if null.
            if let JsValue::Object(params) = &params
                && params.contains_key("previousBinding")
            {
                next.insert(
                    "previousBinding".into(),
                    params.get("previousBinding").cloned().unwrap(),
                );
            }
            return JsValue::Object(next);
        }
        JsValue::Object(fields)
    }
    pub fn sessions(&self) -> Vec<SessionKey> {
        let mut sessions = self.in_flight.clone();
        for session in &self.interactions {
            if !sessions.contains(session) {
                sessions.push(session.clone());
            }
        }
        sessions
    }
}
