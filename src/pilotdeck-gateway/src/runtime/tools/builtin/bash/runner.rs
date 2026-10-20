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
//!   result has already settled. A future cannot outlive its result without a
//!   detached task, so a terminated run that settles before 3 s SIGKILLs the
//!   group at settle time instead (the leader is still unreaped then).
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
    /// `env` with `undefined` members removed and values already `String()`d;
    /// `None` inherits the gateway process environment like Node does.
    pub env: Option<Vec<(String, String)>>,
    pub timeout_ms: f64,
    pub signal: Option<AbortSignal>,
    pub on_stdout: Option<OutputCallback>,
    pub on_stderr: Option<OutputCallback>,
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
    fn run(&self, command: String, options: CommandOptions) -> ToolFuture<CommandResult>;
}

/// The default local runner.
#[derive(Clone, Copy, Debug, Default)]
pub struct NodeShellCommandRunner;

impl CommandRunner for NodeShellCommandRunner {
    fn run(&self, command: String, options: CommandOptions) -> ToolFuture<CommandResult> {
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
    pub shell: String,
    pub kind: CommandShellKind,
}

impl CommandShell {
    pub fn args(&self, command: &str) -> Vec<String> {
        match self.kind {
            CommandShellKind::Cmd => vec![
                "/d".into(),
                "/s".into(),
                "/c".into(),
                format!("\"{command}\""),
            ],
            CommandShellKind::Pwsh => vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-Command".into(),
                command.into(),
            ],
            _ => vec!["-c".into(), command.into()],
        }
    }
}

fn env_get<'a>(env: &'a [(String, String)], key: &str) -> Option<&'a str> {
    env.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn exists(path: &str) -> bool {
    !path.is_empty() && Path::new(path).exists()
}

/// `findOnPath` (POSIX): the first `path` key in any case, `:`-separated,
/// empty entries skipped, candidates `path.join`ed.
fn find_on_path(command: &str, env: &[(String, String)]) -> Option<String> {
    let (_, value) = env.iter().find(|(name, _)| name.to_lowercase() == "path")?;
    if value.is_empty() {
        return None;
    }
    value
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            String::from_utf16_lossy(join(&JsString::from(entry), &JsString::from(command)).units())
        })
        .find(|candidate| exists(candidate))
}

fn command_available(command: &str, env: &[(String, String)]) -> bool {
    if command.contains('/') || command.contains('\\') {
        return exists(command);
    }
    find_on_path(command, env).is_some()
}

fn shell_from_path(shell: &str) -> CommandShell {
    let name = shell
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(shell)
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
        shell: shell.into(),
        kind,
    }
}

/// `resolveDefaultCommandShell({ env })` on a non-Windows platform. The error
/// is the baseline's thrown `Error` message.
pub fn resolve_default_command_shell(env: &[(String, String)]) -> Result<CommandShell, String> {
    if let Some(configured) = env_get(env, "PILOTDECK_SHELL_PATH").filter(|v| !v.is_empty()) {
        if !command_available(configured, env) {
            return Err(format!(
                "Configured PilotDeck shell was not found: {configured}"
            ));
        }
        return Ok(shell_from_path(configured));
    }
    let bash = |shell: String| CommandShell {
        shell,
        kind: CommandShellKind::Bash,
    };
    if exists("/bin/bash") {
        return Ok(bash("/bin/bash".into()));
    }
    if let Some(found) = find_on_path("bash", env) {
        return Ok(bash(found));
    }
    Ok(CommandShell {
        shell: "/bin/sh".into(),
        kind: CommandShellKind::Sh,
    })
}

fn process_env() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
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

