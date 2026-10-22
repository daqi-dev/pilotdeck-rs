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
//! Binary resolution differences from the TS baseline (documented in
//! `tests/fixtures/grep-tool/README.md`):
//! - `PILOTDECK_RIPGREP_PATH` may name the binary explicitly (same operational
//!   knob as `PILOTDECK_SHELL_PATH` for `bash`), so deployments and parity
//!   fixtures can pin the exact engine;
//! - otherwise the first executable `rg` (`rg.exe` on Windows) on `PATH` wins;
//! - otherwise the tool fails with the TS `unsupported_tool` wording.
//!
//! No ripgrep binary is downloaded or bundled by this crate.
use super::{
    ResolvedPath, basename, dirname, invalid, join, normalize, object, os_path, schema, string,
    text, tool_error, truthy,
};
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRunContext},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use regex::Regex;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

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

/// `splitGlobPatterns`: whitespace separates patterns, brace groups stay
/// intact, and a bare comma separates alternatives (ripgrep accepts both).
fn split_glob_patterns(glob: &str) -> Vec<String> {
    let mut patterns = Vec::new();
    for raw in glob.split_whitespace().filter(|part| !part.is_empty()) {
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

fn resolve_ripgrep() -> Result<PathBuf, ToolError> {
    if let Some(configured) = std::env::var_os("PILOTDECK_RIPGREP_PATH") {
        let path = PathBuf::from(configured);
        if is_executable(&path) {
            return Ok(path);
        }
        return Err(ripgrep_unavailable(&format!(
            "PILOTDECK_RIPGREP_PATH={} is not an executable file",
            path.display()
        )));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            for name in ["rg", "rg.exe"] {
                let candidate = directory.join(name);
                if is_executable(&candidate) {
                    return Ok(candidate);
                }
            }
        }
    }
    Err(ripgrep_unavailable("no executable rg found on PATH"))
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
    let executable = resolve_ripgrep()?;
    if context.is_aborted() {
        return Err(tool_error("tool_aborted", "grep search aborted."));
    }
    let mut command = Command::new(&executable);
    command
        .args(args)
        .current_dir(os_path(cwd))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
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
            ripgrep_unavailable(&format!("spawn {}: {error}", executable.display()))
        } else {
            tool_error(
                "tool_execution_failed",
                format!("ripgrep grep search failed: {error}"),
            )
        }
    })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout_task = tokio::spawn(read_all(stdout));
    let stderr_task = tokio::spawn(read_all(stderr));
    let wait = async {
        tokio::select! {
            status = child.wait() => Some(status),
            _ = context.cancelled() => None,
        }
    };
    let mut aborted = false;
    let mut timed_out = false;
    let status = match timeout(Duration::from_millis(RIPGREP_TIMEOUT_MS), wait).await {
        Ok(Some(Ok(status))) => Some(status),
        Ok(Some(Err(error))) => {
            let _ = child.kill().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            return Err(tool_error(
                "tool_execution_failed",
                format!("ripgrep grep search failed: {error}"),
            ));
        }
        Ok(None) => {
            aborted = true;
            let _ = child.kill().await;
            None
        }
        Err(_) => {
            timed_out = true;
            let _ = child.kill().await;
            None
        }
    };
    let out = stdout_task.await.unwrap_or_default();
    let err = stderr_task.await.unwrap_or_default();
    if aborted || context.is_aborted() {
        return Err(tool_error("tool_aborted", "grep search aborted."));
    }
    if timed_out {
        return Err(tool_error(
            "tool_timeout",
            format!("grep search timed out after {RIPGREP_TIMEOUT_MS}ms."),
        ));
    }
    let status = status.expect("status for completed ripgrep run");
    #[cfg(unix)]
    let exit_signal = {
        use std::os::unix::process::ExitStatusExt;
        status.signal()
    };
    #[cfg(not(unix))]
    let exit_signal = None;
    let code = status.code();
    if code == Some(0) || code == Some(1) {
        return Ok(String::from_utf8_lossy(&out).into_owned());
    }
    let error_text = String::from_utf8_lossy(&err);
    let trimmed = js_trim(&error_text);
    if let Some(signal) = exit_signal {
        return Err(tool_error(
            "tool_execution_failed",
            format!("ripgrep grep search exited via signal {signal}."),
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
        .map(|(file, count)| (to_workspace_file(&file, &target.workspace_base_dir), count))
        .collect();
    entries.sort_by(|left, right| locale_compare(&left.0, &right.0));
    let total_matches: i64 = entries.iter().map(|(_, count)| *count).sum();
    let rendered: Vec<(String, String)> = entries
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
    match metadata.modified() {
        Ok(modified) => match modified.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => duration.as_millis() as f64,
            Err(error) => -(error.duration().as_millis() as f64),
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
    static CONTENT: OnceLock<Regex> = OnceLock::new();
    let regex = CONTENT.get_or_init(|| {
        Regex::new(r"^(.*?)([:-])(\d+)([:-])(.*)$").expect("valid content-line regex")
    });
    let Some(captures) = regex.captures(&line) else {
        return Err(tool_error(
            "tool_execution_failed",
            format!("Unexpected ripgrep content output: {line}"),
        ));
    };
    let separator = captures[2].chars().next().expect("separator");
    let line_separator = captures[4].chars().next().expect("line separator");
    Ok(ParsedLine::Content(ContentLine {
        file: normalize_relative_path(&captures[1]),
        separator,
        line_number: captures[3].parse().unwrap_or(0),
        line_separator,
        content: captures[5].to_owned(),
    }))
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
    let value = value.trim_start();
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

/// Node's `String#localeCompare` with the en-US ICU collation (the fixture
/// locale asserted by the generators).
fn locale_compare(left: &str, right: &str) -> std::cmp::Ordering {
    static COLLATOR: OnceLock<icu_collator::CollatorBorrowed<'static>> = OnceLock::new();
    let collator = COLLATOR.get_or_init(|| {
        let locale: icu_locale_core::Locale = "en-US".parse().expect("valid locale");
        icu_collator::Collator::try_new(locale.into(), Default::default())
            .expect("bundled collation data")
    });
    let left: Vec<u16> = left.encode_utf16().collect();
    let right: Vec<u16> = right.encode_utf16().collect();
    collator.compare_utf16(&left, &right)
}

fn js_trim(value: &str) -> &str {
    value.trim()
}

fn js_trim_end(value: &str) -> &str {
    value.trim_end()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::tools::builtin::is_absolute;

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
