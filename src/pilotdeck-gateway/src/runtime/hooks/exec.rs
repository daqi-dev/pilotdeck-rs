//! Hook executors and `parseHookOutput` (TS `extension/hooks/execution/*`).
//!
//! Command hooks run the same shell shape as TS (`/bin/bash -c` on POSIX) in a
//! new process group, so timeout/abort cleanup reaps descendants rather than
//! only the shell. HTTP hooks use the workspace's existing hyper client: plain
//! `http://` only, because adding a TLS crate is outside this change's
//! dependency budget. Prompt/agent execution needs a model runner (A7); this
//! module keeps TS's explicit "not configured" result instead of inventing a
//! different failure mode.
use crate::hooks::protocol::{
    HookAsyncOutput, HookDecision, HookOutput, HookSpecificOutput, HookSyncOutput,
    PermissionDecision, PermissionHookDecision,
};
use crate::js_value::{self, MAX_ARRAY_DEPTH, StackOverflow};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    future::Future,
    path::Path,
    pin::Pin,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::Command,
    sync::watch,
    time::{Sleep, sleep_until},
};

/// TS `PilotDeckHookExecutorKind` (the persistent/runtime executor union).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookExecutorKind {
    Command,
    Prompt,
    Http,
    Agent,
}
impl HookExecutorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Prompt => "prompt",
            Self::Http => "http",
            Self::Agent => "agent",
        }
    }
}

pub const HOOK_TIMEOUT_MS: f64 = 10.0 * 60.0 * 1000.0;
pub const SESSION_END_HOOK_TIMEOUT_MS: f64 = 1_500.0;
/// TS `SubprocessPort` kills with SIGKILL one second after SIGTERM; the shell
/// runner used by hooks waits three seconds instead.
const KILL_GRACE: Duration = Duration::from_secs(3);
/// TS force-resolves a killed shell after this long instead of waiting forever.
const FORCE_RESOLVE: Duration = Duration::from_secs(15);
const FORCE_RESOLVE_NOTE: &str =
    "\n[PilotDeck] Process did not exit within 15s after termination; force-resolved.";

// ------------------------------------------------------------------ cancellation

/// Host-side `AbortSignal`. Cloning shares one abort state.
#[derive(Clone)]
pub struct Cancellation {
    tx: watch::Sender<bool>,
    rx: watch::Receiver<bool>,
}
impl Cancellation {
    pub fn new() -> Self {
        let (tx, rx) = watch::channel(false);
        Self { tx, rx }
    }
    pub fn abort(&self) {
        self.tx.send_replace(true);
    }
    pub fn is_aborted(&self) -> bool {
        *self.rx.borrow()
    }
    /// Resolves immediately when already aborted.
    pub async fn cancelled(&self) {
        let mut rx = self.rx.clone();
        let _ = rx.wait_for(|aborted| *aborted).await;
    }
}
impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------------ results

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookOutcome {
    Success,
    Blocking,
    NonBlockingError,
    Cancelled,
    Timeout,
}
impl HookOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Blocking => "blocking",
            Self::NonBlockingError => "non_blocking_error",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
        }
    }
}

/// `CommandHookExecutionResult`; also the HTTP/callback/prompt/agent shape.
#[derive(Clone, Debug, PartialEq)]
pub struct ExecutionResult {
    pub stdout: JsString,
    pub stderr: JsString,
    pub exit_code: Option<i64>,
    pub outcome: HookOutcome,
    pub output: HookOutput,
}
impl ExecutionResult {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("stdout".into(), JsValue::String(self.stdout.clone()));
        object.insert("stderr".into(), JsValue::String(self.stderr.clone()));
        if let Some(code) = self.exit_code {
            object.insert("exitCode".into(), JsValue::Number(code as f64));
        }
        object.insert("outcome".into(), self.outcome.as_str().into());
        object.insert("output".into(), hook_output_to_value(&self.output));
        JsValue::Object(object)
    }
    fn failure(message: JsString, cancelled: bool) -> Self {
        Self {
            stdout: JsString::default(),
            stderr: message,
            exit_code: None,
            outcome: if cancelled {
                HookOutcome::Cancelled
            } else {
                HookOutcome::NonBlockingError
            },
            output: HookOutput::Sync(Box::default()),
        }
    }
}

// ------------------------------------------------------------------ JSON helpers

/// JS `JSON.stringify` throws `RangeError` past a caller-dependent V8 stack
/// limit. The protocol encoder is iterative, so check the conservative V8
/// container bound first and report the same message TS catches.
pub(crate) fn stringify_json_value(value: &JsValue) -> Result<String, StackOverflow> {
    let mut pending = vec![(value, 0usize)];
    while let Some((value, parent)) = pending.pop() {
        if matches!(value, JsValue::Array(_) | JsValue::Object(_)) {
            let depth = parent + 1;
            if depth > MAX_ARRAY_DEPTH {
                return Err(StackOverflow);
            }
            match value {
                JsValue::Array(items) => {
                    pending.extend(items.iter().map(|item| (item, depth)));
                }
                JsValue::Object(object) => {
                    pending.extend(object.entries().iter().map(|(_, item)| (item, depth)));
                }
                _ => unreachable!(),
            }
        }
    }
    stringify_json(value).ok().flatten().ok_or(StackOverflow)
}

/// JS `String(value)` for messages; iteration-bounded by `js_value`.
pub(crate) fn js_string(value: &JsValue) -> Result<JsString, StackOverflow> {
    Ok(JsString::from_utf16(js_value::to_string_units(value)?))
}

