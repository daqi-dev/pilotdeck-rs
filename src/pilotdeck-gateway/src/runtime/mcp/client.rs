//! `McpClient`, ported from the fixed TypeScript baseline's
//! `src/mcp/client/McpClient.ts` together with the slice of
//! `@modelcontextprotocol/sdk@1.29.0` it drives (`Client`, `Protocol`,
//! `StdioClientTransport`).
//!
//! Behaviours (TS §6.1 numbering): M1 memoized `start`; M3 call/list
//! timeouts; M5/M15 exactly one reconnect on a session-expired error; M5b a
//! timed-out call recycles the transport so the next call spawns a fresh
//! process; M6 a five-minute `tools/list` cache.
//!
//! ## Wire behaviour kept from the SDK
//!
//! - Frames are `JSON.stringify(message) + "\n"`; requests serialise as
//!   `{method, params?, jsonrpc, id}` with ids from 0 per connection.
//! - Inbound lines are UTF-8 decoded lossily, a trailing `\r` is dropped, and
//!   a line that is not JSON or not a JSON-RPC message is ignored.
//! - A server `ping` is answered after the current stdout chunk is processed
//!   (a promise hop in JS); any other server request gets an immediate
//!   `Method not found`.
//! - A request timeout sends `notifications/cancelled` and rejects with
//!   `MCP error -32001: Request timed out`; the child exiting rejects every
//!   pending request with `MCP error -32000: Connection closed`, after which
//!   requests fail with `Not connected` (the TS client keeps status `ready`).
//! - The child gets only the SDK's default inherited variables plus the spec
//!   env; stderr is inherited. Close ends stdin, then SIGTERM after 2 s and
//!   SIGKILL after 2 s more.
//!
//! ## Kept TS quirks
//!
//! - `getInstructions()` is always empty: the baseline reads
//!   `getServerCapabilities().instructions` (stripped by the capabilities
//!   schema) and then `_serverInstructions`, which SDK 1.29 renamed.
//! - A handshake timeout leaves the handshake running; its process is only
//!   closed when the 60 s initialize request itself fails.
//! - Any error whose message matches `/timed out|timeout/i` is treated as a
//!   timeout and recycles the transport.
//!
//! ## Not covered (A6.5-pr10a gaps)
//!
//! - `streamable_http` and legacy `sse`: building either transport fails with
//!   `mcp_unsupported_transport` and a message naming the gap.
//! - Output-schema validation of `structuredContent` (the SDK compiles the
//!   tool's `outputSchema` with Ajv): presence is enforced, the contents are
//!   accepted unvalidated.
//! - `callTool`'s `AbortSignal` option.

use super::naming::{
    CallStackExceeded, build_mcp_tool_wire_name, recursively_sanitize_unicode,
    truncate_mcp_tool_description,
};
use super::protocol::{
    Inbound, LATEST_PROTOCOL_VERSION, McpServerSpec, McpStatus, McpToolSpec, McpTransportSpec,
    ResultSchema, SUPPORTED_PROTOCOL_VERSIONS, errno_text, parse_inbound, parse_result,
};
use crate::js_value;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot, watch},
};

/// Connect handshake timeout when the options give none.
pub const DEFAULT_HANDSHAKE_TIMEOUT_MS: f64 = 10_000.0;
/// The SDK's `DEFAULT_REQUEST_TIMEOUT_MSEC` (used by `initialize`).
const DEFAULT_REQUEST_TIMEOUT_MS: f64 = 60_000.0;
const LIST_TOOLS_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// `TIMEOUT_MAX` of Node timers.
const NODE_TIMEOUT_MAX: f64 = 2_147_483_647.0;
/// `DEFAULT_INHERITED_ENV_VARS` (non-Windows).
const DEFAULT_INHERITED_ENV_VARS: [&str; 6] = ["HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER"];
const CLOSE_GRACE: Duration = Duration::from_secs(2);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// `parseInt(text, 10)`.
pub(crate) fn parse_int(text: &str) -> f64 {
    let text = crate::metadata::js_trim_start(text);
    let (sign, digits) = match text.as_bytes().first() {
        Some(b'-') => (-1.0, &text[1..]),
        Some(b'+') => (1.0, &text[1..]),
        _ => (1.0, text),
    };
    let end = digits
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return f64::NAN;
    }
    sign * digits[..end].parse::<f64>().unwrap_or(f64::NAN)
}

/// `DEFAULT_CALL_TIMEOUT_MS`: `parseInt(process.env.PILOTDECK_MCP_TOOL_TIMEOUT_MS
/// ?? "60000", 10)`, read once like the TS module constant.
pub fn default_call_timeout_ms() -> f64 {
    static VALUE: OnceLock<f64> = OnceLock::new();
    *VALUE.get_or_init(|| {
        let raw = std::env::var("PILOTDECK_MCP_TOOL_TIMEOUT_MS").unwrap_or_else(|_| "60000".into());
        parse_int(&raw)
    })
}

/// A Node `setTimeout` delay: out-of-range or `NaN` becomes 1 ms.
pub(crate) fn node_delay(ms: f64) -> Duration {
    let ms = if (1.0..=NODE_TIMEOUT_MAX).contains(&ms) {
        ms
    } else {
        1.0
    };
    Duration::from_secs_f64(ms / 1000.0)
}

