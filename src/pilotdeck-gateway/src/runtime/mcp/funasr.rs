//! Native first-party FunASR MCP adapter. External ASR CLI remains a source-built SDK.
//! No TS Gateway/helper executes here. Model-visible tool data is separately attributed.
use crate::metadata::{js_trim, object};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::{Child, Command},
    sync::{Mutex, watch},
    task::JoinSet,
};
const OUTPUT_LIMIT: u64 = 32 * 1024 * 1024;
const LINE_LIMIT: usize = 1024 * 1024;
const VERSION: &str = "v0.2.0";
#[derive(Clone, Debug, Default)]
pub struct FunasrOptions {
    pub project_root: Option<PathBuf>,
    pub runtime_root: Option<PathBuf>,
    pub install_command: String,
}
/// Existing gateway binary routes this explicit internal stdio role before proxy/native serving.
pub fn parse_helper_args(args: &[OsString]) -> Option<FunasrOptions> {
    if args.first().and_then(|s| s.to_str()) != Some("--builtin-mcp")
        || args.get(1).and_then(|s| s.to_str()) != Some("funasr")
    {
        return None;
    }
    let mut out = FunasrOptions::default();
    let mut at = 2;
    while at + 1 < args.len() {
        let key = args[at].to_string_lossy();
        if key.starts_with("--") && !args[at + 1].is_empty() {
            let value = &args[at + 1];
            match key.as_ref() {
                "--project-root" => out.project_root = Some(value.into()),
                "--runtime-root" => out.runtime_root = Some(value.into()),
                "--install-command" => out.install_command = value.to_string_lossy().into_owned(),
                _ => {}
            }
            at += 2;
        } else {
            at += 1;
        }
    }
    Some(out)
}
fn f<'a>(v: &'a JsValue, key: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(key).unwrap_or(&U)
}
fn eq(v: &JsValue, text: &str) -> bool {
    v.as_string().is_some_and(|v| v.equals_str(text))
}
fn join(prefix: &str, value: &JsString, suffix: &str) -> JsString {
    JsString::from_utf16(
        prefix
            .encode_utf16()
            .chain(value.units().iter().copied())
            .chain(suffix.encode_utf16())
            .collect(),
    )
}
fn text_error(message: impl Into<JsString>) -> JsValue {
    object([
        (
            "content",
            JsValue::Array(vec![object([
                ("type", "text".into()),
                ("text", JsValue::String(message.into())),
            ])]),
        ),
        ("isError", JsValue::Bool(true)),
    ])
}
fn response(id: JsValue, result: JsValue) -> JsValue {
    object([("jsonrpc", "2.0".into()), ("id", id), ("result", result)])
}
fn rpc_error(id: JsValue, code: i32, message: impl Into<JsString>) -> JsValue {
    object([
        ("jsonrpc", "2.0".into()),
        ("id", id),
        (
            "error",
            object([
                ("code", JsValue::Number(f64::from(code))),
                ("message", JsValue::String(message.into())),
            ]),
        ),
    ])
}
pub fn tool_definition() -> JsValue {
    parse_json(include_str!("funasr_tool.json"))
        .expect("attributed builtin tool schema is valid JSON")
}
fn js_string(value: &JsValue) -> Result<JsString, JsString> {
    crate::js_value::to_string_units_checked(value)
        .map(JsString::from_utf16)
        .map_err(|e| e.message().into())
}
fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(v) => *v != 0.0 && !v.is_nan(),
        JsValue::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn runtime_key() -> String {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        s => s,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => {
            if platform == "win32" {
                "x64"
            } else {
                "arm64"
            }
        }
        "x86_64" => "x64",
        s => s,
    };
    format!("{platform}-{arch}")
}
fn find_binary(root: &Path) -> io::Result<Option<PathBuf>> {
    let name = if cfg!(windows) {
        "llama-funasr-sensevoice.exe"
    } else {
        "llama-funasr-sensevoice"
    };
    let direct = root.join(name);
    if direct.exists() {
        return Ok(Some(direct));
    }
    if !root.exists() {
        return Ok(None);
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let ty = entry.file_type()?;
            if ty.is_file() && entry.file_name() == name {
                return Ok(Some(entry.path()));
            }
            if ty.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(None)
}
fn scheme_url(path: &str) -> bool {
    let Some((scheme, _)) = path.split_once("://") else {
        return false;
    };
    let mut bytes = scheme.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-'))
}
fn node_realpath_error(path: &Path, error: &io::Error) -> JsString {
    let (code, message) = match error.kind() {
        io::ErrorKind::NotFound => ("ENOENT", "no such file or directory"),
        io::ErrorKind::PermissionDenied => ("EACCES", "permission denied"),
        io::ErrorKind::NotADirectory => ("ENOTDIR", "not a directory"),
        _ => ("EIO", "i/o error"),
    };
    format!("{code}: {message}, realpath '{}'", path.display()).into()
}
async fn audio_path(options: &FunasrOptions, value: &JsValue) -> Result<PathBuf, JsString> {
    let Some(raw) = value
        .as_string()
        .filter(|s| !js_trim(&String::from_utf16_lossy(s.units())).is_empty())
    else {
        return Err("audio_path must be a non-empty local file path".into());
    };
    // Node fs encodes strings to UTF-8 with replacement at the OS boundary only.
    let path = String::from_utf16_lossy(raw.units());
    if scheme_url(&path) {
        return Err("audio_path must be a local project file, not a URL".into());
    }
    let Some(project) = &options.project_root else {
        return Err("FunASR MCP is missing its project root configuration".into());
    };
    let project = tokio::fs::canonicalize(project)
        .await
        .map_err(|e| node_realpath_error(project, &e))?;
    let requested = if Path::new(&path).is_absolute() {
        PathBuf::from(path)
    } else {
        project.join(path)
    };
    let audio = tokio::fs::canonicalize(requested)
        .await
        .map_err(|_| join("audio file does not exist or is inaccessible: ", raw, ""))?;
    if audio == project || !audio.starts_with(&project) {
        return Err("audio_path must resolve inside the current project root".into());
    }
    if !tokio::fs::metadata(&audio)
        .await
        .map_err(|e| e.to_string())?
        .is_file()
    {
        return Err("audio_path must resolve to a regular file".into());
    }
    Ok(audio)
}
fn tail(value: &str, max: usize) -> JsString {
    let units = js_trim(value).encode_utf16().collect::<Vec<_>>();
    JsString::from_utf16(units[units.len().saturating_sub(max)..].to_vec())
}
async fn read_output<R: AsyncRead + Unpin>(stream: R) -> io::Result<Vec<u8>> {
    let mut data = Vec::new();
    stream.take(OUTPUT_LIMIT + 1).read_to_end(&mut data).await?;
    if data.len() as u64 > OUTPUT_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "FunASR CLI output limit exceeded",
        ));
    }
    Ok(data)
}
async fn cancelled(mut signal: watch::Receiver<bool>) {
    loop {
        if *signal.borrow() {
            return;
        }
        if signal.changed().await.is_err() {
            return;
        }
    }
}
async fn stop_child(
    child: &mut Child,
    guard: &mut crate::process_group::GroupGuard,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        // Keep the leader unreaped until the last signal: its PID reserves the
        // entire process-group identity even when a wrapper exits first.
        guard.signal(rustix::process::Signal::TERM);
        let _ = crate::process_group::wait_for_exit_unreaped(guard.0, Duration::from_secs(1)).await;
        guard.signal(rustix::process::Signal::KILL);
    }
    #[cfg(not(unix))]
    child.start_kill()?;
    let result = tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "FunASR child did not reap"))?;
    // No group signal after wait has reaped the identity.
    guard.disarm();
    result?;
    Ok(())
}

