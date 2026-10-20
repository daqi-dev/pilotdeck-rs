//! `bash` (baseline `tool/builtin/bash.ts`): foreground shell commands with
//! timeout / abort, background-command guidance, the `BASH_RESULT` success
//! text and the `BASH_FAILURE_DIAGNOSTIC` failure details. Permission
//! classification lives in [`permissions`] and is exposed to the decision
//! engine through [`PermissionTool`]; process execution lives in [`runner`].
//!
//! Strings that the baseline slices by UTF-16 index (`tailSnippet`) or passes
//! through from the input (`command`) are built on UTF-16 units, so lone
//! surrogates and split surrogate pairs come out exactly as in TS.
#![cfg(unix)]
pub mod permissions;
pub mod runner;

pub use permissions::{classify_bash_permission, is_read_only_shell_command};
pub use runner::{
    AbortSignal, CommandOptions, CommandResult, CommandRunner, NodeShellCommandRunner,
    OutputCallback,
};

use super::{field, object, schema, string, truthy};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    js_value,
    runtime::{
        permission::{PermissionContext, PermissionFuture, PermissionTool},
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
    },
};
use permissions::{B, ws};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::Regex;
use std::sync::{Arc, LazyLock};

const DESCRIPTION: &str = "Run a shell command in the PilotDeck workspace.\n\nUsage:\n- The `command` parameter is passed to PilotDeck's resolved command shell (Bash preferred; Unix falls back to `/bin/sh`, Windows falls back to `cmd.exe` and then PowerShell 7).\n- Set `PILOTDECK_SHELL_PATH` to explicitly select a shell executable; on Windows, `PILOTDECK_GIT_BASH_PATH` can select a non-standard Git Bash installation.\n- The shell runs in the current workspace directory and inherits the tool runtime environment.\n- Use `timeout` to override the command timeout in milliseconds. When omitted, the default is 30000ms. Values above 600000ms are rejected; lower the foreground timeout to 600000 or less, or use task_create followed by task_wait for background work that should finish.\n- Use `description` to provide a short, clear label for logs and audits. Prefer 3-10 words that say what the command does.\n- Use this tool for short shell commands, simple pipelines, and running saved workspace scripts.\n- Use task_create for long-running work such as dev servers, watchers, builds, test suites, deploys, CI pollers, or commands that need more than the maximum foreground timeout. For background work that should finish, call task_wait after task_create. Use task_output for progress checks and task_stop for long-lived processes.\n- For non-trivial code or anything you will debug/rerun with changed parameters, first create or edit a script file with write_file/edit_file, then run that file. Avoid large inline heredocs, `python - <<...`, long `python -c`, or long `node -e` programs when a saved script would be reusable.\n- Do not use shell-level backgrounding (`nohup`, `disown`, `setsid`, trailing `&`) in bash. Use task_create so PilotDeck can track lifecycle and output.\n- Read-only shell commands (for example `pwd`, `ls`, `git status`, `git diff`, `git log`) are treated as read-only. Commands with side effects require permission, and known-dangerous commands are denied outright.\n- The tool returns stdout, stderr, exit code, and duration. Non-zero exits raise a tool error, and timeouts raise `tool_timeout`.\n- Successful results begin with `BASH_RESULT[success][...]` plus Assertions. Read `retrieved_data_available` before treating `exit_code: 0` as task progress: exit code 0 only proves the process succeeded, not that useful task data was retrieved.\n- If a task needs content but the result is `empty_stdout` or `stderr_only`, run a follow-up command that prints or verifies the needed data instead of assuming progress.\n- If you have no command to run, respond with text instead of calling bash.";

const INPUT_SCHEMA: &str = r#"{"type":"object","required":["command"],"additionalProperties":false,"properties":{"command":{"type":"string","description":"The shell command to execute (passed to PilotDeck's resolved command shell)."},"timeout":{"type":"integer","description":"Optional timeout in milliseconds. Defaults to 30000. Max 600000; larger values are rejected. Use timeout=600000 or less for foreground bash, or task_create followed by task_wait for background work that should finish."},"description":{"type":"string","description":"Clear, concise description of what this command does in active voice. Prefer 3-10 words."}}}"#;

