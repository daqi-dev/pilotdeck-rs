//! Durable plan/todo state and the `todo_write` session tool.
use super::{object, schema, string, text};
use crate::{
    host::{
        HostError, HostFuture, ModuleError,
        modules::{
            capability::{PlanTodoHandler, ToolDefinition},
            tool_context::SessionService,
        },
    },
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

pub struct TodoWriteTool {
    fields: JsObject,
    handler: Option<Arc<dyn PlanTodoHandler>>,
}
impl TodoWriteTool {
    pub fn new() -> Self {
        Self {
            handler: None,
            fields: object([
                ("name", string("todo_write")),
                ("aliases", JsValue::Array(vec![string("TodoWrite")])),
                (
                    "description",
                    string(
                        "Read or update a lightweight durable session checklist for complex work.",
                    ),
                ),
                ("kind", string("session")),
                (
                    "inputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","additionalProperties":false,"properties":{"markdown":{"type":"string"},"todos":{"type":"array","items":{"type":"object","additionalProperties":false,"properties":{"id":{"type":"string"},"content":{"type":"string"},"status":{"type":"string","enum":["pending","in_progress","completed","cancelled"]},"priority":{"type":"string"}}}},"merge":{"type":"boolean","default":false},"reason":{"type":"string"}}}"#,
                    )),
                ),
            ]),
        }
    }
    pub fn with_plan_todo(mut self, handler: Arc<dyn PlanTodoHandler>) -> Self {
        self.handler = Some(handler);
        self
    }
}
impl Default for TodoWriteTool {
    fn default() -> Self {
        Self::new()
    }
}
impl ToolDefinition for TodoWriteTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}
impl ExecutableTool for TodoWriteTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let handler = self.handler.clone();
        Box::pin(async move {
            let handler = handler
                .or_else(|| context.host.plan_todo.as_ref().and_then(session_handler))
                .ok_or_else(|| {
                    ToolError::new(
                        "unsupported_tool",
                        "todo_write requires a session-scoped plan/todo provider.",
                    )
                })?;
            let turn = context
                .fields
                .get("turnId")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| "todo".into());
            let merge = input.get("merge") == Some(&JsValue::Bool(true));
            let reason = input
                .get("reason")
                .and_then(JsValue::as_string)
                .map(text)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty());
            let (mode, markdown, method, updates) = match input.get("todos") {
                Some(JsValue::Array(updates)) => (
                    "structured",
                    None,
                    "write_todos",
                    Some(
                        updates
                            .iter()
                            .enumerate()
                            .map(|(i, v)| JsValue::Object(normalize_update(v, i, !merge)))
                            .collect(),
                    ),
                ),
                _ if input.get("markdown").and_then(JsValue::as_string).is_some() => {
                    let markdown = input
                        .get("markdown")
                        .and_then(JsValue::as_string)
                        .unwrap()
                        .clone();
                    let parsed = parse_markdown(&text(&markdown));
                    (
                        "markdown",
                        Some(markdown),
                        "record_todo_write",
                        Some(parsed.into_iter().map(JsValue::Object).collect()),
                    )
                }
                _ => ("read", None, "read", None),
            };
            let mut payload = object([
                ("operation", string("plan_todo")),
                ("method", string(method)),
                (
                    "sessionId",
                    context
                        .fields
                        .get("sessionId")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                ("turnId", JsValue::String(turn)),
                ("merge", JsValue::Bool(merge)),
            ]);
            if let Some(updates) = updates {
                payload.insert("todos".into(), JsValue::Array(updates));
            }
            if let Some(markdown) = &markdown {
                payload.insert("markdown".into(), JsValue::String(markdown.clone()));
            }
            if let Some(reason) = &reason {
                payload.insert("reason".into(), string(reason.as_str()));
            }
            let response = handler
                .call(JsValue::Object(object([(
                    "payload",
                    JsValue::Object(payload),
                )])))
                .await
                .map_err(|error| ToolError::failed(error.to_string()))?;
            let snapshot = response
                .get("snapshot")
                .and_then(JsValue::as_object)
                .ok_or_else(|| ToolError::failed("Plan/todo handler returned no snapshot."))?;
            let todos = match snapshot.get("todos") {
                Some(JsValue::Array(todos)) => todos
                    .iter()
                    .map(|todo| {
                        todo.as_object().cloned().ok_or_else(|| {
                            ToolError::failed("Plan/todo handler returned an invalid todo.")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                _ => return Err(ToolError::failed("Plan/todo handler returned no todos.")),
            };
            let diagnostics = snapshot
                .get("todoDiagnostics")
                .and_then(JsValue::as_object)
                .cloned()
                .ok_or_else(|| ToolError::failed("Plan/todo handler returned no diagnostics."))?;
            Ok(output(mode, markdown, todos, merge, reason, diagnostics))
        })
    }
}

#[derive(Clone)]
struct TodoState {
    approved_plan: Option<JsString>,
    requires_initialization: bool,
    tool_calls_since_write: u64,
    last_markdown: Option<JsString>,
    todos: Vec<JsObject>,
    diagnostics: JsObject,
    history: Vec<JsObject>,
}
impl Default for TodoState {
    fn default() -> Self {
        Self {
            approved_plan: None,
            requires_initialization: false,
            tool_calls_since_write: 0,
            last_markdown: None,
            todos: vec![],
            diagnostics: empty_diagnostics(),
            history: vec![],
        }
    }
}

#[derive(Clone)]
pub struct TodoSession {
    path: PathBuf,
    state: Arc<Mutex<TodoState>>,
}
// Bridge existing opaque host contexts by allocation identity, without owning
// their lifetime or choosing a session from caller-supplied fields.
type TodoBindings = HashMap<usize, (Weak<TodoSession>, JsString)>;
fn todo_bindings() -> &'static Mutex<TodoBindings> {
    static BINDINGS: OnceLock<Mutex<TodoBindings>> = OnceLock::new();
    BINDINGS.get_or_init(Mutex::default)
}
fn session_handler(
    context: &crate::host::modules::capability::RuntimeContext,
) -> Option<Arc<dyn PlanTodoHandler>> {
    let key = Arc::as_ptr(context) as *const () as usize;
    let mut bindings = todo_bindings().lock().ok()?;
    let (session, session_id) = bindings.get(&key)?;
    let Some(session) = session.upgrade() else {
        bindings.remove(&key);
        return None;
    };
    Some(Arc::new(BoundTodoHandler {
        session,
        session_id: session_id.clone(),
        turn_id: None,
    }))
}

