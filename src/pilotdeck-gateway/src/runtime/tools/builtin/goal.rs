//! Durable session goal lifecycle (`get_goal`, `create_goal`, `update_goal`).
use super::{object, schema, string, text};
use crate::{
    host::{
        HostError,
        modules::{capability::ToolDefinition, tool_context::SessionService},
    },
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Copy)]
enum Operation {
    Get,
    Create,
    Update,
}

pub struct GoalTool {
    operation: Operation,
    fields: JsObject,
}

impl GoalTool {
    pub fn get() -> Self {
        Self::new(Operation::Get)
    }
    pub fn create() -> Self {
        Self::new(Operation::Create)
    }
    pub fn update() -> Self {
        Self::new(Operation::Update)
    }
    fn new(operation: Operation) -> Self {
        let (name, description, input_schema) = match operation {
            Operation::Get => (
                "get_goal",
                "Read the current durable goal for this session.",
                r#"{"type":"object","properties":{},"additionalProperties":false}"#,
            ),
            Operation::Create => (
                "create_goal",
                "Create the session's durable goal. Only one goal can exist at a time.",
                r#"{"type":"object","required":["objective"],"additionalProperties":false,"properties":{"objective":{"type":"string"},"max_goal_rounds":{"type":"number"},"id":{"type":"string"}}}"#,
            ),
            Operation::Update => (
                "update_goal",
                "Edit or transition the session's durable goal using an optional expected revision for CAS safety.",
                r#"{"type":"object","additionalProperties":false,"properties":{"action":{"type":"string","enum":["edit","pause","resume","complete","block","clear"]},"expected_revision":{"type":"number"},"objective":{"type":"string"},"blocked_reason":{"type":"string"},"max_goal_rounds":{"type":"number"}}}"#,
            ),
        };
        Self {
            operation,
            fields: object([
                ("name", string(name)),
                ("description", string(description)),
                ("kind", string("session")),
                ("inputSchema", JsValue::Object(schema(input_schema))),
            ]),
        }
    }
}

impl ToolDefinition for GoalTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        let read_only = match self.operation {
            Operation::Get => true,
            Operation::Create => false,
            Operation::Update => {
                input
                    .get("action")
                    .and_then(JsValue::as_string)
                    .is_some_and(|v| v.equals_str("edit"))
                    && input.get("objective").is_none()
                    && input.get("blocked_reason").is_none()
                    && input.get("max_goal_rounds").is_none()
            }
        };
        Ok(JsValue::Bool(read_only))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(matches!(self.operation, Operation::Get)))
    }
}

impl ExecutableTool for GoalTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let operation = self.operation;
        Box::pin(async move {
            let session = context
                .host
                .goal
                .as_ref()
                .and_then(|value| value.downcast_ref::<GoalSession>())
                .ok_or_else(|| {
                    ToolError::new(
                        "unsupported_tool",
                        "Goal tools require a session-scoped goal provider.",
                    )
                })?;
            let turn_id = context
                .fields
                .get("turnId")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| "goal".into());
            let goal = match operation {
                Operation::Get => session.snapshot()?.goal,
                Operation::Create => session.create(&input, &turn_id)?,
                Operation::Update => session.update(&input, &turn_id)?,
            };
            Ok(goal_output(
                goal,
                match operation {
                    Operation::Get => "get",
                    Operation::Create => "create",
                    Operation::Update => "update",
                },
            ))
        })
    }
}

#[derive(Clone)]
struct GoalState {
    goal: Option<JsObject>,
    revision: u64,
    last_turn_id: Option<JsString>,
}

impl Default for GoalState {
    fn default() -> Self {
        Self {
            goal: None,
            revision: 0,
            last_turn_id: None,
        }
    }
}

#[derive(Clone)]
pub struct GoalSession {
    path: PathBuf,
    state: Arc<Mutex<GoalState>>,
}

