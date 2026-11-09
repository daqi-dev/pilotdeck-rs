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
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(is_read_only(input)))
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

fn is_read_only(input: &JsValue) -> bool {
    let Some(code) = input.get("code").and_then(JsValue::as_string).map(text) else {
        return false;
    };
    if code.trim().is_empty() {
        return false;
    }
    // TS `isExecuteCodeReadOnly`: no write-capable helper call and only
    // read-only string-literal bash calls. Names are matched after stripping
    // comments and string bodies, so prose cannot vouch for a program.
    let searchable = strip_python_comments_and_strings(&code);
    if ["write_file", "edit_file"]
        .iter()
        .any(|name| function_call_offsets(&searchable, name).next().is_some())
    {
        return false;
    }
    if !read_only_bash_calls_only(&code, &searchable) {
        return false;
    }
    // Named helpers are not the only effect channel: the script runs
    // unconfined Python, so an import of os/shutil/subprocess/pathlib or a
    // builtin open() reaches the filesystem without naming a write helper.
    // Those programs are effectful even though TS would call them read-only,
    // because TS routes every effect through the helper RPC it can judge.
    // Calls are matched as `name (`, while the module references below are
    // plain attribute access (`os.remove`, `subprocess.run`) with no call form.
    const EFFECT_CALLS: [&str; 2] = ["open", "eval"];
    const EFFECT_MODULES: [&str; 8] = [
        "os.",
        "shutil.",
        "subprocess.",
        "pathlib.",
        "socket.",
        "urllib.",
        "requests.",
        "__import__",
    ];
    !EFFECT_CALLS
        .iter()
        .any(|name| function_call_offsets(&searchable, name).next().is_some())
        && !EFFECT_MODULES
            .iter()
            .any(|name| searchable.contains(name))
}

fn read_only_bash_calls_only(code: &str, searchable: &str) -> bool {
    for offset in function_call_offsets(searchable, "bash") {
        let Some(command) = read_first_python_string_argument(code, offset) else {
            return false;
        };
        if !super::bash::is_read_only_shell_command(&command) {
            return false;
        }
    }
    true
}

fn function_call_offsets<'a>(
    searchable: &'a str,
    name: &'a str,
) -> impl Iterator<Item = usize> + 'a {
    searchable
        .match_indices(name)
        .filter_map(move |(start, _)| {
            let left_boundary = start == 0
                || !searchable[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|character| character == '_' || character.is_alphanumeric());
            if !left_boundary {
                return None;
            }
            let mut offset = start + name.len();
            while searchable[offset..]
                .chars()
                .next()
                .is_some_and(char::is_whitespace)
            {
                offset += searchable[offset..].chars().next().unwrap().len_utf8();
            }
            searchable[offset..].starts_with('(').then_some(offset + 1)
        })
}

fn read_first_python_string_argument(code: &str, byte_offset: usize) -> Option<String> {
    let rest = code.get(byte_offset..)?;
    let trimmed = rest.trim_start_matches(char::is_whitespace);
    let quote = trimmed.chars().next()?;
    if !matches!(quote, '\'' | '"') {
        return None;
    }
    let delimiter = if trimmed.starts_with(&quote.to_string().repeat(3)) {
        quote.to_string().repeat(3)
    } else {
        quote.to_string()
    };
    let mut output = String::new();
    let mut index = delimiter.len();
    while index < trimmed.len() {
        let rest = &trimmed[index..];
        if rest.starts_with(&delimiter) {
            return Some(output);
        }
        let character = rest.chars().next()?;
        if character == '\\' {
            let escaped = rest[character.len_utf8()..].chars().next()?;
            output.push(match escaped {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            index += character.len_utf8() + escaped.len_utf8();
        } else {
            output.push(character);
            index += character.len_utf8();
        }
    }
    None
}

fn strip_python_comments_and_strings(code: &str) -> String {
    let chars: Vec<char> = code.chars().collect();
    let mut output = String::with_capacity(chars.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '#' {
            while index < chars.len() && chars[index] != '\n' {
                output.extend(std::iter::repeat_n(' ', chars[index].len_utf8()));
                index += 1;
            }
        } else if matches!(chars[index], '\'' | '"') {
            let quote = chars[index];
            let triple =
                chars.get(index + 1) == Some(&quote) && chars.get(index + 2) == Some(&quote);
            let width = if triple { 3 } else { 1 };
            output.extend(std::iter::repeat_n(' ', width));
            index += width;
            while index < chars.len() {
                if chars[index] == '\\' {
                    output.push(' ');
                    if index + 1 < chars.len() {
                        output.push(' ');
                    }
                    index += 2;
                } else if chars[index] == quote
                    && (!triple
                        || (chars.get(index + 1) == Some(&quote)
                            && chars.get(index + 2) == Some(&quote)))
                {
                    output.extend(std::iter::repeat_n(' ', width));
                    index += width;
                    break;
                } else {
                    if chars[index] == '\n' {
                        output.push('\n');
                    } else {
                        output.extend(std::iter::repeat_n(' ', chars[index].len_utf8()));
                    }
                    index += 1;
                }
            }
        } else {
            output.push(chars[index]);
            index += 1;
        }
    }
    output
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

    #[test]
    fn read_only_probe_ignores_comments_and_strings() {
        let input = JsValue::Object(object([(
            "code",
            string("# write_file(x)\nprint('edit_file(x)')"),
        )]));
        assert!(is_read_only(&input));
        let input = JsValue::Object(object([("code", string("write_file('a', 'b')"))]));
        assert!(!is_read_only(&input));
        let input = JsValue::Object(object([("code", string("bash('git status')"))]));
        assert!(is_read_only(&input));
        let input = JsValue::Object(object([("code", string("bash('rm file')"))]));
        assert!(!is_read_only(&input));
        let input = JsValue::Object(object([("code", string("bash(command)"))]));
        assert!(!is_read_only(&input));
        let input = JsValue::Object(object([(
            "code",
            string("print('中文')\nbash ( 'git status' )"),
        )]));
        assert!(is_read_only(&input));
    }

    /// `open('x', 'w').write('x')` names no write helper, so matching only
    /// `write_file(` / `edit_file(` would let an arbitrary program run as a
    /// read in plan and default modes. Effect channels must fail the probe.
    #[test]
    fn arbitrary_python_effects_are_not_read_only() {
        for code in [
            "open('x', 'w').write('x')",
            "import os\nos.remove('x')",
            "import shutil\nshutil.rmtree('x')",
            "import subprocess\nsubprocess.run(['rm', 'x'])",
            "__import__('os').remove('x')",
        ] {
            let input = JsValue::Object(object([("code", string(code))]));
            assert!(!is_read_only(&input), "{code} must not be read-only");
        }
        let input = JsValue::Object(object([("code", string("print(sum(range(10)))"))]));
        assert!(is_read_only(&input));
    }
}
