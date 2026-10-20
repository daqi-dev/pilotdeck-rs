//! `task_*` builtin tools (baseline `tool/builtin/taskTools.ts`): the public
//! surface of [`crate::runtime::background::BackgroundTaskRuntime`]. The
//! runtime is injected once at construction; without one every tool fails
//! with `unsupported_tool`, as in the baseline.
//!
//! Values the baseline interpolates straight from the input (`taskId`,
//! `offset`, ...) go through `String(value)` (`js_value::display`), and the
//! task_list command preview is cut on UTF-16 units before
//! `JSON.stringify`.
#![cfg(unix)]
use super::{
    bash::{AbortSignal, spawn_env},
    cwd, field, object, schema, string,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    js_value,
    runtime::{
        background::{
            BackgroundBashTask, BackgroundTaskError, BackgroundTaskRuntime, OutputSlice,
            StartTaskSpec, TaskListFilter, TaskStatus, exit_code_value, iso,
        },
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

const DEFAULT_TASK_WAIT_TIMEOUT_MS: f64 = 600_000.0;
const MAX_TASK_WAIT_TIMEOUT_MS: f64 = 600_000.0;
const UNSUPPORTED: &str = "task_* tools require a background-task provider. Configure one via createBuiltinRegistry({ backgroundTasks: { runtime } }).";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Create,
    List,
    Output,
    Wait,
    Stop,
}

/// One of the five task tools.
pub struct TaskTool {
    kind: Kind,
    fields: JsObject,
    runtime: Option<BackgroundTaskRuntime>,
}

/// `createTaskTools({ runtime })` in the baseline's order.
pub fn task_tools(runtime: Option<BackgroundTaskRuntime>) -> Vec<Arc<dyn ExecutableTool>> {
    [
        Kind::Create,
        Kind::List,
        Kind::Output,
        Kind::Wait,
        Kind::Stop,
    ]
    .into_iter()
    .map(|kind| {
        Arc::new(TaskTool {
            kind,
            fields: definition(kind),
            runtime: runtime.clone(),
        }) as Arc<dyn ExecutableTool>
    })
    .collect()
}

const TASK_ID_PROPERTY: &str =
    r#""taskId":{"type":"string","description":"The task id returned by task_create."}"#;

fn definition(kind: Kind) -> JsObject {
    let (name, alias, description, input_schema, max_result_bytes) = match kind {
        Kind::Create => (
            "task_create",
            "TaskCreate",
            "Spawn a shell command as a detached background task. Returns immediately with a taskId; it does not automatically push completion output back into the model context. For long-running tasks that should finish, call task_wait immediately after task_create so the final output returns to the current context. Use task_output only for progress checks, not manual sleep polling. Use task_stop for long-lived services/watchers.",
            r#"{"type":"object","required":["command"],"additionalProperties":false,"properties":{"command":{"type":"string","description":"Shell command to run as a detached background task."},"agentId":{"type":"string","description":"Optional agent id to associate this task with."},"kind":{"type":"string","enum":["bash","monitor"],"description":"Task kind: 'bash' (default) or 'monitor'."}}}"#.to_string(),
            None,
        ),
        Kind::List => (
            "task_list",
            "TaskList",
            "List background tasks (optionally filter by agentId / status / kind).",
            r#"{"type":"object","additionalProperties":false,"properties":{"agentId":{"type":"string","description":"Filter tasks by agent id."},"status":{"type":["string","array"],"description":"Filter by status (e.g. 'running', 'completed'). String or array of strings."},"kind":{"type":"string","enum":["bash","monitor"],"description":"Filter by task kind."}}}"#.to_string(),
            None,
        ),
        Kind::Output => (
            "task_output",
            "TaskOutput",
            "Read newly-produced output for a background task (incremental progress polling). Use task_wait when you need to block until a finite task completes. Use nextOffset for the next read. Stop polling when status is completed, failed, or cancelled and nextOffset >= totalBytes.",
            format!(
                r#"{{"type":"object","required":["taskId"],"additionalProperties":false,"properties":{{{TASK_ID_PROPERTY},"offset":{{"type":"integer","description":"Byte offset to start reading from (for incremental polling). Defaults to 0."}},"maxBytes":{{"type":"integer","description":"Maximum bytes to return in this read. Defaults to tool limit."}}}}}}"#
            ),
            Some(200_000.0),
        ),
        Kind::Wait => (
            "task_wait",
            "TaskWait",
            "Block until a background task finishes or timeoutMs elapses, then return the task status and output. Use this immediately after task_create for finite long-running commands such as builds, tests, conversions, downloads, or batch scripts. It does not stop the task when timeoutMs elapses; call task_wait again or task_output for progress.",
            format!(
                r#"{{"type":"object","required":["taskId"],"additionalProperties":false,"properties":{{{TASK_ID_PROPERTY},"timeoutMs":{{"type":"integer","description":"Maximum time to block in milliseconds. Defaults to 600000. Max 600000."}},"offset":{{"type":"integer","description":"Byte offset to start reading output from after waiting. Defaults to 0."}},"maxBytes":{{"type":"integer","description":"Maximum bytes to return in this read. Defaults to tool limit."}}}}}}"#
            ),
            Some(200_000.0),
        ),
        Kind::Stop => (
            "task_stop",
            "TaskStop",
            "Stop a background task (SIGTERM → grace → SIGKILL).",
            r#"{"type":"object","required":["taskId"],"additionalProperties":false,"properties":{"taskId":{"type":"string","description":"The task id to stop."},"graceMs":{"type":"integer","description":"Grace period in ms between SIGTERM and SIGKILL. Defaults to 5000."}}}"#.to_string(),
            None,
        ),
    };
    let mut fields = object([
        ("name", string(name)),
        ("aliases", JsValue::Array(vec![string(alias)])),
        ("description", string(description)),
        ("kind", string("shell")),
        ("inputSchema", JsValue::Object(schema(&input_schema))),
    ]);
    if let Some(max) = max_result_bytes {
        fields.insert("maxResultBytes".into(), JsValue::Number(max));
    }
    fields
}

impl ToolDefinition for TaskTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(!matches!(
            self.kind,
            Kind::Create | Kind::Stop
        )))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}