impl GoalSession {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let state = if path.exists() {
            decode_state(&fs::read_to_string(&path)?)?
        } else {
            GoalState::default()
        };
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
        })
    }
    fn snapshot(&self) -> Result<GoalState, ToolError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ToolError::failed("Goal provider lock is poisoned."))?
            .clone())
    }
    fn create(&self, input: &JsValue, turn_id: &JsString) -> Result<Option<JsObject>, ToolError> {
        let objective = required_trimmed(input, "objective", "Goal objective must not be empty.")?;
        let max_rounds = positive_integer(
            input.get("max_goal_rounds"),
            "maxGoalRounds must be a positive integer.",
        )?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ToolError::failed("Goal provider lock is poisoned."))?;
        if state.goal.is_some() {
            return Err(conflict(
                "A goal already exists; update or clear it before creating another.",
                None,
                state.revision,
            ));
        }
        state.revision += 1;
        let mut goal = object([
            (
                "id",
                string(
                    input
                        .get("id")
                        .and_then(JsValue::as_string)
                        .map(text)
                        .map(|v| v.trim().to_owned())
                        .filter(|v| !v.is_empty())
                        .unwrap_or_else(|| format!("goal-{}", uuid::Uuid::new_v4())),
                ),
            ),
            ("revision", JsValue::Number(state.revision as f64)),
            ("objective", string(objective)),
            ("phase", string("active")),
        ]);
        if let Some(rounds) = max_rounds {
            goal.insert("maxGoalRounds".into(), JsValue::Number(rounds as f64));
        }
        state.goal = Some(goal.clone());
        state.last_turn_id = Some(turn_id.clone());
        persist(&self.path, &state)?;
        Ok(Some(goal))
    }
    fn update(&self, input: &JsValue, turn_id: &JsString) -> Result<Option<JsObject>, ToolError> {
        let expected = nonnegative_integer(
            input.get("expected_revision"),
            "expected_revision must be a non-negative integer.",
        )?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ToolError::failed("Goal provider lock is poisoned."))?;
        if expected.is_some_and(|value| value != state.revision) {
            return Err(conflict(
                &format!(
                    "Goal revision conflict: expected {}, current is {}.",
                    expected.unwrap(),
                    state.revision
                ),
                expected,
                state.revision,
            ));
        }
        let action = input
            .get("action")
            .and_then(JsValue::as_string)
            .map(text)
            .unwrap_or_else(|| "edit".into());
        if action == "clear" {
            if state.goal.is_none() {
                return Ok(None);
            }
            state.revision += 1;
            state.goal = None;
            state.last_turn_id = Some(turn_id.clone());
            persist(&self.path, &state)?;
            return Ok(None);
        }
        let Some(current) = state.goal.clone() else {
            return Err(ToolError::failed("No goal exists for this session."));
        };
        let objective = input
            .get("objective")
            .and_then(JsValue::as_string)
            .map(text)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
            .or_else(|| {
                current
                    .get("objective")
                    .and_then(JsValue::as_string)
                    .map(text)
            })
            .ok_or_else(|| ToolError::failed("Goal objective must not be empty."))?;
        let phase = match action.as_str() {
            "pause" => "paused".to_owned(),
            "resume" => "active".to_owned(),
            "complete" => "complete".to_owned(),
            "block" => "blocked".to_owned(),
            "edit" => current
                .get("phase")
                .and_then(JsValue::as_string)
                .map(text)
                .unwrap_or_else(|| "active".into()),
            _ => return Err(ToolError::new("invalid_tool_input", "Unknown goal action.")),
        };
        let max_rounds = match positive_integer(
            input.get("max_goal_rounds"),
            "maxGoalRounds must be a positive integer.",
        )? {
            Some(value) => Some(value),
            None => current
                .get("maxGoalRounds")
                .and_then(JsValue::as_number)
                .map(|v| v as u64),
        };
        state.revision += 1;
        let mut goal = object([
            (
                "id",
                current.get("id").cloned().unwrap_or(JsValue::Undefined),
            ),
            ("revision", JsValue::Number(state.revision as f64)),
            ("objective", string(objective)),
            ("phase", string(phase.clone())),
        ]);
        if phase == "blocked" {
            if let Some(reason) = input
                .get("blocked_reason")
                .and_then(JsValue::as_string)
                .map(text)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
            {
                goal.insert("blockedReason".into(), string(reason));
            } else if let Some(reason) = current.get("blockedReason") {
                goal.insert("blockedReason".into(), reason.clone());
            }
        }
        if let Some(rounds) = max_rounds {
            goal.insert("maxGoalRounds".into(), JsValue::Number(rounds as f64));
        }
        state.goal = Some(goal.clone());
        state.last_turn_id = Some(turn_id.clone());
        persist(&self.path, &state)?;
        Ok(Some(goal))
    }
}

pub struct DurableGoalService {
    root: PathBuf,
    sessions: Mutex<HashMap<Vec<u16>, Arc<GoalSession>>>,
}

impl DurableGoalService {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            sessions: Mutex::new(HashMap::new()),
        })
    }
    pub fn session(&self, session_id: &JsString) -> std::io::Result<Arc<GoalSession>> {
        let key = session_id.units().to_vec();
        if let Some(session) = self.sessions.lock().unwrap().get(&key).cloned() {
            return Ok(session);
        }
        let session = Arc::new(GoalSession::open(
            self.root.join(format!("{}.json", digest(session_id))),
        )?);
        self.sessions.lock().unwrap().insert(key, session.clone());
        Ok(session)
    }
}

impl SessionService for DurableGoalService {
    fn for_session(
        &self,
        session_id: &JsString,
    ) -> Option<crate::host::modules::capability::RuntimeContext> {
        self.session(session_id)
            .ok()
            .map(|value| value as crate::host::modules::capability::RuntimeContext)
    }
}

fn goal_output(goal: Option<JsObject>, operation: &str) -> ToolOutput {
    let value = goal.map(JsValue::Object).unwrap_or(JsValue::Null);
    let revision = value.get("revision").cloned().unwrap_or(JsValue::Null);
    ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("json")),
            ("value", value.clone()),
        ]))],
        data: Some(value),
        metadata: Some(object([
            ("operation", string(operation)),
            ("revision", revision),
        ])),
        ..Default::default()
    }
}

