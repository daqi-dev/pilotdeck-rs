//! Session-scoped prompt-cache plans (`context/cache/{CachePlan,PromptCacheCoordinator}.ts`).
use super::prepare::{object, stable_serialize};
use crate::{
    host::{HostError, ModuleError},
    js_value::{StackOverflow, to_string_units},
    project_files::{node_collator, node_locale},
};
use icu_locale_core::Locale;
use pilotdeck_protocol::{JsString, JsValue};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    sync::Mutex,
};

/// `CachePlan.ts:buildCachePlan`'s fingerprint core. TS has exactly one helper
/// and two callers (`PromptCacheCoordinator.createPlan` and
/// `modelRequestAssembly.ts`); the same must hold here, so this one function
/// serves both [`PromptCacheCoordinator::create_plan`] and host-side request
/// finalization (`host::modules::context::request`).
///
/// The tool sort keeps the TS failure modes of
/// `left.name.localeCompare(right.name)`: only the comparator's left operand
/// needs a string `name` (the right is `String()`-coerced), a single tool
/// never reaches the comparator, and a `null`/`undefined` tool element fails
/// at the `name` read itself.
pub(crate) struct CachePlanIdentity {
    /// `selectRecentMessageBreakpoints`: the last 3 non-system indexes.
    pub breakpoints: Vec<usize>,
    pub fingerprint: String,
}
pub(crate) fn cache_plan_identity(
    locale: &Locale,
    provider: &JsValue,
    model: &JsValue,
    system: &JsValue,
    tools: &[JsValue],
    messages: &[JsValue],
) -> Result<CachePlanIdentity, HostError> {
    let mut breakpoints: Vec<usize> = messages
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
    breakpoints.reverse();
    let mut tools = tools.to_vec();
    if tools.len() > 1 {
        let collator = node_collator(locale).map_err(|e| ModuleError::new(e.1))?;
        // `tool.name` throws on a nullish tool before any coercion; any other
        // non-object reads as `undefined`, like JS property access.
        let tool_name = |tool: &JsValue| -> Result<JsValue, HostError> {
            match tool {
                JsValue::Undefined | JsValue::Null => Err(ModuleError::new(format!(
                    "Cannot read properties of {} (reading 'name')",
                    if tool.is_undefined() {
                        "undefined"
                    } else {
                        "null"
                    }
                ))
                .into()),
                _ => Ok(tool.get("name").cloned().unwrap_or(JsValue::Undefined)),
            }
        };
        let compare = |left: &JsValue, right: &JsValue| -> Result<Ordering, HostError> {
            let raw = tool_name(left)?;
            let left_name = raw.as_string().cloned().ok_or_else(|| {
                ModuleError::new(match &raw {
                    JsValue::Undefined => {
                        "Cannot read properties of undefined (reading 'localeCompare')"
                    }
                    JsValue::Null => "Cannot read properties of null (reading 'localeCompare')",
                    _ => "left.name.localeCompare is not a function",
                })
            })?;
            let right_name = to_string_units(&tool_name(right)?)
                .map_err(|_| ModuleError::new(StackOverflow::MESSAGE))?;
            let order = collator.compare_utf16(left_name.units(), &right_name);
            if order != Ordering::Equal {
                return Ok(order);
            }
            // Tie-break on the stable serialization, compared as strings.
            let left =
                stable_serialize(left).map_err(|_| ModuleError::new(StackOverflow::MESSAGE))?;
            let right =
                stable_serialize(right).map_err(|_| ModuleError::new(StackOverflow::MESSAGE))?;
            Ok(collator.compare(&left, &right))
        };
        // The comparator must stay infallible for `sort_by`, so the first
        // TS-shaped failure is captured and surfaced after the sort.
        let mut failure = None;
        tools.sort_by(|a, b| {
            if failure.is_some() {
                return Ordering::Equal;
            }
            match compare(a, b) {
                Ok(order) => order,
                Err(e) => {
                    failure = Some(e);
                    Ordering::Equal
                }
            }
        });
        if let Some(e) = failure {
            return Err(e);
        }
    }
    let nullish = |v: &JsValue| {
        if matches!(v, JsValue::Undefined | JsValue::Null) {
            JsValue::String("".into())
        } else {
            v.clone()
        }
    };
    let identity = object(vec![
        ("provider", nullish(provider)),
        ("model", nullish(model)),
        ("system", nullish(system)),
        ("tools", JsValue::Array(tools)),
        (
            "messages",
            JsValue::Array(breakpoints.iter().map(|i| messages[*i].clone()).collect()),
        ),
    ]);
    let serialized =
        stable_serialize(&identity).map_err(|_| ModuleError::new(StackOverflow::MESSAGE))?;
    Ok(CachePlanIdentity {
        breakpoints,
        fingerprint: format!("{:x}", Sha256::digest(serialized.as_bytes())),
    })
}

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
        let CachePlanIdentity {
            breakpoints,
            fingerprint,
        } = cache_plan_identity(
            &self.locale,
            input.provider,
            input.model,
            &JsValue::String(input.system_prompt.into()),
            input.tools,
            input.messages,
        )?;
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
                    breakpoints
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
