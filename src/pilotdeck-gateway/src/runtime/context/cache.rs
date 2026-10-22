//! Session-scoped prompt-cache plans (`context/cache/{CachePlan,PromptCacheCoordinator}.ts`).
use super::prepare::{object, stable_serialize};
use crate::{
    host::{HostError, ModuleError},
    js_value::StackOverflow,
    project_files::{node_collator, node_locale},
};
use icu_locale_core::Locale;
use pilotdeck_protocol::{JsString, JsValue};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
};

pub struct CachePlanInput<'a> {
    pub provider: &'a JsValue,
    pub model: &'a JsValue,
    pub system_prompt: &'a str,
    pub tools: &'a [JsValue],
    pub messages: &'a [JsValue],
    pub enabled: bool,
}
#[derive(Default)]
struct State {
    sessions: HashMap<Option<JsString>, (String, u64)>,
    resets: HashSet<Option<JsString>>,
}
pub struct PromptCacheCoordinator {
    state: Mutex<State>,
    locale: Locale,
}
impl Default for PromptCacheCoordinator {
    fn default() -> Self {
        Self::new(node_locale(|key| std::env::var(key).ok()))
    }
}
impl PromptCacheCoordinator {
    pub fn new(locale: Locale) -> Self {
        Self {
            state: Mutex::new(State::default()),
            locale,
        }
    }
    pub fn reset(&self, session_id: &JsString) {
        if !session_id.is_empty() {
            self.state
                .lock()
                .unwrap()
                .resets
                .insert(Some(session_id.clone()));
        }
    }
    pub fn release(&self, session_id: &JsString) {
        if session_id.is_empty() {
            return;
        }
        let mut state = self.state.lock().unwrap();
        state.sessions.remove(&Some(session_id.clone()));
        state.resets.remove(&Some(session_id.clone()));
    }
    pub fn create_plan(
        &self,
        session_id: Option<JsString>,
        input: CachePlanInput<'_>,
        commit: bool,
    ) -> Result<Option<JsValue>, HostError> {
        // Disabled committed calls still consume a pending reset; enabled
        // calls compute the fingerprint before mutating coordinator state.
        if !input.enabled {
            if commit {
                self.state.lock().unwrap().resets.remove(&session_id);
            }
            return Ok(None);
        }
        let mut messages: Vec<usize> = input
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                !m.get("role")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str("system"))
            })
            .map(|(i, _)| i)
            .rev()
            .take(3)
            .collect();
        messages.reverse();
        let collator = node_collator(&self.locale).map_err(|e| ModuleError::new(e.1))?;
        // Precompute fallible serialization outside sort's infallible comparator.
        let mut tools_with_keys = input
            .tools
            .iter()
            .cloned()
            .map(|tool| {
                let name = tool
                    .get("name")
                    .and_then(JsValue::as_string)
                    .map(|s| String::from_utf16_lossy(s.units()))
                    .unwrap_or_default();
                let key = stable_serialize(&tool)
                    .map_err(|_| ModuleError::new(StackOverflow::MESSAGE))?;
                Ok((tool, name, key))
            })
            .collect::<Result<Vec<_>, HostError>>()?;
        tools_with_keys.sort_by(|a, b| {
            collator
                .compare(&a.1, &b.1)
                .then_with(|| collator.compare(&a.2, &b.2))
        });
        let nullish = |v: &JsValue| {
            if matches!(v, JsValue::Undefined | JsValue::Null) {
                JsValue::String("".into())
            } else {
                v.clone()
            }
        };
        let identity = object(vec![
            ("provider", nullish(input.provider)),
            ("model", nullish(input.model)),
            ("system", JsValue::String(input.system_prompt.into())),
            (
                "tools",
                JsValue::Array(
                    tools_with_keys
                        .into_iter()
                        .map(|(tool, _, _)| tool)
                        .collect(),
                ),
            ),
            (
                "messages",
                JsValue::Array(
                    messages
                        .iter()
                        .map(|i| input.messages[*i].clone())
                        .collect(),
                ),
            ),
        ]);
        let serialized =
            stable_serialize(&identity).map_err(|_| ModuleError::new(StackOverflow::MESSAGE))?;
        let fingerprint = format!("{:x}", Sha256::digest(serialized.as_bytes()));
        let mut state = self.state.lock().unwrap();
        let force_reset = if commit {
            state.resets.remove(&session_id)
        } else {
            state.resets.contains(&session_id)
        };
        let previous = state.sessions.get(&session_id);
        let generation = if force_reset || previous.is_none_or(|(old, _)| old != &fingerprint) {
            previous.map_or(1, |(_, generation)| generation + 1)
        } else {
            previous.unwrap().1
        };
        if commit {
            state
                .sessions
                .insert(session_id, (fingerprint.clone(), generation));
        }
        Ok(Some(object(vec![
            ("provider", input.provider.clone()),
            ("model", input.model.clone()),
            ("system", JsValue::Bool(!input.system_prompt.is_empty())),
            ("tools", JsValue::Bool(false)),
            (
                "messages",
                JsValue::Array(
                    messages
                        .into_iter()
                        .map(|i| JsValue::Number(i as f64))
                        .collect(),
                ),
            ),
            ("fingerprint", JsValue::String(fingerprint.into())),
            ("generation", JsValue::Number(generation as f64)),
        ])))
    }
}
