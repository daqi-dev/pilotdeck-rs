//! Hook executors and `parseHookOutput` (TS `extension/hooks/execution/*`).
//!
//! Command hooks run the same shell shape as TS (`/bin/bash -c` on POSIX) in a
//! new process group, so timeout/abort cleanup reaps descendants rather than
//! only the shell. stdout/stderr are capped per stream at V8's
//! `MAX_STRING_LENGTH` UTF-16 units, the same bound as the bash tool; a chunk
//! that would not fit reports `Invalid string length`, the message of the
//! `RangeError` TS's `stdout += text` throws in the shell runner's data
//! listener. HTTP hooks use the same rustls connector as the model transport
//! (`http://` and `https://`); redirects follow undici's default policy
//! through `crate::fetch_redirect`. Prompt/agent execution needs a model
//! runner (A7).
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

/// Host-side `AbortSignal`, using the shared timestamped cancellation event.
pub use crate::runtime::network::Cancellation;

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

/// The default local shell. `output_limit` is per stream and counts UTF-16
/// units like the JS string it becomes; tests lower it so the V8-length path
/// is reachable without producing half a gigabyte.
#[derive(Clone, Copy)]
pub struct LocalShellPort {
    output_limit: usize,
}
impl Default for LocalShellPort {
    fn default() -> Self {
        Self {
            output_limit: crate::fetch_redirect::MAX_STRING_LENGTH,
        }
    }
}
impl LocalShellPort {
    /// Test hook: a smaller per-stream UTF-16 limit than V8's
    /// `MAX_STRING_LENGTH`.
    #[doc(hidden)]
    pub fn with_output_limit(limit: usize) -> Self {
        Self {
            output_limit: limit,
        }
    }
}
impl ShellPort for LocalShellPort {
    fn execute(&self, request: ShellRequest) -> ShellFuture {
        let output_limit = self.output_limit;
        Box::pin(async move { run_shell(request, output_limit).await })
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
    // Node's `findOnPath` looks up the `Path` key case-insensitively.
    let path = env
        .entries()
        .iter()
        .find(|(key, _)| {
            key.to_utf8()
                .is_ok_and(|key| key.eq_ignore_ascii_case("path"))
        })
        .and_then(|(_, value)| JsValue::as_string(value))
        .map(|value| String::from_utf16_lossy(value.units()))?;
    for entry in path.split(':').filter(|entry| !entry.is_empty()) {
        let candidate = Path::new(entry).join(name);
        // TS uses `existsSync`, not `isFile`; a directory-shaped entry still
        // counts as present and is rejected later by spawn.
        if candidate.exists() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// `resolveDefaultCommandShell` on POSIX: configured path/name, `/bin/bash`,
/// `bash` on PATH, then `/bin/sh`. An empty `PILOTDECK_SHELL_PATH` is unset
/// (JS truthiness), and a bare configured name is resolved through PATH.
fn resolve_shell(env: &JsObject) -> Result<String, JsString> {
    if let Some(configured) = env.get("PILOTDECK_SHELL_PATH").and_then(JsValue::as_string) {
        let path = String::from_utf16_lossy(configured.units());
        if !path.is_empty() {
            let available = if path.contains('/') || path.contains('\\') {
                Path::new(&path).exists()
            } else {
                find_on_path(&path, env).is_some()
            };
            if !available {
                return Err(JsString::from(format!(
                    "Configured PilotDeck shell was not found: {path}"
                )));
            }
            return Ok(path);
        }
    }
    if Path::new("/bin/bash").exists() {
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
            crate::process_group::kill(self.group, rustix::process::Signal::KILL);
        }
    }
}

/// Wait for the leader without reaping it, so its PGID stays reserved until the
/// caller has sent the final group signal (same invariant as `host/connection`).
async fn observe_leader_exit(group: rustix::process::Pid) {
    loop {
        match crate::process_group::leader_exited_unreaped(group) {
            Ok(true) | Err(_) => return,
            Ok(false) => tokio::time::sleep(Duration::from_millis(5)).await,
        }
    }
}

async fn run_shell(request: ShellRequest, output_limit: usize) -> Result<ShellResult, JsString> {
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
    #[cfg(unix)]
    pilotdeck_child_signals::install_child_signal_reset(command.as_std_mut());
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

    // Both pipes are drained concurrently with the wait. A hook that writes
    // more than the OS pipe buffer (64 KiB on macOS) must not deadlock before
    // `wait` returns, and a grandchild that inherits either pipe keeps EOF
    // pending: that is why the timers below outlive the leader. Each stream is
    // capped at `output_limit` UTF-16 units, like the bash tool's
    // `StreamText`; the rest of the stream is still drained and discarded.
    let mut out_task = tokio::spawn(read_pipe(child.stdout.take(), output_limit));
    let mut err_task = tokio::spawn(read_pipe(child.stderr.take(), output_limit));
    let mut out_done = false;
    let mut err_done = false;
    let mut stdout_pipe = PipeOutput::default();
    let mut stderr_pipe = PipeOutput::default();
    // C6: the writer may block forever when the child does not read stdin but
    // a same-group grandchild keeps the read end open. It is aborted with the
    // rest of the process state instead of outliving the hook.
    let stdin_task = child.stdin.take().map(|mut stdin| {
        let payload = request.stdin.units().to_vec();
        tokio::spawn(async move {
            // EPIPE after an early exit is normal; process exit owns the outcome.
            let bytes: Vec<u8> = String::from_utf16_lossy(&payload).into_bytes();
            let _ = stdin.write_all(&bytes).await;
            let _ = stdin.shutdown().await;
        })
    });

    let (leader_tx, mut leader_rx) = tokio::sync::oneshot::channel();
    if let Some(group) = group {
        tokio::spawn(async move {
            observe_leader_exit(group).await;
            let _ = leader_tx.send(());
        });
    } else {
        let _ = leader_tx.send(());
    }
    let mut leader_exited = group.is_none();

    let started = tokio::time::Instant::now();
    let mut timeout_sleep: Pin<Box<Sleep>> =
        Box::pin(sleep_until(started + duration_ms(request.timeout_ms)));
    let signal = request.signal;
    let initially_aborted = signal.as_ref().is_some_and(Cancellation::is_aborted);
    let mut timed_out = false;
    let mut timed_or_cancelled = false;
    let mut kill_sleep: Option<Pin<Box<Sleep>>> = None;
    // TERM + `KILL_GRACE`: the instant the final group KILL is due. `None`
    // after the KILL has been sent (or when no termination was requested).
    let mut kill_deadline: Option<tokio::time::Instant> = None;
    let mut force_sleep: Option<Pin<Box<Sleep>>> = None;
    let mut forced = false;
    loop {
        if leader_exited && out_done && err_done {
            break;
        }
        tokio::select! {
            _ = &mut leader_rx, if !leader_exited => {
                leader_exited = true;
            }
            result = &mut out_task, if !out_done => {
                stdout_pipe = result.unwrap_or_default();
                out_done = true;
            }
            result = &mut err_task, if !err_done => {
                stderr_pipe = result.unwrap_or_default();
                err_done = true;
            }
            _ = &mut timeout_sleep, if !timed_or_cancelled => {
                timed_out = true;
                timed_or_cancelled = true;
                crate::process_group::kill(group, rustix::process::Signal::TERM);
                let deadline = tokio::time::Instant::now() + KILL_GRACE;
                kill_deadline = Some(deadline);
                kill_sleep = Some(Box::pin(sleep_until(deadline)));
                force_sleep = Some(Box::pin(sleep_until(tokio::time::Instant::now() + FORCE_RESOLVE)));
            }
            _ = async {
                if let Some(signal) = &signal { signal.cancelled().await } else { std::future::pending().await }
            }, if !timed_or_cancelled && !initially_aborted => {
                timed_or_cancelled = true;
                crate::process_group::kill(group, rustix::process::Signal::TERM);
                let deadline = tokio::time::Instant::now() + KILL_GRACE;
                kill_deadline = Some(deadline);
                kill_sleep = Some(Box::pin(sleep_until(deadline)));
                force_sleep = Some(Box::pin(sleep_until(tokio::time::Instant::now() + FORCE_RESOLVE)));
            }
            _ = async {
                if let Some(sleep) = kill_sleep.as_mut() { sleep.as_mut().await } else { std::future::pending().await }
            }, if kill_sleep.is_some() => {
                kill_sleep = None;
                kill_deadline = None;
                crate::process_group::kill(group, rustix::process::Signal::KILL);
            }
            _ = async {
                if let Some(sleep) = force_sleep.as_mut() { sleep.as_mut().await } else { std::future::pending().await }
            }, if force_sleep.is_some() => {
                // A descendant that survives SIGKILL is outside normal reach;
                // keep TS's force-resolve result but still make bounded
                // attempts to reap and collect.
                forced = true;
                timed_out = true;
                break;
            }
        }
    }

    // C6 first: aborting only schedules cancellation, so join the writer
    // before tearing the pipes down.
    if let Some(task) = stdin_task {
        task.abort();
        let _ = task.await;
    }

    // A normal completion must not signal the group: TS `commandRunner` only
    // kills on timeout/abort, so a background process a hook deliberately
    // starts (`cmd >/dev/null 2>&1 &`) survives it.
    //
    // On timeout/abort TS sends TERM immediately, resolves the result at
    // `close`, then lets its 3 s KILL timer run on. The loop here can break on
    // pipe EOF before that timer fires (a leader whose stdio is redirected
    // closes long before the grace expires). Returning at that point must not
    // SIGKILL early, and the leader must stay an unreaped zombie so the PGID
    // cannot be reused; hand both to a background reaper that waits out the
    // remaining grace, KILLs the group, then reaps.
    let mut exit_code = None;
    let deferred_kill = timed_or_cancelled
        && !forced
        && kill_deadline.is_some_and(|deadline| tokio::time::Instant::now() < deadline);
    if timed_or_cancelled && !forced {
        if deferred_kill {
            // The status is available without reaping (`WNOWAIT`), so the
            // result keeps TS's exit code while the zombie reserves the PGID.
            exit_code = crate::process_group::leader_exit_code_unreaped(group);
        } else {
            // The grace already expired (or no deadline was recorded): finish
            // the KILL inline and reap below, as the loop used to do.
            crate::process_group::kill(group, rustix::process::Signal::KILL);
        }
    }
    if deferred_kill {
        let deadline = kill_deadline.expect("deferred kill has a deadline");
        let mut child = child;
        let mut guard = guard;
        tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            crate::process_group::kill(group, rustix::process::Signal::KILL);
            if tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .is_ok_and(|result| result.is_ok())
            {
                guard.reap();
            }
            crate::process_group::wait_for_group_empty(group, KILL_GRACE).await;
        });
    } else if !forced {
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => {
                exit_code = status.code();
                guard.reap();
            }
            _ => {
                // The leader outlived a normal completion: fall back to the
                // group contract so it cannot leak.
                forced = true;
                timed_out = true;
                crate::process_group::kill(group, rustix::process::Signal::KILL);
            }
        }
    } else {
        // Even a force-resolved leader must be reaped before dropping the
        // group id, otherwise GroupGuard::drop could signal a reused PGID.
        if tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .is_ok_and(|result| result.is_ok())
        {
            guard.reap();
        }
    }

    if !out_done {
        if let Ok(Ok(pipe)) = tokio::time::timeout(Duration::from_secs(1), &mut out_task).await {
            stdout_pipe = pipe;
        }
        out_task.abort();
        let _ = out_task.await;
    }
    if !err_done {
        if let Ok(Ok(pipe)) = tokio::time::timeout(Duration::from_secs(1), &mut err_task).await {
            stderr_pipe = pipe;
        }
        err_task.abort();
        let _ = err_task.await;
    }
    // The deferred path keeps the result immediate; its background task owns
    // the group drain so the 3 s grace is not added to the returned duration.
    if !deferred_kill && (timed_or_cancelled || forced) {
        crate::process_group::wait_for_group_empty(group, KILL_GRACE).await;
    }

    // V8's `stdout += text` throws `RangeError: Invalid string length` once a
    // chunk does not fit (TS `tool/builtin/bash/commandRunner.ts:132`, probed
    // in `_tmp/20261020-a65-pr7/probes/oom-rule.out`). A timeout/cancel keeps
    // its own outcome and partial output, like TS's close-based result; a
    // normal completion with an overflowing stream becomes the hook failure
    // the executor's catch would have produced from that RangeError.
    //
    // Exit 2 keeps its blocking outcome even when a stream overflowed: exit 2
    // is the hook's own refusal, and turning it into `non_blocking_error`
    // would fail open. TS also resolves `exitCode: 2` when its data listener's
    // RangeError is caught by an uncaughtException handler that keeps the
    // process alive.
    let blocking = exit_code == Some(2);
    if !timed_or_cancelled
        && !forced
        && !blocking
        && (stdout_pipe.truncated || stderr_pipe.truncated)
    {
        return Err(JsString::from("Invalid string length"));
    }
    let mut stderr = stderr_pipe.bytes;
    if forced {
        stderr.extend_from_slice(FORCE_RESOLVE_NOTE.as_bytes());
    }
    Ok(ShellResult {
        exit_code: if forced { None } else { exit_code },
        stdout: stdout_pipe.bytes,
        stderr,
        timed_out,
    })
}