/// `McpClientError.code`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpClientErrorCode {
    HandshakeFailed,
    CallTimeout,
    SessionExpired,
    CallFailed,
    UnsupportedTransport,
}
impl McpClientErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HandshakeFailed => "mcp_handshake_failed",
            Self::CallTimeout => "mcp_call_timeout",
            Self::SessionExpired => "mcp_session_expired",
            Self::CallFailed => "mcp_call_failed",
            Self::UnsupportedTransport => "mcp_unsupported_transport",
        }
    }
}

/// The `code` property of a thrown value.
#[derive(Clone, Debug, PartialEq)]
pub enum McpErrorCode {
    /// `McpClientError.code`.
    Client(McpClientErrorCode),
    /// A numeric code (`McpError` JSON-RPC codes).
    Number(f64),
    /// A Node system error code (`ENOENT`, ...).
    Errno(&'static str),
}

/// Every error `McpClient` can throw, in the TS `{ name, code, serverId,
/// message }` projection. Most are `McpClientError`; a few paths propagate
/// the underlying value unwrapped exactly as the baseline does (the retry
/// after a reconnect, `RangeError` from sanitising, `buildTransport`
/// failures).
#[derive(Clone, Debug, PartialEq)]
pub struct McpClientError {
    name: &'static str,
    code: Option<McpErrorCode>,
    server_id: Option<String>,
    message: String,
}
impl McpClientError {
    fn client(code: McpClientErrorCode, message: String, server_id: &str) -> Self {
        Self {
            name: "McpClientError",
            code: Some(McpErrorCode::Client(code)),
            server_id: Some(server_id.into()),
            message,
        }
    }
    /// SDK `McpError`: `MCP error <code>: <message>`.
    fn rpc(code: f64, message: &str) -> Self {
        Self {
            name: "McpError",
            code: Some(McpErrorCode::Number(code)),
            server_id: None,
            message: format!("MCP error {}: {message}", js_value::number_string(code)),
        }
    }
    fn plain(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            code: None,
            server_id: None,
            message: message.into(),
        }
    }
    fn errno(error: &std::io::Error, message: String) -> Self {
        Self {
            name: "Error",
            code: Some(McpErrorCode::Errno(errno_text(error).0)),
            server_id: None,
            message,
        }
    }
    fn not_connected() -> Self {
        Self::plain("Error", "Not connected")
    }
    fn connection_closed() -> Self {
        Self::rpc(-32000.0, "Connection closed")
    }
    fn stack_overflow(_: impl Into<CallStackExceeded>) -> Self {
        Self::plain("RangeError", CallStackExceeded::MESSAGE)
    }
    pub fn name(&self) -> &'static str {
        self.name
    }
    pub fn code(&self) -> Option<&McpErrorCode> {
        self.code.as_ref()
    }
    /// The `mcp_*` code when this is an `McpClientError`.
    pub fn client_code(&self) -> Option<McpClientErrorCode> {
        match self.code {
            Some(McpErrorCode::Client(code)) => Some(code),
            _ => None,
        }
    }
    pub fn server_id(&self) -> Option<&str> {
        self.server_id.as_deref()
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    /// `{ name, code: code ?? null, serverId: serverId ?? null, message }`.
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("name".into(), JsValue::String(self.name.into()));
        object.insert(
            "code".into(),
            match &self.code {
                None => JsValue::Null,
                Some(McpErrorCode::Client(code)) => JsValue::String(code.as_str().into()),
                Some(McpErrorCode::Number(code)) => JsValue::Number(*code),
                Some(McpErrorCode::Errno(code)) => JsValue::String((*code).into()),
            },
        );
        object.insert(
            "serverId".into(),
            self.server_id
                .as_deref()
                .map_or(JsValue::Null, |id| JsValue::String(id.into())),
        );
        object.insert(
            "message".into(),
            JsValue::String(self.message.as_str().into()),
        );
        JsValue::Object(object)
    }
}
impl fmt::Display for McpClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for McpClientError {}

/// `McpClientOptions` (the test-only transport/fetch overrides are not
/// ported).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpClientOptions {
    pub call_timeout_ms: Option<f64>,
    /// Connect handshake timeout. Default 10 s.
    pub handshake_timeout_ms: Option<f64>,
}

/// `callTool` options.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpCallOptions {
    pub timeout_ms: Option<f64>,
}

/// `{ content, isError? }` returned by `callTool`.
#[derive(Clone, Debug, PartialEq)]
pub struct McpToolCallResult {
    pub content: JsValue,
    pub is_error: Option<bool>,
}
impl McpToolCallResult {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("content".into(), self.content.clone());
        if let Some(flag) = self.is_error {
            object.insert("isError".into(), JsValue::Bool(flag));
        }
        JsValue::Object(object)
    }
}

// ---------------------------------------------------------------------------
// transport + protocol
// ---------------------------------------------------------------------------

type Reply = Result<JsValue, McpClientError>;

#[derive(Default)]
struct WireState {
    /// `Protocol._transport` is set.
    connected: bool,
    next_id: i64,
    pending: HashMap<i64, oneshot::Sender<Reply>>,
}