/// `util.inspect(string)` as used by Node's `ERR_INVALID_ARG_VALUE`: quote
/// choice, escapes, line splitting past the break length and the 128-unit cut.
fn inspect_string(value: &str) -> String {
    fn escape(piece: &str) -> String {
        let (quote, escape_single) = if !piece.contains('\'') {
            ('\'', true)
        } else if !piece.contains('"') {
            ('"', false)
        } else if !piece.contains('`') && !piece.contains("${") {
            ('`', false)
        } else {
            ('\'', true)
        };
        let mut out = String::new();
        out.push(quote);
        for c in piece.chars() {
            match c {
                '\u{08}' => out.push_str("\\b"),
                '\t' => out.push_str("\\t"),
                '\n' => out.push_str("\\n"),
                '\u{0C}' => out.push_str("\\f"),
                '\r' => out.push_str("\\r"),
                '\\' => out.push_str("\\\\"),
                '\'' if escape_single => out.push_str("\\'"),
                c if (c as u32) < 0x20 || (0x7F..=0x9F).contains(&(c as u32)) => {
                    out.push_str(&format!("\\x{:02X}", c as u32));
                }
                c => out.push(c),
            }
        }
        out.push(quote);
        out
    }
    // `value.length > kMinLineWidth (16) && value.length > breakLength (128) -
    // indentation (0) - 4`; the second bound implies the first.
    let length = value.encode_utf16().count();
    let inspected = if length > 128 - 4 {
        value
            .split_inclusive('\n')
            .map(escape)
            .collect::<Vec<_>>()
            .join(" +\n  ")
    } else {
        escape(value)
    };
    let units: Vec<u16> = inspected.encode_utf16().collect();
    if units.len() > 128 {
        format!("{}...", String::from_utf16_lossy(&units[..128]))
    } else {
        inspected
    }
}

fn errno_name(error: &io::Error) -> Option<&'static str> {
    Some(match error.raw_os_error()? {
        libc::ENOENT => "ENOENT",
        libc::ENOTDIR => "ENOTDIR",
        libc::EACCES => "EACCES",
        libc::EPERM => "EPERM",
        libc::ELOOP => "ELOOP",
        libc::ENAMETOOLONG => "ENAMETOOLONG",
        libc::E2BIG => "E2BIG",
        libc::ENOEXEC => "ENOEXEC",
        libc::EAGAIN => "EAGAIN",
        libc::EMFILE => "EMFILE",
        libc::ENFILE => "ENFILE",
        libc::ENOMEM => "ENOMEM",
        libc::EISDIR => "EISDIR",
        libc::ETXTBSY => "ETXTBSY",
        libc::EINVAL => "EINVAL",
        libc::EIO => "EIO",
        _ => return None,
    })
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

/// Resolves once the leader has exited, without reaping it.
async fn leader_exited(group: Option<rustix::process::Pid>) {
    let Some(group) = group else {
        return;
    };
    #[cfg(not(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    )))]
    loop {
        use rustix::process::{WaitId, WaitIdOptions, waitid};
        match waitid(
            WaitId::Pid(group),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
            Err(rustix::io::Errno::INTR) => continue,
            // Exited, or the child is no longer waitable: the final `wait`
            // reports the real status either way.
            _ => return,
        }
    }
    #[cfg(any(
        target_os = "cygwin",
        target_os = "horizon",
        target_os = "openbsd",
        target_os = "redox",
        target_os = "wasi"
    ))]
    {
        let _ = group;
    }
}

const KILL_AFTER: Duration = Duration::from_millis(3_000);
const FORCE_RESOLVE_AFTER: Duration = Duration::from_millis(15_000);
const FORCE_RESOLVE_NOTE: &str =
    "\n[PilotDeck] Process did not exit within 15s after termination; force-resolved.";

/// Node `setTimeout` delay coercion: outside `[1, 2^31 - 1]` (including NaN)
/// the delay is 1 ms.
fn timer_delay(ms: f64) -> Duration {
    let ms = if (1.0..=2_147_483_647.0).contains(&ms) {
        ms
    } else {
        1.0
    };
    Duration::from_secs_f64(ms / 1000.0)
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_millis() as f64
}