fn duration_ms(value: f64) -> Duration {
    crate::runtime::network::node_timer_duration(value)
}

/// WHATWG `TextDecoder("utf-8")` in streaming mode: maximal-subpart U+FFFD
/// substitution, an incomplete tail carried to the next chunk, and one
/// leading BOM stripped (`ignoreBOM: false`). Same rules as the bash tool's
/// decoder, kept here because that module is `cfg(unix)`-gated.
#[derive(Default)]
struct Utf8Decoder {
    pending: Vec<u8>,
    started: bool,
}

impl Utf8Decoder {
    fn decode(&mut self, chunk: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(chunk);
        let mut out = String::new();
        let mut rest = bytes.as_slice();
        loop {
            match std::str::from_utf8(rest) {
                Ok(valid) => {
                    out.push_str(valid);
                    break;
                }
                Err(error) => {
                    let (valid, tail) = rest.split_at(error.valid_up_to());
                    out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match error.error_len() {
                        Some(length) => {
                            out.push('\u{FFFD}');
                            rest = &tail[length..];
                        }
                        None => {
                            self.pending = tail.to_vec();
                            break;
                        }
                    }
                }
            }
        }
        self.strip_bom(out)
    }
    fn flush(&mut self) -> String {
        let out = if self.pending.is_empty() {
            String::new()
        } else {
            self.pending.clear();
            "\u{FFFD}".into()
        };
        let out = self.strip_bom(out);
        self.started = false;
        out
    }
    fn strip_bom(&mut self, out: String) -> String {
        if self.started || out.is_empty() {
            return out;
        }
        self.started = true;
        match out.strip_prefix('\u{FEFF}') {
            Some(rest) => rest.to_owned(),
            None => out,
        }
    }
}

