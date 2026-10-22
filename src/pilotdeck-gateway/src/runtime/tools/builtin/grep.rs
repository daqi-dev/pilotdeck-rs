//! `grep` (baseline `tool/builtin/grep.ts` + `filesystem/ripgrep.ts`).
//!
//! The TS tool does not implement matching itself: it spawns the ripgrep
//! binary that `@vscode/ripgrep` resolves for the current `platform-arch`
//! (`node_modules/@vscode/ripgrep-<platform>-<arch>/bin/rg`) and formats the
//! stdout. This port keeps the same split: `grep.rs` builds the exact argument
//! vector, runs ripgrep, and reproduces the TS line parsing, filtering,
//! ordering, pagination and error mapping. If no binary is found the tool
//! raises the same `unsupported_tool` message instead of fabricating matches.
//!
//! Binary resolution follows the TS contract
//! (`filesystem/ripgrep.ts:84-103`): the bundled
//! `@vscode/ripgrep-<platform>-<arch>/bin/rg` package is resolved with Node's
//! `node_modules` walk (pnpm's virtual store included) from the runtime root
//! and the gateway executable's ancestors. `PILOTDECK_RIPGREP_PATH` is an
//! explicit deployment/parity override; it is the only way to run an engine
//! outside the bundled package. Without a bundled engine the tool raises the
//! TS `unsupported_tool` wording, exactly like `require("@vscode/ripgrep")`
//! failing in `filesystem/ripgrep.ts:89-103` (no `PATH` fallback in TS).
//!
//! No ripgrep binary is downloaded or bundled by this crate.
use super::{
    ResolvedPath, basename, dirname, invalid, join, normalize, object, os_path, schema, string,
    text, tool_error, truthy,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    metadata::{js_space, js_trim, js_trim_end, js_trim_start},
    project_files::{node_collator, node_locale},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::Duration,
};
use tokio::{
    io::AsyncReadExt,
    process::Command,
    time::{Instant, sleep_until, timeout},
};

const DEFAULT_HEAD_LIMIT: f64 = 250.0;
const MAX_COLUMNS: usize = 500;
const RIPGREP_TIMEOUT_MS: u64 = 20_000;
const EXCLUDED_DIRECTORY_GLOBS: [&str; 8] = [
    "!.git",
    "!.svn",
    "!.hg",
    "!.bzr",
    "!.jj",
    "!.sl",
    "!node_modules",
    "!dist",
];

const DESCRIPTION: &str = "A ripgrep-powered search tool for workspace file contents.\n\nUsage:\n- ALWAYS use `grep` for content search tasks. Do NOT invoke `grep` or `rg` through `bash` when this tool can express the search.\n- Supports full regular expressions, `type` filters, glob filters, and multiline search.\n- Output modes: `content` shows matching lines, `files_with_matches` lists file paths, and `count` shows per-file match counts.\n- `head_limit` and `offset` work in every output mode. Pass `head_limit: 0` only when you truly need unlimited results.\n- For open-ended multi-step exploration, prefer the `agent` tool's `explore` subagent instead of repeatedly broadening searches in the parent agent.";

const INPUT_SCHEMA: &str = r#"{"type":"object","required":["pattern"],"additionalProperties":false,"properties":{"pattern":{"type":"string","description":"The regular expression pattern to search for in file contents."},"path":{"type":"string","description":"File or directory to search within. Defaults to the workspace root."},"glob":{"type":"string","description":"Glob pattern to filter files (for example '*.js' or '*.{ts,tsx}'). Maps to ripgrep's --glob."},"output_mode":{"type":"string","enum":["content","files_with_matches","count"],"description":"Output mode: 'content' shows matching lines, 'files_with_matches' lists file paths, and 'count' shows per-file counts. Defaults to 'files_with_matches'."},"-B":{"type":"integer","description":"Number of lines to show before each match (content mode only)."},"-A":{"type":"integer","description":"Number of lines to show after each match (content mode only)."},"-C":{"type":"integer","description":"Alias for context: number of lines to show before and after each match."},"context":{"type":"integer","description":"Number of lines to show before and after each match (content mode only)."},"-n":{"type":"boolean","description":"When false, hide line numbers in content mode output. Defaults to true. Ignored in other modes."},"-i":{"type":"boolean","description":"When true, perform case-insensitive matching."},"type":{"type":"string","description":"File type to search (for example 'js', 'ts', 'py'). Maps to ripgrep's --type filter."},"head_limit":{"type":"integer","description":"Maximum number of output entries to return. Applies to all output modes. Defaults to 250; pass 0 for unlimited."},"offset":{"type":"integer","description":"Skip this many output entries before returning results."},"multiline":{"type":"boolean","description":"When true, enable multiline mode where . matches newlines and patterns can span lines."}}}"#;

pub struct GrepTool {
    fields: JsObject,
}

impl GrepTool {
    pub fn new() -> Self {
        Self {
            fields: definition(),
        }
    }
}

impl Default for GrepTool {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolDefinition for GrepTool {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _input: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}

impl ExecutableTool for GrepTool {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        Box::pin(async move { execute(&input, &context).await })
    }
}

fn definition() -> JsObject {
    object([
        ("name", string("grep")),
        ("aliases", JsValue::Array(vec![string("Grep")])),
        ("description", string(DESCRIPTION)),
        ("kind", string("filesystem")),
        ("inputSchema", JsValue::Object(schema(INPUT_SCHEMA))),
        ("maxResultBytes", JsValue::Number(200_000.0)),
    ])
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Content,
    FilesWithMatches,
    Count,
}

impl Mode {
    fn parse(name: &str) -> Self {
        match name {
            "content" => Self::Content,
            "count" => Self::Count,
            _ => Self::FilesWithMatches,
        }
    }
}

fn input_string<'a>(input: &'a JsValue, key: &str) -> Option<&'a JsString> {
    input.get(key).and_then(JsValue::as_string)
}

fn input_number(input: &JsValue, key: &str) -> Option<f64> {
    input.get(key).and_then(JsValue::as_number)
}

fn input_bool(input: &JsValue, key: &str) -> Option<bool> {
    input.get(key).and_then(JsValue::as_bool)
}

async fn execute(input: &JsValue, context: &ToolRunContext) -> Result<ToolOutput, ToolError> {
    let pattern = input_string(input, "pattern")
        .ok_or_else(|| invalid("pattern must be a string."))?
        .clone();
    let path = input_string(input, "path")
        .cloned()
        .unwrap_or_else(|| JsString::from("."));
    let resolved = super::resolve_workspace_path(&path, context, false, false)?;
    // TS keeps the raw `output_mode` string in `data.mode`; only the exact
    // "content"/"count" values change the branch.
    let mode_name = input_string(input, "output_mode")
        .map(text)
        .unwrap_or_else(|| "files_with_matches".to_owned());
    let mode = Mode::parse(&mode_name);
    let target = resolve_search_target(&resolved)?;
    let args = build_ripgrep_args(input, mode, &text(&pattern), &target.target);
    let stdout = run_ripgrep(&args, &target.cwd, context).await?;

    match mode {
        Mode::Content => content_mode(input, &stdout, &target),
        Mode::Count => count_mode(input, &stdout, &target),
        Mode::FilesWithMatches => files_mode(input, &stdout, &target, &mode_name).await,
    }
}

struct SearchTarget {
    cwd: JsString,
    target: JsString,
    workspace_base_dir: String,
}

/// `resolveSearchTarget`: a directory search runs from that directory; a file
/// search runs from its parent with the basename as the ripgrep target, and
/// every reported path is re-based onto the file's workspace directory.
fn resolve_search_target(resolved: &ResolvedPath) -> Result<SearchTarget, ToolError> {
    let metadata = std::fs::metadata(os_path(&resolved.absolute))
        .map_err(|error| super::failed(error, "stat", &resolved.absolute))?;
    let relative = super::normalize(&resolved.relative);
    if metadata.is_dir() {
        return Ok(SearchTarget {
            cwd: resolved.absolute.clone(),
            target: JsString::from("."),
            workspace_base_dir: if relative.equals_str(".") {
                String::new()
            } else {
                text(&relative)
            },
        });
    }
    let directory = dirname(&relative);
    Ok(SearchTarget {
        cwd: dirname(&resolved.absolute),
        target: basename(&resolved.absolute),
        workspace_base_dir: if directory.equals_str(".") {
            String::new()
        } else {
            text(&directory)
        },
    })
}

fn build_ripgrep_args(
    input: &JsValue,
    mode: Mode,
    pattern: &str,
    target: &JsString,
) -> Vec<String> {
    let mut args = vec![
        "--hidden".to_owned(),
        "--max-columns".to_owned(),
        MAX_COLUMNS.to_string(),
    ];
    for excluded in EXCLUDED_DIRECTORY_GLOBS {
        args.push("--glob".to_owned());
        args.push(excluded.to_owned());
    }
    if input.get("multiline").is_some_and(truthy) {
        args.push("-U".to_owned());
        args.push("--multiline-dotall".to_owned());
    }
    if input.get("-i").is_some_and(truthy) {
        args.push("-i".to_owned());
    }
    if let Some(kind) = input_string(input, "type").filter(|value| !value.is_empty()) {
        args.push("--type".to_owned());
        args.push(text(kind));
    }
    if let Some(glob) = input_string(input, "glob") {
        for pattern in split_glob_patterns(&text(glob)) {
            args.push("--glob".to_owned());
            args.push(pattern);
        }
    }
    match mode {
        Mode::FilesWithMatches => args.push("-l".to_owned()),
        Mode::Count => {
            args.push("-c".to_owned());
            args.push("--with-filename".to_owned());
        }
        Mode::Content => {
            args.push("--with-filename".to_owned());
            args.push("-n".to_owned());
            add_context_args(&mut args, input);
        }
    }
    if pattern.starts_with('-') {
        args.push("-e".to_owned());
        args.push(pattern.to_owned());
    } else {
        args.push(pattern.to_owned());
    }
    args.push(text(target));
    args
}

