//! One-shot calls and streamed turns over an explicitly selected Gateway.
#[path = "../control_serve.rs"]
mod serve;
#[path = "../control_stream.rs"]
mod stream;
use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{
    env,
    io::{self, Write},
    path::Path,
    time::Duration,
};
#[cfg(unix)]
use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt};
use tokio::time::{Instant, timeout_at};
use tokio_tungstenite::{connect_async_with_config, tungstenite::Message};

const MAX_INPUT: usize = 1024 * 1024;
const MAX_TOKEN: usize = 64 * 1024;
const INPUT_TIMEOUT: Duration = Duration::from_secs(10);
const NETWORK_TIMEOUT: Duration = Duration::from_secs(30);

struct Options {
    url: String,
    token_file: String,
    command: String,
    input_file: Option<String>,
}
#[derive(Debug)]
struct ControlError {
    kind: &'static str,
    message: &'static str,
    gateway: Option<JsValue>,
}
impl ControlError {
    fn new(kind: &'static str, message: &'static str) -> Self {
        Self {
            kind,
            message,
            gateway: None,
        }
    }
}
type Result<T> = std::result::Result<T, ControlError>;

fn parse_args() -> Result<Options> {
    let mut url = None;
    let mut token_file = None;
    let mut command = None;
    let mut input_file = None;
    let mut args = env::args_os().skip(1).map(|value| {
        value
            .into_string()
            .map_err(|_| ControlError::new("input_error", "Arguments must be UTF-8."))
    });

    while let Some(arg) = args.next() {
        let arg = arg?;
        match arg.as_str() {
            "--url" | "--token-file" | "--input-file" => {
                let value = args
                    .next()
                    .transpose()?
                    .filter(|v| !v.is_empty() && !v.starts_with("--"))
                    .ok_or_else(|| ControlError::new("input_error", "Missing option value."))?;
                let slot = match arg.as_str() {
                    "--url" => &mut url,
                    "--token-file" => &mut token_file,
                    _ => &mut input_file,
                };
                if slot.replace(value).is_some() {
                    return Err(ControlError::new("input_error", "Repeated option."));
                }
            }
            "status" | "capabilities" | "call" | "stream" => {
                if command.replace(arg).is_some() {
                    return Err(ControlError::new(
                        "input_error",
                        "Only one command is allowed.",
                    ));
                }
            }
            _ => {
                return Err(ControlError::new(
                    "input_error",
                    "Usage: pilotdeck-control --url <ws-url> --token-file <file> <status|capabilities|call|stream> [--input-file <file>].",
                ));
            }
        }
    }
    let command =
        command.ok_or_else(|| ControlError::new("input_error", "A command is required."))?;
    if !matches!(command.as_str(), "call" | "stream") && input_file.is_some() {
        return Err(ControlError::new(
            "input_error",
            "--input-file applies only to call or stream.",
        ));
    }
    Ok(Options {
        url: url.ok_or_else(|| ControlError::new("input_error", "--url is required."))?,
        token_file: token_file
            .ok_or_else(|| ControlError::new("input_error", "--token-file is required."))?,
        command,
        input_file,
    })
}

