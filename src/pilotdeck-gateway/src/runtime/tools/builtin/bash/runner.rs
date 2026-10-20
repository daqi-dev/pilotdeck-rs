//! `tool/builtin/bash/commandRunner.ts` (`NodeShellCommandRunner`) and the
//! POSIX half of `runtime/commandShell.ts` (`resolveDefaultCommandShell`).
//!
//! Process ownership: the shell is the leader of a new process group
//! (`detached: true` in the baseline). Signals go to the whole group, and the
//! leader is never reaped before the last group signal — its exit is observed
//! with `waitid(WNOWAIT)` (same rule as `host::connection`), so the group ID
//! cannot be reused under us. A [`GroupGuard`] SIGKILLs the group if the run
//! future is dropped mid-flight; the child itself has `kill_on_drop`.
//!
//! Known, documented differences from Node:
//! - `detached` calls `setsid()`; the crate forbids `unsafe` (no `pre_exec`),
//!   so the child gets a new process group via `process_group(0)` but stays in
//!   the parent's session.
//! - Node's stdio pipes are UNIX socket pairs; these are `pipe(2)` FIFOs.
//! - The baseline's SIGKILL follow-up fires 3 s after termination even when the
//!   result has already settled. That one bounded timer is the only work that
//!   outlives a run: it holds the unreaped leader and the group guard, fires on
//!   the baseline's schedule, then reaps. Without WNOWAIT the group is killed
//!   at settle time instead.
use super::super::{join, os_path};
use crate::runtime::tools::{ToolError, ToolFuture, ToolRunContext};
use pilotdeck_protocol::JsString;
use std::{
    io,
    path::Path,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command, time::sleep_until};

pub type OutputCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// The tool context's `abortSignal`.
#[derive(Clone)]
pub struct AbortSignal(ToolRunContext);

impl AbortSignal {
    /// `context.abortSignal`, when the host supplied one.
    pub fn from_context(context: &ToolRunContext) -> Option<Self> {
        context
            .host
            .abort_signal
            .is_some()
            .then(|| Self(context.clone()))
    }
    pub fn aborted(&self) -> bool {
        self.0.is_aborted()
    }
    pub async fn wait(&self) {
        self.0.cancelled().await;
    }
}

/// `PilotDeckCommandOptions` (without the unused `stdin` payload: the bash
/// tool always ends stdin empty).
#[derive(Clone)]
pub struct CommandOptions {
    pub cwd: JsString,
    /// `env` with `undefined` members removed and values already `String()`d,
    /// in `Object.keys` order and kept as UTF-16 so Node's diagnostics can
    /// show lone surrogates; `None` inherits the gateway process environment.
    pub env: Option<Vec<(JsString, JsString)>>,
    pub timeout_ms: f64,
    pub signal: Option<AbortSignal>,
    pub on_stdout: Option<OutputCallback>,
    pub on_stderr: Option<OutputCallback>,
    /// Per-stream cap in UTF-16 units; `None` is V8's `MAX_STRING_LENGTH`.
    /// Only tests lower it (the real limit needs ~0.5 GB per stream).
    pub max_output_units: Option<usize>,
}

/// `stdout += text` against V8's string length limit. The baseline's append
/// throws `RangeError: Invalid string length` for a chunk that would not fit:
/// that chunk is lost (and its progress callback never runs), later chunks
/// that still fit are appended, and the child keeps running
/// (`_tmp/20261020-a65-pr7/probes/oom-rule.out`). The throw itself is an
/// uncaught exception in the data listener; the TS server shuts the whole
/// gateway down on it (`cli/pilotdeck.ts:267`). Here only the overflowing
/// chunk is dropped, which also bounds the memory a command can pin.
struct StreamText {
    text: String,
    units: usize,
    limit: usize,
}

impl StreamText {
    fn new(limit: usize) -> Self {
        Self {
            text: String::new(),
            units: 0,
            limit,
        }
    }
    /// Whether `chunk` was appended.
    fn push(&mut self, chunk: &str) -> bool {
        let units = chunk.encode_utf16().count();
        if self.units.saturating_add(units) > self.limit {
            return false;
        }
        self.units += units;
        self.text.push_str(chunk);
        true
    }
}