fn add_context_args(args: &mut Vec<String>, input: &JsValue) {
    if let Some(context) = input_number(input, "context") {
        args.push("-C".to_owned());
        args.push(js_number_string(context));
        return;
    }
    if let Some(context) = input_number(input, "-C") {
        args.push("-C".to_owned());
        args.push(js_number_string(context));
        return;
    }
    if let Some(before) = input_number(input, "-B") {
        args.push("-B".to_owned());
        args.push(js_number_string(before));
    }
    if let Some(after) = input_number(input, "-A") {
        args.push("-A".to_owned());
        args.push(js_number_string(after));
    }
}

/// `splitGlobPatterns`: JS `/\s+/` separates patterns, brace groups stay
/// intact, and a bare comma separates alternatives (ripgrep accepts both).
fn split_glob_patterns(glob: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    for raw in glob.split(js_space).filter(|part| !part.is_empty()) {
        if raw.contains('{') && raw.contains('}') {
            patterns.push(raw.to_owned());
            continue;
        }
        patterns.extend(
            raw.split(',')
                .filter(|part| !part.is_empty())
                .map(str::to_owned),
        );
    }
    patterns
}

// ---------------------------------------------------------------------------
// ripgrep execution
// ---------------------------------------------------------------------------

fn resolve_ripgrep(context: &ToolRunContext) -> Result<PathBuf, ToolError> {
    let environment = context.fields.get("env").and_then(JsValue::as_object);
    resolve_ripgrep_in(environment, executable_parent())
}

fn executable_parent() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|executable| executable.parent().map(Path::to_path_buf))
}

/// Split from [`resolve_ripgrep`] so a test can pin the module-walk roots
/// without inheriting the real test binary's ancestors. The signature has no
/// `PATH` input on purpose: TS's `require("@vscode/ripgrep")` failure is
/// terminal and never falls back to a system `rg`.
fn resolve_ripgrep_in(
    environment: Option<&JsObject>,
    executable_parent: Option<PathBuf>,
) -> Result<PathBuf, ToolError> {
    let get = |key: &str| -> Option<String> {
        environment
            .and_then(|entries| entries.get(key))
            .and_then(JsValue::as_string)
            .map(text)
            .or_else(|| std::env::var(key).ok())
    };
    // An empty override means "unset" (the same convention as
    // `PILOTDECK_SHELL_PATH`); it never names a directory or a command.
    let override_path = get("PILOTDECK_RIPGREP_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let roots = default_roots_in(get("PILOTDECK_DESKTOP_RUNTIME_ROOT"), executable_parent);
    resolve_ripgrep_with(override_path, &roots)
}

/// Whether the tool can resolve a ripgrep engine in this process. Mirrors the
/// production resolver exactly (override or bundled package); parity harnesses
/// that may use a system `rg` must add their own test-only lookup. Without an
/// engine the tool fails with `unsupported_tool`, so replaying the oracle
/// would otherwise test nothing.
pub fn ripgrep_available() -> bool {
    let override_path = std::env::var_os("PILOTDECK_RIPGREP_PATH")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let roots = default_roots(std::env::var("PILOTDECK_DESKTOP_RUNTIME_ROOT").ok());
    resolve_ripgrep_with(override_path, &roots).is_ok()
}

/// Resolution order: explicit `PILOTDECK_RIPGREP_PATH` (deployment/parity
/// pin) -> the bundled `@vscode/ripgrep-<platform>-<arch>/bin/rg` package ->
/// the TS `unsupported_tool` error. This is the TS contract
/// (`filesystem/ripgrep.ts:89-103`): `require("@vscode/ripgrep")` failure is
/// terminal and never falls back to `PATH`.
fn resolve_ripgrep_with(
    override_path: Option<PathBuf>,
    roots: &[PathBuf],
) -> Result<PathBuf, ToolError> {
    if let Some(path) = override_path {
        if is_executable(&path) {
            return Ok(path);
        }
        return Err(ripgrep_unavailable(&format!(
            "PILOTDECK_RIPGREP_PATH={} is not an executable file",
            path.display()
        )));
    }
    match bundled_ripgrep(roots) {
        Ok(Some(path)) => Ok(path),
        Ok(None) => Err(ripgrep_unavailable("Cannot find module '@vscode/ripgrep'")),
        Err(cause) => Err(ripgrep_unavailable(&cause)),
    }
}

fn default_roots(runtime_root: Option<String>) -> Vec<PathBuf> {
    default_roots_in(runtime_root, executable_parent())
}

fn default_roots_in(
    runtime_root: Option<String>,
    executable_parent: Option<PathBuf>,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = runtime_root.filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(root));
    }
    if let Some(parent) = executable_parent {
        roots.push(parent);
    }
    roots
}

/// Node's `require.resolve("@vscode/ripgrep-<platform>-<arch>/bin/rg")`:
/// walk `node_modules` from every root's ancestors, and for a found
/// `@vscode/ripgrep` package continue from its real path so pnpm's virtual
/// store (`node_modules/.pnpm/@vscode+ripgrep.../node_modules`) resolves too.
fn bundled_ripgrep(roots: &[PathBuf]) -> Result<Option<PathBuf>, String> {
    let (platform, arch) = platform_arch();
    let package = format!("@vscode/ripgrep-{platform}-{arch}");
    let binary = if cfg!(windows) { "rg.exe" } else { "rg" };
    let mut saw_package = false;
    for root in roots {
        for ancestor in root.ancestors() {
            for modules in [
                ancestor.join("node_modules"),
                ancestor
                    .join("resources")
                    .join("PilotDeck")
                    .join("node_modules"),
            ] {
                let package_dir = modules.join("@vscode").join("ripgrep");
                if !package_dir.is_dir() {
                    continue;
                }
                saw_package = true;
                if let Ok(real) = package_dir.canonicalize() {
                    for base in real.ancestors() {
                        if let Some(found) =
                            platform_binary(&base.join("node_modules"), &package, binary)
                        {
                            return Ok(Some(found));
                        }
                    }
                }
            }
        }
    }
    if saw_package {
        Err(format!(
            "Could not find {package}. Ensure optionalDependencies are installed for this platform ({platform}-{arch})."
        ))
    } else {
        Ok(None)
    }
}

fn platform_binary(modules: &Path, package: &str, binary: &str) -> Option<PathBuf> {
    let candidate = modules.join(package).join("bin").join(binary);
    is_executable(&candidate).then_some(candidate)
}

fn platform_arch() -> (&'static str, &'static str) {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    };
    (platform, arch)
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn ripgrep_unavailable(cause: &str) -> ToolError {
    let (platform, arch) = platform_arch();
    ToolError {
        code: "unsupported_tool".to_owned(),
        message: JsString::from(format!(
            "grep requires the bundled ripgrep binary from @vscode/ripgrep, but it is not available for {platform}-{arch}. Reinstall dependencies with optional dependencies enabled."
        )),
        details: Some(object([("cause", string(cause))])),
    }
}

async fn run_ripgrep(
    args: &[String],
    cwd: &JsString,
    context: &ToolRunContext,
) -> Result<String, ToolError> {
    run_ripgrep_with_timeout(
        args,
        cwd,
        context,
        Duration::from_millis(RIPGREP_TIMEOUT_MS),
    )
    .await
}

/// TS `SubprocessPort.executeFile` signals the whole process group (TERM, then
/// KILL after one second) and resolves when the child's stdio closes. The loop
/// below keeps draining both pipes through that window, so a descendant that
/// inherited a pipe cannot leave the tool waiting forever.
const KILL_GRACE: Duration = Duration::from_millis(1_000);
/// Last-resort bound for an escaped descendant that survived the group KILL
/// and still holds a pipe. TS would simply never resolve in that pathological
/// case; a tool call must stay bounded.
const FORCE_RESOLVE: Duration = Duration::from_secs(3);

