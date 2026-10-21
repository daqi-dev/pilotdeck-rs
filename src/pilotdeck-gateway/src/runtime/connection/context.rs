use super::binding::WsBinding;
use serde_json::Value;
use std::sync::Arc;

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
    pub fn inject(&mut self, method: &str, mut params: Value, binding: &WsBinding) -> Value {
        if !matches!(
            method,
            "submit_turn" | "elicitation_respond" | "permission_decide" | "reconnect_interaction"
        ) {
            return params;
        }
        if !params.is_object() {
            params = serde_json::json!({});
        }
        let session = SessionKey::new(params["sessionKey"].clone());
        match method {
            "submit_turn" | "elicitation_respond" => {
                params["interactionBinding"] = binding.to_json();
            }
            "permission_decide" => {
                let sdk_control = self.client_name.as_deref() == Some("sdk")
                    && (session.value().as_str().is_none_or(str::is_empty)
                        || !self.interactions.contains(&session));
                if !sdk_control {
                    params["interactionBinding"] = binding.to_json();
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
                params = next;
            }
            _ => {}
        }
        params
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