const LONG_TASK_HINT: &str = "Use timeout=600000 or less for foreground bash. If the command must run in the background, use task_create and then task_wait to block for completion; use task_output only for progress checks and task_stop to clean up long-lived processes.";

const STREAM_DIAGNOSTIC_MAX_CHARS: usize = 600;

/// `CreateBashToolOptions` (the shell/subprocess provider seams collapse into
/// `runner`).
#[derive(Clone, Default)]
pub struct BashToolOptions {
    pub runner: Option<Arc<dyn CommandRunner>>,
    pub default_timeout_ms: Option<f64>,
    pub max_timeout_ms: Option<f64>,
}

pub struct BashTool {
    fields: JsObject,
    name: JsString,
    kind: JsString,
    runner: Arc<dyn CommandRunner>,
    default_timeout_ms: f64,
    max_timeout_ms: f64,
}

impl BashTool {
    pub fn new() -> Self {
        Self::with_options(BashToolOptions::default())
    }
    pub fn with_options(options: BashToolOptions) -> Self {
        Self {
            fields: definition(),
            name: JsString::from("bash"),
            kind: JsString::from("shell"),
            runner: options
                .runner
                .unwrap_or_else(|| Arc::new(NodeShellCommandRunner)),
            default_timeout_ms: options.default_timeout_ms.unwrap_or(30_000.0),
            max_timeout_ms: options.max_timeout_ms.unwrap_or(600_000.0),
        }
    }
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("bash")),
        ("aliases", JsValue::Array(vec![string("Bash")])),
        ("description", string(DESCRIPTION)),
        ("kind", string("shell")),
        ("inputSchema", JsValue::Object(schema(INPUT_SCHEMA))),
        ("maxResultBytes", JsValue::Number(200_000.0)),
    ])
}

/// `!input.command || isReadOnlyShellCommand(input.command)`. A non-string
/// truthy command has no `.length` for the tokenizer and is not read-only.
fn read_only(input: &JsValue) -> bool {
    match field(input, "command") {
        command if !truthy(command) => true,
        JsValue::String(command) => {
            is_read_only_shell_command(&String::from_utf16_lossy(command.units()))
        }
        _ => false,
    }
}

impl ToolDefinition for BashTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(read_only(input)))
    }
    fn is_concurrency_safe(&self, input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(read_only(input)))
    }
}

/// `checkPermissions(input)`.
pub fn check_bash_permissions(input: &JsValue) -> JsValue {
    let command = field(input, "command");
    if !truthy(command) {
        return JsValue::Object(object([
            ("type", string("allow")),
            (
                "reason",
                JsValue::Object(object([
                    ("type", string("runtime")),
                    ("message", string("Empty command is safe")),
                ])),
            ),
        ]));
    }
    match command {
        JsValue::String(text) => classify_bash_permission(text),
        other => permissions::classify(
            &js_value::display(other).unwrap_or_default(),
            false,
            other.clone(),
        ),
    }
}

impl PermissionTool for BashTool {
    fn name(&self) -> &JsString {
        &self.name
    }
    fn kind(&self) -> Option<&JsString> {
        Some(&self.kind)
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(read_only(input)))
    }
    fn check_permissions<'a>(
        &'a self,
        input: &'a JsValue,
        _context: &'a PermissionContext,
    ) -> PermissionFuture<'a, Option<JsValue>> {
        Box::pin(async move { Ok(Some(check_bash_permissions(input))) })
    }
}

impl ExecutableTool for BashTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let runner = self.runner.clone();
        let default_timeout_ms = self.default_timeout_ms;
        let max_timeout_ms = self.max_timeout_ms;
        Box::pin(async move {
            execute(
                &input,
                &context,
                runner.as_ref(),
                default_timeout_ms,
                max_timeout_ms,
            )
            .await
        })
    }
}

// ---- UTF-16 helpers --------------------------------------------------------

fn space_unit(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(crate::runtime::tools::js_space)
}

fn trim_end_units(units: &[u16]) -> &[u16] {
    let end = units
        .iter()
        .rposition(|unit| !space_unit(*unit))
        .map_or(0, |index| index + 1);
    &units[..end]
}

