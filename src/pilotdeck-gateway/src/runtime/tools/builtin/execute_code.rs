//! `execute_code` (baseline `tool/builtin/executeCode.ts`). This owns the
//! private Python workspace, process deadline/cancellation and result contract.
//! Nested tool RPC is not wired: the daqi session host offers no recursion
//! hook, so the generated helper module raises instead of silently bypassing
//! the normal ToolRuntime permission path. The description says so, and the
//! script always reaches the generic permission path because arbitrary Python
//! cannot be judged read-only by name matching.

use super::{field, object, schema, string, text, tool_error};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::{path::PathBuf, time::Instant};

const DEFAULT_TIMEOUT_SECONDS: f64 = 300.0;
const DEFAULT_MAX_TOOL_CALLS: f64 = 50.0;
/// TS `MAX_STDOUT_BYTES` / `MAX_STDERR_BYTES`: the model only sees a summary,
/// so the retained buffers are capped instead of holding an unbounded run.
const MAX_STDOUT_BYTES: usize = 50_000;
const MAX_STDERR_BYTES: usize = 10_000;

pub struct ExecuteCodeTool {
    fields: JsObject,
}

impl ExecuteCodeTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for ExecuteCodeTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for ExecuteCodeTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        // Arbitrary Python cannot be proven read-only by source scanning.
        Ok(JsValue::Bool(false))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}

impl ExecutableTool for ExecuteCodeTool {
    fn validate_input(
        &self,
        input: JsValue,
        _context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move { Ok(validate(&input)) })
    }

    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move { execute(&input, &context).await })
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("execute_code")),
        (
            "description",
            string(
                "Run a local Python 3 script from the workspace cwd. The script inherits the same runtime environment as normal tools such as bash, including configured API, proxy, PATH, virtualenv, and conda variables; do not print secrets or dump the full environment. Only the script's final stdout/stderr summary is returned to the model. Nested PilotDeck tool RPC is unavailable in this runtime: `pilotdeck_tools` exists but every helper raises RuntimeError, so do the work with plain Python and the standard library. Notebook edits, agent, task tools, MCP tools, and execute_code itself are not available.",
            ),
        ),
        ("kind", string("custom")),
        (
            "inputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","required":["code"],"additionalProperties":false,"properties":{"code":{"type":"string","description":"Python 3 source code to execute. `pilotdeck_tools` helpers raise RuntimeError in this runtime; use the standard library."},"description":{"type":"string","description":"Optional human-readable note; ignored by execution."},"timeout_seconds":{"type":"integer","description":"Maximum execution time in seconds. Defaults to 300; maximum 300."},"max_tool_calls":{"type":"integer","description":"Reserved; nested tool calls are unavailable in this runtime."}}}"#,
            )),
        ),
        (
            "outputSchema",
            JsValue::Object(schema(
                r#"{"type":"object","properties":{"status":{"type":"string"},"output":{"type":"string"},"error":{"type":"string"},"tool_calls_made":{"type":"integer"},"duration_seconds":{"type":"number"},"tool_call_log":{"type":"array"}}}"#,
            )),
        ),
    ])
}

fn validate(input: &JsValue) -> Option<Vec<JsObject>> {
    let mut issues = Vec::new();
    match input.get("code").and_then(JsValue::as_string) {
        Some(code) if !text(code).trim().is_empty() => {}
        _ => issues.push(issue("$.code", "$.code must not be empty.")),
    }
    if let Some(value) = input.get("timeout_seconds").and_then(JsValue::as_number)
        && (!(1.0..=DEFAULT_TIMEOUT_SECONDS).contains(&value) || value.fract() != 0.0)
    {
        issues.push(issue(
            "$.timeout_seconds",
            "$.timeout_seconds must be between 1 and 300.",
        ));
    }
    if let Some(value) = input.get("max_tool_calls").and_then(JsValue::as_number)
        && (!(0.0..=DEFAULT_MAX_TOOL_CALLS).contains(&value) || value.fract() != 0.0)
    {
        issues.push(issue(
            "$.max_tool_calls",
            "$.max_tool_calls must be between 0 and 50.",
        ));
    }
    (!issues.is_empty()).then_some(issues)
}