/// `{ path, code: "invalid_schema", message }`.
fn issue(path: &str, message: &str) -> JsObject {
    object([
        ("path", string(path)),
        ("code", string("invalid_schema")),
        ("message", string(message)),
    ])
}

/// `a > b` / `a < b` for a JS value against a number (`ToNumber`, NaN false).
fn compare(value: &JsValue, limit: f64, greater: bool) -> Result<bool, ToolError> {
    let number = js_value::to_number(value).map_err(|_| stack_overflow())?;
    Ok(if greater {
        number > limit
    } else {
        number < limit
    })
}

fn stack_overflow() -> ToolError {
    ToolError::failed(js_value::StackOverflow::MESSAGE)
}

fn display(value: &JsValue) -> Result<String, ToolError> {
    js_value::display(value).map_err(|_| stack_overflow())
}

impl ExecutableTool for TaskTool {
    fn validate_input(
        &self,
        input: JsValue,
        _context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        let kind = self.kind;
        Box::pin(async move {
            if kind != Kind::Wait {
                return Ok(None);
            }
            let timeout = field(&input, "timeoutMs");
            let offset = field(&input, "offset");
            if !timeout.is_undefined() && compare(timeout, MAX_TASK_WAIT_TIMEOUT_MS, true)? {
                return Ok(Some(vec![issue(
                    "timeoutMs",
                    "timeoutMs must be <= 600000.",
                )]));
            }
            if !timeout.is_undefined() && compare(timeout, 0.0, false)? {
                return Ok(Some(vec![issue(
                    "timeoutMs",
                    "timeoutMs must be non-negative.",
                )]));
            }
            if !offset.is_undefined() && compare(offset, 0.0, false)? {
                return Ok(Some(vec![issue("offset", "offset must be non-negative.")]));
            }
            Ok(None)
        })
    }

    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let kind = self.kind;
        let runtime = self.runtime.clone();
        Box::pin(async move {
            let runtime = runtime.ok_or_else(|| ToolError::new("unsupported_tool", UNSUPPORTED))?;
            match kind {
                Kind::Create => create(&runtime, &input, &context).await,
                Kind::List => list(&runtime, &input, &context),
                Kind::Output => output(&runtime, &input, &context),
                Kind::Wait => wait(&runtime, &input, &context).await,
                Kind::Stop => stop(&runtime, &input, &context).await,
            }
        })
    }
}