fn trim_units(units: &[u16]) -> &[u16] {
    let units = trim_end_units(units);
    let start = units
        .iter()
        .position(|unit| !space_unit(*unit))
        .unwrap_or(units.len());
    &units[start..]
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn contains_units(haystack: &[u16], needle: &[u16]) -> bool {
    needle.is_empty()
        || haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// `value.split(/\r?\n/)` on UTF-16 units.
fn split_lines_units(value: &[u16]) -> Vec<&[u16]> {
    let mut lines = Vec::new();
    let mut start = 0;
    for (index, unit) in value.iter().enumerate() {
        if *unit == u16::from(b'\n') {
            let end = if index > start && value[index - 1] == u16::from(b'\r') {
                index - 1
            } else {
                index
            };
            lines.push(&value[start..end]);
            start = index + 1;
        }
    }
    lines.push(&value[start..]);
    lines
}

fn join_lines(lines: &[Vec<u16>]) -> Vec<u16> {
    let mut out = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            out.push(u16::from(b'\n'));
        }
        out.extend_from_slice(line);
    }
    out
}

fn js_trim_str(value: &str) -> &str {
    value.trim_matches(crate::runtime::tools::js_space)
}

// ---- background guidance ----------------------------------------------------

struct Guidance {
    wrapper: Regex,
    trailing: Regex,
    inline: Regex,
    long_lived: [Regex; 5],
    help: Regex,
    quoted: Regex,
    backtick: Regex,
}

/// `.` in a JS regex: anything but a line terminator.
const DOT: &str = r"[^\n\r\u{2028}\u{2029}]";

static GUIDANCE: LazyLock<Guidance> = LazyLock::new(|| {
    let s = ws();
    let build = |pattern: String| Regex::new(&pattern).expect("bash guidance pattern");
    Guidance {
        wrapper: build(format!(
            r"(?i)(?:^|[;&|]{s}*|&&{s}*|\|\|{s}*|\$\({s}*)(?:nohup|disown|setsid){B}"
        )),
        trailing: build(format!(r"(?:^|[^&])&{s}*(?:[)#}}\]]{s}*)?$")),
        inline: build(format!(r"(?:^|[;|]{s}*)[^\n;&|]+&{s}*(?:[;|]|&&|\|\|)")),
        long_lived: [
            build(format!(
                r"(?i)(?:^|{s})(?:npm|pnpm|yarn|bun){s}+(?:run{s}+)?(?:dev|start|serve|preview|watch){B}"
            )),
            build(format!(
                r"(?i)(?:^|{s})(?:vite|webpack(?:-dev-server)?|next|nuxt|astro|remix|svelte-kit){B}(?:[^\n]*{s}(?:dev|start|preview|--host){B}|[^\n]*$)"
            )),
            build(format!(
                r"(?i)(?:^|{s})(?:python(?:3)?{s}+-m{s}+http\.server|uvicorn|gunicorn|flask{s}+run|streamlit{s}+run|fastapi{s}+dev){B}"
            )),
            build(format!(
                r"(?i)(?:^|{s})(?:nodemon|tsx{s}+watch|ts-node-dev|watchmedo){B}"
            )),
            build(format!(
                r"(?i)(?:^|{s})(?:cargo{s}+watch|watchexec|entr){B}"
            )),
        ],
        help: build(format!(
            r"(?:^|{s})(?:--help|-h|help|--version|-v|version){s}*$"
        )),
        // (['"])(?:\\.|(?!\1).)*\1 — the back-reference expanded per quote.
        quoted: build(format!(
            r#""(?:\\{DOT}|[^"\n\r\u{{2028}}\u{{2029}}])*"|'(?:\\{DOT}|[^'\n\r\u{{2028}}\u{{2029}}])*'"#
        )),
        backtick: build(format!(r"`(?:\\{DOT}|[^`])*`")),
    }
});

fn strip_quoted_text(command: &str) -> String {
    let once = GUIDANCE.quoted.replace_all(command, "");
    GUIDANCE.backtick.replace_all(&once, "").into_owned()
}