/// One spawned server: `StdioClientTransport` plus `Protocol` bookkeeping.
///
/// Frames go through an unbounded queue to a writer task that owns stdin, so
/// no code path awaits a pipe write while holding a lock (a server blocked on
/// a full stdout pipe can never stall the reader). The queue's mutex doubles
/// as the ordering gate: the reader holds it while it handles one stdout
/// chunk, so a request resolved in that chunk enqueues its follow-up frame
/// only after every reply the chunk produced, as JS microtasks do.
struct Wire {
    /// `None` once `close()` started or the process closed; dropping the
    /// sender lets the writer flush the queue and then end stdin.
    writer: Mutex<Option<mpsc::UnboundedSender<String>>>,
    state: Mutex<WireState>,
    pid: Option<u32>,
    exited: watch::Sender<bool>,
    closed: watch::Sender<bool>,
}

fn frame(entries: Vec<(&str, JsValue)>) -> Result<String, McpClientError> {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    let text = js_value::json_stringify(&JsValue::Object(object))
        .map_err(McpClientError::stack_overflow)?
        .unwrap_or_default();
    Ok(text + "\n")
}

/// `Number(id)` for response routing.
fn message_id(id: &JsValue) -> Option<i64> {
    let number = match id {
        JsValue::Number(number) => *number,
        JsValue::String(text) => {
            js_value::string_to_number(&String::from_utf16_lossy(text.units()))
        }
        _ => return None,
    };
    (number.fract() == 0.0 && number.abs() < 9.0e15).then_some(number as i64)
}

impl Wire {
    fn spawn(
        command: &str,
        args: &[String],
        env: &[(String, String)],
        cwd: Option<&str>,
    ) -> Result<Arc<Self>, McpClientError> {
        let mut process = Command::new(command);
        process
            .args(args)
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        // `{ ...getDefaultEnvironment(), ...spec.env }`.
        for key in DEFAULT_INHERITED_ENV_VARS {
            if let Some(value) = std::env::var_os(key)
                && !value.as_encoded_bytes().starts_with(b"()")
            {
                process.env(key, value);
            }
        }
        for (key, value) in env {
            process.env(key, value);
        }
        if let Some(cwd) = cwd {
            process.current_dir(cwd);
        }
        let mut child = process.spawn().map_err(|error| {
            let code = errno_text(&error).0;
            McpClientError::errno(&error, format!("spawn {command} {code}"))
        })?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let (sender, queue) = mpsc::unbounded_channel();
        tokio::spawn(write_queue(stdin, queue));
        let wire = Arc::new(Self {
            writer: Mutex::new(Some(sender)),
            state: Mutex::new(WireState {
                connected: true,
                ..WireState::default()
            }),
            pid: child.id(),
            exited: watch::channel(false).0,
            closed: watch::channel(false).0,
        });
        tokio::spawn(Self::pump(wire.clone(), child, stdout));
        Ok(wire)
    }

    /// Reads stdout and waits for exit; Node's `close` event is both.
    async fn pump(self: Arc<Self>, mut child: Child, stdout: Option<ChildStdout>) {
        let mut stdout = stdout;
        let mut buffer = Vec::new();
        let mut chunk = vec![0u8; 64 * 1024];
        let mut exited = false;
        while stdout.is_some() || !exited {
            tokio::select! {
                read = async { stdout.as_mut().expect("guarded").read(&mut chunk).await }, if stdout.is_some() => {
                    match read {
                        Ok(0) | Err(_) => stdout = None,
                        Ok(read) => {
                            buffer.extend_from_slice(&chunk[..read]);
                            self.process_chunk(&mut buffer);
                        }
                    }
                }
                _ = child.wait(), if !exited => {
                    exited = true;
                    self.exited.send_replace(true);
                }
            }
        }
        // `_onclose`: the transport is gone and every pending request fails.
        *lock(&self.writer) = None;
        let pending = {
            let mut state = lock(&self.state);
            state.connected = false;
            std::mem::take(&mut state.pending)
        };
        for (_, reply) in pending {
            let _ = reply.send(Err(McpClientError::connection_closed()));
        }
        self.closed.send_replace(true);
    }