fn text_output(text: &str, data: JsObject) -> ToolOutput {
    ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(text)),
        ]))],
        data: Some(JsValue::Object(data)),
        metadata: None,
        supplemental_messages: None,
    }
}

fn runtime_error(error: BackgroundTaskError) -> ToolError {
    ToolError::failed(error.message)
}

/// `{ sessionId: context.sessionId }`.
fn access(context: &ToolRunContext) -> JsValue {
    context
        .fields
        .get("sessionId")
        .cloned()
        .unwrap_or(JsValue::Undefined)
}

fn unknown_task(task_id: &JsValue) -> Result<ToolError, ToolError> {
    Ok(ToolError::new(
        "invalid_tool_input",
        format!("Unknown taskId: {}", display(task_id)?).as_str(),
    ))
}

/// The runtime keys tasks by string id; any other input value matches none.
fn task_key(task_id: &JsValue) -> JsString {
    task_id.as_string().cloned().unwrap_or_else(|| {
        // A key no UUID can equal.
        JsString::from_utf16(vec![0xD800, 0])
    })
}

fn pid_value(task: &BackgroundBashTask) -> JsValue {
    task.pid
        .map_or(JsValue::Undefined, |pid| JsValue::Number(f64::from(pid)))
}

async fn create(
    runtime: &BackgroundTaskRuntime,
    input: &JsValue,
    context: &ToolRunContext,
) -> Result<ToolOutput, ToolError> {
    let task = runtime
        .start(StartTaskSpec {
            command: field(input, "command").clone(),
            cwd: cwd(context),
            env: spawn_env(context)?,
            session_id: access(context),
            agent_id: field(input, "agentId").clone(),
            kind: field(input, "kind").clone(),
        })
        .await
        .map_err(runtime_error)?;
    let text = format!(
        "task_create taskId={} status={}",
        String::from_utf16_lossy(task.task_id.units()),
        task.status.as_str()
    );
    Ok(text_output(
        &text,
        object([
            ("taskId", JsValue::String(task.task_id.clone())),
            ("status", string(task.status.as_str())),
            ("pid", pid_value(&task)),
        ]),
    ))
}

fn list(
    runtime: &BackgroundTaskRuntime,
    input: &JsValue,
    context: &ToolRunContext,
) -> Result<ToolOutput, ToolError> {
    let session = access(context);
    let tasks = runtime.list(
        &TaskListFilter {
            agent_id: field(input, "agentId").clone(),
            status: field(input, "status").clone(),
            kind: field(input, "kind").clone(),
        },
        Some(&session),
    );
    let mut lines = vec![format!("task_list count={}", tasks.len())];
    if tasks.is_empty() {
        lines.push("No background tasks matched the filter.".into());
    }
    let mut projected = Vec::new();
    for task in &tasks {
        let exit_code = exit_code_value(task.exit_code);
        let exit_text = match &exit_code {
            JsValue::Number(code) => js_value::number_string(*code),
            _ => "null".into(),
        };
        let pid = task
            .pid
            .map_or_else(|| "null".into(), |pid| pid.to_string());
        // `task.command.length > 160 ? ... : task.command`
        let command = match &task.command {
            JsValue::String(command) if command.units().len() > 160 => {
                let mut units = command.units()[..157].to_vec();
                units.extend("...".encode_utf16());
                JsValue::String(JsString::from_utf16(units))
            }
            JsValue::Undefined => {
                return Err(ToolError::failed(
                    "Cannot read properties of undefined (reading 'length')",
                ));
            }
            JsValue::Null => {
                return Err(ToolError::failed(
                    "Cannot read properties of null (reading 'length')",
                ));
            }
            other => other.clone(),
        };
        let command_json = js_value::json_stringify(&command)
            .map_err(|_| stack_overflow())?
            .unwrap_or_else(|| "undefined".into());
        lines.push(format!(
            "- taskId={} status={} kind={} pid={pid} exitCode={exit_text} outputBytes={} interrupted={} command={command_json}",
            String::from_utf16_lossy(task.task_id.units()),
            task.status.as_str(),
            display(&task.kind)?,
            task.output_bytes,
            task.interrupted,
        ));
        if !task.status.is_terminal() || task.output_bytes > 0 {
            let next = if task.status == TaskStatus::Unknown {
                "inspect persisted output only; task_wait and task_stop are unavailable".into()
            } else {
                format!(
                    "use task_output({{ taskId: \"{}\", offset: 0 }}) to inspect output, or task_wait for a finite running task",
                    String::from_utf16_lossy(task.task_id.units())
                )
            };
            lines.push(format!("  next: {next}."));
        }
        projected.push(JsValue::Object(object([
            ("taskId", JsValue::String(task.task_id.clone())),
            ("agentId", task.agent_id.clone()),
            ("kind", task.kind.clone()),
            ("command", task.command.clone()),
            ("status", string(task.status.as_str())),
            ("pid", pid_value(task)),
            (
                "exitCode",
                match exit_code {
                    JsValue::Null => JsValue::Undefined,
                    other => other,
                },
            ),
            ("interrupted", JsValue::Bool(task.interrupted)),
            ("outputBytes", JsValue::Number(task.output_bytes as f64)),
            ("startedAt", JsValue::String(iso(task.started_at))),
            (
                "endedAt",
                task.ended_at
                    .map_or(JsValue::Undefined, |ended| JsValue::String(iso(ended))),
            ),
        ])));
    }
    Ok(text_output(
        &lines.join("\n"),
        object([("tasks", JsValue::Array(projected))]),
    ))
}