/// `foregroundBackgroundGuidance(command)`.
pub fn foreground_background_guidance(command: &str) -> Option<&'static str> {
    let normalized = js_trim_str(command).to_lowercase();
    if GUIDANCE.help.is_match(&normalized) {
        return None;
    }
    let unquoted = strip_quoted_text(command);
    if GUIDANCE.wrapper.is_match(&unquoted) {
        return Some(
            "Foreground bash command uses shell-level background wrappers (nohup/disown/setsid).",
        );
    }
    if GUIDANCE.inline.is_match(&unquoted) || GUIDANCE.trailing.is_match(&unquoted) {
        return Some("Foreground bash command uses shell-level '&' backgrounding.");
    }
    if GUIDANCE
        .long_lived
        .iter()
        .any(|pattern| pattern.is_match(&unquoted))
    {
        return Some(
            "Foreground bash command appears to start a long-lived server, watcher, or dev process.",
        );
    }
    None
}

// ---- execute ----------------------------------------------------------------

fn tool_error(code: &str, message: impl Into<JsString>) -> ToolError {
    ToolError::new(code, message)
}

/// `input.command.trim()`, including the TypeErrors a non-string throws.
fn trimmed_command(input: &JsValue) -> Result<JsString, ToolError> {
    match field(input, "command") {
        JsValue::String(command) => Ok(JsString::from_utf16(trim_units(command.units()).to_vec())),
        JsValue::Undefined => Err(ToolError::failed(
            "Cannot read properties of undefined (reading 'trim')",
        )),
        JsValue::Null => Err(ToolError::failed(
            "Cannot read properties of null (reading 'trim')",
        )),
        _ => Err(ToolError::failed("input.command.trim is not a function")),
    }
}

fn to_number(value: &JsValue) -> Result<f64, ToolError> {
    js_value::to_number(value).map_err(|_| ToolError::failed(js_value::StackOverflow::MESSAGE))
}

fn number_text(value: f64) -> String {
    js_value::number_string(value)
}

/// `context.env` as Node's spawn reads it: `undefined` members are skipped and
/// every other value is `String()`ed. A missing or non-object env inherits.
fn spawn_env(context: &ToolRunContext) -> Result<Option<Vec<(JsString, JsString)>>, ToolError> {
    let Some(JsValue::Object(env)) = context.fields.get("env") else {
        return Ok(None);
    };
    let mut pairs = Vec::new();
    // `Object.keys` order (array indices first), which is also the order in
    // which Node reports the first entry holding a NUL byte.
    for (key, value) in env.own_entries() {
        if value.is_undefined() {
            continue;
        }
        let mut text = js_value::to_string_units(value)
            .map_err(|_| ToolError::failed(js_value::StackOverflow::MESSAGE))?;
        // Node only NUL-checks string values; any other value is `${value}`d
        // and the C string ends at its first NUL (probes/nul4.out).
        if !matches!(value, JsValue::String(_))
            && let Some(end) = text.iter().position(|unit| *unit == 0)
        {
            text.truncate(end);
        }
        pairs.push((key.clone(), JsString::from_utf16(text)));
    }
    Ok(Some(pairs))
}

fn progress_callback(context: &ToolRunContext, stream: &'static str) -> Option<OutputCallback> {
    let sink = context.host.progress.clone()?;
    let context = context.clone();
    Some(Arc::new(move |chunk: &str| {
        // `try { progress(...) } catch {}`: a failing sink never crashes the tool.
        let sink = |event: JsObject| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(event)));
        };
        let length = chunk.encode_utf16().count();
        // `(context.now?.() ?? new Date()).toISOString()`; a throwing clock is
        // swallowed like every other progress failure.
        let Ok(created_at) = context.now_iso() else {
            return;
        };
        let get = |key: &str| {
            context
                .fields
                .get(key)
                .cloned()
                .unwrap_or(JsValue::Undefined)
        };
        sink(object([
            ("type", string("tool_progress")),
            ("sessionId", get("sessionId")),
            ("turnId", get("turnId")),
            ("toolCallId", string("")),
            ("toolName", string("bash")),
            (
                "message",
                string(format!("{stream}: {length} bytes").as_str()),
            ),
            (
                "metadata",
                JsValue::Object(object([
                    ("stream", string(stream)),
                    ("chunk", string(chunk)),
                    ("byteCount", JsValue::Number(chunk.len() as f64)),
                ])),
            ),
            ("createdAt", string(created_at)),
        ]));
    }))
}