/// `PilotDeckCommandResult`. `pid` is the shell's PID (and process group),
/// kept for owners that must prove the group is gone; it is not on the wire.
#[derive(Clone, Debug, PartialEq)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub duration_ms: f64,
    pub pid: Option<u32>,
}

/// `PilotDeckCommandRunner`.
pub trait CommandRunner: Send + Sync {
    /// `command` is the original UTF-16 text; the OS receives its UTF-8 form
    /// (lone surrogates as U+FFFD), like Node.
    fn run(&self, command: JsString, options: CommandOptions) -> ToolFuture<CommandResult>;
}

/// The default local runner.
#[derive(Clone, Copy, Debug, Default)]
pub struct NodeShellCommandRunner;

impl CommandRunner for NodeShellCommandRunner {
    fn run(&self, command: JsString, options: CommandOptions) -> ToolFuture<CommandResult> {
        Box::pin(run(command, options))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandShellKind {
    Bash,
    Sh,
    Cmd,
    Pwsh,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandShell {
    pub shell: JsString,
    pub kind: CommandShellKind,
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

impl CommandShell {
    pub fn args(&self, command: &JsString) -> Vec<JsString> {
        let literal = |text: &str| JsString::from(text);
        match self.kind {
            CommandShellKind::Cmd => {
                let mut quoted = vec![u16::from(b'"')];
                quoted.extend_from_slice(command.units());
                quoted.push(u16::from(b'"'));
                vec![
                    literal("/d"),
                    literal("/s"),
                    literal("/c"),
                    JsString::from_utf16(quoted),
                ]
            }
            CommandShellKind::Pwsh => vec![
                literal("-NoLogo"),
                literal("-NoProfile"),
                literal("-Command"),
                command.clone(),
            ],
            _ => vec![literal("-c"), command.clone()],
        }
    }
}

fn lossy(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

fn env_get<'a>(env: &'a [(JsString, JsString)], key: &str) -> Option<&'a JsString> {
    env.iter()
        .find(|(name, _)| name.equals_str(key))
        .map(|(_, value)| value)
}

/// `existsSync(path)`: Node hands the OS the UTF-8 form, so a lone surrogate
/// or NUL byte simply fails to exist.
fn exists(path: &str) -> bool {
    !path.is_empty() && !path.contains('\0') && Path::new(path).exists()
}

/// `findOnPath` (POSIX): the first `path` key in any case, `:`-separated,
/// empty entries skipped, candidates `path.join`ed.
fn find_on_path(command: &JsString, env: &[(JsString, JsString)]) -> Option<JsString> {
    let (_, value) = env
        .iter()
        .find(|(name, _)| lossy(name).to_lowercase() == "path")?;
    value
        .units()
        .split(|unit| *unit == u16::from(b':'))
        .filter(|entry| !entry.is_empty())
        .map(|entry| join(&JsString::from_utf16(entry.to_vec()), command))
        .find(|candidate| exists(&lossy(candidate)))
}

fn command_available(command: &JsString, env: &[(JsString, JsString)]) -> bool {
    if command
        .units()
        .iter()
        .any(|unit| *unit == u16::from(b'/') || *unit == u16::from(b'\\'))
    {
        return exists(&lossy(command));
    }
    find_on_path(command, env).is_some()
}

fn shell_from_path(shell: &JsString) -> CommandShell {
    let text = lossy(shell);
    let name = text
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(&text)
        .to_lowercase();
    let kind = match name.as_str() {
        "bash" | "bash.exe" => CommandShellKind::Bash,
        "sh" | "sh.exe" | "dash" | "dash.exe" | "ksh" | "ksh.exe" | "mksh" | "mksh.exe" => {
            CommandShellKind::Sh
        }
        "cmd" | "cmd.exe" => CommandShellKind::Cmd,
        "pwsh" | "pwsh.exe" => CommandShellKind::Pwsh,
        _ => CommandShellKind::Custom,
    };
    CommandShell {
        shell: shell.clone(),
        kind,
    }
}

/// `resolveDefaultCommandShell({ env })` on a non-Windows platform. The error
/// is the baseline's thrown `Error` message, with the configured value's
/// original UTF-16 units.
pub fn resolve_default_command_shell(
    env: &[(JsString, JsString)],
) -> Result<CommandShell, JsString> {
    if let Some(configured) = env_get(env, "PILOTDECK_SHELL_PATH").filter(|v| !v.is_empty()) {
        if !command_available(configured, env) {
            let mut message = units("Configured PilotDeck shell was not found: ");
            message.extend_from_slice(configured.units());
            return Err(JsString::from_utf16(message));
        }
        return Ok(shell_from_path(configured));
    }
    let bash = |shell: JsString| CommandShell {
        shell,
        kind: CommandShellKind::Bash,
    };
    if exists("/bin/bash") {
        return Ok(bash(JsString::from("/bin/bash")));
    }
    if let Some(found) = find_on_path(&JsString::from("bash"), env) {
        return Ok(bash(found));
    }
    Ok(CommandShell {
        shell: JsString::from("/bin/sh"),
        kind: CommandShellKind::Sh,
    })
}

fn process_env() -> Vec<(JsString, JsString)> {
    std::env::vars_os()
        .map(|(key, value)| {
            (
                JsString::from(key.to_string_lossy().as_ref()),
                JsString::from(value.to_string_lossy().as_ref()),
            )
        })
        .collect()
}

/// WHATWG `TextDecoder("utf-8")` in streaming mode: maximal-subpart U+FFFD
/// substitution (Rust's `Utf8Error::error_len`), an incomplete tail carried
/// to the next chunk, and one leading BOM stripped (`ignoreBOM: false`).
#[derive(Default)]
pub(crate) struct Utf8Stream {
    pending: Vec<u8>,
    started: bool,
}

impl Utf8Stream {
    pub(crate) fn decode(&mut self, chunk: &[u8]) -> String {
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
    pub(crate) fn flush(&mut self) -> String {
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

/// `ERR_INVALID_ARG_VALUE` for a string with a NUL byte, through the shared
/// `node_io` formatter (the same contract as Node's fs path validation).
fn nul_message(prefix: &[u16], value: &JsString) -> JsString {
    crate::node_io::invalid_arg_value_message(prefix, value.units())
}

/// libuv's error name for a spawn failure (`uv_err_name`), from the shared
/// complete table.
fn errno_name(error: &io::Error) -> Option<&'static str> {
    crate::node_io::lookup(error).map(|(code, _)| code)
}

/// A validated shell spawn: the resolved shell, its argv, and the exact cwd
/// and env Node would hand the OS.
pub(crate) struct PreparedSpawn {
    pub shell: CommandShell,
    args: Vec<JsString>,
    cwd: JsString,
    env: Option<Vec<(JsString, JsString)>>,
}

impl PreparedSpawn {
    /// `spawn(shell, args, { cwd, env, detached: true })` with `stdin` as
    /// given and piped stdout/stderr. The caller owns kill-on-drop policy.
    pub(crate) fn command(&self, stdin: Stdio) -> Command {
        let mut process = Command::new(lossy(&self.shell.shell));
        process
            .args(self.args.iter().map(lossy))
            .current_dir(os_path(&self.cwd))
            .process_group(0)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(env) = &self.env {
            process.env_clear();
            for (key, value) in env {
                process.env(lossy(key), lossy(value));
            }
        }
        process
    }
}

/// `resolveDefaultCommandShell` plus Node's `normalizeSpawnArguments` NUL
/// checks, in Node's order: args, then cwd, then each env entry (key before
/// value, `Object.keys` order). Without these checks `std::process::Command`
/// fails the spawn with its own InvalidInput error. The error is the message
/// Node throws synchronously. `resolve_env` is the env the shell resolver
/// reads (`env ?? process.env`).
pub(crate) fn prepare_spawn(
    command: &JsString,
    cwd: &JsString,
    env: Option<&[(JsString, JsString)]>,
    resolve_env: &[(JsString, JsString)],
) -> Result<PreparedSpawn, JsString> {
    let shell = resolve_default_command_shell(resolve_env)?;
    let args = shell.args(command);
    if let Some(index) = args.iter().position(|arg| arg.units().contains(&0)) {
        let prefix = units(&format!(
            "The argument 'args[{index}]' must be a string without null bytes. Received "
        ));
        return Err(nul_message(&prefix, &args[index]));
    }
    if cwd.units().contains(&0) {
        let prefix = units(
            "The property 'options.cwd' must be a string, Uint8Array, or URL without null bytes. Received ",
        );
        return Err(nul_message(&prefix, cwd));
    }
    for (key, value) in env.into_iter().flatten() {
        if let Some(bad) = [key, value]
            .into_iter()
            .find(|text| text.units().contains(&0))
        {
            let mut prefix = units("The property 'options.env['");
            prefix.extend_from_slice(key.units());
            prefix.extend(units("']' must be a string without null bytes. Received "));
            return Err(nul_message(&prefix, bad));
        }
    }
    Ok(PreparedSpawn {
        shell,
        args,
        cwd: cwd.clone(),
        env: env.map(<[_]>::to_vec),
    })
}

/// A failed `Command::spawn` in Node's terms.
pub(crate) enum SpawnError {
    /// `spawn <file> <code>`, delivered by Node as an `error` event after
    /// `spawn()` returned (EACCES, EAGAIN, EMFILE, ENFILE, ENOENT).
    Async(JsString),
    /// `spawn <code>`, thrown synchronously by `spawn()`.
    Sync(JsString),
}

pub(crate) fn classify_spawn_error(error: &io::Error, shell: &CommandShell) -> SpawnError {
    match errno_name(error) {
        Some(code @ ("EACCES" | "EAGAIN" | "EMFILE" | "ENFILE" | "ENOENT")) => {
            let mut message = units("spawn ");
            message.extend_from_slice(shell.shell.units());
            message.extend(units(&format!(" {code}")));
            SpawnError::Async(JsString::from_utf16(message))
        }
        Some(code) => SpawnError::Sync(JsString::from(format!("spawn {code}").as_str())),
        None => SpawnError::Sync(JsString::from(format!("spawn {error}").as_str())),
    }
}

/// `process.env` as Node's spawn defaults read it.
pub(crate) fn inherited_env() -> Vec<(JsString, JsString)> {
    process_env()
}

/// Kills the process group on Drop unless disarmed after the leader is reaped.
struct GroupGuard(Option<rustix::process::Pid>);

impl GroupGuard {
    fn signal(&self, signal: rustix::process::Signal) {
        if let Some(group) = self.0 {
            let _ = rustix::process::kill_process_group(group, signal);
        }
    }
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.signal(rustix::process::Signal::KILL);
    }
}

/// Resolves once the leader has exited, without reaping it. `Some(code)` is
/// the leader's status as `child.on("close")` reports it (`None` when it was
/// killed by a signal); the outer `None` means the status is not observable
/// without reaping (no WNOWAIT, or the child is no longer waitable).
async fn leader_exited(group: Option<rustix::process::Pid>) -> Option<Option<i32>> {
    // No longer waitable (or no WNOWAIT): the final `wait` reports the status.
    crate::process_group::wait_exit_unreaped(group?)
        .await
        .ok()
        .flatten()
        .map(|exit| exit.code)
}

/// `try { options.onStdout(text) } catch {}`: a panicking progress callback
/// never crashes the runner.
pub(crate) fn fire_and_forget(callback: &OutputCallback, text: &str) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| callback(text)));
}

const KILL_AFTER: Duration = Duration::from_millis(3_000);
const FORCE_RESOLVE_AFTER: Duration = Duration::from_millis(15_000);
const FORCE_RESOLVE_NOTE: &str =
    "\n[PilotDeck] Process did not exit within 15s after termination; force-resolved.";

/// Node `setTimeout` delay coercion: outside `[1, 2^31 - 1]` (including NaN)
/// the delay is 1 ms; fractional milliseconds are truncated.
pub(crate) fn timer_delay(ms: f64) -> Duration {
    Duration::from_millis(crate::runtime::async_hooks::timer_delay_ms(ms))
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_millis() as f64
}

async fn run(command: JsString, options: CommandOptions) -> Result<CommandResult, ToolError> {
    let started = Instant::now();
    let inherited;
    let env: &[(JsString, JsString)] = match &options.env {
        Some(env) => env,
        None => {
            inherited = process_env();
            &inherited
        }
    };
    let prepared = prepare_spawn(&command, &options.cwd, options.env.as_deref(), env)
        .map_err(ToolError::failed)?;
    let mut process = prepared.command(Stdio::piped());
    process.kill_on_drop(true);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => match classify_spawn_error(&error, &prepared.shell) {
            // Node emits these asynchronously (and resolves as timed out when
            // the signal is already aborted); the rest are thrown synchronously.
            SpawnError::Async(_) if options.signal.as_ref().is_some_and(AbortSignal::aborted) => {
                return Ok(CommandResult {
                    exit_code: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: true,
                    duration_ms: elapsed_ms(started),
                    pid: None,
                });
            }
            SpawnError::Async(message) | SpawnError::Sync(message) => {
                return Err(ToolError::failed(message));
            }
        },
    };
    let pid = child.id();
    let group = pid.and_then(|pid| rustix::process::Pid::from_raw(pid as i32));
    let mut guard = GroupGuard(group);
    // `child.stdin.end("")`.
    drop(child.stdin.take());
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    // `addEventListener("abort")` on an already-aborted signal never fires.
    let mut abort_armed = options
        .signal
        .as_ref()
        .is_some_and(|signal| !signal.aborted());
    let timeout_at = tokio::time::Instant::now() + timer_delay(options.timeout_ms);