#[cfg(unix)]
fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| ControlError::new("input_error", "Cannot open input file."))?;
    if !file
        .metadata()
        .map_err(|_| ControlError::new("input_error", "Cannot inspect input file."))?
        .is_file()
    {
        return Err(ControlError::new(
            "input_error",
            "Input must be a regular file.",
        ));
    }
    let mut bytes = Vec::new();
    file.take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ControlError::new("input_error", "Cannot read input file."))?;
    if bytes.len() > limit {
        return Err(ControlError::new(
            "input_error",
            "Input exceeds the size limit.",
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn read_stdin() -> Result<Vec<u8>> {
    let stdin = io::stdin();
    let stat = rustix::fs::fstat(&stdin)
        .map_err(|_| ControlError::new("input_error", "Cannot inspect input."))?;
    let kind = rustix::fs::FileType::from_raw_mode(stat.st_mode);
    if !matches!(
        kind,
        rustix::fs::FileType::RegularFile
            | rustix::fs::FileType::Fifo
            | rustix::fs::FileType::Socket
    ) {
        return Err(ControlError::new(
            "input_error",
            "Input must be a pipe, socket, or regular file.",
        ));
    }
    let deadline = std::time::Instant::now() + INPUT_TIMEOUT;
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let left = deadline
            .checked_duration_since(std::time::Instant::now())
            .ok_or_else(|| ControlError::new("input_error", "Input timed out."))?;
        if kind != rustix::fs::FileType::RegularFile {
            let duration = rustix::event::Timespec {
                tv_sec: left.as_secs() as i64,
                tv_nsec: left.subsec_nanos() as _,
            };
            let mut descriptors = [rustix::event::PollFd::new(
                &stdin,
                rustix::event::PollFlags::IN,
            )];
            match rustix::event::poll(&mut descriptors, Some(&duration)) {
                Ok(0) => return Err(ControlError::new("input_error", "Input timed out.")),
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(_) => return Err(ControlError::new("input_error", "Cannot read input.")),
            }
        }
        let n = rustix::io::read(&stdin, &mut buf)
            .map_err(|_| ControlError::new("input_error", "Cannot read input."))?;
        if n == 0 {
            return Ok(bytes);
        }
        if bytes.len() + n > MAX_INPUT {
            return Err(ControlError::new(
                "input_error",
                "Input exceeds the size limit.",
            ));
        }
        bytes.extend_from_slice(&buf[..n]);
    }
}

// On non-Unix, isolate synchronous I/O in a detached thread; a timeout never
// leaves the async runtime waiting on an uncancellable blocking task.
#[cfg(not(unix))]
fn read_bounded_background(
    reader: impl FnOnce() -> Result<Vec<u8>> + Send + 'static,
) -> Result<Vec<u8>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let _ = tx.send(reader());
    });
    rx.recv_timeout(INPUT_TIMEOUT)
        .map_err(|_| ControlError::new("input_error", "Input timed out."))?
}
#[cfg(not(unix))]
fn read_regular(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let path = path.to_owned();
    read_bounded_background(move || {
        use std::io::Read;
        let file = std::fs::File::open(path)
            .map_err(|_| ControlError::new("input_error", "Cannot open input file."))?;
        if !file
            .metadata()
            .map_err(|_| ControlError::new("input_error", "Cannot inspect input file."))?
            .is_file()
        {
            return Err(ControlError::new(
                "input_error",
                "Input must be a regular file.",
            ));
        }
        let mut bytes = Vec::new();
        file.take((limit + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ControlError::new("input_error", "Cannot read input file."))?;
        if bytes.len() > limit {
            return Err(ControlError::new(
                "input_error",
                "Input exceeds the size limit.",
            ));
        }
        Ok(bytes)
    })
}
#[cfg(not(unix))]
fn read_stdin() -> Result<Vec<u8>> {
    read_bounded_background(|| {
        use std::io::Read;
        let mut bytes = Vec::new();
        io::stdin()
            .lock()
            .take((MAX_INPUT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| ControlError::new("input_error", "Cannot read input."))?;
        if bytes.len() > MAX_INPUT {
            return Err(ControlError::new(
                "input_error",
                "Input exceeds the size limit.",
            ));
        }
        Ok(bytes)
    })
}

fn check_numbers(value: &JsValue) -> Result<()> {
    let mut pending = vec![value];
    while let Some(item) = pending.pop() {
        match item {
            JsValue::Number(n)
                if !n.is_finite() || (n.fract() == 0.0 && n.abs() > 9_007_199_254_740_991.0) =>
            {
                return Err(ControlError::new(
                    "input_error",
                    "Numbers must be finite and integers must be within the JavaScript safe integer range.",
                ));
            }
            JsValue::Array(values) => pending.extend(values),
            JsValue::Object(values) => {
                pending.extend(values.entries().iter().map(|(_, value)| value))
            }
            _ => {}
        }
    }
    Ok(())
}

fn input_value(options: &Options) -> Result<JsValue> {
    let bytes = match &options.input_file {
        Some(path) => read_regular(Path::new(path), MAX_INPUT)?,
        None => read_stdin()?,
    };
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| ControlError::new("input_error", "Input must be UTF-8 JSON."))?;
    let value =
        parse_json(text).map_err(|_| ControlError::new("input_error", "Invalid JSON input."))?;
    check_numbers(&value)?;
    if value.as_object().is_none() {
        return Err(ControlError::new(
            "input_error",
            "Call input must be an object.",
        ));
    }
    Ok(value)
}

fn input_call(options: &Options) -> Result<(String, JsValue)> {
    let value = input_value(options)?;
    let method = value
        .get("method")
        .and_then(JsValue::as_string)
        .and_then(|v| v.to_utf8().ok())
        .filter(|v| !v.is_empty())
        .ok_or_else(|| ControlError::new("input_error", "A method string is required."))?;
    if method == "submit_turn" {
        return Err(ControlError::new(
            "unsupported",
            "submit_turn requires a streaming client.",
        ));
    }
    let params = value
        .get("params")
        .cloned()
        .ok_or_else(|| ControlError::new("input_error", "The params field is required."))?;
    Ok((method, params))
}

fn object(fields: &[(&str, JsValue)]) -> JsValue {
    let mut obj = JsObject::new();
    for (key, value) in fields {
        obj.insert((*key).into(), value.clone());
    }
    JsValue::Object(obj)
}
fn encode(value: &JsValue) -> Result<String> {
    stringify_json(value)
        .map_err(|_| ControlError::new("protocol_error", "Cannot encode Gateway value."))?
        .ok_or_else(|| ControlError::new("protocol_error", "Gateway returned an undefined value."))
}
fn string_is(value: &JsValue, field: &str, expected: &str) -> bool {
    value
        .get(field)
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str(expected))
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

// Both entry points keep the same authentication and receiving policy.
async fn open_authenticated(options: &Options, token: &str, deadline: Instant) -> Result<Socket> {
    let (mut socket, _) = connect_async_with_config(
        &options.url,
        Some(pilotdeck_cli::ws_config::websocket_config()),
        false,
    )
    .await
    .map_err(|_| ControlError::new("transport_error", "Gateway connection failed."))?;
    let hello = object(&[
        ("type", "hello".into()),
        (
            "protocolVersion",
            pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION.into(),
        ),
        ("clientName", "cli".into()),
        ("clientVersion", "0.1.0".into()),
        ("token", token.into()),
    ]);
    socket
        .send(Message::Text(encode(&hello)?.into()))
        .await
        .map_err(|_| ControlError::new("transport_error", "Gateway connection closed."))?;
    loop {
        if Instant::now() >= deadline {
            return Err(ControlError::new(
                "transport_error",
                "Gateway request timed out.",
            ));
        }
        let frame = socket
            .next()
            .await
            .ok_or_else(|| ControlError::new("transport_error", "Gateway connection closed."))?
            .map_err(|_| ControlError::new("transport_error", "Gateway connection closed."))?;
        if frame.is_close() {
            return Err(ControlError::new(
                "transport_error",
                "Gateway authentication or handshake failed.",
            ));
        }
        if !frame.is_text() {
            continue;
        }
        let value = parse_json(
            frame
                .to_text()
                .map_err(|_| ControlError::new("protocol_error", "Invalid Gateway frame."))?,
        )
        .map_err(|_| ControlError::new("protocol_error", "Invalid Gateway frame."))?;
        if string_is(&value, "type", "hello_ok") {
            if Instant::now() >= deadline {
                return Err(ControlError::new(
                    "transport_error",
                    "Gateway handshake timed out.",
                ));
            }
            break;
        }
    }
    Ok(socket)
}

async fn exchange(
    options: &Options,
    token: &str,
    method: &str,
    params: JsValue,
) -> Result<Option<JsValue>> {
    let deadline = Instant::now() + NETWORK_TIMEOUT;
    let run = async {
        let mut socket = open_authenticated(options, token, deadline).await?;
        let id = uuid::Uuid::new_v4().to_string();
        let request = object(&[
            ("type", "request".into()),
            ("id", id.as_str().into()),
            ("method", method.into()),
            ("params", params),
        ]);
        socket
            .send(Message::Text(encode(&request)?.into()))
            .await
            .map_err(|_| {
                ControlError::new(
                    "transport_error",
                    "Gateway connection closed; the operation may have completed.",
                )
            })?;
        loop {
            if Instant::now() >= deadline {
                return Err(ControlError::new(
                    "transport_error",
                    "Gateway request timed out.",
                ));
            }
            let frame = socket
                .next()
                .await
                .ok_or_else(|| {
                    ControlError::new(
                        "transport_error",
                        "Gateway connection closed; the operation may have completed.",
                    )
                })?
                .map_err(|_| {
                    ControlError::new(
                        "transport_error",
                        "Gateway connection closed; the operation may have completed.",
                    )
                })?;
            if frame.is_close() {
                return Err(ControlError::new(
                    "transport_error",
                    "Gateway connection closed; the operation may have completed.",
                ));
            }
            if !frame.is_text() {
                continue;
            }
            let value = parse_json(
                frame
                    .to_text()
                    .map_err(|_| ControlError::new("protocol_error", "Invalid Gateway frame."))?,
            )
            .map_err(|_| ControlError::new("protocol_error", "Invalid Gateway frame."))?;
            if !string_is(&value, "type", "response") || !string_is(&value, "id", &id) {
                continue;
            }
            let result = match value.get("ok").and_then(JsValue::as_bool) {
                Some(true) => Ok(value.get("result").cloned()),
                Some(false) => Err(ControlError {
                    kind: "gateway_error",
                    message: "Gateway request failed.",
                    gateway: value.get("error").cloned(),
                }),
                None => Err(ControlError::new(
                    "protocol_error",
                    "Gateway response has no valid status.",
                )),
            };
            return Ok((socket, result));
        }
    };
    let (mut socket, result) = match timeout_at(deadline, run).await {
        Ok(Ok(pair)) if Instant::now() < deadline => pair,
        Ok(Err(error)) if Instant::now() < deadline => return Err(error),
        _ => {
            return Err(ControlError::new(
                "transport_error",
                "Gateway request timed out; the outcome may be unknown.",
            ));
        }
    };
    // A settled response is authoritative even when graceful close takes longer.
    settle(
        result,
        async {
            let _ = socket.close(None).await;
        },
        Duration::from_secs(1),
    )
    .await
}

async fn settle<T>(result: T, close: impl std::future::Future<Output = ()>, budget: Duration) -> T {
    let _ = tokio::time::timeout(budget, close).await;
    result
}

// Preserve all non-matching UTF-16 units, including isolated surrogates.
// Short tokens are replaced only as complete values or word-delimited spans;
// long tokens are replaced wherever they occur in a JSON string.
fn scrub_units(units: &[u16], secret: &[u16]) -> Vec<u16> {
    fn word(unit: u16) -> bool {
        unit == b'_' as u16
            || (unit as u32 >= 128)
            || (unit >= b'0' as u16 && unit <= b'9' as u16)
            || (unit >= b'A' as u16 && unit <= b'Z' as u16)
            || (unit >= b'a' as u16 && unit <= b'z' as u16)
    }
    let replacement: Vec<u16> = "[REDACTED]".encode_utf16().collect();
    if secret.is_empty() || units.len() < secret.len() {
        return units.to_vec();
    }
    // KMP bounds matching work even for long repeated-prefix credentials.
    let mut prefix = vec![0_usize; secret.len()];
    let mut matched = 0;
    for i in 1..secret.len() {
        while matched > 0 && secret[i] != secret[matched] {
            matched = prefix[matched - 1];
        }
        if secret[i] == secret[matched] {
            matched += 1;
        }
        prefix[i] = matched;
    }
    let mut result = Vec::with_capacity(units.len());
    let mut cursor = 0;
    matched = 0;
    for (i, &unit) in units.iter().enumerate() {
        while matched > 0 && unit != secret[matched] {
            matched = prefix[matched - 1];
        }
        if unit == secret[matched] {
            matched += 1;
        }
        if matched == secret.len() {
            let end = i + 1;
            let start = end - secret.len();
            let boundary = secret.len() >= 8
                || (start == 0 || !word(units[start - 1]))
                    && (end == units.len() || !word(units[end]));
            if boundary {
                result.extend_from_slice(&units[cursor..start]);
                result.extend_from_slice(&replacement);
                cursor = end;
                matched = 0;
            } else {
                matched = prefix[matched - 1];
            }
        }
    }
    result.extend_from_slice(&units[cursor..]);
    result
}

// Explicit stacks avoid recursion on Gateway-supplied deep JSON.
fn redact(value: &JsValue, token: &str) -> JsValue {
    enum Task<'a> {
        Value(&'a JsValue),
        Array(usize),
        Object(Vec<pilotdeck_protocol::JsString>),
    }
    let secret: Vec<u16> = token.encode_utf16().collect();
    let mut tasks = vec![Task::Value(value)];
    let mut values = Vec::new();
    while let Some(task) = tasks.pop() {
        match task {
            Task::Value(JsValue::Array(items)) => {
                tasks.push(Task::Array(items.len()));
                tasks.extend(items.iter().rev().map(Task::Value));
            }
            Task::Value(JsValue::Object(items)) => {
                let entries = items.entries();
                tasks.push(Task::Object(
                    entries
                        .iter()
                        .map(|(key, _)| {
                            pilotdeck_protocol::JsString::from_utf16(scrub_units(
                                key.units(),
                                &secret,
                            ))
                        })
                        .collect(),
                ));
                tasks.extend(entries.iter().rev().map(|(_, value)| Task::Value(value)));
            }
            Task::Value(JsValue::String(text)) => values.push(JsValue::String(
                pilotdeck_protocol::JsString::from_utf16(scrub_units(text.units(), &secret)),
            )),
            Task::Value(other) => values.push(other.clone()),
            Task::Array(len) => {
                let start = values.len() - len;
                let children = values.split_off(start);
                values.push(JsValue::Array(children));
            }
            Task::Object(keys) => {
                let start = values.len() - keys.len();
                let children = values.split_off(start);
                let mut object = JsObject::new();
                for (key, child) in keys.into_iter().zip(children) {
                    let mut unique = key.clone();
                    let mut suffix = 2_u64;
                    while object.get_key(&unique).is_some() {
                        let mut units = key.units().to_vec();
                        units.extend(format!("#{suffix}").encode_utf16());
                        unique = pilotdeck_protocol::JsString::from_utf16(units);
                        suffix += 1;
                    }
                    object.insert(unique, child);
                }
                values.push(JsValue::Object(object));
            }
        }
    }
    values.pop().expect("one redacted root")
}

fn run() -> Result<Option<JsValue>> {
    let options = parse_args()?;
    let (method, params) = if options.command == "call" {
        input_call(&options)?
    } else if options.command == "stream" {
        ("submit_turn".into(), stream::input(&options)?)
    } else {
        ("describe_server".into(), object(&[]))
    };
    let bytes = read_regular(Path::new(&options.token_file), MAX_TOKEN)?;
    let token = std::str::from_utf8(&bytes)
        .map_err(|_| ControlError::new("input_error", "Token must be UTF-8."))?
        .trim()
        .to_owned();
    if token.is_empty() {
        return Err(ControlError::new("input_error", "Token file is empty."));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ControlError::new("transport_error", "Cannot start runtime."))?;
    if options.command == "stream" {
        return runtime
            .block_on(stream::run(&options, &token, params))
            .map(|()| None);
    }
    let value = match runtime.block_on(exchange(&options, &token, &method, params)) {
        Ok(value) => value,
        Err(mut error) => {
            if let Some(gateway) = error.gateway.take() {
                error.gateway = Some(redact(&gateway, &token));
            }
            return Err(error);
        }
    };
    let output = match options.command.as_str() {
        "status" => {
            let value = value.ok_or_else(|| {
                ControlError::new("protocol_error", "Gateway description is missing.")
            })?;
            Ok(object(&[
                ("version", "pilotdeck-control/1".into()),
                ("source", "gateway.describe_server".into()),
                ("server", redact(&value, &token)),
            ]))
        }
        "capabilities" => value
            .and_then(|v| v.get("capabilities").cloned())
            .map(|capabilities| redact(&capabilities, &token))
            .ok_or_else(|| {
                ControlError::new("protocol_error", "Gateway response has no capabilities.")
            }),
        _ => {
            let mut result = JsObject::new();
            result.insert("ok".into(), JsValue::Bool(true));
            if let Some(value) = value {
                result.insert("result".into(), redact(&value, &token));
            }
            Ok(JsValue::Object(result))
        }
    };
    output.map(Some)
}

const HELP: &str = "pilotdeck-control --url <ws-url> --token-file <file> <status|capabilities|call|stream> [--input-file <file>]

call reads one JSON object from stdin or --input-file, for example:
  {\"method\":\"list_sessions\",\"params\":{}}
Input is limited to 1 MiB; numbers must be finite and integers must be safe JS integers. Calls are not automatically retried.
stream reads submit_turn params with sessionKey, runId, channelKey, message, and canPrompt:false. It emits flushed JSONL frames; final:true closes the protocol, not another delta.
call still rejects submit_turn. Streams do not reconnect or detach; closing may abort the session.
Authentication token echoes in Gateway JSON are redacted; general business data is returned as supplied.
serve starts the existing native Gateway in the foreground with explicit isolation options; run pilotdeck-control serve --help.
Schema export and the new app JSON-RPC server are not available.";

fn write_output(text: &str) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "{text}")?;
    output.flush()
}
fn main() -> std::process::ExitCode {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.is_empty() || (args.len() == 1 && args[0] == "--help") {
        return if write_output(HELP).is_ok() {
            std::process::ExitCode::SUCCESS
        } else {
            std::process::ExitCode::FAILURE
        };
    }
    if args.first().is_some_and(|arg| arg == "serve") {
        if args.len() == 2 && args[1] == "--help" {
            return if write_output(serve::HELP).is_ok() {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            };
        }
        return match serve::run(args.into_iter().skip(1)) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(error) => {
                let _ = writeln!(io::stderr().lock(), "{error}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    match run() {
        Ok(None) => std::process::ExitCode::SUCCESS,
        Ok(Some(value)) => match encode(&value) {
            Ok(raw) => {
                if write_output(&raw).is_ok() {
                    std::process::ExitCode::SUCCESS
                } else {
                    let _ = writeln!(
                        io::stderr().lock(),
                        "{{\"kind\":\"output_error\",\"message\":\"Could not deliver the result; the operation may have completed.\"}}"
                    );
                    std::process::ExitCode::FAILURE
                }
            }
            Err(_) => std::process::ExitCode::FAILURE,
        },
        Err(error) => {
            let mut fields = vec![
                ("kind", JsValue::from(error.kind)),
                ("message", JsValue::from(error.message)),
            ];
            if let Some(gateway) = error.gateway {
                fields.push(("error", gateway));
            }
            let raw =
                encode(&object(&fields)).unwrap_or_else(|_| "{\"kind\":\"protocol_error\"}".into());
            let _ = writeln!(io::stderr().lock(), "{raw}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_preserves_surrogates_and_distinct_keys() {
        let value = parse_json(r#"{"a":"\ud800 actual a","[REDACTED]":"other"}"#).unwrap();
        let redacted = redact(&value, "a");
        let raw = encode(&redacted).unwrap();
        assert!(raw.contains(r#"\ud800 actual [REDACTED]"#));
        assert!(raw.contains(r#""[REDACTED]":"#));
        assert!(raw.contains(r#""[REDACTED]#2":"#));
    }

    #[tokio::test(start_paused = true)]
    async fn completed_response_survives_close_timeout() {
        let result = settle(
            Ok::<_, ControlError>(Some(JsValue::Null)),
            std::future::pending(),
            Duration::from_millis(5),
        )
        .await;
        assert!(matches!(result, Ok(Some(JsValue::Null))));
    }
}