impl FunasrOptions {
    fn install_diagnostic(&self, missing: &str, retry: &str) -> String {
        if self.install_command.trim().is_empty() {
            format!(
                "FunASR local {missing}. No source installer was found or configured. Set PILOTDECK_FUNASR_INSTALL_COMMAND to an available source installer and follow the audio-transcription skill, then {retry}."
            )
        } else {
            format!(
                "FunASR local {missing}. Run {}, then {retry}.",
                self.install_command
            )
        }
    }
    pub async fn transcribe(&self, input: JsValue, signal: watch::Receiver<bool>) -> JsValue {
        let Some(root) = &self.runtime_root else {
            return text_error(
                self.install_diagnostic("runtime is not configured", "retry this tool"),
            );
        };
        if !f(&input, "language").is_undefined() && !eq(f(&input, "language"), "auto") {
            return text_error("language only supports \"auto\" for the local SenseVoice runtime.");
        }
        let audio = match audio_path(self, f(&input, "audio_path")).await {
            Ok(v) => v,
            Err(e) => return text_error(e),
        };
        let runtime = root.join("runtime").join(VERSION).join(runtime_key());
        let binary = match find_binary(&runtime) {
            Ok(v) => v,
            Err(e) => return text_error(e.to_string()),
        };
        let model = root.join("models/sensevoice-small-q8.gguf");
        let vad = root.join("models/fsmn-vad.gguf");
        if binary.is_none()
            || !tokio::fs::metadata(&model).await.is_ok_and(|m| m.is_file())
            || !tokio::fs::metadata(&vad).await.is_ok_and(|m| m.is_file())
        {
            return text_error(self.install_diagnostic(
                "runtime or models are not installed",
                "retry mcp__funasr__transcribe_audio in this session",
            ));
        }
        let binary = binary.unwrap();
        let mut command = Command::new(&binary);
        command
            .args([
                OsString::from("-m"),
                model.into_os_string(),
                OsString::from("--vad"),
                vad.into_os_string(),
                OsString::from("-a"),
                audio.into_os_string(),
                OsString::from("--srt"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            pilotdeck_child_signals::install_child_detach_session(command.as_std_mut());
            pilotdeck_child_signals::install_child_signal_reset(command.as_std_mut());
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.as_std_mut().creation_flags(0x08000000);
        }
        let mut child = match command.spawn() {
            Ok(v) => v,
            Err(e) => {
                let suffix = match e.kind() {
                    io::ErrorKind::NotFound => "ENOENT",
                    io::ErrorKind::PermissionDenied => "EACCES",
                    _ => "EIO",
                };
                return text_error(format!(
                    "FunASR transcription failed (exit 127): spawn {} {suffix}",
                    binary.display()
                ));
            }
        };
        #[cfg(unix)]
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw);
        #[cfg(not(unix))]
        let group = None;
        let mut guard = crate::process_group::GroupGuard(group);
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let result = {
            tokio::select! {
                result=async {
                    #[cfg(unix)]
                    let status = async {
                        let group = group.ok_or_else(|| io::Error::other("FunASR process group is unavailable"))?;
                        crate::process_group::wait_exit_unreaped(group).await.map_err(io::Error::from)?
                            .ok_or_else(|| io::Error::other("FunASR process exit observation is unavailable"))?;
                        Ok::<(), io::Error>(())
                    };
                    #[cfg(unix)]
                    let (_, out, err) = tokio::try_join!(status, read_output(stdout), read_output(stderr))?;
                    // A successful wrapper can exit before its worker finishes writing.
                    // Retain TS's pipe-EOF semantics; cancellation still owns the group.
                    // Only signal after output has drained and before reaping the leader.
                    #[cfg(unix)]
                    guard.signal(rustix::process::Signal::KILL);
                    #[cfg(unix)]
                    let status = child.wait().await;
                    #[cfg(unix)]
                    guard.disarm();
                    #[cfg(unix)]
                    return Ok::<_,io::Error>((status?,out,err));
                    #[cfg(not(unix))]
                    tokio::try_join!(child.wait(), read_output(stdout), read_output(stderr))
                }=>Some(result),
                _=cancelled(signal)=>None
            }
        };
        let Some(result) = result else {
            let cleanup = stop_child(&mut child, &mut guard).await;
            return text_error(match cleanup {
                Ok(()) => "FunASR transcription cancelled.".to_owned(),
                Err(e) => format!("FunASR cancellation could not reap runtime: {e}"),
            });
        };
        let (status, stdout, stderr) = match result {
            Ok(v) => v,
            Err(e) => {
                return text_error(match stop_child(&mut child, &mut guard).await {
                    Ok(()) => e.to_string(),
                    Err(cleanup) => format!("{e}; FunASR runtime cleanup failed: {cleanup}"),
                });
            }
        };
        let stdout = String::from_utf8_lossy(&stdout);
        let stderr = String::from_utf8_lossy(&stderr);
        let code = status.code().unwrap_or(1);
        if code != 0 {
            let diagnostic = if !js_trim(&stderr).is_empty() {
                tail(&stderr, 4000)
            } else if !js_trim(&stdout).is_empty() {
                tail(&stdout, 1000)
            } else {
                "no CLI diagnostic".into()
            };
            return text_error(join(
                &format!("FunASR transcription failed (exit {code}): "),
                &diagnostic,
                "",
            ));
        }
        let segments = parse_srt(&stdout);
        if segments.is_empty() {
            let message = join(
                "FunASR completed but did not produce SRT segments. ",
                &tail(&stderr, 1000),
                "",
            );
            return text_error(JsString::from_utf16(
                crate::metadata::js_trim(&String::from_utf16_lossy(message.units()))
                    .encode_utf16()
                    .collect(),
            ));
        }
        let text = segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let visible = format!(
            "{text}\n\nTimestamped segments:\n{}",
            segments
                .iter()
                .map(|s| format!("[{:.3} - {:.3}] {}", s.start, s.end, s.text))
                .collect::<Vec<_>>()
                .join("\n")
        );
        object([
            (
                "content",
                JsValue::Array(vec![object([
                    ("type", "text".into()),
                    ("text", visible.as_str().into()),
                ])]),
            ),
            (
                "structuredContent",
                object([
                    ("text", text.as_str().into()),
                    (
                        "segments",
                        JsValue::Array(
                            segments
                                .iter()
                                .map(|s| {
                                    object([
                                        ("start", JsValue::Number(s.start)),
                                        ("end", JsValue::Number(s.end)),
                                        ("text", s.text.as_str().into()),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ]),
            ),
        ])
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}
pub fn parse_srt(output: &str) -> Vec<Segment> {
    static TIME: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"^([0-9]{2}:[0-9]{2}:[0-9]{2}[,.][0-9]{1,3})\s+-->\s+([0-9]{2}:[0-9]{2}:[0-9]{2}[,.][0-9]{1,3})").unwrap()
    });
    let normalized = output.replace("\r\n", "\n");
    let mut blocks = Vec::new();
    let mut lines = Vec::new();
    for line in normalized.split('\n') {
        let line = js_trim(line);
        if line.is_empty() {
            if !lines.is_empty() {
                blocks.push(std::mem::take(&mut lines));
            }
        } else {
            lines.push(line);
        }
    }
    if !lines.is_empty() {
        blocks.push(lines);
    }
    blocks
        .into_iter()
        .filter_map(|lines| {
            let at = lines.iter().position(|line| line.contains("-->"))?;
            let captures = TIME.captures(lines[at])?;
            let text = lines[at + 1..].join(" ");
            if text.is_empty() {
                return None;
            }
            let seconds = |value: &str| {
                let text = value.replace(',', ".");
                let mut parts = text.split(':');
                parts
                    .next()?
                    .parse::<f64>()
                    .ok()
                    .zip(parts.next()?.parse::<f64>().ok())
                    .zip(parts.next()?.parse::<f64>().ok())
                    .map(|((h, m), s)| h * 3600.0 + m * 60.0 + s)
            };
            Some(Segment {
                start: seconds(&captures[1])?,
                end: seconds(&captures[2])?,
                text,
            })
        })
        .collect()
}
async fn line<R: AsyncBufRead + Unpin>(input: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = input.fill_buf().await?;
        if available.is_empty() {
            return Ok((!line.is_empty()).then_some(line));
        }
        let newline = available.iter().position(|b| *b == b'\n');
        let count = newline.map_or(available.len(), |n| n + 1);
        if line.len() + count > LINE_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "FunASR MCP input line limit exceeded",
            ));
        }
        line.extend_from_slice(&available[..count]);
        input.consume(count);
        if newline.is_some() {
            return Ok(Some(line));
        }
    }
}
async fn send<W: AsyncWrite + Unpin>(output: &Arc<Mutex<W>>, value: JsValue) -> io::Result<()> {
    let mut text = stringify_json(&value)
        .map_err(io::Error::other)?
        .unwrap_or_default();
    text.push('\n');
    let mut output = output.lock().await;
    output.write_all(text.as_bytes()).await?;
    output.flush().await
}
/// EOF cancels and reaps owned runtime jobs before returning. Cancellation
/// notifications are operational cleanup, never fabricated business success.
pub async fn serve<R, W>(mut input: R, output: W, options: FunasrOptions) -> io::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let output = Arc::new(Mutex::new(output));
    let mut jobs = JoinSet::new();
    let mut signals = Vec::<(String, watch::Sender<bool>)>::new();
    let mut failure = None;
    loop {
        let bytes = match line(&mut input).await {
            Ok(Some(v)) => v,
            Ok(None) => break,
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        let raw = String::from_utf8_lossy(&bytes);
        let Ok(request) = parse_json(&raw) else {
            continue;
        };
        if !eq(f(&request, "jsonrpc"), "2.0") || !truthy(f(&request, "method")) {
            continue;
        }
        let id = f(&request, "id").clone();
        let method = f(&request, "method");
        if eq(method, "notifications/cancelled") {
            let key = stringify_json(f(f(&request, "params"), "requestId"))
                .ok()
                .flatten();
            for (k, signal) in &signals {
                if key.as_ref() == Some(k) {
                    signal.send_replace(true);
                }
            }
            continue;
        }
        let immediate = if eq(method, "initialize") {
            let version = f(f(&request, "params"), "protocolVersion");
            let version = if matches!(version, JsValue::Undefined | JsValue::Null) {
                "2024-11-05".into()
            } else {
                version.clone()
            };
            Some(response(id.clone(),object([("protocolVersion",version),("capabilities",object([("tools",object([]))])),("serverInfo",object([("name","pilotdeck-funasr-local".into()),("version","0.2.0".into())])),("instructions","Use transcribe_audio only with an audio path inside the active PilotDeck project.".into())])))
        } else if eq(method, "tools/list") {
            Some(response(
                id.clone(),
                object([("tools", JsValue::Array(vec![tool_definition()]))]),
            ))
        } else if eq(method, "tools/call") {
            let params = f(&request, "params");
            if !eq(f(params, "name"), "transcribe_audio") {
                match js_string(f(params, "name")) {
                    Ok(name) => Some(response(
                        id.clone(),
                        text_error(join("Unknown tool: ", &name, "")),
                    )),
                    Err(e) => {
                        if id.is_undefined() {
                            None
                        } else {
                            Some(rpc_error(id.clone(), -32603, e))
                        }
                    }
                }
            } else {
                let arguments = f(params, "arguments");
                let arguments = if matches!(arguments, JsValue::Null | JsValue::Undefined) {
                    object([])
                } else {
                    arguments.clone()
                };
                let (tx, rx) = watch::channel(false);
                let key = stringify_json(&id).ok().flatten().unwrap_or_default();
                signals.push((key, tx));
                let output = output.clone();
                let options = options.clone();
                jobs.spawn(async move {
                    send(
                        &output,
                        response(id, options.transcribe(arguments, rx).await),
                    )
                    .await
                });
                None
            }
        } else if id.is_undefined() {
            None
        } else {
            Some(match js_string(method) {
                Ok(method) => {
                    rpc_error(id.clone(), -32601, join("Method not found: ", &method, ""))
                }
                Err(e) => rpc_error(id.clone(), -32603, e),
            })
        };
        if let Some(value) = immediate
            && let Err(e) = send(&output, value).await
        {
            failure = Some(e);
            break;
        }
        while let Some(result) = jobs.try_join_next() {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    failure = Some(e);
                    break;
                }
                Err(e) => {
                    failure = Some(io::Error::other(e));
                    break;
                }
            }
        }
        signals.retain(|(_, signal)| !signal.is_closed());
        if failure.is_some() {
            break;
        }
    }
    for (_, signal) in signals {
        signal.send_replace(true);
    }
    while let Some(result) = jobs.join_next().await {
        let error = match result {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(e),
            Err(e) => Some(io::Error::other(e)),
        };
        if failure.is_none() {
            failure = error;
        }
    }
    failure.map_or(Ok(()), Err)
}
pub async fn serve_stdio(mut options: FunasrOptions) -> io::Result<()> {
    if options.install_command.trim().is_empty() {
        options.install_command = super::native_builtins::install_command(
            &std::env::vars().collect(),
            &std::env::current_exe()?,
            &std::env::current_dir()?,
        )?;
    }
    serve(
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
        options,
    )
    .await
}
#[cfg(test)]
#[path = "funasr_tests.rs"]
mod tests;