pub(crate) fn object_stringify_with_bounded_stack(
    object: &JsObject,
) -> Result<String, StackOverflow> {
    stringify_json_value(&JsValue::Object(object.clone()))
}

// ------------------------------------------------------------------ parseHookOutput

pub fn parse_hook_output(stdout: &str) -> HookOutput {
    let Some(parsed) = parse_first_json_line(stdout) else {
        return HookOutput::Sync(Box::default());
    };
    let Some(record) = parsed.as_object() else {
        return HookOutput::Sync(Box::default());
    };
    if record.get("async").and_then(JsValue::as_bool) == Some(true) {
        let invocation_id = record
            .get("asyncHookId")
            .and_then(JsValue::as_string)
            .filter(|value| !is_js_blank(value))
            .cloned();
        let timeout_ms = record
            .get("asyncTimeout")
            .and_then(JsValue::as_number)
            .filter(|value| value.is_finite() && *value > 0.0)
            .map(|value| (value * 1000.0).ceil());
        return HookOutput::Async(HookAsyncOutput {
            invocation_id,
            timeout_ms,
            raw: Some(parsed),
        });
    }
    HookOutput::Sync(Box::new(HookSyncOutput {
        continue_: record.get("continue").and_then(JsValue::as_bool),
        suppress_output: record.get("suppressOutput").and_then(JsValue::as_bool),
        stop_reason: record
            .get("stopReason")
            .and_then(JsValue::as_string)
            .cloned(),
        decision: match record.get("decision").and_then(JsValue::as_string) {
            Some(value) if value.equals_str("approve") => Some(HookDecision::Approve),
            Some(value) if value.equals_str("block") => Some(HookDecision::Block),
            _ => None,
        },
        reason: record.get("reason").and_then(JsValue::as_string).cloned(),
        system_message: record
            .get("systemMessage")
            .and_then(JsValue::as_string)
            .cloned(),
        specific: parse_specific_output(record.get("hookSpecificOutput")),
        raw: Some(parsed),
    }))
}

fn parse_first_json_line(stdout: &str) -> Option<JsValue> {
    for raw in stdout.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = crate::metadata::js_trim(line);
        if !trimmed.starts_with('{') {
            continue;
        }
        return parse_json(trimmed).ok();
    }
    None
}

fn is_js_blank(value: &JsString) -> bool {
    let lossy = String::from_utf16_lossy(value.units());
    crate::metadata::js_trim(&lossy).is_empty()
}

fn parse_specific_output(value: Option<&JsValue>) -> Option<HookSpecificOutput> {
    let record = value.and_then(JsValue::as_object)?;
    let hook_event_name = record.get("hookEventName").and_then(JsValue::as_string)?;
    Some(HookSpecificOutput {
        hook_event_name: hook_event_name.clone(),
        additional_context: record
            .get("additionalContext")
            .and_then(JsValue::as_string)
            .cloned(),
        initial_user_message: record
            .get("initialUserMessage")
            .and_then(JsValue::as_string)
            .cloned(),
        watch_paths: match record.get("watchPaths") {
            Some(JsValue::Array(items)) => Some(
                items
                    .iter()
                    .filter_map(JsValue::as_string)
                    .cloned()
                    .collect(),
            ),
            _ => None,
        },
        permission_decision: match record
            .get("permissionDecision")
            .and_then(JsValue::as_string)
        {
            Some(value) if value.equals_str("allow") => Some(PermissionDecision::Allow),
            Some(value) if value.equals_str("deny") => Some(PermissionDecision::Deny),
            Some(value) if value.equals_str("ask") => Some(PermissionDecision::Ask),
            Some(value) if value.equals_str("passthrough") => Some(PermissionDecision::Passthrough),
            _ => None,
        },
        permission_decision_reason: record
            .get("permissionDecisionReason")
            .and_then(JsValue::as_string)
            .cloned(),
        updated_input: record
            .get("updatedInput")
            .and_then(JsValue::as_object)
            .cloned(),
        updated_mcp_tool_output: record.get("updatedMCPToolOutput").cloned(),
        decision: parse_permission_request_decision(record.get("decision")),
        retry: record.get("retry").and_then(JsValue::as_bool),
        worktree_path: record
            .get("worktreePath")
            .and_then(JsValue::as_string)
            .cloned(),
    })
}

fn parse_permission_request_decision(value: Option<&JsValue>) -> Option<PermissionHookDecision> {
    let value = value.and_then(JsValue::as_object)?;
    match value.get("behavior").and_then(JsValue::as_string) {
        Some(behavior) if behavior.equals_str("allow") => Some(PermissionHookDecision::Allow {
            updated_input: value
                .get("updatedInput")
                .and_then(JsValue::as_object)
                .cloned(),
            updated_permissions: match value.get("updatedPermissions") {
                Some(JsValue::Array(items)) => Some(items.clone()),
                _ => None,
            },
        }),
        Some(behavior) if behavior.equals_str("deny") => Some(PermissionHookDecision::Deny {
            message: value.get("message").and_then(JsValue::as_string).cloned(),
            interrupt: value.get("interrupt").and_then(JsValue::as_bool),
        }),
        _ => None,
    }
}