    /// One stdout chunk, synchronously and under the queue gate: frames JS
    /// writes synchronously (Method not found) go first, `ping` replies (a
    /// promise hop in JS) at the end of the chunk, and continuations of
    /// requests resolved here only after the gate is released.
    fn process_chunk(&self, buffer: &mut Vec<u8>) {
        let queue = lock(&self.writer);
        let enqueue = |line: Result<String, McpClientError>| {
            if let (Some(queue), Ok(line)) = (queue.as_ref(), line) {
                let _ = queue.send(line);
            }
        };
        let mut deferred = Vec::new();
        while let Some(end) = buffer.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            let text = String::from_utf8_lossy(&line[..line.len() - 1]);
            let text = text.strip_suffix('\r').unwrap_or(&text);
            match parse_inbound(text) {
                Some(Inbound::Result { id, result }) => self.resolve(&id, Ok(result)),
                Some(Inbound::Error { id, code, message }) => {
                    let message = String::from_utf16_lossy(message.units());
                    self.resolve(
                        &id.unwrap_or(JsValue::Undefined),
                        Err(McpClientError::rpc(code, &message)),
                    );
                }
                Some(Inbound::Request { id, method }) if method.equals_str("ping") => {
                    deferred.push(frame(vec![
                        ("result", JsValue::Object(JsObject::new())),
                        ("jsonrpc", "2.0".into()),
                        ("id", id),
                    ]));
                }
                Some(Inbound::Request { id, .. }) => {
                    let mut error = JsObject::new();
                    error.insert("code".into(), JsValue::Number(-32601.0));
                    error.insert("message".into(), "Method not found".into());
                    let reply = frame(vec![
                        ("jsonrpc", "2.0".into()),
                        ("id", id),
                        ("error", JsValue::Object(error)),
                    ]);
                    enqueue(reply);
                }
                // Notifications (cancelled/progress/logging) have no client
                // effect here; unparseable lines go to `onerror`.
                Some(Inbound::Notification) | None => {}
            }
        }
        for reply in deferred {
            enqueue(reply);
        }
    }

    fn resolve(&self, id: &JsValue, reply: Reply) {
        let Some(id) = message_id(id) else {
            return;
        };
        let sender = lock(&self.state).pending.remove(&id);
        if let Some(sender) = sender {
            let _ = sender.send(reply);
        }
    }

    /// `transport.send`: `Not connected` once the transport closed. Write
    /// failures are not reported (Node routes them to `onerror`; the request
    /// then ends with the process or its timeout).
    fn send(&self, line: String) -> Result<(), McpClientError> {
        match lock(&self.writer).as_ref() {
            Some(queue) => {
                let _ = queue.send(line);
                Ok(())
            }
            None => Err(McpClientError::not_connected()),
        }
    }

    /// `Protocol.notification` (no capability or debounce options apply).
    fn notification(&self, method: &str) -> Result<(), McpClientError> {
        if !lock(&self.state).connected {
            return Err(McpClientError::not_connected());
        }
        self.send(frame(vec![
            ("method", method.into()),
            ("jsonrpc", "2.0".into()),
        ])?)
    }

    /// `Protocol.request` with a timeout and result schema.
    async fn request(
        &self,
        method: &str,
        params: Option<JsValue>,
        timeout_ms: f64,
        schema: ResultSchema,
    ) -> Result<JsValue, McpClientError> {
        let (id, reply) = {
            let mut state = lock(&self.state);
            if !state.connected {
                return Err(McpClientError::not_connected());
            }
            let id = state.next_id;
            state.next_id += 1;
            let (sender, reply) = oneshot::channel();
            state.pending.insert(id, sender);
            (id, reply)
        };
        let mut entries = vec![("method", JsValue::String(method.into()))];
        if let Some(params) = params {
            entries.push(("params", params));
        }
        entries.push(("jsonrpc", "2.0".into()));
        entries.push(("id", JsValue::Number(id as f64)));
        let sent = frame(entries).and_then(|line| self.send(line));
        if let Err(error) = sent {
            lock(&self.state).pending.remove(&id);
            return Err(error);
        }
        match tokio::time::timeout(node_delay(timeout_ms), reply).await {
            Ok(Ok(Ok(raw))) => parse_result(schema, &raw)
                .map_err(|message| McpClientError::plain("ZodError", message)),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err(McpClientError::connection_closed()),
            Err(_) => {
                lock(&self.state).pending.remove(&id);
                let error = McpClientError::rpc(-32001.0, "Request timed out");
                let mut params = JsObject::new();
                params.insert("requestId".into(), JsValue::Number(id as f64));
                params.insert(
                    "reason".into(),
                    JsValue::String(format!("McpError: {}", error.message).into()),
                );
                if let Ok(line) = frame(vec![
                    ("jsonrpc", "2.0".into()),
                    ("method", "notifications/cancelled".into()),
                    ("params", JsValue::Object(params)),
                ]) {
                    let _ = self.send(line);
                }
                Err(error)
            }
        }
    }

    /// `StdioClientTransport.close()`.
    async fn close(&self) {
        if lock(&self.writer).take().is_none() {
            return;
        }
        let mut closed = self.closed.subscribe();
        for signal in [rustix::process::Signal::TERM, rustix::process::Signal::KILL] {
            if *closed.borrow() {
                return;
            }
            let _ = tokio::time::timeout(CLOSE_GRACE, closed.wait_for(|closed| *closed)).await;
            if *self.exited.borrow() {
                return;
            }
            if let Some(pid) = self
                .pid
                .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
            {
                let _ = rustix::process::kill_process(pid, signal);
            }
            if signal == rustix::process::Signal::KILL {
                return;
            }
        }
    }
}

/// Owns the child's stdin: writes queued frames in order, then ends stdin
/// (`stdin.end()`) once the queue's sender is dropped. After a failed write
/// the rest of the queue is discarded.
async fn write_queue(stdin: Option<ChildStdin>, mut queue: mpsc::UnboundedReceiver<String>) {
    let mut stdin = stdin;
    while let Some(line) = queue.recv().await {
        if let Some(pipe) = stdin.as_mut()
            && (pipe.write_all(line.as_bytes()).await.is_err() || pipe.flush().await.is_err())
        {
            stdin = None;
        }
    }
}

/// The SDK `Client` on one connection: the cached tool metadata used to gate
/// `callTool`.
struct SdkClient {
    wire: Arc<Wire>,
    output_schema_tools: Mutex<HashSet<JsString>>,
    required_task_tools: Mutex<HashSet<JsString>>,
}