async fn execute(
    input: &JsValue,
    context: &ToolRunContext,
    runner: &dyn CommandRunner,
    default_timeout_ms: f64,
    max_timeout_ms: f64,
) -> Result<ToolOutput, ToolError> {
    let command = trimmed_command(input)?;
    let command_text = String::from_utf16_lossy(command.units());
    let timeout = field(input, "timeout");
    if !timeout.is_undefined() && to_number(timeout)? > max_timeout_ms {
        let shown = js_value::display(timeout)
            .map_err(|_| ToolError::failed(js_value::StackOverflow::MESSAGE))?;
        return Err(tool_error(
            "invalid_tool_input",
            format!(
                "Foreground bash timeout {shown}ms exceeds the maximum of {}ms. {LONG_TASK_HINT}",
                number_text(max_timeout_ms)
            )
            .as_str(),
        ));
    }
    if let Some(guidance) = foreground_background_guidance(&command_text) {
        return Err(tool_error(
            "invalid_tool_input",
            format!("{guidance} {LONG_TASK_HINT}").as_str(),
        ));
    }
    // `Math.max(1, input.timeout ?? defaultTimeoutMs)`.
    let requested = match timeout {
        JsValue::Undefined | JsValue::Null => default_timeout_ms,
        other => to_number(other)?,
    };
    let timeout_ms = if requested.is_nan() {
        f64::NAN
    } else {
        requested.max(1.0)
    };

    let result = runner
        .run(
            command.clone(),
            CommandOptions {
                cwd: super::cwd(context),
                env: spawn_env(context)?,
                timeout_ms,
                signal: AbortSignal::from_context(context),
                on_stdout: progress_callback(context, "stdout"),
                on_stderr: progress_callback(context, "stderr"),
            },
        )
        .await?;

    if result.timed_out {
        return Err(tool_error(
            "tool_timeout",
            format!("Command timed out after {}ms.", number_text(timeout_ms)).as_str(),
        ));
    }
    let exit_code = result
        .exit_code
        .map_or(JsValue::Null, |code| JsValue::Number(f64::from(code)));
    let exit_text = result
        .exit_code
        .map_or_else(|| "null".to_string(), |code| code.to_string());

    if result.exit_code != Some(0) {
        let summary = format_shell_failure(&command, &exit_text, &result);
        let diagnostic = format_shell_failure_diagnostic(&exit_text, &result);
        let mut error = tool_error("tool_execution_failed", summary);
        error.details = Some(object([
            ("command", JsValue::String(command.clone())),
            ("exitCode", exit_code),
            ("diagnostic", JsValue::String(diagnostic)),
            ("stdout", string(result.stdout.as_str())),
            ("stderr", string(result.stderr.as_str())),
            ("timedOut", JsValue::Bool(result.timed_out)),
            ("durationMs", JsValue::Number(result.duration_ms)),
        ]));
        return Err(error);
    }

    let stdout_visible = !js_trim_str(&result.stdout).is_empty();
    let stderr_visible = !js_trim_str(&result.stderr).is_empty();
    let output_state = if stdout_visible {
        "stdout_data"
    } else if stderr_visible {
        "stderr_only"
    } else {
        "empty_stdout"
    };
    let stdout_bytes = result.stdout.len();
    let stderr_bytes = result.stderr.len();
    let interpretation = match output_state {
        "stdout_data" => {
            "Command succeeded and stdout contains visible data; use stdout as the primary evidence for the next step."
        }
        "stderr_only" => {
            "Command succeeded but stdout is empty; stderr contains diagnostic or progress output and does not count as retrieved task data by default."
        }
        _ => {
            "Command succeeded with no visible stdout or stderr; this only confirms process success and does not prove task data was retrieved."
        }
    };
    let mut lines = vec![
        format!("BASH_RESULT[success][{output_state}]"),
        "Assertions:".to_string(),
        format!("- exit_code: {exit_text}"),
        format!("- stdout_visible: {stdout_visible}"),
        format!("- stderr_visible: {stderr_visible}"),
        format!("- retrieved_data_available: {stdout_visible}"),
        format!("- stdout_bytes: {stdout_bytes}"),
        format!("- stderr_bytes: {stderr_bytes}"),
        format!("Interpretation: {interpretation}"),
    ];
    if stdout_visible {
        lines.extend([
            String::new(),
            "stdout:".into(),
            result
                .stdout
                .trim_end_matches(crate::runtime::tools::js_space)
                .into(),
        ]);
    }
    if stderr_visible {
        lines.extend([
            String::new(),
            "stderr:".into(),
            result
                .stderr
                .trim_end_matches(crate::runtime::tools::js_space)
                .into(),
        ]);
    }
    let assertions = object([
        ("commandSucceeded", JsValue::Bool(true)),
        ("stdoutVisible", JsValue::Bool(stdout_visible)),
        ("stderrVisible", JsValue::Bool(stderr_visible)),
        ("retrievedDataAvailable", JsValue::Bool(stdout_visible)),
        ("stdoutBytes", JsValue::Number(stdout_bytes as f64)),
        ("stderrBytes", JsValue::Number(stderr_bytes as f64)),
    ]);
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(lines.join("\n").as_str())),
        ]))],
        data: Some(JsValue::Object(object([
            ("command", JsValue::String(command)),
            ("exitCode", exit_code),
            ("stdout", string(result.stdout.as_str())),
            ("stderr", string(result.stderr.as_str())),
            ("timedOut", JsValue::Bool(result.timed_out)),
            ("durationMs", JsValue::Number(result.duration_ms)),
            ("outputState", string(output_state)),
            ("assertions", JsValue::Object(assertions)),
        ]))),
        metadata: None,
        supplemental_messages: None,
    })
}

