use super::binding::WsBinding;
use serde_json::Value;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct State {
    pub authed: bool,
    pub client_name: Option<String>,
    pub closed: bool,
    // Insertion order is visible in handleClose's gateway calls.
    pub in_flight: Vec<(String, u64)>,
    pub interactions: Vec<String>,
    pub ticket: u64,
}
impl State {
    pub fn track_interaction(&mut self, session: &str) {
        if !session.is_empty() && !self.interactions.iter().any(|s| s == session) {
            self.interactions.push(session.to_owned());
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
        let session = params["sessionKey"].as_str().unwrap_or_default().to_owned();
        match method {
            "submit_turn" | "elicitation_respond" => {
                params["interactionBinding"] = binding.to_json();
            }
            "permission_decide" => {
                let sdk_control = self.client_name.as_deref() == Some("sdk")
                    && (session.is_empty() || !self.interactions.contains(&session));
                if !sdk_control {
                    params["interactionBinding"] = binding.to_json();
                }
            }
            "reconnect_interaction" => {
                self.track_interaction(&session);
                let mut next =
                    serde_json::json!({"sessionKey":session, "nextBinding":binding.to_json()});
                if let Some(previous) = params.get("previousBinding") {
                    next["previousBinding"] = previous.clone();
                }
                params = next;
            }
            _ => {}
        }
        params
    }
    pub fn sessions(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        self.in_flight
            .iter()
            .map(|(s, _)| s)
            .chain(self.interactions.iter())
            .filter(|s| seen.insert((*s).clone()))
            .cloned()
            .collect()
    }
}