pub fn hook_output_to_value(output: &HookOutput) -> JsValue {
    match output {
        HookOutput::Sync(sync) => {
            let mut object = JsObject::new();
            object.insert("type".into(), "sync".into());
            if let Some(value) = sync.continue_ {
                object.insert("continue".into(), JsValue::Bool(value));
            }
            if let Some(value) = sync.suppress_output {
                object.insert("suppressOutput".into(), JsValue::Bool(value));
            }
            if let Some(value) = &sync.stop_reason {
                object.insert("stopReason".into(), JsValue::String(value.clone()));
            }
            if let Some(value) = sync.decision {
                object.insert(
                    "decision".into(),
                    match value {
                        HookDecision::Approve => "approve",
                        HookDecision::Block => "block",
                    }
                    .into(),
                );
            }
            if let Some(value) = &sync.reason {
                object.insert("reason".into(), JsValue::String(value.clone()));
            }
            if let Some(value) = &sync.system_message {
                object.insert("systemMessage".into(), JsValue::String(value.clone()));
            }
            if let Some(value) = &sync.specific {
                object.insert("specific".into(), specific_to_value(value));
            }
            if let Some(value) = &sync.raw {
                object.insert("raw".into(), value.clone());
            }
            JsValue::Object(object)
        }
        HookOutput::Async(r#async) => {
            let mut object = JsObject::new();
            object.insert("type".into(), "async".into());
            if let Some(value) = &r#async.invocation_id {
                object.insert("invocationId".into(), JsValue::String(value.clone()));
            }
            if let Some(value) = r#async.timeout_ms {
                object.insert("timeoutMs".into(), JsValue::Number(value));
            }
            if let Some(value) = &r#async.raw {
                object.insert("raw".into(), value.clone());
            }
            JsValue::Object(object)
        }
    }
}

fn specific_to_value(specific: &HookSpecificOutput) -> JsValue {
    let mut object = JsObject::new();
    object.insert(
        "hookEventName".into(),
        JsValue::String(specific.hook_event_name.clone()),
    );
    if let Some(value) = &specific.additional_context {
        object.insert("additionalContext".into(), JsValue::String(value.clone()));
    }
    if let Some(value) = &specific.initial_user_message {
        object.insert("initialUserMessage".into(), JsValue::String(value.clone()));
    }
    if let Some(values) = &specific.watch_paths {
        object.insert(
            "watchPaths".into(),
            JsValue::Array(values.iter().cloned().map(JsValue::String).collect()),
        );
    }
    if let Some(value) = specific.permission_decision {
        object.insert(
            "permissionDecision".into(),
            match value {
                PermissionDecision::Allow => "allow",
                PermissionDecision::Deny => "deny",
                PermissionDecision::Ask => "ask",
                PermissionDecision::Passthrough => "passthrough",
            }
            .into(),
        );
    }
    if let Some(value) = &specific.permission_decision_reason {
        object.insert(
            "permissionDecisionReason".into(),
            JsValue::String(value.clone()),
        );
    }
    if let Some(value) = &specific.updated_input {
        object.insert("updatedInput".into(), JsValue::Object(value.clone()));
    }
    if let Some(value) = &specific.updated_mcp_tool_output {
        object.insert("updatedMCPToolOutput".into(), value.clone());
    }
    if let Some(value) = &specific.decision {
        let mut decision = JsObject::new();
        match value {
            PermissionHookDecision::Allow {
                updated_input,
                updated_permissions,
            } => {
                decision.insert("behavior".into(), "allow".into());
                if let Some(value) = updated_input {
                    decision.insert("updatedInput".into(), JsValue::Object(value.clone()));
                }
                if let Some(values) = updated_permissions {
                    decision.insert("updatedPermissions".into(), JsValue::Array(values.clone()));
                }
            }
            PermissionHookDecision::Deny { message, interrupt } => {
                decision.insert("behavior".into(), "deny".into());
                if let Some(value) = message {
                    decision.insert("message".into(), JsValue::String(value.clone()));
                }
                if let Some(value) = interrupt {
                    decision.insert("interrupt".into(), JsValue::Bool(*value));
                }
            }
        }
        object.insert("decision".into(), JsValue::Object(decision));
    }
    if let Some(value) = specific.retry {
        object.insert("retry".into(), JsValue::Bool(value));
    }
    if let Some(value) = &specific.worktree_path {
        object.insert("worktreePath".into(), JsValue::String(value.clone()));
    }
    JsValue::Object(object)
}

// ------------------------------------------------------------------ shell port

pub struct ShellRequest {
    pub command: JsString,
    pub cwd: JsString,
    pub env: Option<JsObject>,
    pub timeout_ms: f64,
    pub stdin: JsString,
    pub signal: Option<Cancellation>,
}
pub struct ShellResult {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub timed_out: bool,
}

pub type ShellFuture = Pin<Box<dyn Future<Output = Result<ShellResult, JsString>> + Send>>;
pub trait ShellPort: Send + Sync {
    fn execute(&self, request: ShellRequest) -> ShellFuture;
}

#[derive(Clone, Copy, Default)]
pub struct LocalShellPort;
impl ShellPort for LocalShellPort {
    fn execute(&self, request: ShellRequest) -> ShellFuture {
        Box::pin(async move { run_shell(request).await })
    }
}

pub(crate) fn process_env() -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in std::env::vars_os() {
        let key = key.to_string_lossy();
        let value = value.to_string_lossy();
        object.insert(
            JsString::from(key.into_owned()),
            JsValue::String(value.into_owned().into()),
        );
    }
    object
}

fn env_object(env: Option<&JsObject>) -> JsObject {
    env.cloned().unwrap_or_else(process_env)
}