    let limit = options
        .max_output_units
        .unwrap_or(crate::runtime::model::MAX_STRING_LENGTH);
    let mut out_text = StreamText::new(limit);
    let mut err_text = StreamText::new(limit);
    let mut out_decoder = Utf8Stream::default();
    let mut err_decoder = Utf8Stream::default();
    let mut out_buffer = vec![0u8; 64 * 1024];
    let mut err_buffer = vec![0u8; 64 * 1024];
    let mut timed_out = false;
    let mut timeout_armed = true;
    let mut kill_at: Option<tokio::time::Instant> = None;
    let mut force_at: Option<tokio::time::Instant> = None;
    let mut exited = false;
    let mut leader_status: Option<Option<i32>> = None;

    let terminate = |guard: &GroupGuard,
                     kill_at: &mut Option<tokio::time::Instant>,
                     force_at: &mut Option<tokio::time::Instant>| {
        guard.signal(rustix::process::Signal::TERM);
        let now = tokio::time::Instant::now();
        kill_at.get_or_insert(now + KILL_AFTER);
        force_at.get_or_insert(now + FORCE_RESOLVE_AFTER);
    };

    while stdout.is_some() || stderr.is_some() || !exited {
        let streams_closed = stdout.is_none() && stderr.is_none();
        tokio::select! {
            read = async { stdout.as_mut().unwrap().read(&mut out_buffer).await }, if stdout.is_some() => {
                match read {
                    Ok(length) if length > 0 => {
                        let text = out_decoder.decode(&out_buffer[..length]);
                        if out_text.push(&text)
                            && let Some(callback) = &options.on_stdout
                        {
                            fire_and_forget(callback, &text);
                        }
                    }
                    _ => stdout = None,
                }
            }
            read = async { stderr.as_mut().unwrap().read(&mut err_buffer).await }, if stderr.is_some() => {
                match read {
                    Ok(length) if length > 0 => {
                        let text = err_decoder.decode(&err_buffer[..length]);
                        if err_text.push(&text)
                            && let Some(callback) = &options.on_stderr
                        {
                            fire_and_forget(callback, &text);
                        }
                    }
                    _ => stderr = None,
                }
            }
            status = leader_exited(group), if streams_closed && !exited => {
                exited = true;
                leader_status = status;
            }
            _ = sleep_until(timeout_at), if timeout_armed => {
                timeout_armed = false;
                timed_out = true;
                terminate(&guard, &mut kill_at, &mut force_at);
            }
            _ = async { options.signal.as_ref().unwrap().wait().await }, if abort_armed => {
                abort_armed = false;
                terminate(&guard, &mut kill_at, &mut force_at);
            }
            _ = sleep_until(kill_at.unwrap_or(timeout_at)), if kill_at.is_some() => {
                kill_at = None;
                guard.signal(rustix::process::Signal::KILL);
            }
            _ = sleep_until(force_at.unwrap_or(timeout_at)), if force_at.is_some() => {
                // The baseline force-resolves without flushing the decoders.
                guard.signal(rustix::process::Signal::KILL);
                if matches!(child.try_wait(), Ok(Some(_))) {
                    guard.disarm();
                }
                // `stderr + note` throws past the limit in the baseline too
                // (and the force-resolve never settles); the note is dropped.
                err_text.push(FORCE_RESOLVE_NOTE);
                return Ok(CommandResult {
                    exit_code: None,
                    stdout: out_text.text,
                    stderr: err_text.text,
                    timed_out: true,
                    duration_ms: elapsed_ms(started),
                    pid,
                });
            }
        }
    }