/// `TaskOutputResult` / the shared part of `TaskWaitResult`.
fn output_data(task_id: &JsValue, slice: &OutputSlice, task: &BackgroundBashTask) -> JsObject {
    object([
        ("taskId", task_id.clone()),
        ("content", string(slice.content.as_str())),
        ("nextOffset", JsValue::Number(slice.next_offset)),
        ("totalBytes", JsValue::Number(slice.total_bytes)),
        ("truncated", JsValue::Bool(slice.truncated)),
        ("status", string(task.status.as_str())),
        ("exitCode", exit_code_value(task.exit_code)),
    ])
}

struct Header {
    task_id: String,
    status: TaskStatus,
    offset: String,
    next_offset: String,
    total_bytes: String,
    truncated: bool,
    exit_code: String,
    has_new_output: bool,
    finished: bool,
}

impl Header {
    fn new(
        task_id: &JsValue,
        slice: &OutputSlice,
        task: &BackgroundBashTask,
        requested_offset: &JsValue,
    ) -> Result<Self, ToolError> {
        Ok(Self {
            task_id: display(task_id)?,
            status: task.status,
            offset: display(requested_offset)?,
            next_offset: js_value::number_string(slice.next_offset),
            total_bytes: js_value::number_string(slice.total_bytes),
            truncated: slice.truncated,
            exit_code: match exit_code_value(task.exit_code) {
                JsValue::Number(code) => js_value::number_string(code),
                _ => "null".into(),
            },
            has_new_output: !slice.content.is_empty(),
            finished: task.status.is_terminal() && slice.next_offset >= slice.total_bytes,
        })
    }
    fn render(&self, tool: &str, waited: Option<f64>) -> String {
        let waited = waited.map_or(String::new(), |ms| {
            format!(" waitedMs={}", js_value::number_string(ms))
        });
        format!(
            "{tool} taskId={} status={}{waited} offset={} nextOffset={} totalBytes={} truncated={} exitCode={}",
            self.task_id,
            self.status.as_str(),
            self.offset,
            self.next_offset,
            self.total_bytes,
            self.truncated,
            self.exit_code
        )
    }
}

const UNKNOWN_STATE: &str = "Task state is unknown after runtime recovery; it cannot be waited or stopped. Inspect persisted output only.";

fn requested_offset(input: &JsValue) -> JsValue {
    match field(input, "offset") {
        JsValue::Undefined | JsValue::Null => JsValue::Number(0.0),
        value => value.clone(),
    }
}