fn find_on_path(name: &str, env: &JsObject) -> Option<String> {
    let path = env
        .get("PATH")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))?;
    for entry in path.split(':').filter(|entry| !entry.is_empty()) {
        let candidate = Path::new(entry).join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// `resolveDefaultCommandShell` on POSIX: configured path, `/bin/bash`, `bash`
/// on PATH, then `/bin/sh`.
fn resolve_shell(env: &JsObject) -> Result<String, JsString> {
    if let Some(configured) = env.get("PILOTDECK_SHELL_PATH").and_then(JsValue::as_string) {
        let path = String::from_utf16_lossy(configured.units());
        if Path::new(&path).is_file() {
            return Ok(path);
        }
        return Err(JsString::from(format!(
            "Configured PilotDeck shell was not found: {path}"
        )));
    }
    if Path::new("/bin/bash").is_file() {
        return Ok("/bin/bash".into());
    }
    if let Some(bash) = find_on_path("bash", env) {
        return Ok(bash);
    }
    Ok("/bin/sh".into())
}

fn spawn_error(shell: &str, error: &std::io::Error) -> JsString {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => "ENOENT",
        std::io::ErrorKind::PermissionDenied => "EACCES",
        _ => return JsString::from(error.to_string()),
    };
    JsString::from(format!("spawn {shell} {code}"))
}

fn kill_group(group: Option<rustix::process::Pid>, signal: rustix::process::Signal) {
    #[cfg(unix)]
    if let Some(group) = group {
        let _ = rustix::process::kill_process_group(group, signal);
    }
    #[cfg(not(unix))]
    let _ = (group, signal);
}

/// Kills the owned process group if the future is dropped before `reap`.
struct GroupGuard {
    group: Option<rustix::process::Pid>,
    reaped: bool,
}
impl GroupGuard {
    fn new(group: Option<rustix::process::Pid>) -> Self {
        Self {
            group,
            reaped: false,
        }
    }
    fn reap(&mut self) {
        self.reaped = true;
        self.group = None;
    }
}
impl Drop for GroupGuard {
    fn drop(&mut self) {
        if !self.reaped {
            kill_group(self.group, rustix::process::Signal::KILL);
        }
    }
}

async fn run_shell(request: ShellRequest) -> Result<ShellResult, JsString> {
    let env = env_object(request.env.as_ref());
    let shell = resolve_shell(&env)?;
    let mut command = Command::new(&shell);
    command
        .arg("-c")
        .arg(String::from_utf16_lossy(request.command.units()))
        .current_dir(String::from_utf16_lossy(request.cwd.units()))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // Node spawn with `env` replaces the environment; undefined values are
    // skipped and every other value is stringified (`null` -> "null").
    if request.env.is_some() {
        command.env_clear();
    }
    for (key, value) in env.entries() {
        if value.is_undefined() {
            continue;
        }
        let Ok(key) = key.to_utf8() else {
            continue;
        };
        let value = js_string(value).map_err(|error| JsString::from(error_message(error)))?;
        command.env(key, String::from_utf16_lossy(value.units()));
    }
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| spawn_error(&shell, &error))?;
    #[cfg(unix)]
    let group = child
        .id()
        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32));
    #[cfg(not(unix))]
    let group = None;
    let mut guard = GroupGuard::new(group);

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out = read_pipe(stdout);
    let err = read_pipe(stderr);
    if let Some(mut stdin) = child.stdin.take() {
        let payload = request.stdin.units().to_vec();
        tokio::spawn(async move {
            // EPIPE after an early exit is normal; process exit owns the outcome.
            let bytes: Vec<u8> = String::from_utf16_lossy(&payload).into_bytes();
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await;
        });
    }

    let started = tokio::time::Instant::now();
    let mut timeout_sleep: Pin<Box<Sleep>> =
        Box::pin(sleep_until(started + duration_ms(request.timeout_ms)));
    let signal = request.signal;
    let initially_aborted = signal.as_ref().is_some_and(Cancellation::is_aborted);
    let mut timed_out = false;
    let mut killed = false;
    let mut kill_sleep: Option<Pin<Box<Sleep>>> = None;
    let mut force_sleep: Option<Pin<Box<Sleep>>> = None;
    let status = loop {
        tokio::select! {
            result = child.wait() => {
                break result;
            }
            _ = &mut timeout_sleep, if !timed_out && !killed => {
                timed_out = true;
                killed = true;
                kill_group(group, rustix::process::Signal::TERM);
                kill_sleep = Some(Box::pin(sleep_until(tokio::time::Instant::now() + KILL_GRACE)));
                force_sleep = Some(Box::pin(sleep_until(tokio::time::Instant::now() + FORCE_RESOLVE)));
            }
            _ = async {
                if let Some(signal) = &signal { signal.cancelled().await } else { std::future::pending().await }
            }, if !timed_out && !killed && !initially_aborted => {
                killed = true;
                kill_group(group, rustix::process::Signal::TERM);
                kill_sleep = Some(Box::pin(sleep_until(tokio::time::Instant::now() + KILL_GRACE)));
                force_sleep = Some(Box::pin(sleep_until(tokio::time::Instant::now() + FORCE_RESOLVE)));
            }
            _ = async {
                if let Some(sleep) = kill_sleep.as_mut() { sleep.as_mut().await } else { std::future::pending().await }
            }, if kill_sleep.is_some() => {
                kill_sleep = None;
                kill_group(group, rustix::process::Signal::KILL);
            }
            _ = async {
                if let Some(sleep) = force_sleep.as_mut() { sleep.as_mut().await } else { std::future::pending().await }
            }, if force_sleep.is_some() => {
                // A child that survives SIGKILL is outside normal reach; keep
                // TS's force-resolve result but still make a bounded reaping
                // attempt so the leader cannot leak as a permanent zombie.
                let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                guard.reap();
                let stdout = tokio::time::timeout(Duration::from_secs(1), out)
                    .await
                    .unwrap_or_default();
                let mut stderr_bytes = tokio::time::timeout(Duration::from_secs(1), err)
                    .await
                    .unwrap_or_default();
                stderr_bytes.extend_from_slice(FORCE_RESOLVE_NOTE.as_bytes());
                return Ok(ShellResult {
                    exit_code: None,
                    stdout,
                    stderr: stderr_bytes,
                    timed_out: true,
                });
            }
        }
    };
    let exit_code = match status {
        Ok(status) => status.code(),
        Err(_) => None,
    };
    guard.reap();
    Ok(ShellResult {
        exit_code,
        stdout: out.await,
        stderr: err.await,
        timed_out,
    })
}