impl SdkClient {
    /// `Client.connect(transport)`: start, `initialize`, version check,
    /// `notifications/initialized`. On failure after the spawn the
    /// transport is closed in the background (`void this.close()`).
    async fn connect(wire: Arc<Wire>) -> Result<Arc<Self>, McpClientError> {
        let client = Arc::new(Self {
            wire,
            output_schema_tools: Mutex::new(HashSet::new()),
            required_task_tools: Mutex::new(HashSet::new()),
        });
        match client.initialize().await {
            Ok(()) => Ok(client),
            Err(error) => {
                let wire = client.wire.clone();
                tokio::spawn(async move { wire.close().await });
                Err(error)
            }
        }
    }

    async fn initialize(&self) -> Result<(), McpClientError> {
        let mut capabilities = JsObject::new();
        capabilities.insert("elicitation".into(), JsValue::Object(JsObject::new()));
        let mut client_info = JsObject::new();
        client_info.insert("name".into(), "pilotdeck".into());
        client_info.insert("version".into(), "0.1.0".into());
        let mut params = JsObject::new();
        params.insert("protocolVersion".into(), LATEST_PROTOCOL_VERSION.into());
        params.insert("capabilities".into(), JsValue::Object(capabilities));
        params.insert("clientInfo".into(), JsValue::Object(client_info));
        let result = self
            .wire
            .request(
                "initialize",
                Some(JsValue::Object(params)),
                DEFAULT_REQUEST_TIMEOUT_MS,
                ResultSchema::Initialize,
            )
            .await?;
        let version = result
            .get("protocolVersion")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        if !SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .any(|supported| version.equals_str(supported))
        {
            return Err(McpClientError::plain(
                "Error",
                format!(
                    "Server's protocol version is not supported: {}",
                    String::from_utf16_lossy(version.units())
                ),
            ));
        }
        self.wire.notification("notifications/initialized")
    }

    async fn list_tools(&self, timeout_ms: f64) -> Result<JsValue, McpClientError> {
        let result = self
            .wire
            .request("tools/list", None, timeout_ms, ResultSchema::ListTools)
            .await?;
        // `cacheToolMetadata(result.tools)`.
        let mut output = HashSet::new();
        let mut required = HashSet::new();
        if let Some(JsValue::Array(tools)) = result.get("tools") {
            for tool in tools {
                let Some(name) = tool.get("name").and_then(JsValue::as_string) else {
                    continue;
                };
                if tool.get("outputSchema").is_some() {
                    output.insert(name.clone());
                }
                let support = tool
                    .get("execution")
                    .and_then(|execution| execution.get("taskSupport"))
                    .and_then(JsValue::as_string);
                if support.is_some_and(|support| support.equals_str("required")) {
                    required.insert(name.clone());
                }
            }
        }
        *lock(&self.output_schema_tools) = output;
        *lock(&self.required_task_tools) = required;
        Ok(result)
    }

    async fn call_tool(
        &self,
        name: &JsString,
        arguments: JsValue,
        timeout_ms: f64,
    ) -> Result<JsValue, McpClientError> {
        let display = String::from_utf16_lossy(name.units());
        if lock(&self.required_task_tools).contains(name) {
            return Err(McpClientError::rpc(
                -32600.0,
                &format!(
                    "Tool \"{display}\" requires task-based execution. Use client.experimental.tasks.callToolStream() instead."
                ),
            ));
        }
        let mut params = JsObject::new();
        params.insert("name".into(), JsValue::String(name.clone()));
        params.insert("arguments".into(), arguments);
        let result = self
            .wire
            .request(
                "tools/call",
                Some(JsValue::Object(params)),
                timeout_ms,
                ResultSchema::CallTool,
            )
            .await?;
        if lock(&self.output_schema_tools).contains(name) {
            let structured = result.get("structuredContent").is_some();
            let is_error = matches!(result.get("isError"), Some(JsValue::Bool(true)));
            if !structured && !is_error {
                return Err(McpClientError::rpc(
                    -32600.0,
                    &format!(
                        "Tool {display} has an output schema but did not return structured content"
                    ),
                ));
            }
        }
        Ok(result)
    }

    async fn close(&self) {
        self.wire.close().await;
    }
}

// ---------------------------------------------------------------------------
// McpClient
// ---------------------------------------------------------------------------

type ConnectResult = Option<Result<(), McpClientError>>;

struct State {
    status: McpStatus,
    client: Option<Arc<SdkClient>>,
    list_tools_cache: Option<(Instant, Vec<McpToolSpec>)>,
    server_instructions: JsString,
    connect: Option<watch::Receiver<ConnectResult>>,
    reconnect_in_flight: bool,
    per_session_dir: Option<PathBuf>,
}

struct Inner {
    spec: McpServerSpec,
    options: McpClientOptions,
    state: Mutex<State>,
}

/// Single-server MCP client façade (`McpClient`). Cloning shares the client.
#[derive(Clone)]
pub struct McpClient {
    inner: Arc<Inner>,
}

impl fmt::Debug for McpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpClient")
            .field("spec", &self.inner.spec)
            .field("status", &self.status())
            .finish()
    }
}

/// `/session.*expired/i` (`.` stops at line terminators).
fn is_session_expired_message(message: &str) -> bool {
    message
        .split(['\n', '\r', '\u{2028}', '\u{2029}'])
        .any(|line| {
            let line = line.to_ascii_lowercase();
            line.find("session")
                .is_some_and(|at| line[at + "session".len()..].contains("expired"))
        })
}

