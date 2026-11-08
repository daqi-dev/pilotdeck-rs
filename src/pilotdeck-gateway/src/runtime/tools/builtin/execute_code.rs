//! Python orchestration with same-turn, fully gated nested tool calls.
#![cfg(unix)]
#[cfg(test)]
mod boundary_tests;
mod process;
mod rpc;
#[cfg(test)]
mod syntax_tests;
#[cfg(test)]
mod tests;
use super::{object, schema, string, text};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::{
        project::SandboxMode,
        tools::{
            ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext,
            builtin::bash::runner::AbortSignal, sandbox::NativeSandbox,
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::net::TcpListener;
const HELPERS: [&str; 8] = [
    "web_search",
    "web_fetch",
    "read_file",
    "write_file",
    "edit_file",
    "grep",
    "glob",
    "bash",
];
const MAX_CODE_BYTES: usize = 1024 * 1024;
pub struct ExecuteCodeTool {
    fields: JsObject,
    sandbox: Option<Arc<NativeSandbox>>,
}
impl Default for ExecuteCodeTool {
    fn default() -> Self {
        Self::new()
    }
}
impl ExecuteCodeTool {
    pub fn new() -> Self {
        Self::with_sandbox(None)
    }
    pub fn with_sandbox(sandbox: Option<Arc<NativeSandbox>>) -> Self {
        Self {
            sandbox,
            fields: object([
                ("name", string("execute_code")),
                ("kind", string("custom")),
                (
                    "description",
                    string(
                        "Run a local Python 3 script using import pilotdeck_tools. Helpers: web_search, web_fetch, read_file, write_file, edit_file, grep, glob, bash. Helpers re-enter this turn's permission, hooks and file-freshness checks. Only the final stdout/stderr summary returns to the model. Agent, task, notebook, MCP and recursive execute_code helpers are unavailable. Python inherits the tool environment; never print secrets. Read-only automatic permissions are enforced by a real filesystem sandbox; approved executions still obey the project sandbox.",
                    ),
                ),
                (
                    "inputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","required":["code"],"additionalProperties":false,"properties":{"code":{"type":"string"},"description":{"type":"string"},"timeout_seconds":{"type":"integer"},"max_tool_calls":{"type":"integer"}}}"#,
                    )),
                ),
                (
                    "outputSchema",
                    JsValue::Object(schema(
                        r#"{"type":"object","properties":{"status":{"type":"string"},"output":{"type":"string"},"error":{"type":"string"},"tool_calls_made":{"type":"integer"},"duration_seconds":{"type":"number"},"tool_call_log":{"type":"array"}}}"#,
                    )),
                ),
            ]),
        }
    }
}
impl ToolDefinition for ExecuteCodeTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(read_only(input)))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}
fn issue(path: &str, message: impl Into<JsString>) -> JsObject {
    object([
        ("path", string(path)),
        ("code", string("invalid_schema")),
        ("message", string(message)),
    ])
}
fn validation(input: &JsValue) -> Vec<JsObject> {
    let mut issues = Vec::new();
    if input
        .get("code")
        .and_then(JsValue::as_string)
        .is_none_or(|v| {
            text(v)
                .trim_matches(crate::runtime::tools::js_space)
                .is_empty()
        })
    {
        issues.push(issue("$.code", "$.code must not be empty."));
    }
    for (key, min, max) in [("timeout_seconds", 1., 300.), ("max_tool_calls", 0., 50.)] {
        if let Some(n) = input.get(key).and_then(JsValue::as_number)
            && (n < min || n > max)
        {
            issues.push(issue(
                &format!("$.{key}"),
                format!("$.{key} must be between {min} and {max}."),
            ));
        }
    }
    issues
}
impl ExecutableTool for ExecuteCodeTool {
    fn enforces_native_sandbox(
        &self,
        _context: &ToolRunContext,
        provider: &Arc<NativeSandbox>,
    ) -> bool {
        self.sandbox
            .as_ref()
            .is_some_and(|sandbox| Arc::ptr_eq(sandbox, provider))
    }