fn duration_ms(value: f64) -> Duration {
    if !value.is_finite() || value < 1.0 {
        return Duration::from_millis(1);
    }
    Duration::from_millis(value as u64)
}

async fn read_pipe(pipe: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut buffer = Vec::new();
    let _ = tokio::io::AsyncReadExt::read_to_end(&mut pipe, &mut buffer).await;
    buffer
}

fn error_message(_: StackOverflow) -> String {
    StackOverflow::MESSAGE.into()
}

// ------------------------------------------------------------------ command executor

pub struct CommandHookOptions {
    pub command: JsString,
    pub hook_input: JsObject,
    pub cwd: JsString,
    pub env: Option<JsObject>,
    pub signal: Option<Cancellation>,
    pub timeout_ms: Option<f64>,
}

#[derive(Clone)]
pub struct CommandHookExecutor {
    shell: Arc<dyn ShellPort>,
    session_cwd: Option<JsString>,
}
impl Default for CommandHookExecutor {
    fn default() -> Self {
        Self {
            shell: Arc::new(LocalShellPort),
            session_cwd: None,
        }
    }
}
impl CommandHookExecutor {
    pub fn new(shell: Arc<dyn ShellPort>) -> Self {
        Self {
            shell,
            session_cwd: None,
        }
    }
    pub fn with_session_cwd(mut self, cwd: JsString) -> Self {
        self.session_cwd = Some(cwd);
        self
    }
    pub async fn execute(&self, options: CommandHookOptions) -> ExecutionResult {
        let cancelled = options
            .signal
            .as_ref()
            .is_some_and(Cancellation::is_aborted);
        let stdin = match object_stringify_with_bounded_stack(&options.hook_input) {
            Ok(value) => JsString::from(value),
            Err(error) => return ExecutionResult::failure(error_message(error).into(), cancelled),
        };
        let timeout_ms = options.timeout_ms.unwrap_or(HOOK_TIMEOUT_MS);
        let cwd = self.session_cwd.clone().unwrap_or(options.cwd);
        match self
            .shell
            .execute(ShellRequest {
                command: options.command,
                cwd,
                env: options.env,
                timeout_ms,
                stdin,
                signal: options.signal.clone(),
            })
            .await
        {
            Ok(result) => {
                let cancelled = options
                    .signal
                    .as_ref()
                    .is_some_and(Cancellation::is_aborted);
                let exit_code = result.exit_code.map(i64::from);
                let outcome = if cancelled {
                    HookOutcome::Cancelled
                } else if result.timed_out {
                    HookOutcome::Timeout
                } else if exit_code == Some(0) {
                    HookOutcome::Success
                } else if exit_code == Some(2) {
                    HookOutcome::Blocking
                } else {
                    HookOutcome::NonBlockingError
                };
                let stdout = String::from_utf8_lossy(&result.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&result.stderr).into_owned();
                ExecutionResult {
                    output: parse_hook_output(&stdout),
                    stdout: stdout.into(),
                    stderr: stderr.into(),
                    exit_code,
                    outcome,
                }
            }
            Err(message) => {
                let cancelled = options
                    .signal
                    .as_ref()
                    .is_some_and(Cancellation::is_aborted);
                ExecutionResult::failure(message, cancelled)
            }
        }
    }
}

// ------------------------------------------------------------------ HTTP executor

pub struct HttpHookOptions {
    pub url: JsString,
    pub headers: Option<JsObject>,
    pub allowed_env_vars: Option<Vec<JsString>>,
    pub hook_input: JsObject,
    pub env: Option<JsObject>,
    pub signal: Option<Cancellation>,
}

