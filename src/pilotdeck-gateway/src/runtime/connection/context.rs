use super::binding::WsBinding;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use serde_json::Value;
use std::sync::Arc;

fn js_value(value: &Value) -> JsValue {
    parse_json(&value.to_string()).expect("serde JSON is valid JS JSON")
}

// JSON scalars use JS SameValueZero; arrays/objects retain request identity.
// Cloning a key for cleanup/close must preserve that identity.
#[derive(Clone, Debug)]
pub(super) struct SessionKey(Arc<Value>);
impl SessionKey {
    pub fn new(value: Value) -> Self {
        Self(Arc::new(value))
    }
    pub fn value(&self) -> &Value {
        &self.0
    }
}
impl PartialEq for SessionKey {
    fn eq(&self, other: &Self) -> bool {
        match (self.value(), other.value()) {
            (Value::Array(_) | Value::Object(_), _) => Arc::ptr_eq(&self.0, &other.0),
            (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
            (a, b) => a == b,
        }
    }
}
pub(super) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}
#[derive(Default)]
pub(super) struct State {
    pub authed: bool,
    pub client_name: Option<String>,
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
    pub fn inject(&mut self, method: &str, params: Value, binding: &WsBinding) -> JsValue {
        if !matches!(
            method,
            "submit_turn" | "elicitation_respond" | "permission_decide" | "reconnect_interaction"
        ) {
            return js_value(&params);
        }
        let session = SessionKey::new(params["sessionKey"].clone());
        // JS object spread enumerates UTF-16 code units, including surrogate
        // halves. Keep those in JsValue all the way to the gateway dispatch.
        let mut fields = match &params {
            Value::Object(_) => js_value(&params).as_object().unwrap().clone(),
            Value::String(text) => {
                let mut fields = JsObject::new();
                for (index, unit) in text.encode_utf16().enumerate() {
                    fields.insert(
                        index.to_string().into(),
                        JsValue::String(JsString::from_utf16(vec![unit])),
                    );
                }
                fields
            }
            Value::Array(items) => {
                let mut fields = JsObject::new();
                for (index, item) in items.iter().enumerate() {
                    fields.insert(index.to_string().into(), js_value(item));
                }
                fields
            }
            _ => JsObject::new(),
        };
        match method {
            "submit_turn" | "elicitation_respond" => {
                fields.insert("interactionBinding".into(), js_value(&binding.to_json()));
            }
            "permission_decide" => {
                let sdk_control = self.client_name.as_deref() == Some("sdk")
                    && (session.value().as_str().is_none_or(str::is_empty)
                        || !self.interactions.contains(&session));
                if !sdk_control {
                    fields.insert("interactionBinding".into(), js_value(&binding.to_json()));
                }
            }
            "reconnect_interaction" => {
                self.track_interaction(&session);
                let forwarded = if session.value().is_null() {
                    serde_json::json!("")
                } else {
                    session.value().clone()
                };
                let mut next =
                    serde_json::json!({"sessionKey":forwarded, "nextBinding":binding.to_json()});
                if let Some(previous) = params.get("previousBinding") {
                    next["previousBinding"] = previous.clone();
                }
                return js_value(&next);
            }
            _ => {}
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
