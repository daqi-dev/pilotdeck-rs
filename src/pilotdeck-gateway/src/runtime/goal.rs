//! Durable, session-scoped goals over the existing transcript writer/projection.
#![allow(clippy::result_large_err)]
use crate::{
    host::modules::{capability::RuntimeContext, tool_context::SessionService},
    runtime::{
        session::factory::SessionProjection,
        tools::{ToolError, ToolFuture},
    },
    storage::recorder::Recorder,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GoalState {
    pub goal: JsValue,
    pub revision: f64,
}
impl Default for GoalState {
    fn default() -> Self {
        Self {
            goal: JsValue::Null,
            revision: 0.0,
        }
    }
}

#[derive(Clone)]
pub(crate) struct GoalRuntime {
    inner: Arc<GoalSession>,
}
pub(crate) struct GoalSession {
    session: JsString,
    recorder: Arc<Recorder>,
    projection: Arc<SessionProjection>,
    mutation: Arc<tokio::sync::Mutex<()>>,
    uuid: Arc<dyn Fn() -> JsString + Send + Sync>,
}
impl GoalRuntime {
    pub fn new(
        session: JsString,
        recorder: Arc<Recorder>,
        projection: Arc<SessionProjection>,
    ) -> Self {
        Self {
            inner: Arc::new(GoalSession {
                session,
                recorder,
                projection,
                mutation: Arc::default(),
                uuid: Arc::new(new_id),
            }),
        }
    }
}
impl SessionService for GoalRuntime {
    fn for_session(&self, session: &JsString) -> Option<RuntimeContext> {
        (session == &self.inner.session).then(|| self.inner.clone() as RuntimeContext)
    }
}
impl GoalSession {
    pub fn snapshot(&self) -> Result<GoalState, ToolError> {
        self.projection
            .fold_domain(GoalState::default(), |state, entry| {
                if entry.get("type") != Some(&"goal_changed".into()) {
                    return Ok(state);
                }
                let revision = entry.get("revision").and_then(JsValue::as_number);
                if revision != Some(state.revision + 1.0) {
                    return Err(crate::runtime::project::RuntimeError::new(
                        "Goal revision must increase by one.",
                    ));
                }
                let goal = entry.get("goal").cloned().unwrap_or(JsValue::Null);
                if !matches!(goal, JsValue::Null)
                    && goal.get("revision").and_then(JsValue::as_number) != revision
                {
                    return Err(crate::runtime::project::RuntimeError::new(
                        "Goal snapshot revision must match the event revision.",
                    ));
                }
                Ok(GoalState {
                    goal,
                    revision: revision.unwrap(),
                })
            })
            .map_err(|e| ToolError::failed(e.message))
    }
    pub fn mutate(
        self: Arc<Self>,
        create: bool,
        input: JsValue,
        turn: JsString,
    ) -> ToolFuture<JsValue> {
        Box::pin(async move {
            let guard = self.mutation.clone().lock_owned().await;
            // The admitted blocking append owns its guard through durable commit,
            // including when its tool future is cancelled. No stale writer can
            // race the next CAS or release the session writer lease early.
            tokio::task::spawn_blocking(move || {
                let _guard = guard;
                let current = self.snapshot()?;
                let (goal, changed) = transition(&current, create, &input, || (self.uuid)())?;
                if changed {
                    let mut event = JsObject::new();
                    event.insert("type".into(), "goal_changed".into());
                    event.insert("goal".into(), goal.clone());
                    event.insert("revision".into(), JsValue::Number(current.revision + 1.0));
                    self.recorder
                        .writer()
                        .record_session_event_checked(&self.session, &turn, event)
                        .map_err(|e| ToolError::failed(e.to_string()))?;
                }
                Ok(goal)
            })
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?
        })
    }
}
fn trim(value: &JsString) -> JsString {
    crate::model_catalog::trim(value)
}
fn text(input: &JsValue, key: &str) -> Option<JsString> {
    input.get(key).and_then(JsValue::as_string).map(trim)
}
fn max_rounds(value: Option<&JsValue>) -> Result<Option<JsValue>, ToolError> {
    let Some(value) = value.filter(|v| !v.is_undefined()) else {
        return Ok(None);
    };
    if !value
        .as_number()
        .is_some_and(|n| (1.0..=9_007_199_254_740_991.0).contains(&n) && n.fract() == 0.0)
    {
        return Err(ToolError::failed(
            "maxGoalRounds must be a positive integer.",
        ));
    }
    Ok(Some(value.clone()))
}
fn conflict(message: String, expected: Option<JsValue>, actual: f64) -> ToolError {
    let mut details = JsObject::new();
    if let Some(expected) = expected {
        details.insert("expectedRevision".into(), expected);
    }
    details.insert("actualRevision".into(), JsValue::Number(actual));
    let mut error = ToolError::failed(message);
    error.details = Some(details);
    error
}
pub(crate) fn transition(
    current: &GoalState,
    create: bool,
    input: &JsValue,
    uuid: impl FnOnce() -> JsString,
) -> Result<(JsValue, bool), ToolError> {
    if create {
        if !matches!(current.goal, JsValue::Null) {
            return Err(conflict(
                "A goal already exists; update or clear it before creating another.".into(),
                None,
                current.revision,
            ));
        }
        let objective = text(input, "objective")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| ToolError::failed("Goal objective must not be empty."))?;
        let rounds = max_rounds(input.get("max_goal_rounds"))?;
        let id = text(input, "id")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                let mut units: Vec<u16> = "goal-".encode_utf16().collect();
                units.extend(uuid().units());
                JsString::from_utf16(units)
            });
        let mut goal = JsObject::new();
        for (key, value) in [
            ("id", JsValue::String(id)),
            ("revision", JsValue::Number(current.revision + 1.0)),
            ("objective", JsValue::String(objective)),
            ("phase", "active".into()),
        ] {
            goal.insert(key.into(), value);
        }
        if let Some(rounds) = rounds {
            goal.insert("maxGoalRounds".into(), rounds);
        }
        return Ok((JsValue::Object(goal), true));
    }
    if let Some(expected) = input.get("expected_revision").filter(|v| !v.is_undefined())
        && expected.as_number() != Some(current.revision)
    {
        return Err(conflict(
            format!(
                "Goal revision conflict: expected {}, current is {}.",
                crate::js_value::display(expected).unwrap_or_default(),
                current.revision
            ),
            Some(expected.clone()),
            current.revision,
        ));
    }
    let action = text(input, "action").unwrap_or_else(|| "edit".into());
    let Some(mut goal) = current.goal.as_object().cloned() else {
        return if action.equals_str("clear") {
            Ok((JsValue::Null, false))
        } else {
            Err(ToolError::failed("No goal exists for this session."))
        };
    };
    if action.equals_str("clear") {
        return Ok((JsValue::Null, true));
    }
    if let Some(objective) = text(input, "objective").filter(|s| !s.is_empty()) {
        goal.insert("objective".into(), JsValue::String(objective));
    }
    let phase = match action.to_utf8().as_deref() {
        Ok("pause") => Some("paused"),
        Ok("resume") => Some("active"),
        Ok("complete") => Some("complete"),
        Ok("block") => Some("blocked"),
        _ => None,
    };
    if let Some(phase) = phase {
        goal.insert("phase".into(), phase.into());
    }
    goal.insert("revision".into(), JsValue::Number(current.revision + 1.0));
    if goal.get("phase") == Some(&"blocked".into()) {
        if let Some(reason) = text(input, "blocked_reason").filter(|s| !s.is_empty()) {
            goal.insert("blockedReason".into(), JsValue::String(reason));
        }
    } else {
        goal = goal
            .entries()
            .iter()
            .filter(|(key, _)| !key.equals_str("blockedReason"))
            .fold(JsObject::new(), |mut next, (key, value)| {
                next.insert(key.clone(), value.clone());
                next
            });
    }
    let rounds = input
        .get("max_goal_rounds")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .or_else(|| goal.get("maxGoalRounds"));
    if let Some(rounds) = max_rounds(rounds)? {
        goal.insert("maxGoalRounds".into(), rounds);
    }
    Ok((JsValue::Object(goal), true))
}

fn new_id() -> JsString {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{now}-{}",
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
    )
    .into()
}
#[cfg(test)]
#[path = "goal_tests.rs"]
mod tests;