pub struct BoundTodoHandler {
    session: Arc<TodoSession>,
    session_id: JsString,
    turn_id: Option<JsString>,
}
impl PlanTodoHandler for BoundTodoHandler {
    fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        let session = self.session.clone();
        let session_id = self.session_id.clone();
        let turn_id = self.turn_id.clone();
        Box::pin(async move {
            let payload = call
                .get("payload")
                .ok_or_else(|| ModuleError::new("Missing payload."))?;
            let required = |key: &str| -> Result<JsString, HostError> {
                payload
                    .get(key)
                    .and_then(JsValue::as_string)
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .ok_or_else(|| ModuleError::new(format!("Missing {key}.")).into())
            };
            let incoming_session = required("sessionId")?;
            let turn = required("turnId")?;
            if incoming_session != session_id
                || turn_id.as_ref().is_some_and(|expected| *expected != turn)
            {
                return Err(ModuleError::new(
                    "Plan/todo capability identity does not match the active host turn.",
                )
                .into());
            }
            let method = required("method")?;
            let reason = payload.get("reason").and_then(JsValue::as_string).map(text);
            let run = || -> Result<(), ToolError> {
                match text(&method).as_str() {
                    "read" => {}
                    "mark_plan_approved" => {
                        let plan =
                            required("plan").map_err(|e| ToolError::failed(e.to_string()))?;
                        session.mark_plan_approved(&plan, &turn)?;
                    }
                    "write_todos" | "record_todo_write" => {
                        let markdown_mode = method.equals_str("record_todo_write");
                        let updates = match payload.get("todos") {
                            Some(JsValue::Array(todos)) => todos,
                            _ => {
                                return Err(ToolError::failed(if markdown_mode {
                                    "Plan/todo items must be an array."
                                } else {
                                    "Plan/todo updates must be an array."
                                }));
                            }
                        };
                        for item in updates {
                            let item = item.as_object().ok_or_else(|| {
                                ToolError::failed("Plan/todo entry must be an object.")
                            })?;
                            let status = item.get("status");
                            let valid = status.and_then(JsValue::as_string).is_some_and(|s| {
                                ["pending", "in_progress", "completed", "cancelled"]
                                    .iter()
                                    .any(|v| s.equals_str(v))
                            });
                            if markdown_mode
                                && (!valid
                                    || item
                                        .get("content")
                                        .and_then(JsValue::as_string)
                                        .is_none_or(JsString::is_empty))
                            {
                                return Err(ToolError::failed("Plan/todo item is invalid."));
                            }
                            if !markdown_mode && status.is_some_and(|s| !s.is_undefined()) && !valid
                            {
                                return Err(ToolError::failed(
                                    "Plan/todo update status is invalid.",
                                ));
                            }
                        }
                        if markdown_mode {
                            let markdown = payload
                                .get("markdown")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .ok_or_else(|| ToolError::failed("Missing markdown."))?;
                            let todos = replace_todos(
                                &updates
                                    .iter()
                                    .enumerate()
                                    .map(|(i, v)| normalize_update(v, i, true))
                                    .collect::<Vec<_>>(),
                            );
                            session.write_markdown(todos, &markdown, reason.as_deref(), &turn)?;
                        } else {
                            let merge = payload.get("merge") == Some(&JsValue::Bool(true));
                            let markdown = payload
                                .get("markdown")
                                .and_then(JsValue::as_string)
                                .cloned();
                            if markdown.is_some() {
                                session.write_structured_with_markdown(
                                    updates,
                                    merge,
                                    markdown,
                                    reason.as_deref(),
                                    &turn,
                                )?;
                            } else {
                                session.write_structured(
                                    updates,
                                    merge,
                                    reason.as_deref(),
                                    &turn,
                                )?;
                            }
                        }
                    }
                    "mark_tool_progress" => {
                        let name =
                            required("toolName").map_err(|e| ToolError::failed(e.to_string()))?;
                        session.mark_tool_progress(&name, &turn)?;
                    }
                    _ => {
                        return Err(ToolError::failed(format!(
                            "Unsupported sidecar plan/todo method: {}",
                            text(&method)
                        )));
                    }
                }
                Ok(())
            };
            run().map_err(|e| ModuleError::new(text(&e.message)))?;
            let state = session
                .snapshot()
                .map_err(|e| ModuleError::new(text(&e.message)))?;
            let mut snapshot = encode_state(&state).as_object().cloned().unwrap();
            snapshot.insert(
                "todoHistory".into(),
                JsValue::Array(
                    state
                        .history
                        .iter()
                        .filter(|entry| {
                            entry
                                .get("type")
                                .and_then(JsValue::as_string)
                                .is_some_and(|kind| kind.equals_str("plan_todo_written"))
                        })
                        .cloned()
                        .map(JsValue::Object)
                        .collect(),
                ),
            );
            snapshot.insert(
                "activeTodos".into(),
                JsValue::Array(
                    state
                        .todos
                        .iter()
                        .filter(|v| active(v))
                        .cloned()
                        .map(JsValue::Object)
                        .collect(),
                ),
            );
            Ok(object([("snapshot", JsValue::Object(snapshot))]))
        })
    }
}
impl TodoSession {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let state = if path.exists() {
            decode_state(&fs::read_to_string(&path)?)?
        } else {
            TodoState::default()
        };
        Ok(Self {
            path,
            state: Arc::new(Mutex::new(state)),
        })
    }
    fn snapshot(&self) -> Result<TodoState, ToolError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| ToolError::failed("Todo provider lock is poisoned."))?
            .clone())
    }
    pub fn mark_plan_approved(&self, plan: &JsString, turn_id: &JsString) -> Result<(), ToolError> {
        let trimmed = text(plan).trim().to_owned();
        let mut state = self
            .state
            .lock()
            .map_err(|_| ToolError::failed("Todo provider lock is poisoned."))?;
        *state = TodoState {
            approved_plan: (!trimmed.is_empty()).then(|| trimmed.clone().into()),
            requires_initialization: !trimmed.is_empty(),
            ..TodoState::default()
        };
        let approved = state.approved_plan.clone().unwrap_or_default();
        state.history.push(object([
            ("type", string("plan_todo_plan_changed")),
            ("turnId", JsValue::String(turn_id.clone())),
            ("plan", string(approved)),
        ]));
        persist(&self.path, &state)
    }
    fn write_markdown(
        &self,
        todos: Vec<JsObject>,
        markdown: &JsString,
        reason: Option<&str>,
        turn: &JsString,
    ) -> Result<(Vec<JsObject>, JsObject), ToolError> {
        self.record(
            todos,
            "markdown",
            false,
            Some(markdown.clone()),
            reason,
            turn,
        )
    }
    fn write_structured(
        &self,
        updates: &[JsValue],
        merge: bool,
        reason: Option<&str>,
        turn: &JsString,
    ) -> Result<(Vec<JsObject>, JsObject), ToolError> {
        self.write_structured_with_markdown(updates, merge, None, reason, turn)
    }
    fn write_structured_with_markdown(
        &self,
        updates: &[JsValue],
        merge: bool,
        markdown: Option<JsString>,
        reason: Option<&str>,
        turn: &JsString,
    ) -> Result<(Vec<JsObject>, JsObject), ToolError> {
        let updates = updates
            .iter()
            .enumerate()
            .map(|(index, value)| normalize_update(value, index, !merge))
            .collect::<Vec<_>>();
        let current = self.snapshot()?.todos;
        let next = if merge {
            merge_todos(&current, &updates)
        } else {
            replace_todos(&updates)
        };
        self.record(next, "structured", merge, markdown, reason, turn)
    }
    fn mark_tool_progress(&self, tool: &JsString, turn: &JsString) -> Result<(), ToolError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ToolError::failed("Todo provider lock is poisoned."))?;
        if state.approved_plan.is_none()
            || state.requires_initialization
            || tool.equals_str("todo_write")
        {
            return Ok(());
        }
        state.tool_calls_since_write = state
            .tool_calls_since_write
            .checked_add(1)
            .ok_or_else(|| ToolError::failed("Todo progress counter exhausted."))?;
        state.history.push(object([
            ("type", string("plan_todo_progressed")),
            ("turnId", JsValue::String(turn.clone())),
            ("toolName", JsValue::String(tool.clone())),
        ]));
        persist(&self.path, &state)
    }
    fn record(
        &self,
        next: Vec<JsObject>,
        mode: &str,
        merge: bool,
        markdown: Option<JsString>,
        reason: Option<&str>,
        turn: &JsString,
    ) -> Result<(Vec<JsObject>, JsObject), ToolError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ToolError::failed("Todo provider lock is poisoned."))?;
        let previous = state.todos.clone();
        let previous_ids = previous.iter().map(id).collect::<HashSet<_>>();
        let next_ids = next.iter().map(id).collect::<HashSet<_>>();
        let added = next_ids.difference(&previous_ids).count();
        let removed_items = previous
            .iter()
            .filter(|todo| !next_ids.contains(&id(todo)))
            .collect::<Vec<_>>();
        let removed = removed_items.len();
        let deleted_open = removed_items.iter().filter(|todo| active(todo)).count();
        let changed = next
            .iter()
            .filter(|todo| {
                previous
                    .iter()
                    .find(|old| id(old) == id(todo))
                    .is_some_and(|old| *old != **todo)
            })
            .count();
        let preserved = next_ids.intersection(&previous_ids).count();
        let large_rewrite =
            !previous.is_empty() && !next.is_empty() && preserved < previous.len().div_ceil(2);
        let previous_active = previous.iter().filter(|todo| active(todo)).count();
        let all_completed = !next.is_empty()
            && next
                .iter()
                .all(|todo| status(todo) == "completed" || status(todo) == "cancelled");
        state.todos = next.clone();
        state.last_markdown = markdown.clone();
        state.requires_initialization = false;
        state.tool_calls_since_write = 0;
        increment(&mut state.diagnostics, "writeCount", 1);
        if large_rewrite {
            increment(&mut state.diagnostics, "largeRewriteCount", 1);
        }
        increment(
            &mut state.diagnostics,
            "deletedOpenItemCount",
            deleted_open as u64,
        );
        if all_completed && previous_active > 0 {
            increment(&mut state.diagnostics, "completedWithoutActiveCount", 1);
        }
        set_counts(&mut state.diagnostics, &next);
        let mut last_write = object([
            ("mode", string(mode)),
            ("merge", JsValue::Bool(merge)),
            ("addedCount", JsValue::Number(added as f64)),
            ("removedCount", JsValue::Number(removed as f64)),
            ("changedCount", JsValue::Number(changed as f64)),
            ("deletedOpenItemCount", JsValue::Number(deleted_open as f64)),
            ("largeRewrite", JsValue::Bool(large_rewrite)),
            ("allCompleted", JsValue::Bool(all_completed)),
        ]);
        if let Some(reason) = reason.filter(|v| !v.trim().is_empty()) {
            last_write.insert("reason".into(), string(reason.trim()));
        }
        state
            .diagnostics
            .insert("lastWrite".into(), JsValue::Object(last_write.clone()));
        let mut entry = object([
            ("type", string("plan_todo_written")),
            ("turnId", JsValue::String(turn.clone())),
            ("mode", string(mode)),
            ("merge", JsValue::Bool(merge)),
            (
                "todos",
                JsValue::Array(next.iter().cloned().map(JsValue::Object).collect()),
            ),
            ("diagnostics", JsValue::Object(state.diagnostics.clone())),
        ]);
        if let Some(markdown) = markdown {
            entry.insert("markdown".into(), JsValue::String(markdown));
        }
        if let Some(reason) = reason.filter(|v| !v.trim().is_empty()) {
            entry.insert("reason".into(), string(reason.trim()));
        }
        state.history.push(entry);
        persist(&self.path, &state)?;
        Ok((next, state.diagnostics.clone()))
    }
}