async fn run_ripgrep_with_timeout(
    args: &[String],
    cwd: &JsString,
    context: &ToolRunContext,
    limit: Duration,
) -> Result<String, ToolError> {
    let executable = resolve_ripgrep(context)?;
    if context.is_aborted() {
        return Err(tool_aborted());
    }
    let mut command = Command::new(&executable);
    command
        .args(args)
        .current_dir(os_path(cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // `spawn(..., { detached: true })` on every platform TS supports a group
    // for, plus libuv's child-init signal reset. The reset matters when the
    // gateway itself inherited ignored/blocked signals.
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(unix)]
    pilotdeck_child_signals::install_child_signal_reset(command.as_std_mut());
    // The TS baseline hands `context.env` to `spawn` as the child environment
    // (Node replaces, it does not merge), so mirror that when the host
    // supplies one.
    if let Some(environment) = context.fields.get("env").and_then(JsValue::as_object) {
        command.env_clear();
        for (key, value) in environment.entries() {
            if let Some(value) = value.as_string() {
                command.env(text(key), text(value));
            }
        }
    }
    let mut child = command.spawn().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            // Node's async spawn error text: `spawn <path> ENOENT`.
            ripgrep_unavailable(&format!("spawn {} ENOENT", executable.display()))
        } else if let Some((code, _)) = crate::node_io::lookup(&error) {
            tool_error(
                "tool_execution_failed",
                format!(
                    "ripgrep grep search failed: spawn {} {code}",
                    executable.display()
                ),
            )
        } else {
            tool_error(
                "tool_execution_failed",
                format!("ripgrep grep search failed: {error}"),
            )
        }
    })?;
    #[cfg(unix)]
    let group = child
        .id()
        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32));
    #[cfg(not(unix))]
    let group: Option<rustix::process::Pid> = None;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut out_task = tokio::spawn(read_all(stdout));
    let mut err_task = tokio::spawn(read_all(stderr));
    let mut out_bytes = Vec::new();
    let mut err_bytes = Vec::new();
    let mut out_done = false;
    let mut err_done = false;

    // Observe the leader without reaping it (`WNOWAIT`), so its PGID stays
    // reserved until the final timeout/abort group signal has been sent.
    let (leader_tx, mut leader_rx) = tokio::sync::oneshot::channel();
    if let Some(group) = group {
        tokio::spawn(async move {
            let _ = leader_tx.send(crate::process_group::wait_exit_unreaped(group).await);
        });
    } else {
        let _ = leader_tx.send(Ok(None));
    }
    let mut leader_done = group.is_none();
    let mut leader_exit: Option<crate::process_group::LeaderExit> = None;

    let mut timed_out = false;
    let mut aborted = false;
    let mut forced = false;
    let mut kill_sent = false;
    let mut kill_deadline: Option<Instant> = None;
    let mut timeout_sleep = Box::pin(sleep_until(Instant::now() + limit));
    let mut kill_sleep: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;
    let mut force_sleep: Option<std::pin::Pin<Box<tokio::time::Sleep>>> = None;

    loop {
        if leader_done && out_done && err_done {
            break;
        }
        tokio::select! {
            result = &mut leader_rx, if !leader_done => {
                leader_done = true;
                if let Ok(Ok(Some(exit))) = result {
                    leader_exit = Some(exit);
                }
            }
            result = &mut out_task, if !out_done => {
                out_bytes = result.unwrap_or_default();
                out_done = true;
            }
            result = &mut err_task, if !err_done => {
                err_bytes = result.unwrap_or_default();
                err_done = true;
            }
            _ = &mut timeout_sleep, if !timed_out && !aborted => {
                timed_out = true;
                signal_group(group, &mut child, rustix::process::Signal::TERM);
                kill_deadline = Some(Instant::now() + KILL_GRACE);
                kill_sleep = Some(Box::pin(sleep_until(Instant::now() + KILL_GRACE)));
                force_sleep = Some(Box::pin(sleep_until(Instant::now() + FORCE_RESOLVE)));
            }
            _ = context.cancelled(), if !timed_out && !aborted => {
                aborted = true;
                signal_group(group, &mut child, rustix::process::Signal::TERM);
                kill_deadline = Some(Instant::now() + KILL_GRACE);
                kill_sleep = Some(Box::pin(sleep_until(Instant::now() + KILL_GRACE)));
                force_sleep = Some(Box::pin(sleep_until(Instant::now() + FORCE_RESOLVE)));
            }
            _ = async {
                if let Some(sleep) = kill_sleep.as_mut() {
                    sleep.as_mut().await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if kill_sleep.is_some() => {
                kill_sleep = None;
                kill_deadline = None;
                kill_sent = true;
                signal_group(group, &mut child, rustix::process::Signal::KILL);
            }
            _ = async {
                if let Some(sleep) = force_sleep.as_mut() {
                    sleep.as_mut().await;
                } else {
                    std::future::pending::<()>().await;
                }
            }, if force_sleep.is_some() => {
                forced = true;
                break;
            }
        }
    }

    if forced {
        out_task.abort();
        err_task.abort();
        let _ = out_task.await;
        let _ = err_task.await;
    }
    // On timeout/abort TS sends TERM immediately, resolves at `close`, and lets
    // its 1 s KILL timer run on (`SubprocessPort.ts:83-92`). The loop can break
    // at pipe EOF long before that timer fires (a leader whose stdio is closed
    // exits while a descendant is still cleaning up). Returning at that point
    // must not SIGKILL early; hand the deferred KILL and the reap to a
    // background task so the leader stays an unreaped zombie and the PGID
    // cannot be reused before the final group signal.
    if timed_out || aborted {
        let deferred =
            !kill_sent && kill_deadline.is_some_and(|deadline| Instant::now() < deadline);
        if deferred {
            let deadline = kill_deadline.expect("deferred kill has a deadline");
            tokio::spawn(async move {
                sleep_until(deadline).await;
                signal_group(group, &mut child, rustix::process::Signal::KILL);
                let _ = timeout(Duration::from_secs(5), child.wait()).await;
                crate::process_group::wait_for_group_empty(group, KILL_GRACE).await;
            });
        } else {
            if !kill_sent {
                signal_group(group, &mut child, rustix::process::Signal::KILL);
            }
            let _ = timeout(Duration::from_secs(5), child.wait()).await;
        }
        if aborted || context.is_aborted() {
            return Err(tool_aborted());
        }
        return Err(tool_error(
            "tool_timeout",
            format!("grep search timed out after {RIPGREP_TIMEOUT_MS}ms."),
        ));
    }
    let status = match timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => Some(status),
        Ok(Err(error)) => {
            return Err(tool_error(
                "tool_execution_failed",
                format!("ripgrep grep search failed: {error}"),
            ));
        }
        Err(_) => {
            signal_group(group, &mut child, rustix::process::Signal::KILL);
            None
        }
    };
    if context.is_aborted() {
        return Err(tool_aborted());
    }
    let Some(status) = status else {
        return Err(tool_error(
            "tool_execution_failed",
            "ripgrep grep search failed: process did not exit.".to_owned(),
        ));
    };
    let (code, exit_signal) = match leader_exit {
        Some(exit) => (exit.code, exit.signal),
        None => {
            #[cfg(unix)]
            let signal = {
                use std::os::unix::process::ExitStatusExt;
                status.signal()
            };
            #[cfg(not(unix))]
            let signal = None;
            (status.code(), signal)
        }
    };
    if code == Some(0) || code == Some(1) {
        return Ok(String::from_utf8_lossy(&out_bytes).into_owned());
    }
    let error_text = String::from_utf8_lossy(&err_bytes);
    let trimmed = js_trim(&error_text);
    if let Some(signal) = exit_signal {
        return Err(tool_error(
            "tool_execution_failed",
            format!(
                "ripgrep grep search exited via signal {}.",
                node_signal_name(signal)
            ),
        ));
    }
    let exit_code = code.unwrap_or(-1);
    let mut details = object([("exitCode", JsValue::Number(f64::from(exit_code)))]);
    if !trimmed.is_empty() {
        details.insert("stderr".into(), string(trimmed));
    }
    Err(ToolError {
        code: "tool_execution_failed".to_owned(),
        message: JsString::from(if trimmed.is_empty() {
            format!("ripgrep grep search failed with exit code {exit_code}.")
        } else {
            format!("ripgrep grep search failed: {trimmed}")
        }),
        details: Some(details),
    })
}

fn tool_aborted() -> ToolError {
    tool_error("tool_aborted", "grep search aborted.")
}

fn signal_group(
    group: Option<rustix::process::Pid>,
    child: &mut tokio::process::Child,
    signal: rustix::process::Signal,
) {
    #[cfg(unix)]
    if group.is_some() {
        crate::process_group::kill(group, signal);
        return;
    }
    #[cfg(not(unix))]
    let _ = (group, signal);
    let _ = child.start_kill();
}