    // A flush that no longer fits throws inside the baseline's `close`
    // handler, so its run never settles; the tail is dropped here instead.
    out_text.push(&out_decoder.flush());
    err_text.push(&err_decoder.flush());
    let exit_code = match (kill_at, leader_status) {
        // The baseline's 3 s SIGKILL is still pending after a terminated run
        // settled: it fires on schedule for group members that ignored TERM
        // without holding the pipes. The leader stays unreaped (a zombie)
        // until then, so the group ID cannot be reused. The timer owns only
        // the zombie and the guard; if the runtime drops it, the guard kills
        // the group at once.
        (Some(deadline), Some(code)) => {
            tokio::spawn(async move {
                sleep_until(deadline).await;
                guard.signal(rustix::process::Signal::KILL);
                guard.disarm();
                let _ = child.wait().await;
            });
            code
        }
        (pending, _) => {
            if pending.is_some() {
                // Status unobservable without reaping: kill now instead of
                // signalling a group ID that may be reused after the wait.
                guard.signal(rustix::process::Signal::KILL);
            }
            let status = child.wait().await;
            guard.disarm();
            status.ok().and_then(|status| status.code())
        }
    };
    Ok(CommandResult {
        exit_code,
        stdout: out_text.text,
        stderr: err_text.text,
        timed_out,
        duration_ms: elapsed_ms(started),
        pid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_delay_truncates_fractional_milliseconds() {
        for (input, expected) in [
            (1.9, 1),
            (1500.7, 1500),
            (2_147_483_646.9, 2_147_483_646),
            (0.9, 1),
            (f64::NAN, 1),
            (f64::INFINITY, 1),
            (2_147_483_647.1, 1),
        ] {
            assert_eq!(timer_delay(input), Duration::from_millis(expected));
        }
    }

    #[test]
    fn utf8_stream_matches_text_decoder() {
        // Node probe: decode([ff 61 e4 b8], {stream}) + decode() === "�a|�".
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.decode(&[0xff, 0x61, 0xe4, 0xb8]), "\u{FFFD}a");
        assert_eq!(stream.flush(), "\u{FFFD}");
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.decode(&[0xef, 0xbb]), "");
        assert_eq!(stream.decode(&[0xbf, b'h', 0xef, 0xbb, 0xbf]), "h\u{FEFF}");
        assert_eq!(stream.decode(&[0xe4]), "");
        assert_eq!(stream.decode(&[0xb8, 0xad]), "中");
    }

    #[test]
    fn nul_messages_use_the_shared_node_formatter() {
        // Raw values from `_tmp/20261020-a65-pr7/probes/nul2.out` / `nul5.out`.
        let args = |value: &str| {
            let message = nul_message(
                &units("The argument 'args[1]' must be a string without null bytes. Received "),
                &JsString::from(value),
            );
            String::from_utf16(message.units())
                .unwrap()
                .split_once("Received ")
                .unwrap()
                .1
                .to_string()
        };
        assert_eq!(args("a\0b"), "'a\\x00b'");
        assert_eq!(args("it's\0"), "\"it's\\x00\"");
        assert_eq!(args("it's \"q\"\0"), "`it's \"q\"\\x00`");
        assert_eq!(args("it's \"q\" `b`\0"), "'it\\'s \"q\" `b`\\x00'");
        assert_eq!(
            args(&format!("{}\0", "x".repeat(200))),
            format!("'{}...", "x".repeat(127))
        );
        let lone = nul_message(&units("p "), &JsString::from_utf16(vec![0xd800, 0]));
        assert_eq!(String::from_utf16_lossy(lone.units()), "p '\\ud800\\x00'");
    }
}