fn issue(path: &str, message: &str) -> JsObject {
    object([
        ("path", string(path)),
        ("code", string("invalid_schema")),
        ("message", string(message)),
    ])
}

struct ExecutionWorkspace(PathBuf);
impl ExecutionWorkspace {
    fn create() -> Result<Self, ToolError> {
        let path =
            std::env::temp_dir().join(format!("pilotdeck_execute_code_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)
            .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;
        Ok(Self(path))
    }
}
impl Drop for ExecutionWorkspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let code = input
        .get("code")
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| tool_error("invalid_tool_input", "code must be a string."))?;
    let timeout_seconds = field(input, "timeout_seconds")
        .as_number()
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
    let max_tool_calls = field(input, "max_tool_calls")
        .as_number()
        .unwrap_or(DEFAULT_MAX_TOOL_CALLS);
    let started = Instant::now();
    let workspace = ExecutionWorkspace::create()?;
    let helper = workspace.0.join("pilotdeck_tools.py");
    let script = workspace.0.join("script.py");
    std::fs::write(&helper, helper_module(max_tool_calls as usize))
        .and_then(|_| std::fs::write(&script, text(&code)))
        .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;

    let python = find_python(context).await;
    let Some(python) = python else {
        return Ok(output(
            "unsupported",
            "",
            "execute_code requires python3 on PATH.",
            started,
            context,
        ));
    };
    let mut command = tokio::process::Command::new(python);
    command
        .arg(&script)
        .current_dir(super::os_path(&super::cwd(context)))
        .kill_on_drop(true)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(JsValue::Object(env)) = context.fields.get("env") {
        command.env_clear();
        for (key, value) in env.own_entries() {
            if let Some(value) = value.as_string() {
                command.env(text(key), text(value));
            }
        }
    }
    let inherited_pythonpath = context
        .fields
        .get("env")
        .and_then(|value| value.get("PYTHONPATH"))
        .and_then(JsValue::as_string)
        .map(text)
        .or_else(|| std::env::var("PYTHONPATH").ok());
    let mut pythonpath = workspace.0.to_string_lossy().into_owned();
    if let Some(existing) = inherited_pythonpath.filter(|value| !value.is_empty()) {
        pythonpath.push(':');
        pythonpath.push_str(&existing);
    }
    command
        .env("PYTHONPATH", pythonpath)
        .env("PILOTDECK_EXECUTE_CODE_TEMP_ROOT", &workspace.0);
    // The script leads its own group so a timeout or cancel can signal the
    // whole tree, not only the interpreter that spawned it. TS runs the same
    // process under a code runtime that terminates the tree on abort.
    #[cfg(unix)]
    command.process_group(0);
    let child = command
        .spawn()
        .map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;
    let group = child
        .id()
        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32));
    let mut guard = crate::process_group::GroupGuard(group);
    let mut wait = Box::pin(child.wait_with_output());
    let outcome = tokio::select! {
        value = wait.as_mut() => Some(value),
        _ = tokio::time::sleep(std::time::Duration::from_secs_f64(timeout_seconds)) => None,
        _ = context.cancelled() => None,
    };
    let Some(result) = outcome else {
        // Signal the group, then reap the leader so the call leaves no
        // orphan behind and the pipe buffers are released.
        guard.signal(rustix::process::Signal::KILL);
        let _ = wait.as_mut().await;
        let status = if context.is_aborted() { "cancelled" } else { "timeout" };
        let message = if context.is_aborted() {
            "Script execution was cancelled."
        } else {
            "Script execution timed out."
        };
        return Ok(output(status, "", message, started, context));
    };
    guard.disarm();
    let result =
        result.map_err(|error| tool_error("tool_execution_failed", error.to_string()))?;
    let stdout = truncate_bytes(
        &String::from_utf8_lossy(&result.stdout),
        MAX_STDOUT_BYTES,
    );
    let stderr = truncate_bytes(
        &String::from_utf8_lossy(&result.stderr),
        MAX_STDERR_BYTES,
    );
    if result.status.success() {
        Ok(output("success", &stdout, &stderr, started, context))
    } else {
        let error = if stderr.is_empty() {
            format!("Python exited with status {}.", result.status)
        } else {
            stderr
        };
        Ok(output("error", &stdout, &error, started, context))
    }
}