fn format_shell_failure(command: &JsString, exit_text: &str, result: &CommandResult) -> JsString {
    let mut out = utf16(&format!("Command exited with code {exit_text}: "));
    out.extend_from_slice(command.units());
    if !result.stderr.is_empty() {
        out.extend(utf16("\n\nstderr:\n"));
        out.extend(utf16(
            result
                .stderr
                .trim_end_matches(crate::runtime::tools::js_space),
        ));
    }
    if !result.stdout.is_empty() {
        out.extend(utf16("\n\nstdout:\n"));
        out.extend(utf16(
            result
                .stdout
                .trim_end_matches(crate::runtime::tools::js_space),
        ));
    }
    JsString::from_utf16(out)
}

struct Diagnostics {
    traceback_marker: Regex,
    file_line: Regex,
    exception: Regex,
    missing_module: Regex,
    command_not_found: Regex,
    syntax_error: Regex,
}

static DIAGNOSTICS: LazyLock<Diagnostics> = LazyLock::new(|| {
    let s = ws();
    let build = |pattern: String| Regex::new(&pattern).expect("bash diagnostic pattern");
    Diagnostics {
        traceback_marker: build(r"Traceback \(most recent call last\):".into()),
        file_line: build(format!(
            r#"^{s}*File "([^"]+)", line ([0-9]+)(?:, in ({DOT}*))?"#
        )),
        exception: build(format!(
            r"^[A-Za-z_][A-Za-z0-9_.]*(?:Error|Exception|Warning){B}"
        )),
        missing_module: build(
            r#"(?:ModuleNotFoundError|ImportError): No module named ['"]([^'"]+)['"]"#.into(),
        ),
        command_not_found: build(format!(
            r"(?:^|\n)(?:{DOT}*?:{s}*)?([^{}:]+): command not found{B}",
            crate::metadata::SPACE
        )),
        syntax_error: build(format!(r"(?:^|\n){s}*(SyntaxError:[^\n]+)")),
    }
});

/// `text.split(/\r?\n/)` on a Rust string.
fn split_lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