/// `/timed out|timeout/i`.
fn is_timeout_message(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    message.contains("timed out") || message.contains("timeout")
}

/// `path.join(base, name)` for a POSIX base.
fn join_normalized(base: &str, name: &str) -> String {
    let joined = format!("{base}/{name}");
    let absolute = joined.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let body = parts.join("/");
    if absolute {
        format!("/{body}")
    } else if body.is_empty() {
        ".".into()
    } else {
        body
    }
}

/// `mkdtempSync(prefix)`: six random `[A-Za-z0-9]` characters.
fn make_temp_dir(prefix: &str) -> std::io::Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    loop {
        let random = uuid::Uuid::new_v4();
        let suffix: String = random.as_bytes()[..6]
            .iter()
            .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]))
            .collect();
        let path = format!("{prefix}{suffix}");
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

fn env_value<'a>(env: Option<&'a [(String, String)]>, key: &str) -> Option<&'a str> {
    env?.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

impl McpClient {
    pub fn new(spec: McpServerSpec, options: McpClientOptions) -> Self {
        Self {
            inner: Arc::new(Inner {
                spec,
                options,
                state: Mutex::new(State {
                    status: McpStatus::Idle,
                    client: None,
                    list_tools_cache: None,
                    server_instructions: JsString::default(),
                    connect: None,
                    reconnect_in_flight: false,
                    per_session_dir: None,
                }),
            }),
        }
    }