/// One stream's retained bytes and whether a chunk was dropped at the cap.
#[derive(Default)]
struct PipeOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Reads one pipe with the bash tool's rule (`StreamText::push`): a decoded
/// chunk that does not fit is dropped whole, later chunks that do fit are
/// appended, and the pipe keeps draining so the child never blocks on a full
/// OS buffer. `limit` counts UTF-16 units, matching the JS string the TS
/// shell runner builds.
async fn read_pipe(
    pipe: Option<impl tokio::io::AsyncRead + Unpin + Send + 'static>,
    limit: usize,
) -> PipeOutput {
    use tokio::io::AsyncReadExt;

    let Some(mut pipe) = pipe else {
        return PipeOutput::default();
    };
    let mut output = PipeOutput::default();
    let mut decoder = Utf8Decoder::default();
    let mut units = 0usize;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let count = match pipe.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(count) => count,
        };
        let text = decoder.decode(&chunk[..count]);
        append_capped(&mut output, &mut units, limit, &text);
    }
    let tail = decoder.flush();
    append_capped(&mut output, &mut units, limit, &tail);
    output
}

fn append_capped(output: &mut PipeOutput, units: &mut usize, limit: usize, text: &str) {
    let count = text.encode_utf16().count();
    if units.saturating_add(count) > limit {
        output.truncated = true;
        return;
    }
    *units += count;
    output.bytes.extend_from_slice(text.as_bytes());
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
            shell: Arc::new(LocalShellPort::default()),
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
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        http_body_util::Full<bytes::Bytes>,
    >,
    /// undici `headersTimeout` (per dispatch, so per redirect hop) and
    /// `bodyTimeout` (idle gap between body frames). Tests inject a shorter
    /// value; production keeps the TS value.
    timeout: Duration,
    body_limit: usize,
}
/// The TS CLI replaces fetch's global dispatcher at startup
/// (`cli/pilotdeck.ts:28` → `cli/proxy.ts:108-111,149-160`):
/// `new undici.Agent({ headersTimeout, bodyTimeout:
/// UNDICI_TRANSPORT_TIMEOUT_MS })`, i.e. 600 000 ms
/// (`model/streaming/streamModel.ts:56`), not undici's 300 s default.
pub const HTTP_TIMEOUT: Duration = crate::runtime::network::UNDICI_BODY_TIMEOUT;
impl Default for HttpHookExecutor {
    fn default() -> Self {
        Self::new()
    }
}
impl HttpHookExecutor {
    pub fn new() -> Self {
        // Node's fetch trusts its bundled CA store; `webpki-roots` is the
        // equivalent Mozilla bundle the model transport already uses.
        Self::with_config(crate::runtime::model::http::webpki_client_config())
    }
    /// Trust exactly `roots` (tests use the fixture's throwaway CA).
    pub fn with_roots(roots: rustls::RootCertStore) -> Self {
        Self::with_config(crate::runtime::model::http::rooted_client_config(roots))
    }
    pub fn with_timeout(timeout: Duration) -> Self {
        let mut executor = Self::with_config(crate::runtime::model::http::webpki_client_config());
        executor.timeout = timeout;
        executor
    }
    fn with_config(config: rustls::ClientConfig) -> Self {
        let client =
            hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
                .build(crate::runtime::model::http::https_connector(config));
        Self {
            client,
            timeout: HTTP_TIMEOUT,
            body_limit: crate::fetch_redirect::MAX_STRING_LENGTH,
        }
    }
    /// Test hook: a smaller response-body byte limit than Node's
    /// `MAX_STRING_LENGTH`, so the over-limit path is cheap to exercise.
    #[doc(hidden)]
    pub fn with_body_limit(mut self, limit: usize) -> Self {
        self.body_limit = limit;
        self
    }
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
    pub async fn execute(&self, options: HttpHookOptions) -> ExecutionResult {
        match self.execute_inner(options).await {
            Ok(result) => result,
            Err(message) => ExecutionResult::failure(message, false),
        }
    }
    async fn execute_inner(&self, options: HttpHookOptions) -> Result<ExecutionResult, JsString> {
        let client = self.client.clone();
        self.execute_with(options, move |request| {
            let client = client.clone();
            crate::runtime::network::ExchangeFuture::io(async move {
                let scheme = request.uri().scheme_str().unwrap_or_default();
                if scheme != "http" && scheme != "https" {
                    return Err(JsString::from(format!(
                        "Unsupported HTTP hook URL scheme: {scheme}"
                    )));
                }
                client
                    .request(request)
                    .await
                    .map_err(|_| JsString::from("fetch failed"))
            })
        })
        .await
    }
    // The real executor and paused-clock tests use the same redirects, event
    // dispatcher and body reader; only the individual wire exchange is injected.
    async fn execute_with<B>(
        &self,
        options: HttpHookOptions,
        mut send: impl FnMut(
            hyper::Request<http_body_util::Full<bytes::Bytes>>,
        ) -> crate::runtime::network::ExchangeFuture<
            Result<hyper::Response<B>, JsString>,
        >,
    ) -> Result<ExecutionResult, JsString>
    where
        B: hyper::body::Body<Data = bytes::Bytes> + Send + 'static,
        B::Error: Send,
    {
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
        // Other schemes are fetch network errors, reported at send like the
        // TS side; `follow` re-checks every redirect target.
        // `fetch` builds `Headers` from the plain object (node probes in
        // _tmp/20261019-a67-pr4/probe-p1/ and probe-r3/headers-*.mjs): the
        // WebIDL record conversion makes every key, then its value, a
        // ByteString first (entry by entry, before any append); then undici's `appendHeader` runs per
        // entry in object key order — trim HTTP whitespace, reject an invalid
        // name, reject a value holding NUL/CR/LF, and join a name that differs
        // only in case onto the first one with ", ". All of this happens while
        // constructing the Request, so before the abort check; any other byte
        // the wire rejects (e.g. 0x01, 0x7f) only fails at send: `fetch failed`.
        let mut entries = Vec::with_capacity(headers.len());
        for (name, value) in headers {
            entries.push((byte_string(&name)?, byte_string(&value)?));
        }
        let mut merged: Vec<(hyper::header::HeaderName, Vec<u8>, Vec<u8>)> =
            Vec::with_capacity(entries.len());
        for (name, bytes) in entries {
            let is_ws = |b: &u8| matches!(b, b'\t' | b'\n' | b'\r' | b' ');
            let start = bytes.iter().position(|b| !is_ws(b)).unwrap_or(bytes.len());
            let end = bytes
                .iter()
                .rposition(|b| !is_ws(b))
                .map_or(start, |i| i + 1);
            let bytes = &bytes[start..end];
            let header_name = hyper::header::HeaderName::from_bytes(&name).map_err(|_| {
                let text: String = name.iter().map(|b| char::from(*b)).collect();
                JsString::from(format!(
                    "Headers.append: \"{text}\" is an invalid header name."
                ))
            })?;
            if bytes.iter().any(|b| matches!(b, 0 | b'\n' | b'\r')) {
                let text: String = bytes.iter().map(|b| char::from(*b)).collect();
                return Err(JsString::from(format!(
                    "Headers.append: \"{text}\" is an invalid header value."
                )));
            }
            if let Some(entry) = merged
                .iter_mut()
                .find(|(_, seen, _)| seen.eq_ignore_ascii_case(&name))
            {
                entry.2.extend_from_slice(b", ");
                entry.2.extend_from_slice(bytes);
            } else {
                merged.push((header_name, name, bytes.to_vec()));
            }
        }
        if options
            .signal
            .as_ref()
            .is_some_and(Cancellation::is_aborted)
        {
            return Err("This operation was aborted".into());
        }
        let mut wire_headers = hyper::HeaderMap::with_capacity(merged.len());
        for (name, _, value) in merged {
            let value = hyper::header::HeaderValue::from_bytes(&value)
                .map_err(|_| JsString::from("fetch failed"))?;
            wire_headers.append(name, value);
        }
        // Default `fetch` follows redirects (undici, 20 hops). Every hop is its
        // own dispatch with its own headers timer (undici `headersTimeout`,
        // probe-r3/timeouts-3s.mjs); the abort signal spans the whole fetch.
        // The TLS connector accepts both schemes, so a hop may upgrade or
        // downgrade just like `fetch`.
        let response = crate::fetch_redirect::follow(
            url,
            hyper::Method::POST,
            wire_headers,
            bytes::Bytes::from(body),
            || JsString::from("fetch failed"),
            |request| {
                let deadline = tokio::time::Instant::now() + self.timeout;
                let sent = send(request);
                let signal = options.signal.as_ref();
                async move {
                    let (response, before_abort) =
                        http_exchange(sent, signal, Some(deadline)).await?;
                    let response = response?;
                    Ok(response.map(|body| (body, before_abort)))
                }
            },
        )
        .await?;
        let status = response.status();
        let ok = status.is_success();
        // `response.text()`: undici `bodyTimeout` is an idle timer between
        // frames, and a body failure after the headers is `terminated`
        // (probe-r3/timeouts-3s.mjs, body-reset.mjs). Node cannot build a
        // string past MAX_STRING_LENGTH bytes (probe-r3/body-cap.mjs).
        let body_limit = self.body_limit;
        let timeout = self.timeout;
        let (body, before_abort) = response.into_body();
        let body = crate::fetch_redirect::read_body_with_signal(
            body,
            body_limit,
            Some(timeout),
            options.signal.as_ref(),
            before_abort,
        )
        .await
        .map_err(|error| match error {
            crate::fetch_redirect::BodyError::TooLong => {
                JsString::from(crate::fetch_redirect::STRING_TOO_LONG)
            }
            crate::fetch_redirect::BodyError::Terminated => JsString::from("terminated"),
            crate::fetch_redirect::BodyError::Aborted => {
                JsString::from("This operation was aborted")
            }
        })?;
        let stdout = String::from_utf8_lossy(&body).into_owned();
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

/// Fetch headers and response.text() share the Node event dispatcher. Only
/// headers have a total deadline; read_body owns the idle body timer.
async fn http_exchange<T: Send + 'static>(
    send: crate::runtime::network::ExchangeFuture<T>,
    signal: Option<&Cancellation>,
    deadline: Option<tokio::time::Instant>,
) -> Result<(T, bool), JsString> {
    use crate::runtime::network::{Exchange, exchange};
    match exchange(
        send,
        async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending().await,
            }
        },
        || signal.and_then(Cancellation::abort_event_at),
        deadline,
    )
    .await
    {
        Exchange::Response(result, before_abort) => Ok((result, before_abort)),
        Exchange::Aborted => Err("This operation was aborted".into()),
        Exchange::Timeout { .. } => Err("fetch failed".into()),
    }
}