pub struct HttpHookExecutor {
    client: hyper_util::client::legacy::Client<
        hyper_util::client::legacy::connect::HttpConnector,
        http_body_util::Full<bytes::Bytes>,
    >,
}
impl Default for HttpHookExecutor {
    fn default() -> Self {
        Self::new()
    }
}
impl HttpHookExecutor {
    pub fn new() -> Self {
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build_http();
        Self { client }
    }
    pub async fn execute(&self, options: HttpHookOptions) -> ExecutionResult {
        match self.execute_inner(options).await {
            Ok(result) => result,
            Err(message) => ExecutionResult::failure(message, false),
        }
    }
    async fn execute_inner(&self, options: HttpHookOptions) -> Result<ExecutionResult, JsString> {
        let body = object_stringify_with_bounded_stack(&options.hook_input)
            .map_err(|error| JsString::from(error_message(error)))?;
        let headers = resolve_headers(
            options.headers.as_ref(),
            options.allowed_env_vars.as_deref(),
            options.env.as_ref(),
        )?;
        let url_text = String::from_utf16_lossy(options.url.units());
        let url = url::Url::parse(&url_text)
            .map_err(|_| JsString::from(format!("Failed to parse URL from {url_text}")))?;
        if url.scheme() != "http" {
            return Err(JsString::from(format!(
                "Unsupported HTTP hook URL scheme: {}",
                url.scheme()
            )));
        }
        if options
            .signal
            .as_ref()
            .is_some_and(Cancellation::is_aborted)
        {
            return Err("This operation was aborted".into());
        }
        let mut request = hyper::Request::builder().method("POST").uri(url.as_str());
        for (name, value) in headers {
            let name = hyper::header::HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
                JsString::from(format!(
                    "Headers.append: \"{name}\" is an invalid header name."
                ))
            })?;
            let value = hyper::header::HeaderValue::from_str(&value).map_err(|_| {
                JsString::from(format!(
                    "Headers.append: \"{value}\" is an invalid header value."
                ))
            })?;
            request = request.header(name, value);
        }
        let request = request
            .body(http_body_util::Full::new(bytes::Bytes::from(body)))
            .map_err(|error| JsString::from(error.to_string()))?;
        let response = {
            let send = self.client.request(request);
            if let Some(signal) = &options.signal {
                tokio::select! {
                    response = send => response.map_err(|_| JsString::from("fetch failed"))?,
                    _ = signal.cancelled() => return Err("This operation was aborted".into()),
                }
            } else {
                send.await.map_err(|_| JsString::from("fetch failed"))?
            }
        };
        let status = response.status();
        let ok = status.is_success();
        let body = {
            use http_body_util::BodyExt;
            let collect = response.into_body().collect();
            if let Some(signal) = &options.signal {
                tokio::select! {
                    body = collect => body.map_err(|_| JsString::from("fetch failed"))?,
                    _ = signal.cancelled() => return Err("This operation was aborted".into()),
                }
            } else {
                collect.await.map_err(|_| JsString::from("fetch failed"))?
            }
        };
        let stdout = String::from_utf8_lossy(&body.to_bytes()).into_owned();
        Ok(ExecutionResult {
            output: parse_hook_output(&stdout),
            stdout: stdout.into(),
            stderr: if ok {
                JsString::default()
            } else {
                JsString::from(format!("HTTP hook returned {}.", status.as_u16()))
            },
            exit_code: Some(if ok { 0 } else { 1 }),
            outcome: if ok {
                HookOutcome::Success
            } else {
                HookOutcome::NonBlockingError
            },
        })
    }
}

fn resolve_headers(
    headers: Option<&JsObject>,
    allowed_env_vars: Option<&[JsString]>,
    env: Option<&JsObject>,
) -> Result<Vec<(String, String)>, JsString> {
    let owned_env;
    let env = match env {
        Some(env) => env,
        None => {
            owned_env = process_env();
            &owned_env
        }
    };
    let allowed = allowed_env_vars.unwrap_or_default();
    let mut output = vec![("content-type".to_owned(), "application/json".to_owned())];
    for (key, value) in headers.into_iter().flat_map(JsObject::entries) {
        let Some(value) = value.as_string() else {
            return Err(JsString::from("value.replace is not a function"));
        };
        let resolved = substitute_env(value, allowed, env)?;
        let key = key
            .to_utf8()
            .map_err(|_| JsString::from("Headers.append: invalid header name."))?;
        if let Some(entry) = output.iter_mut().find(|(name, _)| name == &key) {
            entry.1 = resolved;
        } else {
            output.push((key, resolved));
        }
    }
    Ok(output)
}

fn substitute_env(
    value: &JsString,
    allowed: &[JsString],
    env: &JsObject,
) -> Result<String, JsString> {
    let units = value.units();
    let mut output = String::new();
    let mut index = 0;
    while index < units.len() {
        if units[index] != u16::from(b'$') {
            push_unit(&mut output, units[index]);
            index += 1;
            continue;
        }
        let start = index;
        index += 1;
        let name = if units.get(index) == Some(&u16::from(b'{')) {
            index += 1;
            let name_start = index;
            while index < units.len() && is_word(units[index]) {
                index += 1;
            }
            if index == name_start || units.get(index) != Some(&u16::from(b'}')) {
                // Not `${name}`; the regex may still match `$name` after it.
                push_unit(&mut output, units[start]);
                index = start + 1;
                continue;
            }
            let name = &units[name_start..index];
            index += 1;
            name
        } else {
            let name_start = index;
            while index < units.len() && is_word(units[index]) {
                index += 1;
            }
            if index == name_start {
                push_unit(&mut output, units[start]);
                continue;
            }
            &units[name_start..index]
        };
        if allowed.iter().any(|allowed| allowed.units() == name)
            && let Some(value) = env.get_key(&JsString::from_utf16(name.to_vec()))
            && !value.is_undefined()
            && !matches!(value, JsValue::Null)
        {
            output.push_str(&String::from_utf16_lossy(
                js_string(value)
                    .map_err(|error| JsString::from(error_message(error)))?
                    .units(),
            ));
        }
    }
    Ok(output)
}

fn is_word(unit: u16) -> bool {
    matches!(unit, 48..=57 | 65..=90 | 97..=122 | 95)
}
fn push_unit(output: &mut String, unit: u16) {
    output.push_str(&String::from_utf16_lossy(&[unit]));
}

// ------------------------------------------------------------------ callback executor