pub struct DurableTodoService {
    root: PathBuf,
    sessions: Mutex<HashMap<Vec<u16>, Arc<TodoSession>>>,
}
impl DurableTodoService {
    pub fn open(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self {
            root,
            sessions: Mutex::new(HashMap::new()),
        })
    }
    pub fn session(&self, session_id: &JsString) -> std::io::Result<Arc<TodoSession>> {
        let key = session_id.units().to_vec();
        // Look up and insert under one lock: releasing it between the lookup
        // and the insert let two concurrent callers for one session open two
        // `TodoSession`s over the same path, so each kept its own copy of the
        // list and the later write silently dropped the other's todos.
        let mut sessions = self.sessions.lock().unwrap();
        if let Some(value) = sessions.get(&key).cloned() {
            return Ok(value);
        }
        let value = Arc::new(TodoSession::open(
            self.root.join(format!("{}.json", digest(session_id))),
        )?);
        sessions.insert(key, value.clone());
        let mut bindings = todo_bindings().lock().unwrap();
        bindings.retain(|_, (session, _)| session.strong_count() > 0);
        bindings.insert(
            Arc::as_ptr(&value) as usize,
            (Arc::downgrade(&value), session_id.clone()),
        );
        Ok(value)
    }
    pub fn handler(
        &self,
        session_id: &JsString,
        turn_id: &JsString,
    ) -> std::io::Result<Arc<dyn PlanTodoHandler>> {
        Ok(Arc::new(BoundTodoHandler {
            session: self.session(session_id)?,
            session_id: session_id.clone(),
            turn_id: Some(turn_id.clone()),
        }))
    }
}
impl SessionService for DurableTodoService {
    fn for_session(
        &self,
        session_id: &JsString,
    ) -> Option<crate::host::modules::capability::RuntimeContext> {
        self.session(session_id)
            .ok()
            .map(|value| value as crate::host::modules::capability::RuntimeContext)
    }
}