/// WebIDL ByteString conversion of a header record key or value: every
/// UTF-16 code unit must be <= 0xFF (a lone surrogate fails too), and the
/// wire bytes are those code units (latin1), not UTF-8.
fn byte_string(value: &JsString) -> Result<Vec<u8>, JsString> {
    let mut bytes = Vec::with_capacity(value.units().len());
    for (index, unit) in value.units().iter().enumerate() {
        if *unit > 0xFF {
            return Err(JsString::from(format!(
                "Cannot convert argument to a ByteString because the character at index {index} has a value of {unit} which is greater than 255."
            )));
        }
        bytes.push(*unit as u8);
    }
    Ok(bytes)
}

fn resolve_headers(
    headers: Option<&JsObject>,
    allowed_env_vars: Option<&[JsString]>,
    env: Option<&JsObject>,
) -> Result<Vec<(JsString, JsString)>, JsString> {
    let owned_env;
    let env = match env {
        Some(env) => env,
        None => {
            owned_env = process_env();
            &owned_env
        }
    };
    let allowed = allowed_env_vars.unwrap_or_default();
    let mut output = vec![(
        JsString::from("content-type"),
        JsString::from("application/json"),
    )];
    for (key, value) in headers.into_iter().flat_map(JsObject::entries) {
        let Some(value) = value.as_string() else {
            return Err(JsString::from("value.replace is not a function"));
        };
        let resolved = substitute_env(value, allowed, env)?;
        if let Some(entry) = output.iter_mut().find(|(name, _)| name == key) {
            entry.1 = JsString::from_utf16(resolved);
        } else {
            output.push((key.clone(), JsString::from_utf16(resolved)));
        }
    }
    // `{ "content-type": …, ...resolved }` is a plain object, so integer-like
    // keys enumerate first in ascending order (JS OrdinaryOwnPropertyKeys).
    let mut indexed: Vec<(u32, (JsString, JsString))> = Vec::new();
    let mut named = Vec::with_capacity(output.len());
    for entry in output {
        match entry.0.array_index() {
            Some(index) => indexed.push((index, entry)),
            None => named.push(entry),
        }
    }
    indexed.sort_by_key(|(index, _)| *index);
    Ok(indexed
        .into_iter()
        .map(|(_, entry)| entry)
        .chain(named)
        .collect())
}