    pub fn spec(&self) -> &McpServerSpec {
        &self.inner.spec
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.inner.state)
    }

    pub fn status(&self) -> McpStatus {
        self.state().status
    }

    pub fn instructions(&self) -> JsString {
        self.state().server_instructions.clone()
    }

    fn id(&self) -> &str {
        &self.inner.spec.id
    }

    fn call_timeout_ms(&self, explicit: Option<f64>) -> f64 {
        explicit
            .or(self.inner.options.call_timeout_ms)
            .or(self.inner.spec.call_timeout_ms)
            .unwrap_or_else(default_call_timeout_ms)
    }

    /// M1 — memoized connect: concurrent callers share one attempt; a failed
    /// attempt is forgotten so the next `start` retries.
    pub async fn start(&self) -> Result<(), McpClientError> {
        let mut receiver = {
            let mut state = self.state();
            match &state.connect {
                Some(receiver) => receiver.clone(),
                None => {
                    let (sender, receiver) = watch::channel(None);
                    state.connect = Some(receiver.clone());
                    let client = self.clone();
                    tokio::spawn(async move {
                        let result = client.run_connect().await;
                        if result.is_err() {
                            client.state().connect = None;
                        }
                        sender.send_replace(Some(result));
                    });
                    receiver
                }
            }
        };
        let outcome = receiver
            .wait_for(Option::is_some)
            .await
            .map(|value| value.clone().expect("checked by wait_for"));
        outcome.unwrap_or_else(|_| {
            Err(McpClientError::client(
                McpClientErrorCode::HandshakeFailed,
                "MCP handshake failed: connect task ended".into(),
                self.id(),
            ))
        })
    }

    async fn run_connect(&self) -> Result<(), McpClientError> {
        self.state().status = McpStatus::Connecting;
        // `buildTransport()` throws outside the handshake try: unwrapped,
        // status stays `connecting`.
        let wire = self.build_transport()?;
        let handshake_ms = self
            .inner
            .options
            .handshake_timeout_ms
            .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_MS);
        let handshake = tokio::spawn(async move {
            match wire {
                Ok(wire) => SdkClient::connect(wire).await,
                Err(error) => Err(error),
            }
        });
        let outcome = match tokio::time::timeout(node_delay(handshake_ms), handshake).await {
            Err(_) => Err(McpClientError::client(
                McpClientErrorCode::HandshakeFailed,
                format!(
                    "MCP handshake timed out after {}ms (server={})",
                    js_value::number_string(handshake_ms),
                    self.id()
                ),
                self.id(),
            )),
            Ok(Err(join)) => Err(McpClientError::plain("Error", join.to_string())),
            Ok(Ok(result)) => result.map_err(|error| {
                if error.client_code().is_some() {
                    error
                } else {
                    McpClientError::client(
                        McpClientErrorCode::HandshakeFailed,
                        format!("MCP handshake failed: {}", error.message),
                        self.id(),
                    )
                }
            }),
        };
        let mut state = self.state();
        match outcome {
            Ok(client) => {
                state.client = Some(client);
                state.status = McpStatus::Ready;
                // See the module docs: the baseline never surfaces them.
                state.server_instructions = JsString::default();
                Ok(())
            }
            Err(error) => {
                state.status = McpStatus::Error;
                Err(error)
            }
        }
    }

    /// `buildTransport()`. The outer error is thrown synchronously (not
    /// wrapped); the inner one is the spawn failure, which surfaces through
    /// the handshake like the SDK's rejected `start()`.
    #[allow(clippy::type_complexity)]
    fn build_transport(&self) -> Result<Result<Arc<Wire>, McpClientError>, McpClientError> {
        let spec = &self.inner.spec;
        match &spec.transport {
            McpTransportSpec::Stdio {
                command,
                args,
                env,
                cwd,
                per_session,
            } => {
                let mut args = args.clone().unwrap_or_default();
                let env_ref = env.as_deref();
                let storage_root = env_value(env_ref, "PILOTDECK_SESSION_STORAGE_ROOT");
                if *per_session && storage_root.is_none_or(str::is_empty) {
                    let base = match env_value(env_ref, "PILOTDECK_SESSION_TMPDIR") {
                        Some(base) => base.to_string(),
                        None => std::env::temp_dir().to_string_lossy().into_owned(),
                    };
                    std::fs::create_dir_all(&base).map_err(|error| {
                        let message =
                            super::protocol::node_fs_message(&error, "mkdir", Path::new(&base));
                        McpClientError::errno(&error, message)
                    })?;
                    let prefix = join_normalized(&base, &format!("pilotdeck-mcp-{}-", spec.id));
                    let dir = make_temp_dir(&prefix).map_err(|error| {
                        let message = super::protocol::node_fs_message(
                            &error,
                            "mkdtemp",
                            Path::new(&format!("{prefix}XXXXXX")),
                        );
                        McpClientError::errno(&error, message)
                    })?;
                    args.push(format!("--user-data-dir={dir}"));
                    self.state().per_session_dir = Some(PathBuf::from(dir));
                }
                Ok(Wire::spawn(
                    command,
                    &args,
                    env.as_deref().unwrap_or_default(),
                    cwd.as_deref(),
                ))
            }
            McpTransportSpec::StreamableHttp { url, .. } | McpTransportSpec::Sse { url, .. } => {
                let kind = if matches!(spec.transport, McpTransportSpec::Sse { .. }) {
                    "sse"
                } else {
                    "streamable_http"
                };
                if url::Url::parse(url).is_err() {
                    return Err(McpClientError::plain("TypeError", "Invalid URL"));
                }
                Err(McpClientError::client(
                    McpClientErrorCode::UnsupportedTransport,
                    format!(
                        "Unsupported transport: {kind} is not ported yet (A6.5-pr10a covers stdio only)"
                    ),
                    &spec.id,
                ))
            }
        }
    }

    /// M6 — cached `tools/list`.
    pub async fn list_tools(&self) -> Result<Vec<McpToolSpec>, McpClientError> {
        if let Some((expires, tools)) = &self.state().list_tools_cache
            && *expires > Instant::now()
        {
            return Ok(tools.clone());
        }
        self.start().await?;
        if self.state().client.is_none() {
            return Err(self.not_connected());
        }
        let timeout = self.call_timeout_ms(None);
        let result = self
            .call_with_reconnect(|client| async move {
                match client {
                    Some(client) => client.list_tools(timeout).await,
                    None => Err(null_property("listTools")),
                }
            })
            .await?;
        let mut tools = Vec::new();
        if let Some(JsValue::Array(raw)) = result.get("tools") {
            for tool in raw {
                tools.push(
                    self.to_tool_spec(tool)
                        .map_err(McpClientError::stack_overflow)?,
                );
            }
        }
        self.state().list_tools_cache =
            Some((Instant::now() + LIST_TOOLS_CACHE_TTL, tools.clone()));
        Ok(tools)
    }

    /// M3 + M5 + M14 + M15 — call a tool with timeout and one reconnect.
    /// `args` of `undefined`/`null` is sent as `{}`.
    pub async fn call_tool(
        &self,
        tool_name: &JsString,
        args: &JsValue,
        options: McpCallOptions,
    ) -> Result<McpToolCallResult, McpClientError> {
        self.start().await?;
        if self.state().client.is_none() {
            return Err(self.not_connected());
        }
        let timeout = self.call_timeout_ms(options.timeout_ms);
        let arguments = match args {
            JsValue::Undefined | JsValue::Null => JsValue::Object(JsObject::new()),
            other => other.clone(),
        };
        let result = self
            .call_with_reconnect(|client| {
                let arguments = arguments.clone();
                async move {
                    match client {
                        Some(client) => client.call_tool(tool_name, arguments, timeout).await,
                        None => Err(null_property("callTool")),
                    }
                }
            })
            .await?;
        let content = result.get("content").cloned().unwrap_or(JsValue::Undefined);
        Ok(McpToolCallResult {
            content: recursively_sanitize_unicode(&content)
                .map_err(McpClientError::stack_overflow)?,
            is_error: result.get("isError").and_then(JsValue::as_bool),
        })
    }

    fn not_connected(&self) -> McpClientError {
        McpClientError::client(
            McpClientErrorCode::HandshakeFailed,
            "Client not connected".into(),
            self.id(),
        )
    }

    /// M5 + M15 wrapper; `call` reads the current connection each time, as
    /// the TS closure re-reads `this.client`.
    async fn call_with_reconnect<T, F, Fut>(&self, call: F) -> Result<T, McpClientError>
    where
        F: Fn(Option<Arc<SdkClient>>) -> Fut,
        Fut: Future<Output = Result<T, McpClientError>>,
    {
        let current = self.state().client.clone();
        let error = match call(current).await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if is_session_expired_message(&error.message) {
            let claimed = {
                let mut state = self.state();
                !std::mem::replace(&mut state.reconnect_in_flight, true)
            };
            if claimed {
                let result = async {
                    self.reconnect().await?;
                    let current = self.state().client.clone();
                    call(current).await
                }
                .await;
                self.state().reconnect_in_flight = false;
                return result;
            }
        }
        if error.name == "McpClientError" {
            return Err(error);
        }
        let id = self.id();
        if error.code == Some(McpErrorCode::Number(-32001.0)) || is_timeout_message(&error.message)
        {
            self.recycle_transport_after_timeout();
            return Err(McpClientError::client(
                McpClientErrorCode::CallTimeout,
                format!("MCP call timed out (server={id}): {}", error.message),
                id,
            ));
        }
        Err(McpClientError::client(
            McpClientErrorCode::CallFailed,
            format!("MCP call failed (server={id}): {}", error.message),
            id,
        ))
    }

    /// M5b — drop the wedged transport synchronously; close it and remove
    /// the per-session directory in the background.
    fn recycle_transport_after_timeout(&self) {
        let (client, dir) = {
            let mut state = self.state();
            let client = state.client.take();
            state.connect = None;
            state.list_tools_cache = None;
            let dir = state.per_session_dir.take();
            state.status = McpStatus::Error;
            (client, dir)
        };
        tokio::spawn(async move {
            if let Some(client) = client {
                client.close().await;
            }
            if let Some(dir) = dir {
                let _ = std::fs::remove_dir_all(dir);
            }
        });
    }

    /// M5 — close the existing connection and connect again.
    async fn reconnect(&self) -> Result<(), McpClientError> {
        let client = {
            let mut state = self.state();
            state.status = McpStatus::Connecting;
            state.list_tools_cache = None;
            state.client.clone()
        };
        if let Some(client) = client {
            client.close().await;
        }
        {
            let mut state = self.state();
            state.client = None;
            state.connect = None;
        }
        self.start().await
    }

    pub async fn close(&self) {
        let client = self.state().client.clone();
        if let Some(client) = client {
            client.close().await;
        }
        let dir = {
            let mut state = self.state();
            state.client = None;
            state.connect = None;
            state.status = McpStatus::Idle;
            state.list_tools_cache = None;
            state.per_session_dir.take()
        };
        if let Some(dir) = dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    fn to_tool_spec(&self, raw: &JsValue) -> Result<McpToolSpec, CallStackExceeded> {
        let sanitized = recursively_sanitize_unicode(raw)?;
        let server_id = JsString::from(self.id());
        let tool_name = sanitized
            .get("name")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let description = sanitized
            .get("description")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let input_schema = sanitized.get("inputSchema").cloned().unwrap_or_else(|| {
            let mut fallback = JsObject::new();
            fallback.insert("type".into(), "object".into());
            fallback.insert("properties".into(), JsValue::Object(JsObject::new()));
            JsValue::Object(fallback)
        });
        Ok(McpToolSpec {
            wire_name: build_mcp_tool_wire_name(&server_id, &tool_name),
            server_id,
            tool_name,
            description: truncate_mcp_tool_description(&description),
            input_schema,
            annotations: sanitized.get("annotations").cloned(),
            meta: sanitized.get("_meta").cloned(),
        })
    }
}