fn output(
    runtime: &BackgroundTaskRuntime,
    input: &JsValue,
    context: &ToolRunContext,
) -> Result<ToolOutput, ToolError> {
    let session = access(context);
    let task_id = field(input, "taskId");
    let key = task_key(task_id);
    let Some(task) = runtime.get(&key, Some(&session)) else {
        return Err(unknown_task(task_id)?);
    };
    let offset = requested_offset(input);
    let slice = runtime
        .get_output(&key, &offset, field(input, "maxBytes"), Some(&session))
        .map_err(runtime_error)?;
    let header = Header::new(task_id, &slice, &task, &offset)?;
    let head = header.render("task_output", None);
    let text = if task.status == TaskStatus::Unknown {
        let body = if header.has_new_output {
            format!("\n{}", slice.content)
        } else {
            String::new()
        };
        format!("{head}{body}\n{UNKNOWN_STATE}")
    } else if !header.has_new_output {
        let message = if header.finished {
            format!(
                "No new output. Task is {} and polling is finished.",
                task.status.as_str()
            )
        } else {
            format!(
                "No new output yet. Use nextOffset={} for the next poll.",
                header.next_offset
            )
        };
        format!("{head}\n{message}")
    } else if header.finished {
        format!(
            "{head}\n{}\nTask is {} and all output has been read; polling is finished.",
            slice.content,
            task.status.as_str()
        )
    } else {
        format!("{head}\n{}", slice.content)
    };
    Ok(text_output(&text, output_data(task_id, &slice, &task)))
}

async fn wait(
    runtime: &BackgroundTaskRuntime,
    input: &JsValue,
    context: &ToolRunContext,
) -> Result<ToolOutput, ToolError> {
    let session = access(context);
    let task_id = field(input, "taskId");
    let key = task_key(task_id);
    if runtime.get(&key, Some(&session)).is_none() {
        return Err(unknown_task(task_id)?);
    }
    let offset = requested_offset(input);
    let timeout = match field(input, "timeoutMs") {
        JsValue::Undefined | JsValue::Null => JsValue::Number(DEFAULT_TASK_WAIT_TIMEOUT_MS),
        value => value.clone(),
    };
    let Some(waited) = runtime
        .wait(
            &key,
            &timeout,
            AbortSignal::from_context(context),
            Some(&session),
        )
        .await
        .map_err(runtime_error)?
    else {
        return Err(unknown_task(task_id)?);
    };
    if waited.outcome == "aborted" {
        return Err(ToolError::new(
            "tool_aborted",
            format!(
                "task_wait aborted before task {} finished.",
                display(task_id)?
            )
            .as_str(),
        ));
    }
    let slice = runtime
        .get_output(&key, &offset, field(input, "maxBytes"), Some(&session))
        .map_err(runtime_error)?;
    let header = Header::new(task_id, &slice, &waited.task, &offset)?;
    let head = header.render("task_wait", Some(waited.waited_ms));
    let body = if header.has_new_output {
        format!("\n{}", slice.content)
    } else {
        String::new()
    };
    let text = if waited.outcome == "unknown" || waited.task.status == TaskStatus::Unknown {
        format!("{head}{body}\n{UNKNOWN_STATE}")
    } else if header.finished {
        format!("{head}{body}\nTask finished; no further polling is needed.")
    } else if waited.timed_out {
        format!(
            "{head}{body}\nTask is still running after timeoutMs; use task_wait again to block or task_output for progress."
        )
    } else {
        format!("{head}{body}")
    };
    let mut data = output_data(task_id, &slice, &waited.task);
    data.insert("waitedMs".into(), JsValue::Number(waited.waited_ms));
    data.insert("timedOut".into(), JsValue::Bool(waited.timed_out));
    data.insert("outcome".into(), string(waited.outcome));
    Ok(text_output(&text, data))
}

async fn stop(
    runtime: &BackgroundTaskRuntime,
    input: &JsValue,
    context: &ToolRunContext,
) -> Result<ToolOutput, ToolError> {
    let session = access(context);
    let task_id = field(input, "taskId");
    let key = task_key(task_id);
    let Some(task) = runtime.get(&key, Some(&session)) else {
        return Err(unknown_task(task_id)?);
    };
    if task.status == TaskStatus::Unknown {
        return Err(ToolError::new(
            "invalid_tool_input",
            format!(
                "Task {} has an unknown restored state and cannot be stopped.",
                display(task_id)?
            )
            .as_str(),
        ));
    }
    runtime
        .stop(&key, field(input, "graceMs"), Some(&session))
        .await
        .map_err(runtime_error)?;
    let after = runtime.get(&key, Some(&session)).ok_or_else(|| {
        ToolError::failed("Cannot read properties of undefined (reading 'status')")
    })?;
    Ok(text_output(
        &format!(
            "task_stop taskId={} status={}",
            display(task_id)?,
            after.status.as_str()
        ),
        object([
            ("taskId", task_id.clone()),
            ("status", string(after.status.as_str())),
        ]),
    ))
}