async fn run(command: String, options: CommandOptions) -> Result<CommandResult, ToolError> {
    let started = Instant::now();
    let inherited;
    let env: &[(String, String)] = match &options.env {
        Some(env) => env,
        None => {
            inherited = process_env();
            &inherited
        }
    };
    let shell = resolve_default_command_shell(env).map_err(ToolError::failed)?;
    let args = shell.args(&command);
    if let Some(index) = args.iter().position(|arg| arg.contains('\0')) {
        return Err(ToolError::failed(format!(
            "The argument 'args[{index}]' must be a string without null bytes. Received {}",
            inspect_string(&args[index])
        )));
    }

    let mut process = Command::new(&shell.shell);
    process
        .args(&args)
        .current_dir(os_path(&options.cwd))
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(env) = &options.env {
        process.env_clear();
        for (key, value) in env {
            process.env(key, value);
        }
    }
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => {
            let name = errno_name(&error);
            // Node emits these asynchronously as `spawn <file> <code>` (and
            // resolves as timed out when the signal is already aborted);
            // every other errno is thrown synchronously as `spawn <code>`.
            if let Some(code @ ("EACCES" | "EAGAIN" | "EMFILE" | "ENFILE" | "ENOENT")) = name {
                if options.signal.as_ref().is_some_and(AbortSignal::aborted) {
                    return Ok(CommandResult {
                        exit_code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                        timed_out: true,
                        duration_ms: elapsed_ms(started),
                        pid: None,
                    });
                }
                return Err(ToolError::failed(format!("spawn {} {code}", shell.shell)));
            }
            return Err(ToolError::failed(match name {
                Some(code) => format!("spawn {code}"),
                None => format!("spawn {error}"),
            }));
        }
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

    let mut out_text = String::new();
    let mut err_text = String::new();
    let mut out_decoder = Utf8Stream::default();
    let mut err_decoder = Utf8Stream::default();
    let mut out_buffer = vec![0u8; 64 * 1024];
    let mut err_buffer = vec![0u8; 64 * 1024];
    let mut timed_out = false;
    let mut timeout_armed = true;
    let mut terminated = false;
    let mut kill_at: Option<tokio::time::Instant> = None;
    let mut force_at: Option<tokio::time::Instant> = None;
    let mut exited = false;

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
                        out_text.push_str(&text);
                        if let Some(callback) = &options.on_stdout {
                            callback(&text);
                        }
                    }
                    _ => stdout = None,
                }
            }
            read = async { stderr.as_mut().unwrap().read(&mut err_buffer).await }, if stderr.is_some() => {
                match read {
                    Ok(length) if length > 0 => {
                        let text = err_decoder.decode(&err_buffer[..length]);
                        err_text.push_str(&text);
                        if let Some(callback) = &options.on_stderr {
                            callback(&text);
                        }
                    }
                    _ => stderr = None,
                }
            }
            _ = leader_exited(group), if streams_closed && !exited => exited = true,
            _ = sleep_until(timeout_at), if timeout_armed => {
                timeout_armed = false;
                timed_out = true;
                terminated = true;
                terminate(&guard, &mut kill_at, &mut force_at);
            }
            _ = async { options.signal.as_ref().unwrap().wait().await }, if abort_armed => {
                abort_armed = false;
                terminated = true;
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
                err_text.push_str(FORCE_RESOLVE_NOTE);
                return Ok(CommandResult {
                    exit_code: None,
                    stdout: out_text,
                    stderr: err_text,
                    timed_out: true,
                    duration_ms: elapsed_ms(started),
                    pid,
                });
            }
        }
    }

    out_text.push_str(&out_decoder.flush());
    err_text.push_str(&err_decoder.flush());
    if terminated {
        // Stands in for the baseline's pending 3 s SIGKILL (see module docs).
        guard.signal(rustix::process::Signal::KILL);
    }
    let status = child.wait().await;
    guard.disarm();
    let exit_code = status.ok().and_then(|status| status.code());
    Ok(CommandResult {
        exit_code,
        stdout: out_text,
        stderr: err_text,
        timed_out,
        duration_ms: elapsed_ms(started),
        pid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn inspect_matches_node_err_invalid_arg_value() {
        // Raw values from `_tmp/20261020-a65-pr7/probes/nul2.out`.
        assert_eq!(inspect_string("a\0b"), "'a\\x00b'");
        assert_eq!(inspect_string("it's\0"), "\"it's\\x00\"");
        assert_eq!(inspect_string("it's \"q\"\0"), "`it's \"q\"\\x00`");
        assert_eq!(
            inspect_string("it's \"q\" `b`\0"),
            "'it\\'s \"q\" `b`\\x00'"
        );
        assert_eq!(
            inspect_string("\x01\x7f\u{85}\u{a0}\0"),
            "'\\x01\\x7F\\x85\u{a0}\\x00'"
        );
        let long = format!("{}\0", "x".repeat(200));
        assert_eq!(inspect_string(&long), format!("'{}...", "x".repeat(127)));
        let split = format!("line1\n{}\0\nz", "y".repeat(150));
        assert_eq!(
            inspect_string(&split),
            format!("'line1\\n' +\n  '{}...", "y".repeat(113))
        );
    }
}