/// `os.constants.signals[signo]`, the value Node exposes as `exitSignal`.
fn node_signal_name(signal: i32) -> String {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    let name = match signal {
        1 => Some("SIGHUP"),
        2 => Some("SIGINT"),
        3 => Some("SIGQUIT"),
        4 => Some("SIGILL"),
        5 => Some("SIGTRAP"),
        6 => Some("SIGABRT"),
        8 => Some("SIGFPE"),
        9 => Some("SIGKILL"),
        10 => Some("SIGBUS"),
        11 => Some("SIGSEGV"),
        12 => Some("SIGSYS"),
        13 => Some("SIGPIPE"),
        14 => Some("SIGALRM"),
        15 => Some("SIGTERM"),
        16 => Some("SIGURG"),
        17 => Some("SIGSTOP"),
        18 => Some("SIGTSTP"),
        19 => Some("SIGCONT"),
        20 => Some("SIGCHLD"),
        21 => Some("SIGTTIN"),
        22 => Some("SIGTTOU"),
        23 => Some("SIGIO"),
        24 => Some("SIGXCPU"),
        25 => Some("SIGXFSZ"),
        26 => Some("SIGVTALRM"),
        27 => Some("SIGPROF"),
        28 => Some("SIGWINCH"),
        29 => Some("SIGINFO"),
        30 => Some("SIGUSR1"),
        31 => Some("SIGUSR2"),
        _ => None,
    };
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let name = match signal {
        1 => Some("SIGHUP"),
        2 => Some("SIGINT"),
        3 => Some("SIGQUIT"),
        4 => Some("SIGILL"),
        5 => Some("SIGTRAP"),
        6 => Some("SIGABRT"),
        7 => Some("SIGBUS"),
        8 => Some("SIGFPE"),
        9 => Some("SIGKILL"),
        10 => Some("SIGUSR1"),
        11 => Some("SIGSEGV"),
        12 => Some("SIGUSR2"),
        13 => Some("SIGPIPE"),
        14 => Some("SIGALRM"),
        15 => Some("SIGTERM"),
        16 => Some("SIGSTKFLT"),
        17 => Some("SIGCHLD"),
        18 => Some("SIGCONT"),
        19 => Some("SIGSTOP"),
        20 => Some("SIGTSTP"),
        21 => Some("SIGTTIN"),
        22 => Some("SIGTTOU"),
        23 => Some("SIGURG"),
        24 => Some("SIGXCPU"),
        25 => Some("SIGXFSZ"),
        26 => Some("SIGVTALRM"),
        27 => Some("SIGPROF"),
        28 => Some("SIGWINCH"),
        29 => Some("SIGIO"),
        30 => Some("SIGPWR"),
        31 => Some("SIGSYS"),
        _ => None,
    };
    name.map(str::to_owned)
        .unwrap_or_else(|| signal.to_string())
}

async fn read_all(mut reader: impl tokio::io::AsyncRead + Unpin) -> Vec<u8> {
    let mut bytes = Vec::new();
    let _ = reader.read_to_end(&mut bytes).await;
    bytes
}

// ---------------------------------------------------------------------------
// output modes
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum ParsedLine {
    Separator(String),
    Content(ContentLine),
}

#[derive(Debug)]
struct ContentLine {
    file: String,
    separator: char,
    line_number: i64,
    line_separator: char,
    content: String,
}

fn content_mode(
    input: &JsValue,
    stdout: &str,
    target: &SearchTarget,
) -> Result<ToolOutput, ToolError> {
    let parsed: Vec<ParsedLine> = split_ripgrep_lines(stdout)
        .into_iter()
        .map(parse_content_line)
        .collect::<Result<Vec<ParsedLine>, ToolError>>()?
        .into_iter()
        .filter(|line| match line {
            ParsedLine::Separator(_) => true,
            ParsedLine::Content(line) => !is_ignored_path(&line.file),
        })
        .collect();
    let show_line_numbers = input_bool(input, "-n").unwrap_or(true);
    let rendered: Vec<String> = parsed
        .iter()
        .map(|line| match line {
            ParsedLine::Separator(raw) => raw.clone(),
            ParsedLine::Content(line) => {
                format_content_line(line, &target.workspace_base_dir, show_line_numbers)
            }
        })
        .collect();
    let page = paginate(
        &rendered,
        input_number(input, "head_limit"),
        input_number(input, "offset"),
    );
    let mut files: Vec<String> = parsed
        .iter()
        .filter_map(|line| match line {
            ParsedLine::Content(line) => {
                Some(to_workspace_file(&line.file, &target.workspace_base_dir))
            }
            ParsedLine::Separator(_) => None,
        })
        .collect();
    files.sort_by(|left, right| js_compare(left, right));
    files.dedup();
    let text = format_paged_text_result(
        &page.items.join("\n"),
        &page,
        parsed.len(),
        input_number(input, "head_limit"),
    );
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(text)),
        ]))],
        supplemental_messages: None,
        data: Some(JsValue::Object(object([
            ("mode", string("content")),
            (
                "files",
                JsValue::Array(files.into_iter().map(string).collect()),
            ),
            ("count", JsValue::Number(parsed.len() as f64)),
            ("truncated", JsValue::Bool(page.truncated)),
        ]))),
        metadata: Some(object([("truncated", JsValue::Bool(page.truncated))])),
    })
}

fn count_mode(
    input: &JsValue,
    stdout: &str,
    target: &SearchTarget,
) -> Result<ToolOutput, ToolError> {
    let mut entries: Vec<(String, i64)> = split_ripgrep_lines(stdout)
        .into_iter()
        .filter_map(parse_count_entry)
        .filter(|(file, _)| !is_ignored_path(file))
        .collect();
    // TS sorts the raw rg paths before rebasing them onto `workspaceBaseDir`.
    entries.sort_by(|left, right| locale_compare(&left.0, &right.0));
    let total_matches: i64 = entries.iter().map(|(_, count)| *count).sum();
    let mapped: Vec<(String, i64)> = entries
        .iter()
        .map(|(file, count)| (to_workspace_file(file, &target.workspace_base_dir), *count))
        .collect();
    let rendered: Vec<(String, String)> = mapped
        .iter()
        .map(|(file, count)| (file.clone(), format!("{file}:{count}")))
        .collect();
    let page = paginate(
        &rendered,
        input_number(input, "head_limit"),
        input_number(input, "offset"),
    );
    let body = page
        .items
        .iter()
        .map(|(_, line)| line.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let text = format_paged_text_result(
        &body,
        &page,
        entries.len(),
        input_number(input, "head_limit"),
    );
    let files: Vec<JsValue> = page
        .items
        .iter()
        .map(|(file, _)| string(file.clone()))
        .collect();
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(text)),
        ]))],
        supplemental_messages: None,
        data: Some(JsValue::Object(object([
            ("mode", string("count")),
            ("files", JsValue::Array(files)),
            ("count", JsValue::Number(total_matches as f64)),
            ("truncated", JsValue::Bool(page.truncated)),
        ]))),
        metadata: Some(object([("truncated", JsValue::Bool(page.truncated))])),
    })
}

async fn files_mode(
    input: &JsValue,
    stdout: &str,
    target: &SearchTarget,
    mode_name: &str,
) -> Result<ToolOutput, ToolError> {
    let mut files: Vec<String> = split_ripgrep_lines(stdout)
        .into_iter()
        .map(|line| normalize_relative_path(&line))
        .filter(|file| !is_ignored_path(file))
        .collect();
    sort_files_by_modified_time(&mut files, &target.cwd).await;
    let workspace_files: Vec<String> = files
        .iter()
        .map(|file| to_workspace_file(file, &target.workspace_base_dir))
        .collect();
    let page = paginate(
        &workspace_files,
        input_number(input, "head_limit"),
        input_number(input, "offset"),
    );
    let text = format_paged_text_result(
        &page.items.join("\n"),
        &page,
        workspace_files.len(),
        input_number(input, "head_limit"),
    );
    Ok(ToolOutput {
        content: vec![JsValue::Object(object([
            ("type", string("text")),
            ("text", string(text)),
        ]))],
        supplemental_messages: None,
        data: Some(JsValue::Object(object([
            ("mode", string(mode_name)),
            (
                "files",
                JsValue::Array(page.items.iter().map(|file| string(file.clone())).collect()),
            ),
            ("count", JsValue::Number(workspace_files.len() as f64)),
            ("truncated", JsValue::Bool(page.truncated)),
        ]))),
        metadata: Some(object([("truncated", JsValue::Bool(page.truncated))])),
    })
}

async fn sort_files_by_modified_time(files: &mut [String], cwd: &JsString) {
    let mut keyed: Vec<(String, f64)> = Vec::with_capacity(files.len());
    for file in files.iter() {
        let path = join(cwd, &JsString::from(file.as_str()));
        let modified = std::fs::metadata(os_path(&path))
            .ok()
            .map(|metadata| metadata_modified_ms(&metadata))
            .unwrap_or(0.0);
        keyed.push((file.clone(), modified));
    }
    keyed.sort_by(|left, right| {
        right
            .1
            .partial_cmp(&left.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| locale_compare(&left.0, &right.0))
    });
    for (slot, (file, _)) in files.iter_mut().zip(keyed) {
        *slot = file;
    }
}

fn metadata_modified_ms(metadata: &std::fs::Metadata) -> f64 {
    // Node's `fs.stat().mtimeMs` keeps the sub-millisecond fraction; truncating
    // to whole milliseconds would change `sortFilesByModifiedTime` ties.
    match metadata.modified() {
        Ok(modified) => match modified.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => duration.as_secs_f64() * 1_000.0,
            Err(error) => -(error.duration().as_secs_f64() * 1_000.0),
        },
        Err(_) => 0.0,
    }
}

// ---------------------------------------------------------------------------
// parsing / formatting
// ---------------------------------------------------------------------------