    fn validate_input(
        &self,
        input: JsValue,
        context: ToolRunContext,
    ) -> ToolFuture<Option<Vec<JsObject>>> {
        Box::pin(async move {
            let mut issues = validation(&input);
            if issues.is_empty() {
                let source = input
                    .get("code")
                    .and_then(JsValue::as_string)
                    .map(text)
                    .unwrap_or_default();
                if source.len() > MAX_CODE_BYTES {
                    return Err(ToolError::new(
                        "code_input_limit",
                        "execute_code source exceeded 1MiB.",
                    ));
                }
                // compile() only parses; source is stdin data, never argv code.
                let env = environment(&context);
                let cwd = context
                    .fields
                    .get("cwd")
                    .and_then(JsValue::as_string)
                    .map(text)
                    .unwrap_or_else(|| ".".into());
                let result = process::run(
                    process::Request {
                        executable: "python3".into(),
                        args: vec!["-c".into(), SYNTAX_CHECKER.into(), "execute_code.py".into()],
                        cwd,
                        env,
                        stdin: Some(source.into_bytes()),
                        timeout: Duration::from_secs(2),
                        stop: None,
                    },
                    &context,
                )
                .await;
                // Baseline diagnostics are advisory when Python is missing,
                // checker fails to spawn, times out or emits no JSON.
                if let Ok(result) = result
                    && !result.timed_out
                    && result.exit_code != Some(0)
                    && let Some(diagnostic) = syntax_issue(&result.stdout)
                {
                    issues.push(diagnostic);
                }
            }
            Ok((!issues.is_empty()).then_some(issues))
        })
    }
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let sandbox = self.sandbox.clone();
        Box::pin(async move {
            let started = context
                .now_ms()
                .map_err(|e| ToolError::failed(e.to_string()))?;
            let mut state = None;
            let result = run(&input, &context, sandbox, &mut state).await;
            let (status, output, error) = match result {
                Ok(value) => value,
                Err(error) => (
                    "error".to_string(),
                    String::new(),
                    Some(text(&error.message)),
                ),
            };
            let ended = context
                .now_ms()
                .map_err(|e| ToolError::failed(e.to_string()))?;
            let (count, log) = state
                .map(|state| {
                    let count = state.made.load(std::sync::atomic::Ordering::SeqCst);
                    let log = state.log.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    (count, log)
                })
                .unwrap_or_default();
            Ok(output_value(
                &context,
                &status,
                &output,
                error.as_deref(),
                count,
                (ended - started) / 1000.,
                log,
            ))
        })
    }
}
// syntaxDiagnostics.ts clips the checker string before JSON parsing; its
// MAX_CHECKER_OUTPUT_BYTES name is historical, but String.slice counts UTF-16.
fn syntax_issue(stdout: &str) -> Option<JsObject> {
    let clipped = stdout.encode_utf16().take(16_384).collect::<Vec<_>>();
    let value = pilotdeck_protocol::parse_json(&String::from_utf16_lossy(&clipped)).ok()?;
    if !matches!(&value, JsValue::Array(_) | JsValue::Object(_)) {
        return None;
    }
    let positive = |key: &str| {
        value
            .get(key)
            .and_then(JsValue::as_number)
            .filter(|n| n.is_finite())
            .map(f64::floor)
            .filter(|n| *n > 0.)
            .unwrap_or(1.)
    };
    let line = positive("line");
    let column = positive("column");
    let message = value
        .get("message")
        .and_then(JsValue::as_string)
        .filter(|message| !message.is_empty())
        .cloned()
        .unwrap_or_else(|| "Invalid Python syntax.".into());
    let compact = compact_diagnostic(&message);
    let mut full = format!("Python syntax error at L{line}:{column}: ")
        .encode_utf16()
        .collect::<Vec<_>>();
    full.extend_from_slice(compact.units());
    Some(issue("$.code", JsString::from_utf16(full)))
}
fn compact_diagnostic(message: &JsString) -> JsString {
    let space =
        |unit: u16| char::from_u32(u32::from(unit)).is_some_and(crate::runtime::tools::js_space);
    let mut compact = Vec::new();
    let mut pending_space = false;
    for unit in message.units() {
        if space(*unit) {
            if !compact.is_empty() {
                pending_space = true;
            }
        } else {
            if pending_space {
                compact.push(32);
                pending_space = false;
            }
            compact.push(*unit);
        }
    }
    if compact.len() > 220 {
        compact.truncate(217);
        while compact.last().is_some_and(|unit| space(*unit)) {
            compact.pop();
        }
        compact.extend_from_slice(&[46, 46, 46]);
    }
    JsString::from_utf16(compact)
}
const SYNTAX_CHECKER: &str = "import json,sys\ntry:\n compile(sys.stdin.read(),sys.argv[1],'exec')\nexcept (SyntaxError,ValueError) as e:\n print(json.dumps({'line':getattr(e,'lineno',1) or 1,'column':getattr(e,'offset',1) or 1,'message':getattr(e,'msg',str(e))}))\n sys.exit(1)\n";
fn environment(context: &ToolRunContext) -> Vec<(String, String)> {
    if let Some(env) = context.fields.get("env").and_then(JsValue::as_object) {
        env.own_entries()
            .iter()
            .filter_map(|(key, value)| value.as_string().map(|v| (text(key), text(v))))
            .collect()
    } else {
        std::env::vars_os()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }
}
fn set_env(env: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(entry) = env.iter_mut().find(|(k, _)| k == key) {
        entry.1 = value
    } else {
        env.push((key.into(), value));
    }
}
fn get_env<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}
struct Workspace {
    path: PathBuf,
    dev: u64,
    ino: u64,
    // Keep the original inode alive: an unlink/recreate cannot reuse its ino.
    directory: fs::File,
}
impl Workspace {
    fn create() -> Result<Self, ToolError> {
        let path =
            std::env::temp_dir().join(format!("pilotdeck_execute_code_{}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|e| ToolError::failed(e.to_string()))?;
        let directory = rustix::fs::open(
            &path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::empty(),
        )
        .map(fs::File::from)
        .map_err(|e| ToolError::failed(e.to_string()))?;
        let meta = directory
            .metadata()
            .map_err(|e| ToolError::failed(e.to_string()))?;
        Ok(Self {
            path,
            dev: meta.dev(),
            ino: meta.ino(),
            directory,
        })
    }
    fn write(&self, name: &str, bytes: &[u8]) -> Result<(), ToolError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.path.join(name))
            .map_err(|e| ToolError::failed(e.to_string()))?;
        file.write_all(bytes)
            .map_err(|e| ToolError::failed(e.to_string()))
    }
    fn cleanup(&self) -> Result<(), ToolError> {
        let changed = || {
            ToolError::new(
                "execution_cleanup_failed",
                "execute_code private workspace identity changed; refusing to remove a replacement or claim successful cleanup.",
            )
        };
        let meta = match fs::symlink_metadata(&self.path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let original = self
                    .directory
                    .metadata()
                    .map_err(|e| ToolError::failed(e.to_string()))?;
                return if original.nlink() == 0 {
                    Ok(())
                } else {
                    Err(changed())
                };
            }
            Err(error) => {
                return Err(ToolError::new(
                    "execution_cleanup_failed",
                    error.to_string(),
                ));
            }
        };
        if !meta.is_dir() || meta.dev() != self.dev || meta.ino() != self.ino {
            return Err(changed());
        }
        fs::remove_dir_all(&self.path)
            .map_err(|error| ToolError::new("execution_cleanup_failed", error.to_string()))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        // Drop is a cancellation backstop. The normal completed path checks
        // cleanup explicitly and cannot report success on an unremoved root.
        let _ = self.cleanup();
    }
}
fn explicitly_authorized(context: &ToolRunContext) -> bool {
    let Some(decision) = context.fields.get("currentPermissionDecision") else {
        return false;
    };
    if decision.get("type") != Some(&string("allow")) {
        return false;
    }
    match decision.get("reason") {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => true, // exact native once-grant
        Some(reason) => {
            let kind = reason.get("type").and_then(JsValue::as_string);
            kind.is_some_and(|v| v.equals_str("rule"))
                || (kind.is_some_and(|v| v.equals_str("mode"))
                    && reason
                        .get("mode")
                        .and_then(JsValue::as_string)
                        .is_some_and(|v| v.equals_str("bypassPermissions")))
        }
    }
}
async fn run(
    input: &JsValue,
    context: &ToolRunContext,
    sandbox: Option<Arc<NativeSandbox>>,
    state: &mut Option<Arc<rpc::State>>,
) -> Result<(String, String, Option<String>), ToolError> {
    let Some(hook) = context.execute_tool.clone() else {
        return Ok(("unsupported".into(),"".into(),Some("execute_code requires a ToolRuntime recursion hook, but this host did not provide one.".into())));
    };
    let issues = validation(input);
    if !issues.is_empty() {
        return Err(ToolError::failed("Invalid execute_code input.").with_details(issues));
    }
    for (key, min, max) in [("timeout_seconds", 1., 300.), ("max_tool_calls", 0., 50.)] {
        if let Some(n) = input.get(key).and_then(JsValue::as_number)
            && (!n.is_finite() || n.fract() != 0. || n < min || n > max)
        {
            return Err(ToolError::failed(format!("Invalid execute_code {key}.")));
        }
    }
    let mut env = environment(context);
    let cwd = context
        .fields
        .get("cwd")
        .and_then(JsValue::as_string)
        .map(text)
        .unwrap_or_else(|| ".".into());
    let mut python = None;
    for candidate in ["python3", "python"] {
        if context.is_aborted() {
            return Err(ToolError::aborted());
        }
        if let Ok(result) = process::run(
            process::Request {
                executable: candidate.into(),
                args: vec!["--version".into()],
                cwd: cwd.clone(),
                env: env.clone(),
                stdin: None,
                timeout: Duration::from_secs(2),
                stop: None,
            },
            context,
        )
        .await
            && result.exit_code == Some(0)
        {
            python = Some(candidate.to_string());
            break;
        }
    }
    let Some(python) = python else {
        return Ok((
            "unsupported".into(),
            "".into(),
            Some("execute_code requires python3 on PATH.".into()),
        ));
    };
    let workspace = Workspace::create()?;
    let code = input
        .get("code")
        .and_then(JsValue::as_string)
        .map(text)
        .unwrap_or_default();
    if code.len() > MAX_CODE_BYTES {
        return Err(ToolError::new(
            "code_input_limit",
            "execute_code source exceeded 1MiB.",
        ));
    }
    workspace.write(
        "pilotdeck_tools.py",
        include_bytes!("execute_code/helper.py"),
    )?;
    workspace.write("script.py", code.as_bytes())?;
    // Full arbitrary Python requires an actual permission grant. The baseline
    // lexical heuristic is retained for policy classification, but its automatic
    // allowance never grants filesystem mutation authority.
    let constrained = if !explicitly_authorized(context) {
        Arc::new(NativeSandbox::new(
            SandboxMode::ReadOnly,
            workspace.path.clone(),
        ))
    } else {
        sandbox.unwrap_or_else(|| {
            Arc::new(NativeSandbox::new(
                SandboxMode::DangerFullAccess,
                workspace.path.clone(),
            ))
        })
    };
    let mode = constrained.mode();
    let signal = AbortSignal::from_context(context);
    let (executable, args) = constrained
        .prepare(
            python.into(),
            vec![
                workspace
                    .path
                    .join("script.py")
                    .to_string_lossy()
                    .into_owned()
                    .into(),
            ],
            &JsString::from(cwd.clone()),
            signal.as_ref(),
        )
        .await?;
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| ToolError::failed(e.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|e| ToolError::failed(e.to_string()))?
        .port();
    let token = uuid::Uuid::new_v4().to_string();
    let allowed = HELPERS
        .iter()
        .copied()
        .filter(|name| {
            mode != SandboxMode::ReadOnly || !["bash", "write_file", "edit_file"].contains(name)
        })
        .collect();
    let rpc = Arc::new(rpc::State::new(
        hook,
        Some(token.clone()),
        allowed,
        context
            .fields
            .get("currentToolCallId")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "execute_code".into()),
        input
            .get("max_tool_calls")
            .and_then(JsValue::as_number)
            .unwrap_or(50.) as usize,
        context.clone(),
    ));
    *state = Some(rpc.clone());
    let old_python_path = get_env(&env, "PYTHONPATH")
        .filter(|v| !v.is_empty())
        .map(str::to_owned);
    let private = workspace.path.to_string_lossy().into_owned();
    set_env(
        &mut env,
        "PYTHONPATH",
        old_python_path.map_or_else(|| private.clone(), |old| format!("{private}:{old}")),
    );
    set_env(&mut env, "PILOTDECK_WORKSPACE_CWD", cwd.clone());
    set_env(&mut env, "PILOTDECK_EXECUTE_CODE_TEMP_ROOT", private);
    set_env(&mut env, "PILOTDECK_RPC_HOST", "127.0.0.1".into());
    set_env(&mut env, "PILOTDECK_RPC_PORT", port.to_string());
    set_env(&mut env, "PILOTDECK_RPC_TOKEN", token);
    set_env(&mut env, "PYTHONDONTWRITEBYTECODE", "1".into());
    // Remove stale alternate-transport authority inherited from an outer tool.
    env.retain(|(key, _)| key != "PILOTDECK_RPC_SOCKET");
    let seconds = input
        .get("timeout_seconds")
        .and_then(JsValue::as_number)
        .unwrap_or(300.);
    let stop = crate::runtime::network::Cancellation::new();
    let request = process::Request {
        executable: text(&executable),
        args: args.iter().map(text).collect(),
        cwd,
        env,
        stdin: None,
        timeout: Duration::from_secs_f64(seconds),
        stop: Some(stop.clone()),
    };
    let mut server = rpc::Server::new(listener, rpc);
    let process = process::run(request, context);
    tokio::pin!(process);
    let result = tokio::select! {
        result=&mut process=>result,
        failure=server.serve()=>{
            // Keep owning the real Python cleanup when RPC fails. Dropping
            // its future here would defer reaping until after tool return.
            stop.cancel();
            let settled=tokio::time::timeout(Duration::from_secs(5),&mut process).await
                .map_err(|_|ToolError::failed("execute_code Python cancellation could not be confirmed."));
            match settled {
                Err(error)=>Err(error),
                Ok(Err(error))=>Err(error),
                Ok(Ok(_))=>match failure {Err(error)=>Err(error),Ok(())=>Err(ToolError::failed("execute_code RPC listener stopped."))},
            }
        }
    };
    let drained = server.shutdown().await;
    let cleaned = workspace.cleanup();
    let result = result?;
    drained?;
    cleaned?;
    Ok(finish_execution(result, seconds))
}
fn finish_execution(result: process::ResultData, seconds: f64) -> (String, String, Option<String>) {
    let status = if result.cancelled {
        "cancelled"
    } else if result.timed_out {
        "timeout"
    } else if result.exit_code != Some(0) {
        "error"
    } else {
        "success"
    };
    let error = match status {
        "cancelled" => Some("Script execution was cancelled.".into()),
        "timeout" => Some(format!("Script timed out after {seconds}s and was killed.")),
        "error" => Some(if result.stderr.is_empty() {
            format!(
                "Script exited with code {}.",
                result
                    .exit_code
                    .map_or_else(|| "unknown".into(), |n| n.to_string())
            )
        } else {
            result.stderr.clone()
        }),
        _ => None,
    };
    let output = if status == "error" && !result.stderr.is_empty() {
        format!("{}\n--- stderr ---\n{}", result.stdout, result.stderr)
            .trim()
            .to_string()
    } else {
        result.stdout
    };
    (
        status.into(),
        strip_ansi(&output),
        error.map(|e| strip_ansi(&e)),
    )
}
fn strip_ansi(s: &str) -> String {
    regex::Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]")
        .expect("ANSI pattern")
        .replace_all(s, "")
        .into_owned()
}
fn output_value(
    context: &ToolRunContext,
    status: &str,
    output: &str,
    error: Option<&str>,
    made: usize,
    duration: f64,
    log: Vec<JsValue>,
) -> ToolOutput {
    let duration = (duration * 100.).round() / 100.;
    let mut data = object([("status", string(status)), ("output", string(output))]);
    if let Some(error) = error.filter(|s| !s.is_empty()) {
        data.insert("error".into(), string(error));
    }
    data.insert("tool_calls_made".into(), JsValue::Number(made as f64));
    data.insert("duration_seconds".into(), JsValue::Number(duration));
    data.insert("tool_call_log".into(), JsValue::Array(log));
    let mut message = format!(
        "status: {status}\nduration_seconds: {}\ntool_calls_made: {made}",
        crate::js_value::number_string(duration)
    );
    if let Some(error) = error.filter(|s| !s.is_empty()) {
        message.push_str("\nerror: ");
        message.push_str(error);
    }
    if !output.is_empty() {
        message.push_str("\n\n");
        message.push_str(output);
    }
    ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(message)),
        ]))],
        data: Some(JsValue::Object(data)),
        metadata: Some(object([
            ("status", string(status)),
            ("tool_calls_made", JsValue::Number(made as f64)),
            ("duration_seconds", JsValue::Number(duration)),
            (
                "cwd",
                context
                    .fields
                    .get("cwd")
                    .cloned()
                    .unwrap_or(JsValue::Undefined),
            ),
            ("env_inheritance", string("full")),
            ("python_path_augmented", JsValue::Bool(true)),
        ])),
        ..ToolOutput::default()
    }
}
fn read_only(input: &JsValue) -> bool {
    let Some(code) = input
        .get("code")
        .and_then(JsValue::as_string)
        .filter(|v| !v.is_empty())
    else {
        return false;
    };
    let code = code.units();
    let mut mask = code.to_vec();
    let mut i = 0;
    while i < code.len() {
        if code[i] == 35 {
            while i < code.len() && code[i] != 10 {
                mask[i] = 32;
                i += 1;
            }
            continue;
        }
        if matches!(code[i], 34 | 39) {
            let quote = code[i];
            let triple = i + 2 < code.len() && code[i..i + 3].iter().all(|u| *u == quote);
            let width = if triple { 3 } else { 1 };
            for _ in 0..width {
                mask[i] = 32;
                i += 1;
            }
            while i < code.len() {
                if code[i] == 92 {
                    mask[i] = 32;
                    i += 1;
                    if i < code.len() {
                        mask[i] = 32;
                        i += 1;
                    }
                    continue;
                }
                if i + width <= code.len() && code[i..i + width].iter().all(|u| *u == quote) {
                    for _ in 0..width {
                        mask[i] = 32;
                        i += 1;
                    }
                    break;
                }
                if code[i] != 10 {
                    mask[i] = 32;
                }
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    let word = |u: u16| matches!(u,48..=57|65..=90|97..=122|95);
    for name in ["write_file", "edit_file", "bash"] {
        let token: Vec<_> = name.encode_utf16().collect();
        for offset in 0..mask.len() {
            if offset > 0 && word(mask[offset - 1]) {
                continue;
            }
            if !mask[offset..].starts_with(&token) {
                continue;
            }
            let mut at = offset + token.len();
            while at < mask.len()
                && crate::runtime::tools::js_space(
                    char::from_u32(mask[at] as u32).unwrap_or('\u{fffd}'),
                )
            {
                at += 1;
            }
            if mask.get(at) != Some(&40) {
                continue;
            }
            if name != "bash" {
                return false;
            }
            let Some(command) = first_string(code, at + 1) else {
                return false;
            };
            if !super::bash::is_read_only_shell_command(&text(&command)) {
                return false;
            }
        }
    }
    true
}
fn first_string(code: &[u16], mut i: usize) -> Option<JsString> {
    while i < code.len()
        && crate::runtime::tools::js_space(char::from_u32(code[i] as u32).unwrap_or('\u{fffd}'))
    {
        i += 1;
    }
    let quote = *code.get(i)?;
    if !matches!(quote, 34 | 39) {
        return None;
    }
    let width = if i + 2 < code.len() && code[i..i + 3].iter().all(|v| *v == quote) {
        3
    } else {
        1
    };
    i += width;
    let mut out = Vec::new();
    while i < code.len() {
        if code[i] == 92 {
            let escaped = *code.get(i + 1)?;
            out.push(match escaped {
                110 => 10,
                116 => 9,
                _ => escaped,
            });
            i += 2;
            continue;
        }
        if i + width <= code.len() && code[i..i + width].iter().all(|v| *v == quote) {
            return Some(JsString::from_utf16(out));
        }
        out.push(code[i]);
        i += 1;
    }
    None
}