fn output(
    mode: &str,
    markdown: Option<JsString>,
    todos: Vec<JsObject>,
    merge: bool,
    reason: Option<String>,
    diagnostics: JsObject,
) -> ToolOutput {
    let mut data = object([
        (
            "todos",
            JsValue::Array(todos.iter().cloned().map(JsValue::Object).collect()),
        ),
        ("mode", string(mode)),
        ("merge", JsValue::Bool(merge)),
        ("diagnostics", JsValue::Object(diagnostics.clone())),
    ]);
    if let Some(markdown) = markdown {
        data.insert("markdown".into(), JsValue::String(markdown));
    }
    if let Some(reason) = reason.as_ref() {
        data.insert("reason".into(), string(reason.as_str()));
    }
    let mut lines = vec![
        if mode == "read" {
            "Todo list read:".into()
        } else {
            "Todo list updated:".into()
        },
        format!("mode={mode} merge={merge} count={}", todos.len()),
    ];
    if let Some(reason) = reason {
        lines.push(format!("reason: {reason}"));
    }
    if todos.is_empty() {
        lines.push("No todos are currently recorded.".into());
    } else {
        for todo in &todos {
            lines.push(format!(
                "- [{}] id={}{} {}",
                status(todo),
                id(todo),
                todo.get("priority")
                    .and_then(JsValue::as_string)
                    .map(|v| format!(" priority={}", text(v)))
                    .unwrap_or_default(),
                todo.get("content")
                    .and_then(JsValue::as_string)
                    .map(text)
                    .unwrap_or_default()
            ));
        }
    }
    lines.push(String::new());
    lines.push("Diagnostics:".into());
    lines.push(
        stringify_json(&JsValue::Object(diagnostics.clone()))
            .ok()
            .flatten()
            .unwrap_or_default(),
    );
    ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(lines.join("\n"))),
        ]))],
        data: Some(JsValue::Object(data)),
        metadata: Some(object([
            ("todoCount", JsValue::Number(todos.len() as f64)),
            ("mode", string(mode)),
            ("diagnostics", JsValue::Object(diagnostics)),
        ])),
        ..Default::default()
    }
}
fn parse_markdown(markdown: &str) -> Vec<JsObject> {
    let mut result = Vec::new();
    let mut in_progress = false;
    for line in markdown.lines() {
        let line = line.trim();
        let marker = line
            .strip_prefix("- [")
            .or_else(|| line.strip_prefix("* ["));
        let Some(marker) = marker else {
            continue;
        };
        let Some(end) = marker.find(']') else {
            continue;
        };
        let checked = matches!(&marker[..end], "x" | "X");
        let content = marker[end + 1..].trim();
        if content.is_empty() {
            continue;
        }
        let status = if checked {
            "completed"
        } else if !in_progress {
            in_progress = true;
            "in_progress"
        } else {
            "pending"
        };
        result.push(object([
            ("id", string(format!("todo-{}", result.len() + 1))),
            ("content", string(content)),
            ("status", string(status)),
        ]));
    }
    result
}
fn normalize_update(value: &JsValue, index: usize, fill_defaults: bool) -> JsObject {
    let id_value = value
        .get("id")
        .and_then(JsValue::as_string)
        .map(text)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| format!("todo-{}", index + 1));
    let content = value
        .get("content")
        .and_then(JsValue::as_string)
        .map(text)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty());
    let status_value = value
        .get("status")
        .and_then(JsValue::as_string)
        .map(text)
        .filter(|v| {
            matches!(
                v.as_str(),
                "pending" | "in_progress" | "completed" | "cancelled"
            )
        });
    let mut result = JsObject::new();
    result.insert("id".into(), string(id_value));
    if let Some(content) = content {
        result.insert("content".into(), string(content));
    } else if fill_defaults {
        result.insert("content".into(), string("(no description)"));
    }
    if let Some(status) = status_value {
        result.insert("status".into(), string(status));
    } else if fill_defaults {
        result.insert("status".into(), string("pending"));
    }
    if let Some(priority) = value
        .get("priority")
        .and_then(JsValue::as_string)
        .map(text)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
    {
        result.insert("priority".into(), string(priority));
    }
    result
}
fn replace_todos(updates: &[JsObject]) -> Vec<JsObject> {
    let mut last = HashMap::new();
    for (index, value) in updates.iter().enumerate() {
        last.insert(id(value), index);
    }
    updates
        .iter()
        .enumerate()
        .filter(|(index, value)| last.get(&id(value)) == Some(index))
        .map(|(_, value)| value.clone())
        .collect()
}
fn merge_todos(existing: &[JsObject], updates: &[JsObject]) -> Vec<JsObject> {
    let mut result = existing.to_vec();
    for update in replace_todos(updates) {
        let update_id = id(&update);
        if let Some(current) = result.iter_mut().find(|todo| id(todo) == update_id) {
            for key in ["content", "status", "priority"] {
                if let Some(value) = update.get(key) {
                    current.insert(key.into(), value.clone());
                }
            }
        } else {
            let mut normalized = update;
            if normalized.get("content").is_none() {
                normalized.insert("content".into(), string("(no description)"));
            }
            if normalized.get("status").is_none() {
                normalized.insert("status".into(), string("pending"));
            }
            result.push(normalized);
        }
    }
    result
}
fn id(todo: &JsObject) -> String {
    todo.get("id")
        .and_then(JsValue::as_string)
        .map(text)
        .unwrap_or_default()
}
fn status(todo: &JsObject) -> String {
    todo.get("status")
        .and_then(JsValue::as_string)
        .map(text)
        .unwrap_or_default()
}
fn active(todo: &JsObject) -> bool {
    matches!(status(todo).as_str(), "pending" | "in_progress")
}
fn empty_diagnostics() -> JsObject {
    object([
        ("writeCount", JsValue::Number(0.0)),
        ("todoCount", JsValue::Number(0.0)),
        ("activeCount", JsValue::Number(0.0)),
        ("completedCount", JsValue::Number(0.0)),
        ("cancelledCount", JsValue::Number(0.0)),
        ("largeRewriteCount", JsValue::Number(0.0)),
        ("deletedOpenItemCount", JsValue::Number(0.0)),
        ("completedWithoutActiveCount", JsValue::Number(0.0)),
    ])
}
fn increment(diagnostics: &mut JsObject, key: &str, amount: u64) {
    let value = diagnostics
        .get(key)
        .and_then(JsValue::as_number)
        .unwrap_or(0.0)
        + amount as f64;
    diagnostics.insert(key.into(), JsValue::Number(value));
}
fn set_counts(diagnostics: &mut JsObject, todos: &[JsObject]) {
    for (key, count) in [
        ("todoCount", todos.len()),
        ("activeCount", todos.iter().filter(|v| active(v)).count()),
        (
            "completedCount",
            todos.iter().filter(|v| status(v) == "completed").count(),
        ),
        (
            "cancelledCount",
            todos.iter().filter(|v| status(v) == "cancelled").count(),
        ),
    ] {
        diagnostics.insert(key.into(), JsValue::Number(count as f64));
    }
}
fn digest(value: &JsString) -> String {
    let mut hasher = Sha256::new();
    for unit in value.units() {
        hasher.update(unit.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}
fn encode_state(state: &TodoState) -> JsValue {
    JsValue::Object(object([
        (
            "approvedPlan",
            state
                .approved_plan
                .clone()
                .map(JsValue::String)
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "requiresInitialization",
            JsValue::Bool(state.requires_initialization),
        ),
        (
            "toolCallsSinceLastTodoWrite",
            JsValue::Number(state.tool_calls_since_write as f64),
        ),
        (
            "lastMarkdown",
            state
                .last_markdown
                .clone()
                .map(JsValue::String)
                .unwrap_or(JsValue::Undefined),
        ),
        (
            "todos",
            JsValue::Array(state.todos.iter().cloned().map(JsValue::Object).collect()),
        ),
        (
            "todoDiagnostics",
            JsValue::Object(state.diagnostics.clone()),
        ),
        (
            "todoHistory",
            JsValue::Array(state.history.iter().cloned().map(JsValue::Object).collect()),
        ),
    ]))
}
fn decode_state(raw: &str) -> std::io::Result<TodoState> {
    let value = parse_json(raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    Ok(TodoState {
        approved_plan: value
            .get("approvedPlan")
            .and_then(JsValue::as_string)
            .cloned(),
        requires_initialization: value
            .get("requiresInitialization")
            .and_then(JsValue::as_bool)
            .unwrap_or(false),
        tool_calls_since_write: value
            .get("toolCallsSinceLastTodoWrite")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0) as u64,
        last_markdown: value
            .get("lastMarkdown")
            .and_then(JsValue::as_string)
            .cloned(),
        todos: match value.get("todos") {
            Some(JsValue::Array(v)) => v.iter().filter_map(|v| v.as_object().cloned()).collect(),
            _ => vec![],
        },
        diagnostics: value
            .get("todoDiagnostics")
            .and_then(JsValue::as_object)
            .cloned()
            .unwrap_or_else(empty_diagnostics),
        history: match value.get("todoHistory") {
            Some(JsValue::Array(v)) => v.iter().filter_map(|v| v.as_object().cloned()).collect(),
            _ => vec![],
        },
    })
}
fn persist(path: &Path, state: &TodoState) -> Result<(), ToolError> {
    let raw = stringify_json(&encode_state(state))
        .map_err(|e| ToolError::failed(e.to_string()))?
        .ok_or_else(|| ToolError::failed("Todo state is not serializable."))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ToolError::failed(e.to_string()))?;
    }
    let temp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
    fs::write(&temp, raw)
        .and_then(|_| fs::rename(&temp, path))
        .map_err(|e| ToolError::failed(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};
    fn array(value: &JsValue) -> &[JsValue] {
        if let JsValue::Array(values) = value {
            values
        } else {
            panic!("expected array")
        }
    }
    fn call(method: &str, extra: &[(&str, JsValue)]) -> JsValue {
        let mut payload = object([
            ("operation", string("plan_todo")),
            ("method", string(method)),
            ("sessionId", string("s")),
            ("turnId", string("t")),
        ]);
        for (key, value) in extra {
            payload.insert((*key).into(), value.clone());
        }
        JsValue::Object(object([("payload", JsValue::Object(payload))]))
    }
    fn context(
        plan_todo: Option<crate::host::modules::capability::RuntimeContext>,
    ) -> ToolRunContext {
        use crate::host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext};
        ToolRunContext::new(Arc::new(ToolRuntimeContext {
            fields: object([("sessionId", string("s")), ("turnId", string("t"))]),
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: None,
            progress: None,
            subagent: None,
            plan_todo,
            goal: None,
            plan_directory: None,
        }))
    }
    #[tokio::test]
    async fn bound_plan_handler_mutates_real_state_and_checks_turn_identity() {
        let service = DurableTodoService::open(root()).unwrap();
        let handler = service.handler(&"s".into(), &"t".into()).unwrap();
        let approved = handler
            .call(call("mark_plan_approved", &[("plan", string(" work "))]))
            .await
            .unwrap();
        let snapshot = approved.get("snapshot").unwrap();
        assert_eq!(snapshot.get("approvedPlan"), Some(&string("work")));
        assert_eq!(
            snapshot.get("requiresInitialization"),
            Some(&JsValue::Bool(true))
        );
        assert_eq!(
            snapshot.get("todoHistory"),
            Some(&JsValue::Array(Vec::new()))
        );
        let updates =
            parse_json(r#"[{"id":"a","content":"first","status":"in_progress"}]"#).unwrap();
        let written = handler
            .call(call(
                "write_todos",
                &[
                    ("todos", updates),
                    ("reason", string(" init ")),
                    ("markdown", string("raw")),
                ],
            ))
            .await
            .unwrap();
        assert_eq!(
            written.get("snapshot").unwrap().get("lastMarkdown"),
            Some(&string("raw"))
        );
        handler
            .call(call(
                "mark_tool_progress",
                &[("toolName", string("read_file"))],
            ))
            .await
            .unwrap();
        handler
            .call(call(
                "mark_tool_progress",
                &[("toolName", string("todo_write"))],
            ))
            .await
            .unwrap();
        let read = handler.call(call("read", &[])).await.unwrap();
        let snapshot = read.get("snapshot").unwrap();
        assert_eq!(
            snapshot.get("toolCallsSinceLastTodoWrite"),
            Some(&JsValue::Number(1.0))
        );
        assert_eq!(array(snapshot.get("todoHistory").unwrap()).len(), 1);
        let wrong = call(
            "write_todos",
            &[
                ("todos", JsValue::Array(Vec::new())),
                ("turnId", string("other")),
            ],
        );
        assert!(
            handler
                .call(wrong)
                .await
                .unwrap_err()
                .to_string()
                .contains("identity does not match")
        );
        let invalid = parse_json(r#"[{"content":"bad","status":"invalid"}]"#).unwrap();
        assert!(
            handler
                .call(call("write_todos", &[("todos", invalid)]))
                .await
                .is_err()
        );
        assert_eq!(
            service
                .session(&"s".into())
                .unwrap()
                .snapshot()
                .unwrap()
                .todos
                .len(),
            1
        );
    }
    struct Probe {
        handler: Arc<dyn PlanTodoHandler>,
        methods: Arc<Mutex<Vec<JsString>>>,
    }
    impl PlanTodoHandler for Probe {
        fn call(&self, call: JsValue) -> HostFuture<JsObject> {
            self.methods.lock().unwrap().push(
                call.get("payload")
                    .unwrap()
                    .get("method")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .clone(),
            );
            self.handler.call(call)
        }
    }
    #[tokio::test]
    async fn todo_tool_uses_handler_for_structured_markdown_and_read_with_same_output() {
        let service = DurableTodoService::open(root()).unwrap();
        let methods = Arc::new(Mutex::new(Vec::new()));
        let tool = TodoWriteTool::new().with_plan_todo(Arc::new(Probe {
            handler: service.handler(&"s".into(), &"t".into()).unwrap(),
            methods: methods.clone(),
        }));
        let written = tool.execute(parse_json(r#"{"todos":[{"id":"a","content":" first ","status":"in_progress"}],"reason":" init "}"#).unwrap(), context(None)).await.unwrap();
        let data = written.data.unwrap();
        assert_eq!(data.get("mode"), Some(&string("structured")));
        assert_eq!(data.get("reason"), Some(&string("init")));
        assert_eq!(
            data.get("diagnostics").unwrap().get("writeCount"),
            Some(&JsValue::Number(1.0))
        );
        assert_eq!(
            written.metadata.unwrap().get("todoCount"),
            Some(&JsValue::Number(1.0))
        );
        let markdown = tool
            .execute(
                parse_json(r#"{"markdown":"- [x] done\n- [ ] next","merge":true}"#).unwrap(),
                context(None),
            )
            .await
            .unwrap();
        assert_eq!(
            markdown.data.as_ref().unwrap().get("mode"),
            Some(&string("markdown"))
        );
        assert_eq!(
            markdown.data.as_ref().unwrap().get("merge"),
            Some(&JsValue::Bool(true))
        );
        assert!(
            text(
                markdown.content[0]
                    .get("text")
                    .unwrap()
                    .as_string()
                    .unwrap()
            )
            .contains("mode=markdown merge=true count=2")
        );
        let read = tool
            .execute(JsValue::Object(JsObject::new()), context(None))
            .await
            .unwrap();
        assert_eq!(
            read.data.as_ref().unwrap().get("todos"),
            markdown.data.as_ref().unwrap().get("todos")
        );
        assert_eq!(
            *methods.lock().unwrap(),
            ["write_todos", "record_todo_write", "read"].map(JsString::from)
        );
        let empty = tool
            .execute(
                JsValue::Object(object([("markdown", string(""))])),
                context(None),
            )
            .await
            .unwrap();
        assert_eq!(empty.data.unwrap().get("markdown"), Some(&string("")));
    }
    #[tokio::test]
    async fn existing_session_context_routes_to_same_handler_without_any_downcast() {
        let service = DurableTodoService::open(root()).unwrap();
        let opaque = service.for_session(&"s".into()).unwrap();
        let tool = TodoWriteTool::new();
        tool.execute(
            parse_json(r#"{"todos":[{"content":"kept"}]}"#).unwrap(),
            context(Some(opaque.clone())),
        )
        .await
        .unwrap();
        let handler = service.handler(&"s".into(), &"t".into()).unwrap();
        let read = handler.call(call("read", &[])).await.unwrap();
        assert_eq!(
            array(read.get("snapshot").unwrap().get("todos").unwrap()).len(),
            1
        );
        let mut wrong = context(Some(opaque));
        wrong.fields.insert("sessionId".into(), string("other"));
        assert!(
            tool.execute(JsValue::Object(JsObject::new()), wrong)
                .await
                .is_err()
        );
        let unrelated: crate::host::modules::capability::RuntimeContext = Arc::new(42_u64);
        assert_eq!(
            tool.execute(JsValue::Object(JsObject::new()), context(Some(unrelated)))
                .await
                .unwrap_err()
                .code,
            "unsupported_tool"
        );
    }
    fn root() -> PathBuf {
        std::env::temp_dir().join(format!(
            "pdrs-todo-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    #[test]
    fn markdown_merge_diagnostics_and_restart() {
        let root = root();
        let service = DurableTodoService::open(&root).unwrap();
        let session = service.session(&"s".into()).unwrap();
        let (todos, _) = session
            .write_markdown(
                parse_markdown("- [ ] first\n- [ ] second\n- [x] done"),
                &"list".into(),
                Some("start"),
                &"t1".into(),
            )
            .unwrap();
        assert_eq!(status(&todos[0]), "in_progress");
        let parsed = parse_json(r#"[{"id":"todo-1","status":"completed"},{"id":"new","content":"third","priority":"high"}]"#).unwrap();
        let updates = match &parsed {
            JsValue::Array(v) => v,
            _ => unreachable!(),
        };
        let (todos, diagnostics) = session
            .write_structured(&updates, true, Some("advance"), &"t2".into())
            .unwrap();
        assert_eq!(todos.len(), 4);
        assert_eq!(diagnostics.get("writeCount"), Some(&JsValue::Number(2.0)));
        drop(service);
        assert_eq!(
            DurableTodoService::open(root)
                .unwrap()
                .session(&"s".into())
                .unwrap()
                .snapshot()
                .unwrap()
                .todos
                .len(),
            4
        );
    }

    /// Two threads asking for one session must receive the same instance. The
    /// old split lock released between the lookup and the insert, so both
    /// threads could open their own `TodoSession` over one path and one write
    /// would silently overwrite the other.
    #[test]
    fn concurrent_session_lookup_returns_one_instance() {
        let root = root();
        let service = Arc::new(DurableTodoService::open(&root).unwrap());
        let (started, starting) = std::sync::mpsc::channel();
        let handles = (0..8)
            .map(|_| {
                let service = Arc::clone(&service);
                let started = started.clone();
                std::thread::spawn(move || {
                    started.send(()).unwrap();
                    service.session(&"s".into()).unwrap()
                })
            })
            .collect::<Vec<_>>();
        for _ in 0..8 {
            starting.recv().unwrap();
        }
        let sessions = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        for session in &sessions[1..] {
            assert!(
                Arc::ptr_eq(&sessions[0], session),
                "every caller must share one TodoSession"
            );
        }
    }
}