#[derive(Clone, Debug, PartialEq)]
pub enum CallbackResult {
    Output(HookOutput),
    Text(JsString),
    Void,
}

pub type CallbackFuture = Pin<Box<dyn Future<Output = Result<CallbackResult, JsString>> + Send>>;

pub struct CallbackInput {
    pub hook_input: JsObject,
    pub signal: Option<Cancellation>,
}
pub trait CallbackHookHandler: Send + Sync {
    fn call(&self, input: CallbackInput) -> CallbackFuture;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CallbackState {
    #[default]
    Active,
    Draining,
    Disposed,
}
impl CallbackState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Disposed => "disposed",
        }
    }
}

struct CallbackEntry {
    handler: Arc<dyn CallbackHookHandler>,
    registration: Arc<CallbackRegistrationImpl>,
}
#[derive(Default)]
struct CallbackStateInner {
    callbacks: Vec<(JsString, CallbackEntry)>,
    state: CallbackState,
    generation: u64,
    in_flight: usize,
}
#[derive(Default)]
struct CallbackInner {
    state: Mutex<CallbackStateInner>,
    drained: watch::Sender<usize>,
}

/// TS `CallbackHookRegistration`.
#[derive(Clone)]
pub struct CallbackRegistration {
    inner: Arc<CallbackRegistrationImpl>,
}
struct CallbackRegistrationImpl {
    name: JsString,
    generation: u64,
    active: AtomicBool,
    executor: std::sync::Weak<CallbackInner>,
}
impl CallbackRegistration {
    pub fn name(&self) -> &JsString {
        &self.inner.name
    }
    pub fn generation(&self) -> u64 {
        self.inner.generation
    }
    pub fn active(&self) -> bool {
        self.inner.active.load(Ordering::SeqCst)
    }
    pub fn dispose(&self) {
        if !self.inner.active.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(inner) = self.inner.executor.upgrade() {
            inner
                .state
                .lock()
                .unwrap()
                .callbacks
                .retain(|(_, entry)| !Arc::ptr_eq(&entry.registration, &self.inner));
        }
    }
}

impl CallbackRegistrationImpl {
    fn deactivate(&self) {
        self.active.store(false, Ordering::SeqCst);
    }
}

struct CallbackInFlightGuard(Arc<CallbackInner>);
impl Drop for CallbackInFlightGuard {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap();
        state.in_flight = state.in_flight.saturating_sub(1);
        self.0.drained.send_replace(state.in_flight);
    }
}

#[derive(Clone, Default)]
pub struct CallbackHookExecutor(Arc<CallbackInner>);
impl CallbackHookExecutor {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn state(&self) -> CallbackState {
        self.0.state.lock().unwrap().state
    }
    pub fn register(
        &self,
        name: &JsString,
        handler: Arc<dyn CallbackHookHandler>,
    ) -> Result<CallbackRegistration, JsString> {
        let mut state = self.0.state.lock().unwrap();
        if state.state != CallbackState::Active {
            return Err(JsString::from(format!(
                "Cannot register a callback hook; callback hook executor is {}.",
                state.state.as_str()
            )));
        }
        let trimmed = trim_js(name);
        if trimmed.is_empty() {
            return Err("Callback hook name must not be empty.".into());
        }
        if let Some((_, existing)) = state.callbacks.iter().find(|(key, _)| key == &trimmed) {
            existing.registration.deactivate();
        }
        state.generation += 1;
        let registration = Arc::new(CallbackRegistrationImpl {
            name: trimmed.clone(),
            generation: state.generation,
            active: AtomicBool::new(true),
            executor: Arc::downgrade(&self.0),
        });
        let entry = CallbackEntry {
            handler,
            registration: registration.clone(),
        };
        if let Some(slot) = state.callbacks.iter_mut().find(|(key, _)| key == &trimmed) {
            slot.1 = entry;
        } else {
            state.callbacks.push((trimmed, entry));
        }
        Ok(CallbackRegistration {
            inner: registration,
        })
    }
    pub fn unregister(&self, name: &JsString) -> Result<bool, JsString> {
        let mut state = self.0.state.lock().unwrap();
        if state.state != CallbackState::Active {
            return Err(JsString::from(format!(
                "Cannot unregister a callback hook; callback hook executor is {}.",
                state.state.as_str()
            )));
        }
        let Some(index) = state.callbacks.iter().position(|(key, _)| key == name) else {
            return Ok(false);
        };
        let (_, entry) = state.callbacks.remove(index);
        entry.registration.deactivate();
        Ok(true)
    }
    pub async fn dispose(&self) {
        {
            let mut state = self.0.state.lock().unwrap();
            if state.state == CallbackState::Disposed {
                return;
            }
            state.state = CallbackState::Draining;
            for (_, entry) in &state.callbacks {
                entry.registration.deactivate();
            }
            state.callbacks.clear();
        }
        let mut drained = self.0.drained.subscribe();
        let _ = drained.wait_for(|count| *count == 0).await;
        self.0.state.lock().unwrap().state = CallbackState::Disposed;
    }
    pub async fn execute(&self, name: &JsString, input: CallbackInput) -> ExecutionResult {
        let handler = {
            // Lookup and in-flight accounting are one critical section: dispose
            // sets Draining and clears the map under this same lock, so an
            // execution can never start after the drain snapshot.
            let mut state = self.0.state.lock().unwrap();
            let handler = state
                .callbacks
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, entry)| entry.handler.clone());
            if handler.is_some() {
                state.in_flight += 1;
                self.0.drained.send_replace(state.in_flight);
            }
            handler
        };
        let Some(handler) = handler else {
            return ExecutionResult {
                stdout: JsString::default(),
                stderr: JsString::from(format!(
                    "Callback hook {} is not registered.",
                    String::from_utf16_lossy(name.units())
                )),
                exit_code: None,
                outcome: HookOutcome::NonBlockingError,
                output: HookOutput::Sync(Box::default()),
            };
        };
        // A dropped execute future must still release the drain counter.
        let guard = CallbackInFlightGuard(self.0.clone());
        let result = handler.call(input).await;
        drop(guard);
        match result {
            Ok(CallbackResult::Text(text)) => ExecutionResult {
                output: parse_hook_output(&String::from_utf16_lossy(text.units())),
                stdout: text,
                stderr: JsString::default(),
                exit_code: Some(0),
                outcome: HookOutcome::Success,
            },
            Ok(CallbackResult::Output(output)) => ExecutionResult {
                stdout: JsString::default(),
                stderr: JsString::default(),
                exit_code: Some(0),
                outcome: HookOutcome::Success,
                output,
            },
            Ok(CallbackResult::Void) => ExecutionResult {
                stdout: JsString::default(),
                stderr: JsString::default(),
                exit_code: Some(0),
                outcome: HookOutcome::Success,
                output: HookOutput::Sync(Box::default()),
            },
            Err(message) => ExecutionResult::failure(message, false),
        }
    }
}