fn split_ripgrep_lines(stdout: &str) -> Vec<String> {
    stdout
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .map(js_trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn parse_content_line(line: String) -> Result<ParsedLine, ToolError> {
    if line == "--" {
        return Ok(ParsedLine::Separator(line));
    }
    // JS `/^(.*?)([:-])(\d+)([:-])(.*)$/` without flags: `.` stops at every
    // LineTerminator, `\d` is ASCII-only, and `$` matches only at the very end
    // of the input. It does NOT accept a trailing LineTerminator; the oracle's
    // trailing-U+2028 case succeeds because `splitRipgrepLines().trimEnd()` has
    // already removed it (`filesystem/ripgrep.ts:117-122`), not because `$`
    // matches before it.
    let units: Vec<char> = line.chars().collect();
    let end = units.len();
    let mut index = 0;
    while index < end {
        let separator = units[index];
        if is_line_terminator(separator) {
            break;
        }
        if separator != ':' && separator != '-' {
            index += 1;
            continue;
        }
        let digits_start = index + 1;
        let mut cursor = digits_start;
        while cursor < end && units[cursor].is_ascii_digit() {
            cursor += 1;
        }
        if cursor > digits_start && cursor < end {
            let line_separator = units[cursor];
            if (line_separator == ':' || line_separator == '-')
                && !units[cursor + 1..end]
                    .iter()
                    .any(|unit| is_line_terminator(*unit))
            {
                let file: String = units[..index].iter().collect();
                let digits: String = units[digits_start..cursor].iter().collect();
                let content: String = units[cursor + 1..end].iter().collect();
                return Ok(ParsedLine::Content(ContentLine {
                    file: normalize_relative_path(&file),
                    separator,
                    line_number: digits.parse().unwrap_or(0),
                    line_separator,
                    content,
                }));
            }
        }
        index += 1;
    }
    Err(tool_error(
        "tool_execution_failed",
        format!("Unexpected ripgrep content output: {line}"),
    ))
}

fn is_line_terminator(unit: char) -> bool {
    matches!(unit, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn parse_count_entry(line: String) -> Option<(String, i64)> {
    let separator = line.rfind(':')?;
    if separator == 0 {
        return None;
    }
    let file = normalize_relative_path(&line[..separator]);
    let count = js_parse_int(&line[separator + 1..])?;
    Some((file, count))
}

/// `Number.parseInt(value, 10)`: leading whitespace and sign accepted, the
/// parse stops at the first non-digit, and no digits means `NaN`.
fn js_parse_int(value: &str) -> Option<i64> {
    let value = js_trim_start(value);
    let mut chars = value.chars().peekable();
    let mut sign = 1i64;
    if let Some(first) = chars.peek() {
        match first {
            '-' => {
                sign = -1;
                chars.next();
            }
            '+' => {
                chars.next();
            }
            _ => {}
        }
    }
    let mut digits = String::new();
    while let Some(character) = chars.peek() {
        if character.is_ascii_digit() {
            digits.push(*character);
            chars.next();
        } else {
            break;
        }
    }
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok().map(|value| sign * value)
}

fn format_content_line(
    line: &ContentLine,
    workspace_base_dir: &str,
    show_line_numbers: bool,
) -> String {
    let file = to_workspace_file(&line.file, workspace_base_dir);
    if show_line_numbers {
        format!(
            "{file}{}{}{}{}",
            line.separator, line.line_number, line.line_separator, line.content
        )
    } else {
        format!("{file}{}{}", line.separator, line.content)
    }
}

fn normalize_relative_path(file: &str) -> String {
    let without_dot = file.strip_prefix("./").unwrap_or(file);
    text(&normalize(&JsString::from(without_dot)))
}

fn to_workspace_file(file: &str, workspace_base_dir: &str) -> String {
    let normalized = normalize_relative_path(file);
    if workspace_base_dir.is_empty() {
        return normalized;
    }
    text(&join(
        &JsString::from(workspace_base_dir),
        &JsString::from(normalized),
    ))
}

fn is_ignored_path(file: &str) -> bool {
    file.split('/')
        .any(|segment| matches!(segment, ".git" | "node_modules" | "dist"))
}

// ---------------------------------------------------------------------------
// pagination
// ---------------------------------------------------------------------------

struct Page<T> {
    items: Vec<T>,
    truncated: bool,
    offset: f64,
    limit: Option<f64>,
    next_offset: Option<f64>,
}

fn paginate<T: Clone>(items: &[T], head_limit: Option<f64>, offset: Option<f64>) -> Page<T> {
    let normalized_offset = normalize_count(offset.unwrap_or(0.0));
    if head_limit == Some(0.0) {
        let start = index_at(normalized_offset, items.len());
        return Page {
            items: items[start..].to_vec(),
            truncated: false,
            offset: normalized_offset,
            limit: None,
            next_offset: None,
        };
    }
    let normalized_limit = normalize_count(head_limit.unwrap_or(DEFAULT_HEAD_LIMIT));
    let start = index_at(normalized_offset, items.len());
    let end = index_at(normalized_offset + normalized_limit, items.len()).max(start);
    let page = items[start..end].to_vec();
    let next_offset = normalized_offset + page.len() as f64;
    let truncated = items.len() as f64 > normalized_offset + normalized_limit;
    Page {
        items: page,
        truncated,
        offset: normalized_offset,
        limit: Some(normalized_limit),
        next_offset: if truncated { Some(next_offset) } else { None },
    }
}

/// `Math.max(0, value)` preserving JavaScript's NaN and infinities.
fn normalize_count(value: f64) -> f64 {
    if value.is_nan() {
        f64::NAN
    } else {
        value.max(0.0)
    }
}

fn index_at(value: f64, length: usize) -> usize {
    if value.is_nan() || value <= 0.0 {
        return 0;
    }
    if value >= length as f64 {
        return length;
    }
    value.trunc() as usize
}

fn format_paged_text_result<T>(
    body: &str,
    page: &Page<T>,
    total: usize,
    head_limit: Option<f64>,
) -> String {
    let mut lines = vec![if body.is_empty() {
        "[No matches on this page]".to_owned()
    } else {
        body.to_owned()
    }];
    let mut summary = format!(
        "[grep pagination] returned={} total={} offset={}",
        page.items.len(),
        total,
        js_number_string(page.offset)
    );
    if let Some(limit) = page.limit {
        summary.push_str(&format!(" limit={}", js_number_string(limit)));
    }
    summary.push_str(&format!(" truncated={}", page.truncated));
    lines.push(String::new());
    lines.push(summary);
    if page.truncated
        && let Some(next) = page.next_offset
    {
        if let Some(limit) = head_limit {
            lines.push(format!(
                "More results are available. Call grep again with offset={} and head_limit={}.",
                js_number_string(next),
                js_number_string(limit)
            ));
        } else {
            lines.push(format!(
                "More results are available. Call grep again with offset={}.",
                js_number_string(next)
            ));
        }
    }
    lines.join("\n")
}

fn js_number_string(value: f64) -> String {
    crate::js_value::number_string(value)
}

// ---------------------------------------------------------------------------
// sorting / string helpers
// ---------------------------------------------------------------------------

/// `Array.prototype.sort` default: UTF-16 code unit order.
fn js_compare(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

/// Node's `String#localeCompare` with the process default locale
/// (`LC_ALL`/`LC_MESSAGES`/`LANG`, the same `node_locale`/`node_collator`
/// calibration used by `project_files`).
fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    static COLLATOR: OnceLock<Option<icu_collator::CollatorBorrowed<'static>>> = OnceLock::new();
    let collator = COLLATOR.get_or_init(|| {
        let locale = node_locale(|key| std::env::var(key).ok());
        node_collator(&locale).ok()
    });
    let Some(collator) = collator else {
        return js_compare(left, right);
    };
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    collator.compare_utf16(&left, &right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        host::modules::{
            capability::RuntimeContext,
            tool_context::{HostToolCheckpoint, ToolRuntimeContext},
        },
        runtime::network::Cancellation,
        runtime::tools::builtin::is_absolute,
    };
    use std::{
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration as StdDuration, UNIX_EPOCH},
    };

    static SCRATCH: AtomicUsize = AtomicUsize::new(0);

    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(label: &str) -> Self {
            let base = std::fs::canonicalize(std::env::temp_dir()).expect("temp dir");
            let root = base.join(format!(
                "pdrs-grep-unit-{label}-{}-{}",
                std::process::id(),
                SCRATCH.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir(&root).expect("create scratch");
            Self { root }
        }

        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.root.join(name);
            std::fs::write(&path, body).expect("write script");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod script");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn context(env: &[(&str, &str)], abort: Option<Cancellation>, cwd: &Path) -> ToolRunContext {
        let mut environment = JsObject::new();
        for (key, value) in env {
            environment.insert((*key).into(), JsValue::from(*value));
        }
        let fields = object([
            ("cwd", JsValue::from(cwd.to_string_lossy().as_ref())),
            ("env", JsValue::Object(environment)),
        ]);
        let host = ToolRuntimeContext {
            fields,
            files: HostToolCheckpoint::default().tool_context_state(),
            abort_signal: abort.map(|signal| Arc::new(signal) as RuntimeContext),
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: None,
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        };
        ToolRunContext::new(Arc::new(host))
    }

    fn futures_block<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    fn read_pid(path: &Path) -> rustix::process::Pid {
        let raw: i32 = std::fs::read_to_string(path)
            .expect("descendant pid file")
            .trim()
            .parse()
            .expect("descendant pid");
        rustix::process::Pid::from_raw(raw).expect("valid pid")
    }

    /// Poll under the caller's runtime so a deferred background cleanup task
    /// can run; a blocking sleep would starve the current-thread runtime.
    async fn wait_process_gone(pid: rustix::process::Pid, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if rustix::process::test_kill_process(pid).is_err() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn kill_descendant_if_alive(pid: rustix::process::Pid) {
        // Keep a failing negative control from leaving `sleep 30` behind.
        crate::process_group::kill(Some(pid), rustix::process::Signal::KILL);
    }

    fn cwd_string(root: &Path) -> JsString {
        JsString::from(root.to_string_lossy().as_ref())
    }

    fn set_mtime(path: &Path, millis: f64) {
        let taken = UNIX_EPOCH + StdDuration::from_secs_f64(millis / 1_000.0);
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("open for mtime")
            .set_modified(taken)
            .expect("set mtime");
    }

    /// Raw filesystem precision, independent of `metadata_modified_ms`, so a
    /// truncating regression cannot turn the test into a skip.
    fn raw_mtime_ms(path: &Path) -> f64 {
        std::fs::metadata(path)
            .expect("metadata")
            .modified()
            .expect("mtime")
            .duration_since(UNIX_EPOCH)
            .expect("after epoch")
            .as_secs_f64()
            * 1_000.0
    }

    #[test]
    fn content_lines_follow_js_regex_dot_and_digit_semantics() {
        assert!(parse_content_line("cr.txt:1:foo\rbar".to_owned()).is_err());
        assert!(parse_content_line("ls.txt:1:foo\u{2028}bar".to_owned()).is_err());
        // JS `$` without `m` does not match before a trailing LineTerminator.
        for terminator in ['\n', '\r', '\u{2028}', '\u{2029}'] {
            let line = format!("trail.txt:1:foo{terminator}");
            assert!(
                parse_content_line(line.clone()).is_err(),
                "JS $ must reject a trailing {terminator:?}"
            );
            // The product path trims it first; that (not `$`) is why the
            // trailing-U+2028 oracle scenario succeeds.
            let mut stdout = line;
            stdout.push('\n');
            let trimmed = split_ripgrep_lines(&stdout);
            let parsed =
                parse_content_line(trimmed[0].clone()).expect("trimmed trailing terminator");
            let ParsedLine::Content(line) = parsed else {
                panic!("expected content")
            };
            assert_eq!(line.content, "foo");
            assert_eq!(line.file, "trail.txt");
        }

        let parsed =
            parse_content_line("a:\u{0661}:note.txt:1:foo".to_owned()).expect("unicode digits");
        let ParsedLine::Content(line) = parsed else {
            panic!("expected content")
        };
        assert_eq!(line.file, "a:\u{0661}:note.txt");
        assert_eq!(line.line_number, 1);
        assert_eq!(line.content, "foo");
    }

    #[test]
    fn js_whitespace_covers_bom_and_line_terminators_but_not_nel() {
        assert_eq!(js_trim_end("foo\u{feff}"), "foo");
        assert_eq!(js_trim_end("foo\u{0085}"), "foo\u{0085}");
        assert_eq!(
            split_ripgrep_lines("a\u{feff}\nb\u{0085}\n"),
            vec!["a".to_owned(), "b\u{0085}".to_owned()]
        );
        assert_eq!(
            split_glob_patterns("*.ts\u{feff}*.md"),
            vec!["*.ts", "*.md"]
        );
        assert_eq!(
            split_glob_patterns("*.ts\u{0085}*.md"),
            vec!["*.ts\u{0085}*.md"]
        );
        assert_eq!(js_parse_int("\u{feff} 12"), Some(12));
        assert_eq!(js_parse_int("\u{0085}12"), None);
    }

    #[test]
    fn files_sort_keeps_submillisecond_mtime_precision() {
        let scratch = Scratch::new("mtime");
        let z = scratch.root.join("z.ts");
        let a = scratch.root.join("a.ts");
        std::fs::write(&z, "foo\n").expect("write z");
        std::fs::write(&a, "foo\n").expect("write a");
        set_mtime(&z, 1_700_000_000_100.8);
        set_mtime(&a, 1_700_000_000_100.2);
        let z_ms = raw_mtime_ms(&z);
        let a_ms = raw_mtime_ms(&a);
        if (a_ms - z_ms).abs() < 0.05 {
            eprintln!("skip: filesystem did not preserve sub-millisecond mtimes");
            return;
        }
        let mut files = vec!["z.ts".to_owned(), "a.ts".to_owned()];
        futures_block(sort_files_by_modified_time(
            &mut files,
            &cwd_string(&scratch.root),
        ));
        assert_eq!(files, vec!["z.ts".to_owned(), "a.ts".to_owned()]);
    }

    #[test]
    fn bundled_ripgrep_resolves_to_the_platform_binary_and_matches_the_ts_lookup() {
        let scratch = Scratch::new("bundled");
        let (platform, arch) = platform_arch();
        let app = scratch.root.join("app");
        let bin = app
            .join("node_modules")
            .join(format!("@vscode/ripgrep-{platform}-{arch}"))
            .join("bin");
        std::fs::create_dir_all(&bin).expect("bundle bin");
        let package_dir = app.join("node_modules").join("@vscode").join("ripgrep");
        std::fs::create_dir_all(&package_dir).expect("wrapper package");
        std::fs::write(package_dir.join("package.json"), "{}").expect("wrapper manifest");
        let bundled = bin.join("rg");
        std::fs::write(&bundled, "#!/bin/sh\nprintf 'x.txt:1:hit\\n'\n").expect("write bundled");
        std::fs::set_permissions(&bundled, std::fs::Permissions::from_mode(0o755))
            .expect("chmod bundled");

        let resolved =
            resolve_ripgrep_with(None, std::slice::from_ref(&app)).expect("bundled resolution");
        assert_eq!(resolved, bundled, "bundled package platform binary");

        let context = context(
            &[
                (
                    "PILOTDECK_DESKTOP_RUNTIME_ROOT",
                    app.to_str().expect("utf8 app"),
                ),
                ("PILOTDECK_RIPGREP_PATH", ""),
            ],
            None,
            &scratch.root,
        );
        let output = futures_block(run_ripgrep_with_timeout(
            &[],
            &cwd_string(&scratch.root),
            &context,
            StdDuration::from_secs(5),
        ))
        .expect("bundled engine runs");
        assert_eq!(output, "x.txt:1:hit\n");
    }

    #[test]
    fn missing_bundled_ripgrep_reports_the_ts_unsupported_tool_error() {
        let error = resolve_ripgrep_with(None, &[]).expect_err("missing");
        assert_eq!(error.code, "unsupported_tool");
        assert_eq!(
            error.message.to_utf8().unwrap(),
            format!(
                "grep requires the bundled ripgrep binary from @vscode/ripgrep, but it is not available for {}-{}. Reinstall dependencies with optional dependencies enabled.",
                platform_arch().0,
                platform_arch().1
            )
        );
        let cause = error
            .details
            .expect("details")
            .get("cause")
            .and_then(JsValue::as_string)
            .map(text)
            .expect("cause");
        assert_eq!(cause, "Cannot find module '@vscode/ripgrep'");
    }

    #[test]
    fn missing_bundled_ripgrep_never_falls_back_to_path() {
        let scratch = Scratch::new("no-path-fallback");
        let path_dir = scratch.root.join("path-bin");
        std::fs::create_dir(&path_dir).expect("path bin");
        let on_path = path_dir.join("rg");
        std::fs::write(&on_path, "#!/bin/sh\nexit 0\n").expect("write path rg");
        std::fs::set_permissions(&on_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod path rg");
        assert!(is_executable(&on_path), "precondition: rg is on PATH");
        // Drive the same entry point the tool uses, with only a scratch root
        // and a pinned executable parent so the real machine's module tree
        // cannot answer the lookup. `PATH` names an executable rg, yet
        // `require("@vscode/ripgrep")` failure is terminal in TS
        // (`filesystem/ripgrep.ts:89-103`), so resolution must still fail; a
        // reintroduced PATH fallback would return `Ok`.
        let environment = object([
            ("PILOTDECK_RIPGREP_PATH", JsValue::from("")),
            (
                "PILOTDECK_DESKTOP_RUNTIME_ROOT",
                JsValue::from(scratch.root.to_string_lossy().as_ref()),
            ),
            ("PATH", JsValue::from(path_dir.to_string_lossy().as_ref())),
        ]);
        let fake_executable = scratch.root.join("fake-exe");
        std::fs::create_dir(&fake_executable).expect("fake exe dir");
        let error = resolve_ripgrep_in(Some(&environment), Some(fake_executable))
            .expect_err("bundled-only resolution");
        assert_eq!(error.code, "unsupported_tool");
        let cause = error
            .details
            .expect("details")
            .get("cause")
            .and_then(JsValue::as_string)
            .map(text)
            .expect("cause");
        assert_eq!(cause, "Cannot find module '@vscode/ripgrep'");
    }

    #[test]
    fn timeout_kills_a_descendant_that_holds_the_stdout_pipe() {
        let scratch = Scratch::new("timeout");
        let pid_file = scratch.root.join("descendant.pid");
        let script = scratch.script(
            "rg-hold",
            &format!(
                "#!/bin/sh\n( /bin/sleep 30 ) &\necho $! > '{}'\nprintf 'partial\\n'\nexit 0\n",
                pid_file.display()
            ),
        );
        let context = context(
            &[
                (
                    "PILOTDECK_RIPGREP_PATH",
                    script.to_str().expect("utf8 script"),
                ),
                ("PATH", "/usr/bin:/bin"),
            ],
            None,
            &scratch.root,
        );
        let cwd = cwd_string(&scratch.root);
        let (error, elapsed) = futures_block(async move {
            let started = std::time::Instant::now();
            let error =
                run_ripgrep_with_timeout(&[], &cwd, &context, StdDuration::from_millis(3000))
                    .await
                    .expect_err("timeout");
            let elapsed = started.elapsed();
            let pid = read_pid(&pid_file);
            if !wait_process_gone(pid, StdDuration::from_secs(3)).await {
                kill_descendant_if_alive(pid);
                panic!("descendant {pid:?} survived the timeout");
            }
            (error, elapsed)
        });
        assert_eq!(error.code, "tool_timeout");
        assert!(
            elapsed < StdDuration::from_secs(5),
            "timeout must bound pipe draining: {elapsed:?}"
        );
    }

    #[test]
    fn timeout_keeps_the_ts_grace_when_the_pipes_close_early() {
        let scratch = Scratch::new("grace");
        let marker = scratch.root.join("descendant-cleaned");
        // The descendant keeps stdout open, so the loop cannot finish before
        // the 3 s timeout fires. On TERM it closes the pipes immediately
        // (the Rust loop then breaks at pipe EOF) but keeps cleaning up for
        // 0.35 s. TS's 1 s KILL timer must not cut that short; the deferred
        // group KILL fires at ~4 s, after the marker is written.
        let script = scratch.script(
            "rg-grace",
            &format!(
                "#!/bin/sh\n(\n  trap 'exec 1>&- 2>&-; sleep 0.35; printf done > \"{}\"; exit 0' TERM\n  while :; do sleep 0.05; done\n) &\nprintf 'partial\\n'\nexit 0\n",
                marker.display()
            ),
        );
        let context = context(
            &[
                (
                    "PILOTDECK_RIPGREP_PATH",
                    script.to_str().expect("utf8 script"),
                ),
                ("PATH", "/usr/bin:/bin"),
            ],
            None,
            &scratch.root,
        );
        let cwd = cwd_string(&scratch.root);
        let marker_for_check = marker.clone();
        let (error, elapsed) = futures_block(async move {
            let started = std::time::Instant::now();
            let error =
                run_ripgrep_with_timeout(&[], &cwd, &context, StdDuration::from_millis(3000))
                    .await
                    .expect_err("timeout");
            let elapsed = started.elapsed();
            // The deferred group KILL runs in the background one second after
            // TERM (limit + 1 s); this 0.35 s cleanup must finish before it.
            let deadline = Instant::now() + StdDuration::from_secs(2);
            while !marker_for_check.exists() && Instant::now() < deadline {
                tokio::time::sleep(StdDuration::from_millis(10)).await;
            }
            (error, elapsed)
        });
        assert_eq!(error.code, "tool_timeout");
        assert!(
            elapsed < StdDuration::from_millis(3_900),
            "pipe EOF should return before the grace timer: {elapsed:?}"
        );
        assert!(
            marker.exists(),
            "descendant was SIGKILLed before its 0.35 s cleanup finished"
        );
    }

    #[test]
    fn deferred_kill_still_fires_after_the_pipe_eof_return() {
        let scratch = Scratch::new("grace-kill");
        let pid_file = scratch.root.join("descendant.pid");
        // The descendant survives TERM (closes the pipes, then keeps running),
        // so only the deferred group KILL can end it. The tool must return at
        // pipe EOF while the descendant is still alive, then the background
        // task must still fire the KILL one second after TERM.
        let script = scratch.script(
            "rg-grace-kill",
            &format!(
                "#!/bin/sh\n(\n  trap 'exec 1>&- 2>&-; while :; do sleep 0.05; done' TERM\n  while :; do sleep 0.05; done\n) &\necho $! > '{}'\nprintf 'partial\\n'\nexit 0\n",
                pid_file.display()
            ),
        );
        let context = context(
            &[
                (
                    "PILOTDECK_RIPGREP_PATH",
                    script.to_str().expect("utf8 script"),
                ),
                ("PATH", "/usr/bin:/bin"),
            ],
            None,
            &scratch.root,
        );
        let cwd = cwd_string(&scratch.root);
        let (error, alive_at_return, gone) = futures_block(async move {
            let error =
                run_ripgrep_with_timeout(&[], &cwd, &context, StdDuration::from_millis(2000))
                    .await
                    .expect_err("timeout");
            let pid = read_pid(&pid_file);
            let alive_at_return = rustix::process::test_kill_process(pid).is_ok();
            let gone = wait_process_gone(pid, StdDuration::from_secs(3)).await;
            if !gone {
                kill_descendant_if_alive(pid);
            }
            (error, alive_at_return, gone)
        });
        assert_eq!(error.code, "tool_timeout");
        assert!(
            alive_at_return,
            "the 1 s grace was cut short: descendant was already gone at return"
        );
        assert!(
            gone,
            "the deferred group KILL never fired; the descendant survived"
        );
    }

    #[test]
    fn cancellation_during_drain_after_leader_exit_aborts_the_run() {
        let scratch = Scratch::new("cancel");
        let pid_file = scratch.root.join("descendant.pid");
        let script = scratch.script(
            "rg-hold",
            &format!(
                "#!/bin/sh\n( /bin/sleep 30 ) &\necho $! > '{}'\nprintf 'partial\\n'\nexit 0\n",
                pid_file.display()
            ),
        );
        let signal = Cancellation::new();
        let context = context(
            &[
                (
                    "PILOTDECK_RIPGREP_PATH",
                    script.to_str().expect("utf8 script"),
                ),
                ("PATH", "/usr/bin:/bin"),
            ],
            Some(signal.clone()),
            &scratch.root,
        );
        let cwd = cwd_string(&scratch.root);
        let pid_file_for_trigger = pid_file.clone();
        let (error, elapsed) = futures_block(async move {
            let started = std::time::Instant::now();
            let trigger = tokio::spawn(async move {
                // Cancel only after the shell has started and recorded the
                // descendant; a fixed delay raced with process startup (>200 ms
                // in this sandbox) and sometimes cancelled before the fork.
                let deadline = Instant::now() + StdDuration::from_secs(8);
                while !pid_file_for_trigger.exists() && Instant::now() < deadline {
                    tokio::time::sleep(StdDuration::from_millis(10)).await;
                }
                tokio::time::sleep(StdDuration::from_millis(100)).await;
                signal.cancel();
            });
            let result =
                run_ripgrep_with_timeout(&[], &cwd, &context, StdDuration::from_secs(10)).await;
            let _ = trigger.await;
            let elapsed = started.elapsed();
            let pid = read_pid(&pid_file);
            if !wait_process_gone(pid, StdDuration::from_secs(3)).await {
                kill_descendant_if_alive(pid);
                panic!("descendant {pid:?} survived cancellation");
            }
            (result.expect_err("aborted"), elapsed)
        });
        assert_eq!(error.code, "tool_aborted");
        assert!(
            elapsed < StdDuration::from_secs(5),
            "cancellation must bound pipe draining: {elapsed:?}"
        );
    }

    #[test]
    fn signal_exit_uses_the_node_signal_name() {
        let scratch = Scratch::new("signal");
        let script = scratch.script("rg-term", "#!/bin/sh\nkill -TERM $$\n");
        let context = context(
            &[
                (
                    "PILOTDECK_RIPGREP_PATH",
                    script.to_str().expect("utf8 script"),
                ),
                ("PATH", "/usr/bin:/bin"),
            ],
            None,
            &scratch.root,
        );
        let error = futures_block(run_ripgrep_with_timeout(
            &[],
            &cwd_string(&scratch.root),
            &context,
            StdDuration::from_secs(5),
        ))
        .expect_err("signalled");
        assert_eq!(
            error.message.to_utf8().unwrap(),
            "ripgrep grep search exited via signal SIGTERM."
        );
    }

    #[test]
    fn nonzero_exit_carries_exit_code_and_stderr_details() {
        let scratch = Scratch::new("nonzero");
        let script = scratch.script("rg-fail", "#!/bin/sh\nprintf 'boom' >&2\nexit 2\n");
        let context = context(
            &[
                (
                    "PILOTDECK_RIPGREP_PATH",
                    script.to_str().expect("utf8 script"),
                ),
                ("PATH", "/usr/bin:/bin"),
            ],
            None,
            &scratch.root,
        );
        let error = futures_block(run_ripgrep_with_timeout(
            &[],
            &cwd_string(&scratch.root),
            &context,
            StdDuration::from_secs(5),
        ))
        .expect_err("failed");
        assert_eq!(error.code, "tool_execution_failed");
        assert_eq!(
            error.message.to_utf8().unwrap(),
            "ripgrep grep search failed: boom"
        );
        let details = error.details.expect("details");
        assert_eq!(
            details.get("exitCode").and_then(JsValue::as_number),
            Some(2.0)
        );
        assert_eq!(
            details
                .get("stderr")
                .and_then(JsValue::as_string)
                .map(text)
                .as_deref(),
            Some("boom")
        );
    }

    #[test]
    fn locale_compare_uses_the_icu_collator() {
        // Default JS sort would put "B" before "a"; ICU en-US puts "a" first.
        assert_eq!(locale_compare("a", "B"), std::cmp::Ordering::Less);
    }

    #[test]
    fn glob_patterns_follow_the_ts_split_rules() {
        assert!(split_glob_patterns("").is_empty());
        assert_eq!(split_glob_patterns("*.ts"), vec!["*.ts"]);
        assert_eq!(
            split_glob_patterns("*.{ts,tsx}"),
            vec!["*.{ts,tsx}"],
            "brace groups stay intact"
        );
        assert_eq!(
            split_glob_patterns("*.ts,*.js"),
            vec!["*.ts", "*.js"],
            "comma alternatives split"
        );
        assert_eq!(
            split_glob_patterns("*.ts *.md"),
            vec!["*.ts", "*.md"],
            "whitespace separates"
        );
        assert_eq!(
            split_glob_patterns("src/**/*.ts,  docs/*.md "),
            vec!["src/**/*.ts", "docs/*.md"]
        );
    }

    #[test]
    fn content_lines_parse_match_and_context_separators() {
        let parsed = parse_content_line("src/a.ts:12:needle".to_owned()).expect("content line");
        let ParsedLine::Content(line) = parsed else {
            panic!("expected content")
        };
        assert_eq!(line.file, "src/a.ts");
        assert_eq!(line.separator, ':');
        assert_eq!(line.line_number, 12);
        assert_eq!(line.line_separator, ':');
        assert_eq!(line.content, "needle");

        // Context lines use `-` on either side and keep a `:` inside content.
        let parsed = parse_content_line("src/a.ts-9-  before: x".to_owned()).expect("context");
        let ParsedLine::Content(line) = parsed else {
            panic!("expected content")
        };
        assert_eq!(
            (line.separator, line.line_number, line.line_separator),
            ('-', 9, '-')
        );
        assert_eq!(line.content, "  before: x");

        assert!(matches!(
            parse_content_line("--".to_owned()).unwrap(),
            ParsedLine::Separator(raw) if raw == "--"
        ));
        let error = parse_content_line("no line number here".to_owned()).unwrap_err();
        assert_eq!(error.code, "tool_execution_failed");
        assert_eq!(
            error.message.to_utf8().unwrap(),
            "Unexpected ripgrep content output: no line number here"
        );
    }

    #[test]
    fn count_entries_use_the_last_colon_and_js_parse_int() {
        assert_eq!(
            parse_count_entry("src/a.ts:12".to_owned()),
            Some(("src/a.ts".to_owned(), 12))
        );
        assert_eq!(
            parse_count_entry("./src/c.js:1".to_owned()),
            Some(("src/c.js".to_owned(), 1))
        );
        assert_eq!(parse_count_entry(":3".to_owned()), None);
        assert_eq!(parse_count_entry("src/a.ts".to_owned()), None);
        assert_eq!(parse_count_entry("src/a.ts:x".to_owned()), None);
        assert_eq!(js_parse_int("12abc"), Some(12));
        assert_eq!(js_parse_int("  -7"), Some(-7));
        assert_eq!(js_parse_int("+0"), Some(0));
        assert_eq!(js_parse_int(""), None);
    }

    #[test]
    fn workspace_paths_rebase_and_ignore_baseline_segments() {
        assert_eq!(to_workspace_file("./src/a.ts", ""), "src/a.ts");
        assert_eq!(to_workspace_file("a.ts", "src"), "src/a.ts");
        assert_eq!(to_workspace_file("./a.ts", "src/deep"), "src/deep/a.ts");
        assert!(is_ignored_path("node_modules/pkg/x.ts"));
        assert!(is_ignored_path(".git/config"));
        assert!(is_ignored_path("dist/y.ts"));
        assert!(!is_ignored_path("src/dist.ts"));
        assert!(is_absolute(&JsString::from("/tmp/x")));
        assert!(!is_absolute(&JsString::from("src/x")));
    }

    #[test]
    fn pagination_matches_js_slice_and_truncation_flags() {
        let items: Vec<String> = (0..10).map(|index| index.to_string()).collect();
        let page = paginate(&items, None, None);
        assert_eq!(page.items.len(), 10);
        assert!(!page.truncated);

        let page = paginate(&items, Some(3.0), None);
        assert_eq!(page.items, vec!["0", "1", "2"]);
        assert!(page.truncated);
        assert_eq!(page.next_offset, Some(3.0));
        assert_eq!(page.limit, Some(3.0));

        let page = paginate(&items, Some(3.0), Some(8.0));
        assert_eq!(page.items, vec!["8", "9"]);
        assert!(!page.truncated);
        assert_eq!(page.next_offset, None);

        // `head_limit: 0` is unlimited and reports no limit/next offset.
        let page = paginate(&items, Some(0.0), Some(2.0));
        assert_eq!(page.items.len(), 8);
        assert!(!page.truncated);
        assert_eq!(page.limit, None);
        assert_eq!(page.offset, 2.0);

        // Negative limits clamp to zero and therefore truncate.
        let page = paginate(&items, Some(-1.0), None);
        assert!(page.items.is_empty());
        assert!(page.truncated);

        let page = paginate(&items, Some(f64::INFINITY), None);
        assert_eq!(page.items.len(), 10);
        assert!(!page.truncated);
    }

    #[test]
    fn paged_text_reports_the_ts_summary_and_next_hint() {
        let page = paginate(
            &["a".to_owned(), "b".to_owned(), "c".to_owned()],
            Some(2.0),
            None,
        );
        assert_eq!(
            format_paged_text_result("a\nb", &page, 3, None),
            "a\nb\n\n[grep pagination] returned=2 total=3 offset=0 limit=2 truncated=true\nMore results are available. Call grep again with offset=2."
        );
        let page = paginate(&Vec::<String>::new(), None, None);
        assert_eq!(
            format_paged_text_result("", &page, 0, Some(0.0)),
            "[No matches on this page]\n\n[grep pagination] returned=0 total=0 offset=0 limit=250 truncated=false"
        );
        let empty_page = paginate(&["a".to_owned()], Some(0.0), Some(1.0));
        assert_eq!(
            format_paged_text_result("", &empty_page, 1, None),
            "[No matches on this page]\n\n[grep pagination] returned=0 total=1 offset=1 truncated=false"
        );
    }

    #[test]
    fn ripgrep_args_follow_the_ts_vector() {
        let input = JsValue::Object(object([
            ("pattern", string("foo")),
            ("glob", string("*.ts,*.md")),
            ("output_mode", string("content")),
            ("-C", JsValue::Number(2.0)),
            ("-B", JsValue::Number(5.0)),
            ("multiline", JsValue::Bool(true)),
        ]));
        let args = build_ripgrep_args(&input, Mode::Content, "foo", &JsString::from("."));
        assert_eq!(
            args,
            vec![
                "--hidden",
                "--max-columns",
                "500",
                "--glob",
                "!.git",
                "--glob",
                "!.svn",
                "--glob",
                "!.hg",
                "--glob",
                "!.bzr",
                "--glob",
                "!.jj",
                "--glob",
                "!.sl",
                "--glob",
                "!node_modules",
                "--glob",
                "!dist",
                "-U",
                "--multiline-dotall",
                "--glob",
                "*.ts",
                "--glob",
                "*.md",
                "--with-filename",
                "-n",
                "-C",
                "2",
                "foo",
                ".",
            ]
        );
        let input = JsValue::Object(object([
            ("pattern", string("-foo")),
            ("-B", JsValue::Number(1.0)),
            ("-A", JsValue::Number(2.0)),
        ]));
        let args = build_ripgrep_args(
            &input,
            Mode::FilesWithMatches,
            "-foo",
            &JsString::from("src"),
        );
        assert_eq!(
            args.iter().rev().take(3).cloned().collect::<Vec<_>>(),
            vec!["src".to_owned(), "-foo".to_owned(), "-e".to_owned()]
        );
        assert!(args.contains(&"-l".to_owned()));
        assert!(
            !args.contains(&"-B".to_owned()),
            "context ignored outside content"
        );
    }

    #[test]
    fn missing_ripgrep_error_matches_the_ts_wording() {
        let error = ripgrep_unavailable("no executable rg found on PATH");
        assert_eq!(error.code, "unsupported_tool");
        let message = text(&error.message);
        assert!(
            message.starts_with(
                "grep requires the bundled ripgrep binary from @vscode/ripgrep, but it is not available for "
            ),
            "{message}"
        );
        assert!(message.ends_with(" Reinstall dependencies with optional dependencies enabled."));
        let details = error.details.expect("cause details");
        assert_eq!(
            text(details.get("cause").and_then(JsValue::as_string).unwrap()),
            "no executable rg found on PATH"
        );
    }

    #[test]
    fn mode_defaults_to_files_with_matches() {
        assert!(matches!(Mode::parse(""), Mode::FilesWithMatches));
        assert!(matches!(Mode::parse("content"), Mode::Content));
        assert!(matches!(Mode::parse("count"), Mode::Count));
        assert!(matches!(Mode::parse("unknown"), Mode::FilesWithMatches));
    }
}
