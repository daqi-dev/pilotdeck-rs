//! Shallow session overrides. Every set/get copies only the top-level object.
//! Nested objects and arrays are live handles, including permission rules,
//! excludeTools and unknown properties. The last actual owner releases them.

use crate::config::{permission::ConfiguredRuleSet, session_value::SessionValue};
use pilotdeck_protocol::JsString;
use std::collections::HashMap;

pub const UNATTENDED_SESSION_EXCLUDED_TOOLS: [&str; 3] =
    ["enter_plan_mode", "exit_plan_mode", "ask_user_question"];

#[derive(Default)]
pub struct SessionConfigOverrides {
    entries: HashMap<JsString, SessionValue>,
}

impl SessionConfigOverrides {
    pub fn new() -> Self {
        Self::default()
    }

    /// Convert wire values to SessionValue once before calling this method.
    /// Reusing that handle preserves the original nested object identity.
    pub fn set(&mut self, session_key: &JsString, value: &SessionValue) {
        self.entries.insert(session_key.clone(), value.spread());
    }

    pub fn get(&self, session_key: &JsString) -> Option<SessionValue> {
        self.entries.get(session_key).map(SessionValue::spread)
    }

    pub fn permission_rules(&self, session_key: &JsString) -> Option<ConfiguredRuleSet> {
        self.entries
            .get(session_key)?
            .get("permissionRules")
            .filter(SessionValue::is_object)
            .map(ConfiguredRuleSet::from_shared)
    }

    pub fn same_permission_rules(&self, session_key: &JsString, value: &SessionValue) -> bool {
        let Some(held) = value.get("permissionRules") else {
            return false;
        };
        self.permission_rules(session_key)
            .is_some_and(|current| current.value().same_object(&held))
    }

    pub fn delete(&mut self, session_key: &JsString) {
        self.entries.remove(session_key);
    }

    pub fn delete_prefix(&mut self, prefix: &JsString) {
        self.entries
            .retain(|key, _| !key.units().starts_with(prefix.units()));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