fn substitute_env(
    value: &JsString,
    allowed: &[JsString],
    env: &JsObject,
) -> Result<Vec<u16>, JsString> {
    let units = value.units();
    let mut output = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        if units[index] != u16::from(b'$') {
            output.push(units[index]);
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
                output.push(units[start]);
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
                output.push(units[start]);
                continue;
            }
            &units[name_start..index]
        };
        if allowed.iter().any(|allowed| allowed.units() == name)
            && let Some(value) = env.get_key(&JsString::from_utf16(name.to_vec()))
            && !value.is_undefined()
            && !matches!(value, JsValue::Null)
        {
            output.extend_from_slice(
                js_string(value)
                    .map_err(|error| JsString::from(error_message(error)))?
                    .units(),
            );
        }
    }
    Ok(output)
}

fn is_word(unit: u16) -> bool {
    matches!(unit, 48..=57 | 65..=90 | 97..=122 | 95)
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

/// `String.prototype.replace(string, string)`: first occurrence only, and the
/// replacement goes through `GetSubstitution` (`$&`, `` $` ``, `$'`, `$$`).
fn replace_first(value: &JsString, needle: &str, replacement: &str) -> JsString {
    let units = value.units();
    let needle: Vec<u16> = needle.encode_utf16().collect();
    let Some(index) = units
        .windows(needle.len())
        .position(|window| window == needle.as_slice())
    else {
        return value.clone();
    };
    let replacement: Vec<u16> = replacement.encode_utf16().collect();
    let mut output = Vec::with_capacity(units.len() + replacement.len());
    output.extend_from_slice(&units[..index]);
    crate::runtime::mcp::sdk::expand_replacement(
        &mut output,
        &replacement,
        units,
        index,
        index + needle.len(),
    );
    output.extend_from_slice(&units[index + needle.len()..]);
    JsString::from_utf16(output)
}

#[cfg(test)]
mod replace_tests {
    use super::*;

    /// Node v22.22.0: `"pre $ARGUMENTS post".replace("$ARGUMENTS",
    /// JSON.stringify({a:"$&|$`|$'|$$|$1"}))` (probe in
    /// _tmp/20261019-a67-pr4/probe-p1/probe.out).
    #[test]
    fn arguments_replacement_uses_get_substitution() {
        let out = replace_first(
            &JsString::from("pre $ARGUMENTS post"),
            "$ARGUMENTS",
            r#"{"a":"$&|$`|$'|$$|$1"}"#,
        );
        assert_eq!(
            out,
            JsString::from(r#"pre {"a":"$ARGUMENTS|pre | post|$|$1"} post"#)
        );
        // Only the first occurrence is replaced.
        let out = replace_first(&JsString::from("$ARGUMENTS $ARGUMENTS"), "$ARGUMENTS", "x");
        assert_eq!(out, JsString::from("x $ARGUMENTS"));
    }

    /// Node v22.22.0 sends `{"x-z":"z","123":"n"}` as `123`, `content-type`,
    /// `x-z` (probe2.out): integer-like keys of the spread object come first.
    #[test]
    fn headers_follow_js_object_key_order() {
        let mut headers = JsObject::new();
        headers.insert("x-z".into(), "z".into());
        headers.insert("123".into(), "n".into());
        headers.insert("01".into(), "o".into());
        let resolved = resolve_headers(Some(&headers), None, Some(&JsObject::new())).unwrap();
        let names: Vec<String> = resolved
            .iter()
            .map(|(name, _)| name.to_utf8().unwrap())
            .collect();
        assert_eq!(names, ["123", "content-type", "x-z", "01"]);
    }
}

#[cfg(test)]
#[path = "exec_http_tests.rs"]
mod http_tests;

#[cfg(all(test, unix))]
mod signal_tests {
    use super::*;
    use crate::process_spawn::{child_signals, test_support};

    #[test]
    fn hook_resets_inherited_signals() {
        child_signals::ChildSignalProbe::new().launch(
            "runtime::hooks::exec::signal_tests::hook_signal_worker",
            false,
        );
    }

    #[tokio::test]
    #[ignore = "isolated worker launched with inherited ignored/blocked signals"]
    async fn hook_signal_worker() {
        test_support::assert_inherited();
        let probe = child_signals::probe_path();
        let mut env = JsObject::new();
        // Inspect the configured shell itself, before bash can reset signals.
        env.insert(
            "PILOTDECK_SHELL_PATH".into(),
            probe.to_str().unwrap().into(),
        );
        let result = LocalShellPort::default()
            .execute(ShellRequest {
                command: "report".into(),
                cwd: probe.parent().unwrap().to_str().unwrap().into(),
                env: Some(env),
                timeout_ms: 5_000.0,
                stdin: "".into(),
                signal: None,
            })
            .await
            .unwrap();
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
        test_support::assert_reset_report(std::str::from_utf8(&result.stdout).unwrap());
        test_support::assert_inherited();
    }
}

#[cfg(test)]
#[test]
fn hook_timeout_conversion_matches_node_timer_lists() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../../tests/fixtures/hook-router-timers/oracle.json"
    ));
    let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
        panic!("timer cases")
    };
    for case in cases {
        let input = case.get("input").unwrap();
        let value = match input.as_string().map(|s| s.to_utf8().unwrap()).as_deref() {
            Some("NaN") => f64::NAN,
            Some("Infinity") => f64::INFINITY,
            Some("-Infinity") => f64::NEG_INFINITY,
            _ => input.as_number().unwrap(),
        };
        assert_eq!(
            duration_ms(value).as_millis() as f64,
            case.get("hookMs").unwrap().as_number().unwrap(),
            "input={input:?}"
        );
    }
}