/// `this.client!.<method>` on a `null` client.
fn null_property(method: &str) -> McpClientError {
    McpClientError::plain(
        "TypeError",
        format!("Cannot read properties of null (reading '{method}')"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_matches_js() {
        for (input, expected) in [
            ("60000", 60000.0),
            ("  42ms", 42.0),
            ("-7", -7.0),
            ("+8", 8.0),
            ("1e3", 1.0),
            ("0x10", 0.0),
        ] {
            assert_eq!(parse_int(input), expected, "{input:?}");
        }
        assert!(parse_int("abc").is_nan());
        assert!(parse_int("").is_nan());
        assert!(parse_int("-").is_nan());
    }

    #[test]
    fn node_delay_clamps_like_set_timeout() {
        assert_eq!(node_delay(f64::NAN), Duration::from_millis(1));
        assert_eq!(node_delay(0.0), Duration::from_millis(1));
        assert_eq!(node_delay(-5.0), Duration::from_millis(1));
        assert_eq!(node_delay(2_147_483_648.0), Duration::from_millis(1));
        assert_eq!(node_delay(300.0), Duration::from_millis(300));
    }

    #[test]
    fn error_regexes_are_line_bound_and_ascii_case_insensitive() {
        assert!(is_session_expired_message(
            "MCP error -32000: Session expired"
        ));
        assert!(is_session_expired_message("session has EXPIRED"));
        assert!(!is_session_expired_message("session\nexpired"));
        assert!(!is_session_expired_message("expired session"));
        assert!(is_timeout_message("upstream Timeout"));
        assert!(is_timeout_message("Request TIMED OUT"));
        assert!(!is_timeout_message("time out"));
    }

    #[test]
    fn join_normalizes_like_path_join() {
        assert_eq!(join_normalized("/a/b/", "p-"), "/a/b/p-");
        assert_eq!(join_normalized("/a/./b/../c", "p-"), "/a/c/p-");
        assert_eq!(join_normalized("rel//x", "p-"), "rel/x/p-");
        assert_eq!(join_normalized("../x", "p-"), "../x/p-");
    }

    #[test]
    fn message_ids_follow_number_coercion() {
        assert_eq!(message_id(&JsValue::Number(3.0)), Some(3));
        assert_eq!(message_id(&JsValue::String("0".into())), Some(0));
        assert_eq!(message_id(&JsValue::String(" 2 ".into())), Some(2));
        assert_eq!(message_id(&JsValue::String("x".into())), None);
        assert_eq!(message_id(&JsValue::Undefined), None);
    }
}