fn parse_python_traceback(text: &str) -> Option<(String, Option<String>)> {
    if !DIAGNOSTICS.traceback_marker.is_match(text) {
        return None;
    }
    let lines = split_lines(text);
    let mut location = None;
    for line in &lines {
        if let Some(captures) = DIAGNOSTICS.file_line.captures(line) {
            let function = captures
                .get(3)
                .map(|m| m.as_str())
                .filter(|m| !m.is_empty())
                .map(|m| format!(" in {}", js_trim_str(m)))
                .unwrap_or_default();
            location = Some(format!("{}:{}{function}", &captures[1], &captures[2]));
        }
    }
    let exception = lines
        .iter()
        .rev()
        .map(|line| js_trim_str(line))
        .find(|line| DIAGNOSTICS.exception.is_match(line))
        .map_or_else(|| "Python traceback".to_string(), str::to_string);
    Some((exception, location))
}

/// `tailSnippet(value, maxChars)` on UTF-16 units.
fn tail_snippet(value: &str, max: usize) -> Vec<u16> {
    let units = utf16(value);
    let trimmed = trim_end_units(&units);
    if trimmed.is_empty() {
        return Vec::new();
    }
    if trimmed.len() <= max {
        return trimmed.to_vec();
    }
    let mut out = utf16(&format!(
        "... [{} chars omitted] ...\n",
        trimmed.len() - max
    ));
    out.extend_from_slice(&trimmed[trimmed.len() - max..]);
    out
}

fn indent_block(value: &[u16]) -> Vec<u16> {
    let lines: Vec<Vec<u16>> = split_lines_units(value)
        .into_iter()
        .map(|line| {
            let mut indented = utf16("  ");
            indented.extend_from_slice(line);
            indented
        })
        .collect();
    join_lines(&lines)
}

fn format_shell_failure_diagnostic(exit_text: &str, result: &CommandResult) -> JsString {
    let stream = if js_trim_str(&result.stderr).is_empty() {
        &result.stdout
    } else {
        &result.stderr
    };
    let mut lines: Vec<Vec<u16>> = vec![
        utf16("BASH_FAILURE_DIAGNOSTIC"),
        utf16(&format!("- exit_code: {exit_text}")),
    ];
    let mut push = |line: String| lines.push(utf16(&line));
    if let Some((exception, location)) = parse_python_traceback(stream) {
        push(format!("- likely_cause: {exception}"));
        if let Some(location) = location {
            push(format!("- failing_location: {location}"));
        }
        push("- next_step: inspect the failing file/line, fix that specific cause, then rerun the smallest command that exercises it.".into());
    } else if let Some(module) = DIAGNOSTICS.missing_module.captures(stream) {
        push(format!(
            "- likely_cause: missing Python module {}",
            &module[1]
        ));
        push("- next_step: install the missing module if allowed, or rewrite the script to use available dependencies.".into());
    } else if let Some(command) = DIAGNOSTICS.command_not_found.captures(stream) {
        push(format!(
            "- likely_cause: command not found: {}",
            &command[1]
        ));
        push(
            "- next_step: check whether the command is installed or use an available equivalent."
                .into(),
        );
    } else if let Some(syntax) = DIAGNOSTICS.syntax_error.captures(stream) {
        push(format!("- likely_cause: {}", js_trim_str(&syntax[1])));
        push("- next_step: fix the syntax in the saved script or command, then rerun a minimal check.".into());
    } else {
        push(
            "- likely_cause: command exited non-zero; inspect stderr/stdout below for the root cause."
                .into(),
        );
        push(
            "- next_step: change the command or script before retrying; do not rerun unchanged."
                .into(),
        );
    }
    let stderr_tail = tail_snippet(&result.stderr, STREAM_DIAGNOSTIC_MAX_CHARS);
    if !stderr_tail.is_empty() {
        lines.push(utf16("- stderr_tail:"));
        lines.push(indent_block(&stderr_tail));
    }
    let stdout_tail = tail_snippet(&result.stdout, STREAM_DIAGNOSTIC_MAX_CHARS);
    if !stdout_tail.is_empty() && !contains_units(&stderr_tail, &stdout_tail) {
        lines.push(utf16("- stdout_tail:"));
        lines.push(indent_block(&stdout_tail));
    }
    JsString::from_utf16(join_lines(&lines))
}