fn required_trimmed(input: &JsValue, key: &str, message: &str) -> Result<String, ToolError> {
    input
        .get(key)
        .and_then(JsValue::as_string)
        .map(text)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ToolError::new("invalid_tool_input", message))
}
fn positive_integer(value: Option<&JsValue>, message: &str) -> Result<Option<u64>, ToolError> {
    integer(value, 1, message)
}
fn nonnegative_integer(value: Option<&JsValue>, message: &str) -> Result<Option<u64>, ToolError> {
    integer(value, 0, message)
}
fn integer(value: Option<&JsValue>, minimum: u64, message: &str) -> Result<Option<u64>, ToolError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(number) = value.as_number() else {
        return Err(ToolError::new("invalid_tool_input", message));
    };
    if !number.is_finite()
        || number.fract() != 0.0
        || number < minimum as f64
        || number > 9_007_199_254_740_991.0
    {
        return Err(ToolError::new("invalid_tool_input", message));
    }
    Ok(Some(number as u64))
}
fn conflict(message: &str, expected: Option<u64>, actual: u64) -> ToolError {
    let mut details = object([
        (
            "expectedRevision",
            expected
                .map(|v| JsValue::Number(v as f64))
                .unwrap_or(JsValue::Undefined),
        ),
        ("actualRevision", JsValue::Number(actual as f64)),
    ]);
    if expected.is_none() {
        details.insert("expectedRevision".into(), JsValue::Undefined);
    }
    ToolError {
        code: "tool_execution_failed".into(),
        message: message.into(),
        details: Some(details),
    }
}
fn digest(value: &JsString) -> String {
    let mut hasher = Sha256::new();
    for unit in value.units() {
        hasher.update(unit.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}
fn encode_state(state: &GoalState) -> JsValue {
    let mut value = object([
        ("revision", JsValue::Number(state.revision as f64)),
        (
            "goal",
            state
                .goal
                .clone()
                .map(JsValue::Object)
                .unwrap_or(JsValue::Null),
        ),
    ]);
    if let Some(turn_id) = &state.last_turn_id {
        value.insert("lastTurnId".into(), JsValue::String(turn_id.clone()));
    }
    JsValue::Object(value)
}
fn decode_state(raw: &str) -> std::io::Result<GoalState> {
    let value = parse_json(raw)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?;
    let revision = value
        .get("revision")
        .and_then(JsValue::as_number)
        .filter(|v| v.is_finite() && v.fract() == 0.0 && *v >= 0.0)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid goal revision")
        })? as u64;
    let goal = match value.get("goal") {
        Some(JsValue::Null) => None,
        Some(JsValue::Object(goal)) => Some(goal.clone()),
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid goal snapshot",
            ));
        }
    };
    Ok(GoalState {
        goal,
        revision,
        last_turn_id: value
            .get("lastTurnId")
            .and_then(JsValue::as_string)
            .cloned(),
    })
}
fn persist(path: &Path, state: &GoalState) -> Result<(), ToolError> {
    let text = stringify_json(&encode_state(state))
        .map_err(|error| ToolError::failed(error.to_string()))?
        .ok_or_else(|| ToolError::failed("Goal state is not serializable."))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| ToolError::failed(error.to_string()))?;
    }
    let temporary = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
    fs::write(&temporary, text)
        .and_then(|_| fs::rename(&temporary, path))
        .map_err(|error| ToolError::failed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "pdrs-goal-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    #[test]
    fn lifecycle_cas_and_restart() {
        let root = root();
        let service = DurableGoalService::open(&root).unwrap();
        let session = service.session(&"s".into()).unwrap();
        assert!(session.snapshot().unwrap().goal.is_none());
        let created = session
            .create(
                &parse_json(r#"{"objective":" ship ","max_goal_rounds":3}"#).unwrap(),
                &"t1".into(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(created.get("objective"), Some(&string("ship")));
        assert!(
            session
                .create(
                    &parse_json(r#"{"objective":"again"}"#).unwrap(),
                    &"t2".into()
                )
                .is_err()
        );
        let blocked = session
            .update(
                &parse_json(
                    r#"{"action":"block","expected_revision":1,"blocked_reason":"waiting"}"#,
                )
                .unwrap(),
                &"t2".into(),
            )
            .unwrap()
            .unwrap();
        assert_eq!(blocked.get("phase"), Some(&string("blocked")));
        let error = session
            .update(
                &parse_json(r#"{"action":"resume","expected_revision":1}"#).unwrap(),
                &"t3".into(),
            )
            .unwrap_err();
        assert_eq!(
            error.details.unwrap().get("actualRevision"),
            Some(&JsValue::Number(2.0))
        );
        drop(service);
        let reopened = DurableGoalService::open(root)
            .unwrap()
            .session(&"s".into())
            .unwrap();
        assert_eq!(reopened.snapshot().unwrap().revision, 2);
        assert_eq!(
            reopened
                .update(
                    &parse_json(r#"{"action":"clear","expected_revision":2}"#).unwrap(),
                    &"t4".into()
                )
                .unwrap(),
            None
        );
    }
}