/// Byte cap with a visible marker, mirroring the TS truncation notice.
fn truncate_bytes(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}[truncated {} bytes]", &value[..end], value.len() - end)
}

async fn find_python(context: &ToolRunContext) -> Option<&'static str> {
    for candidate in ["python3", "python"] {
        let status = tokio::process::Command::new(candidate)
            .arg("--version")
            .kill_on_drop(true)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        let available = tokio::select! {
            value = status => value.is_ok_and(|status| status.success()),
            _ = context.cancelled() => false,
        };
        if context.is_aborted() {
            return None;
        }
        if available {
            return Some(candidate);
        }
    }
    None
}

fn helper_module(max_tool_calls: usize) -> String {
    format!(
        r#"""PilotDeck helper surface; host recursion is unavailable in this runtime."""
_MAX_TOOL_CALLS = {max_tool_calls}
def _unavailable(*_args, **_kwargs):
    raise RuntimeError("PilotDeck nested tool RPC is unavailable in this runtime")
read_file = write_file = edit_file = glob = grep = bash = web_search = web_fetch = _unavailable
"#
    )
}

fn output(
    status: &str,
    stdout: &str,
    error: &str,
    started: Instant,
    context: &ToolRunContext,
) -> ToolOutput {
    let duration = (started.elapsed().as_secs_f64() * 100.0).round() / 100.0;
    let data = object([
        ("status", string(status)),
        ("output", string(stdout)),
        ("error", string(error)),
        ("tool_calls_made", JsValue::Number(0.0)),
        ("duration_seconds", JsValue::Number(duration)),
        ("tool_call_log", JsValue::Array(Vec::new())),
    ]);
    let rendered = format!(
        "status: {status}\nduration_seconds: {duration}\ntool_calls_made: 0\noutput:\n{stdout}{}",
        if error.is_empty() {
            String::new()
        } else {
            format!("\nerror:\n{error}")
        }
    );
    ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(rendered)),
        ]))],
        data: Some(JsValue::Object(data)),
        metadata: Some(object([
            ("status", string(status)),
            ("tool_calls_made", JsValue::Number(0.0)),
            ("duration_seconds", JsValue::Number(duration)),
            ("cwd", string(super::cwd(context))),
            // Report what actually happened: a caller that supplied `env`
            // replaced the environment; otherwise the child inherited it.
            (
                "env_inheritance",
                string(if matches!(context.fields.get("env"), Some(JsValue::Object(_))) {
                    "replaced"
                } else {
                    "full"
                }),
            ),
            ("python_path_augmented", JsValue::Bool(true)),
        ])),
        supplemental_messages: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_bounds_and_empty_code() {
        let input = JsValue::Object(object([
            ("code", string("  ")),
            ("timeout_seconds", JsValue::Number(301.0)),
            ("max_tool_calls", JsValue::Number(-1.0)),
        ]));
        assert_eq!(validate(&input).unwrap().len(), 3);
    }

    const PYTHON_INPUTS: &[&str] = &[
        "from os import remove",
        "import os as o",
        "import os\nos .remove('x')",
        r#"exec("import os")"#,
        "open('x', 'w').write('x')",
        "eval('1 + 1')",
        "import os\nos.remove('x')",
        "import shutil\nshutil.rmtree('x')",
        "import subprocess\nsubprocess.run(['rm', 'x'])",
        "__import__('os').remove('x')",
        "print(sum(range(10)))",
        "print('hello')",
        "# write_file(x)\nprint('edit_file(x)')",
        "write_file('a', 'b')",
        "bash('git status')",
        "bash('rm file')",
        "bash(command)",
        "print('中文')\nbash ( 'git status' )",
        "",
        "  ",
    ];

    #[test]
    fn read_only_probe_always_returns_false_for_python() {
        let tool = ExecuteCodeTool::new();
        for code in PYTHON_INPUTS {
            let input = JsValue::Object(object([("code", string(*code))]));
            assert_eq!(
                ToolDefinition::is_read_only(&tool, &input).unwrap(),
                JsValue::Bool(false),
                "{code:?} must not be read-only"
            );
        }
    }

    #[test]
    fn read_only_probe_rejects_non_string_and_missing_code() {
        let tool = ExecuteCodeTool::new();
        for input in [
            JsValue::Undefined,
            JsValue::Null,
            JsValue::Bool(true),
            JsValue::Number(1.0),
            string("print('hello')"),
            JsValue::Array(Vec::new()),
            JsValue::Object(JsObject::new()),
        ] {
            assert_eq!(
                ToolDefinition::is_read_only(&tool, &input).unwrap(),
                JsValue::Bool(false)
            );
            let input = JsValue::Object(object([("code", input)]));
            assert_eq!(
                ToolDefinition::is_read_only(&tool, &input).unwrap(),
                JsValue::Bool(false)
            );
        }
    }

    // Delegate to the real tool predicate, as the runtime permission adapter does.
    struct PermissionAdapter(ExecuteCodeTool);

    impl crate::runtime::permission::PermissionTool for PermissionAdapter {
        fn name(&self) -> &pilotdeck_protocol::JsString {
            self.0.fields().get("name").unwrap().as_string().unwrap()
        }

        fn kind(&self) -> Option<&pilotdeck_protocol::JsString> {
            self.0.fields().get("kind").and_then(JsValue::as_string)
        }

        fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
            ToolDefinition::is_read_only(&self.0, input)
        }
    }

    #[tokio::test]
    async fn plan_mode_denies_execute_code() {
        use crate::runtime::permission::{self, DefaultPermissionContextOptions};

        let tool = PermissionAdapter(ExecuteCodeTool::new());
        for can_prompt in [false, true] {
            let context = permission::create_default_permission_context(
                DefaultPermissionContextOptions {
                    cwd: "/ws/project".into(),
                    mode: Some("plan".into()),
                    can_prompt: Some(can_prompt),
                    ..Default::default()
                },
            );
            for code in PYTHON_INPUTS {
                let input = JsValue::Object(object([("code", string(*code))]));
                let decision = permission::decide(&context, &tool, &input, &"call-1".into())
                    .await
                    .expect("permission decision");
                assert_eq!(
                    decision.get("type"),
                    Some(&string("deny")),
                    "plan mode must deny {code:?}, can_prompt={can_prompt}"
                );
            }
        }
    }

    #[tokio::test]
    async fn default_mode_requires_permission_for_execute_code() {
        use crate::runtime::permission::{self, DefaultPermissionContextOptions};

        let tool = PermissionAdapter(ExecuteCodeTool::new());
        for (can_prompt, expected) in [(true, "ask"), (false, "deny")] {
            let context = permission::create_default_permission_context(
                DefaultPermissionContextOptions {
                    cwd: "/ws/project".into(),
                    can_prompt: Some(can_prompt),
                    ..Default::default()
                },
            );
            for code in PYTHON_INPUTS {
                let input = JsValue::Object(object([("code", string(*code))]));
                let decision = permission::decide(&context, &tool, &input, &"call-1".into())
                    .await
                    .expect("permission decision");
                assert_eq!(
                    decision.get("type"),
                    Some(&string(expected)),
                    "default mode must require permission for {code:?}"
                );
            }
        }
    }
}