fn trim_js(value: &JsString) -> JsString {
    let lossy = String::from_utf16_lossy(value.units());
    JsString::from(crate::metadata::js_trim(&lossy))
}

// ------------------------------------------------------------------ model-backed executors

pub type EvaluatorFuture = Pin<Box<dyn Future<Output = Result<JsString, JsString>> + Send>>;
pub struct EvaluatorInput {
    pub prompt: JsString,
    pub model: Option<JsString>,
    pub hook_input: JsObject,
    pub signal: Option<Cancellation>,
}
pub trait PromptEvaluator: Send + Sync {
    fn evaluate(&self, input: EvaluatorInput) -> EvaluatorFuture;
}
pub trait AgentRunner: Send + Sync {
    fn run(&self, input: EvaluatorInput) -> EvaluatorFuture;
}

#[derive(Clone, Default)]
pub struct PromptHookExecutor {
    evaluator: Option<Arc<dyn PromptEvaluator>>,
}
impl PromptHookExecutor {
    pub fn new(evaluator: Arc<dyn PromptEvaluator>) -> Self {
        Self {
            evaluator: Some(evaluator),
        }
    }
    pub async fn execute(
        &self,
        prompt: &JsString,
        model: Option<&JsString>,
        hook_input: &JsObject,
        signal: Option<Cancellation>,
    ) -> ExecutionResult {
        let Some(evaluator) = &self.evaluator else {
            return ExecutionResult::failure(
                "Prompt hook evaluator is not configured.".into(),
                false,
            );
        };
        execute_model_hook(prompt, model, hook_input, signal, |input| {
            evaluator.evaluate(input)
        })
        .await
    }
}

#[derive(Clone, Default)]
pub struct AgentHookExecutor {
    runner: Option<Arc<dyn AgentRunner>>,
}
impl AgentHookExecutor {
    pub fn new(runner: Arc<dyn AgentRunner>) -> Self {
        Self {
            runner: Some(runner),
        }
    }
    pub async fn execute(
        &self,
        prompt: &JsString,
        model: Option<&JsString>,
        hook_input: &JsObject,
        signal: Option<Cancellation>,
    ) -> ExecutionResult {
        let Some(runner) = &self.runner else {
            return ExecutionResult::failure("Agent hook runner is not configured.".into(), false);
        };
        execute_model_hook(prompt, model, hook_input, signal, |input| runner.run(input)).await
    }
}

async fn execute_model_hook<F>(
    prompt: &JsString,
    model: Option<&JsString>,
    hook_input: &JsObject,
    signal: Option<Cancellation>,
    invoke: F,
) -> ExecutionResult
where
    F: FnOnce(EvaluatorInput) -> EvaluatorFuture,
{
    let arguments = match object_stringify_with_bounded_stack(hook_input) {
        Ok(value) => value,
        Err(error) => return ExecutionResult::failure(error_message(error).into(), false),
    };
    let prompt = replace_first(prompt, "$ARGUMENTS", &arguments);
    match invoke(EvaluatorInput {
        prompt,
        model: model.cloned(),
        hook_input: hook_input.clone(),
        signal,
    })
    .await
    {
        Ok(stdout) => {
            let text = String::from_utf16_lossy(stdout.units());
            ExecutionResult {
                output: parse_hook_output(&text),
                stdout,
                stderr: JsString::default(),
                exit_code: Some(0),
                outcome: HookOutcome::Success,
            }
        }
        Err(message) => ExecutionResult::failure(message, false),
    }
}

fn replace_first(value: &JsString, needle: &str, replacement: &str) -> JsString {
    let units = value.units();
    let needle: Vec<u16> = needle.encode_utf16().collect();
    let Some(index) = units
        .windows(needle.len())
        .position(|window| window == needle.as_slice())
    else {
        return value.clone();
    };
    let mut output = Vec::with_capacity(units.len() + replacement.len());
    output.extend_from_slice(&units[..index]);
    output.extend(replacement.encode_utf16());
    output.extend_from_slice(&units[index + needle.len()..]);
    JsString::from_utf16(output)
}
