//! `McpClient`, ported from the fixed TypeScript baseline's
//! `src/mcp/client/McpClient.ts` together with the slice of
//! `@modelcontextprotocol/sdk@1.29.0` it drives (`Client`; the `Protocol`
//! and transports live in `wire.rs` and `http.rs`).
//!
//! Behaviours (TS §6.1 numbering): M1 memoized `start`; M2 transports
//! `stdio`, `streamable_http` and legacy `sse` (`http` and `https`); M3
//! call/list timeouts; M5/M15 exactly one reconnect on a session-expired
//! error; M5b a timed-out call recycles the transport so the next call
//! spawns a fresh process; M6 a five-minute `tools/list` cache.
//!
//! ## Wire behaviour kept from the SDK
//!
//! - Frames are `JSON.stringify(message)` (`+ "\n"` on stdio); requests
//!   serialise as `{method, params?, jsonrpc, id}` with ids from 0 per
//!   connection.
//! - Inbound stdio lines are UTF-8 decoded lossily, a trailing `\r` is
//!   dropped, and a line that is not JSON or not a JSON-RPC message is
//!   ignored. Frame order within one stdout chunk follows the JS microtask
//!   depths (see `wire.rs`).
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
//! - A handshake timeout leaves the handshake running (not cancelled); its
//!   process is closed when the 60 s initialize request itself fails, or —
//!   see below — when the handshake succeeds late.
//! - Any error whose message matches `/timed out|timeout/i` is treated as a
//!   timeout and recycles the transport. HTTP 404 is not a session expiry by
//!   itself: the baseline checks `statusCode`, which the SDK never sets.
//!
//! ## Not covered
//!
//! - OAuth (`authProvider` is never set by the baseline).
//! - Remaining output-schema gaps are listed in `output_schema.rs`.
//!
//! `https:` MCP endpoints use the shared model transport's rustls connector
//! (webpki roots, like Node's bundled CA store); `with_tls_roots` is the test
//! seam that trusts a fixture CA instead.
//!
//! ## Intentional difference (gate2 P1-1, #361)
//!
//! Already human-approved: specs/parity/accepted-diffs.md #4 (`handshakeClose`, 2026-10-21).
//!
//! `close()` while a handshake is in flight: the baseline installs the late
//! connection and reports `ready`, and a `start()` racing it opens a second
//! one; the loser's child is never closed. Here `close()` bumps an epoch: the
//! late connection is closed and that `start()` fails with
//! `mcp_handshake_failed` ("MCP handshake aborted: client closed during
//! connect"), so at most one connection is live; status stays as `close()`
//! left it. The epoch is taken when an attempt is published (`start()` /
//! `reconnect()`), so a `close()` that runs before the connect task is first
//! polled also aborts it, and `close()`'s tail never marks a newer attempt
//! idle. A handshake that succeeds after the handshake timeout is closed
//! rather than left unowned.
//! A timed-out request can recycle only the connection it ran on; a second
//! timeout from an old connection leaves the newer handshake and client alone.

use super::http::{HttpKind, HttpLink, parse_endpoint};
use super::naming::{
    CallStackExceeded, build_mcp_tool_wire_name, recursively_sanitize_unicode,
    truncate_mcp_tool_description,
};
use super::protocol::{
    LATEST_PROTOCOL_VERSION, McpServerSpec, McpStatus, McpToolSpec, McpTransportSpec, ResultSchema,
    SUPPORTED_PROTOCOL_VERSIONS, errno_text, parse_result,
};
pub use super::wire::{McpAbort, McpAbortSignal};
use super::wire::{Reply, Settled, Wire, lock, node_delay};
use crate::js_value;
use crate::runtime::model::http::ProxySettings;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashMap;
use std::{
    collections::HashSet,
    fmt,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};

/// Connect handshake timeout when the options give none.
pub const DEFAULT_HANDSHAKE_TIMEOUT_MS: f64 = 10_000.0;
/// The SDK's `DEFAULT_REQUEST_TIMEOUT_MSEC` (used by `initialize`).
const DEFAULT_REQUEST_TIMEOUT_MS: f64 = 60_000.0;
const LIST_TOOLS_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
/// GET timeout of the streamable HTTP transport when the options give none.
const DEFAULT_GET_TIMEOUT_MS: f64 = 10_000.0;

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
    /// Any other string code (`network_timeout`, `ERR_INVALID_URL`, ...).
    Text(&'static str),
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
    pub(super) fn client(code: McpClientErrorCode, message: String, server_id: &str) -> Self {
        Self {
            name: "McpClientError",
            code: Some(McpErrorCode::Client(code)),
            server_id: Some(server_id.into()),
            message,
        }
    }
    /// SDK `McpError`: `MCP error <code>: <message>`.
    pub(super) fn rpc(code: f64, message: &str) -> Self {
        Self {
            name: "McpError",
            code: Some(McpErrorCode::Number(code)),
            server_id: None,
            message: format!("MCP error {}: {message}", js_value::number_string(code)),
        }
    }
    pub(super) fn plain(name: &'static str, message: impl Into<String>) -> Self {
        Self {
            name,
            code: None,
            server_id: None,
            message: message.into(),
        }
    }
    /// An arbitrary thrown value in the `{ name, code, message }` shape (for
    /// injected runtime ports).
    pub fn new(name: &'static str, code: Option<McpErrorCode>, message: impl Into<String>) -> Self {
        Self {
            name,
            code,
            server_id: None,
            message: message.into(),
        }
    }
    pub(super) fn with_code(name: &'static str, code: McpErrorCode, message: String) -> Self {
        Self {
            name,
            code: Some(code),
            server_id: None,
            message,
        }
    }
    pub(super) fn errno(error: &std::io::Error, message: String) -> Self {
        Self {
            name: "Error",
            code: Some(McpErrorCode::Errno(errno_text(error).0)),
            server_id: None,
            message,
        }
    }
    pub(super) fn not_connected() -> Self {
        Self::plain("Error", "Not connected")
    }
    pub(super) fn connection_closed() -> Self {
        Self::rpc(-32000.0, "Connection closed")
    }
    pub(super) fn stack_overflow(_: impl Into<CallStackExceeded>) -> Self {
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
                Some(McpErrorCode::Errno(code) | McpErrorCode::Text(code)) => {
                    JsValue::String((*code).into())
                }
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
    /// `pilotdeck.yaml` `proxy` (`url` / `noProxy`), resolved against the
    /// environment by the shared connector. `None` keeps the env-only source.
    pub proxy: Option<ProxySettings>,
}

/// `callTool` options.
#[derive(Clone, Debug, Default)]
pub struct McpCallOptions {
    pub timeout_ms: Option<f64>,
    /// `options.signal`: aborting rejects the pending request and sends
    /// `notifications/cancelled`, like an `AbortSignal` passed to the SDK.
    pub signal: Option<McpAbortSignal>,
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

/// The SDK `Client` on one connection: the cached tool metadata used to gate
/// `callTool`.
struct SdkClient {
    wire: Arc<Wire>,
    /// Tool name → `outputSchema` (`_cachedToolOutputValidators`).
    output_schemas: Mutex<HashMap<JsString, JsValue>>,
    required_task_tools: Mutex<HashSet<JsString>>,
}

impl SdkClient {
    /// `Client.connect(transport)`: start, `initialize`, version check,
    /// `notifications/initialized`. On failure after the spawn the
    /// transport is closed in the background (`void this.close()`).
    async fn connect(
        wire: Arc<Wire>,
        close_finished: oneshot::Sender<()>,
    ) -> Result<Arc<Self>, McpClientError> {
        // `Protocol.connect` → `transport.start()`, outside the try that
        // closes the transport on failure.
        if let Some(link) = wire.http_link() {
            link.start(&wire).await?;
        }
        let client = Arc::new(Self {
            wire,
            output_schemas: Mutex::new(HashMap::new()),
            required_task_tools: Mutex::new(HashSet::new()),
        });
        match client.initialize().await {
            Ok(()) => Ok(client),
            Err(error) => {
                let wire = client.wire.clone();
                tokio::spawn(async move {
                    wire.close().await;
                    let _ = close_finished.send(());
                });
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
                Some(initialize_settled),
                None,
            )
            .await?;
        let version = supported_version(&result)?;
        if let Some(link) = self.wire.http_link() {
            link.set_protocol_version(version);
        }
        // On stdio the `notifications/initialized` frame was already queued
        // in the stdout chunk that settled the request (`initialize_settled`).
        if self.wire.is_stdio() {
            return Ok(());
        }
        self.wire.notification("notifications/initialized").await
    }

    async fn list_tools(
        &self,
        timeout_ms: f64,
        server_id: &str,
    ) -> Result<JsValue, McpClientError> {
        let result = self
            .wire
            .request(
                "tools/list",
                None,
                timeout_ms,
                ResultSchema::ListTools,
                None,
                None,
            )
            .await?;
        // `cacheToolMetadata(result.tools)`.
        let mut output = HashMap::new();
        let mut required = HashSet::new();
        if let Some(JsValue::Array(tools)) = result.get("tools") {
            for tool in tools {
                let Some(name) = tool.get("name").and_then(JsValue::as_string) else {
                    continue;
                };
                if let Some(schema) = tool.get("outputSchema") {
                    // The SDK compiles every outputSchema while caching
                    // `tools/list` (`cacheToolMetadata`); a bad schema
                    // rejects the whole list instead of silently turning
                    // into a permissive validator later.
                    if let Err(error) = super::output_schema::check_schema(schema) {
                        return Err(McpClientError::client(
                            McpClientErrorCode::CallFailed,
                            format!("MCP call failed (server={server_id}): {}", error.0),
                            server_id,
                        ));
                    }
                    output.insert(name.clone(), schema.clone());
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
        *lock(&self.output_schemas) = output;
        *lock(&self.required_task_tools) = required;
        Ok(result)
    }

    async fn call_tool(
        &self,
        name: &JsString,
        arguments: JsValue,
        timeout_ms: f64,
        signal: Option<&McpAbortSignal>,
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
                None,
                signal,
            )
            .await?;
        let schema = lock(&self.output_schemas).get(name).cloned();
        if let Some(schema) = schema {
            let structured = result.get("structuredContent");
            let is_error = matches!(result.get("isError"), Some(JsValue::Bool(true)));
            if structured.is_none() && !is_error {
                return Err(McpClientError::rpc(
                    -32600.0,
                    &format!(
                        "Tool {display} has an output schema but did not return structured content"
                    ),
                ));
            }
            if let Some(structured) = structured {
                validate_structured_content(&schema, structured)?;
            }
        }
        Ok(result)
    }

    async fn close(&self) {
        self.wire.close().await;
    }
}

/// The version check of `Client.connect`; the version string on success.
fn supported_version(result: &JsValue) -> Result<String, McpClientError> {
    let version = result
        .get("protocolVersion")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let text = String::from_utf16_lossy(version.units());
    if SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .any(|supported| version.equals_str(supported))
    {
        Ok(text)
    } else {
        Err(McpClientError::plain(
            "Error",
            format!("Server's protocol version is not supported: {text}"),
        ))
    }
}

/// What the stdio transport sees one microtask after the initialize
/// response is routed: the `Client.connect` continuation either sends
/// `notifications/initialized` or closes the transport.
fn initialize_settled(reply: &Reply) -> Settled {
    match reply {
        Ok(raw) => match parse_result(ResultSchema::Initialize, raw) {
            Ok(result) if supported_version(&result).is_ok() => Settled::Initialized,
            _ => Settled::End,
        },
        Err(_) => Settled::End,
    }
}

// ---------------------------------------------------------------------------
// McpClient
// ---------------------------------------------------------------------------

type ConnectResult = Option<Result<(), McpClientError>>;

/// Retains the failed handshake's original close completion, since calling
/// `Wire::close` a second time returns immediately during its grace period.
struct PerSessionDirectory {
    path: PathBuf,
    failed_close: oneshot::Receiver<()>,
}

impl PerSessionDirectory {
    async fn remove(self) {
        // Success/spawn failure drops the sender; failed initialization keeps
        // it in the background close task until that task finishes.
        let _ = self.failed_close.await;
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(self.path)).await;
    }
}

/// The connection retains both the task and a cancellable wait handle.
/// Cancelling a close caller cannot remove the task from a later dispose.
struct Cleanup {
    task: tokio::task::JoinHandle<()>,
    done: watch::Receiver<bool>,
    wait_on_close: bool,
}

struct State {
    status: McpStatus,
    client: Option<Arc<SdkClient>>,
    list_tools_cache: Option<(Instant, Vec<McpToolSpec>)>,
    server_instructions: JsString,
    connect: Option<watch::Receiver<ConnectResult>>,
    /// Connection generation. Bumped when a connect attempt is published
    /// (`start`, `reconnect`) and by `close()`; an attempt only installs its
    /// connection while the epoch is still the one it was published under.
    epoch: u64,
    reconnect_in_flight: bool,
    per_session_dir: Option<PerSessionDirectory>,
    cleanups: Vec<Cleanup>,
}

impl State {
    fn retire_connection(&mut self) -> watch::Receiver<bool> {
        let client = self.client.take();
        let dir = self.per_session_dir.take();
        self.spawn_cleanup(
            async move {
                if let Some(client) = client {
                    client.close().await;
                }
                if let Some(dir) = dir {
                    dir.remove().await;
                }
            },
            true,
        )
    }

    fn spawn_cleanup(
        &mut self,
        cleanup: impl Future<Output = ()> + Send + 'static,
        wait_on_close: bool,
    ) -> watch::Receiver<bool> {
        self.cleanups.retain(|cleanup| !cleanup.task.is_finished());
        let (sender, done) = watch::channel(false);
        let task = tokio::spawn(async move {
            cleanup.await;
            sender.send_replace(true);
        });
        self.cleanups.push(Cleanup {
            task,
            done: done.clone(),
            wait_on_close,
        });
        done
    }
}

async fn await_cleanup(mut done: watch::Receiver<bool>) {
    let _ = done.wait_for(|done| *done).await;
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
    /// TLS trust for `https:` transports; `None` = the shared webpki roots.
    tls_config: Option<Arc<rustls::ClientConfig>>,
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

/// Base directory for per-session MCP temp dirs: node's `os.tmpdir()` order,
/// but a non-UTF-8 value is skipped like an unset one. The downstream path
/// handling here is `&str`, so a lossy conversion would silently create a
/// different `U+FFFD` directory; skipping keeps this call site's behavior from
/// before the shared helper. Node itself keeps the raw bytes, so a non-UTF-8
/// TMPDIR remains a residual deviation on this path (A6.4-MODEL-DUMP-SECURE-001).
fn mcp_session_tmpdir(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> String {
    crate::node_io::node_tmpdir(|key| var(key).filter(|value| value.to_str().is_some()))
        .into_string()
        .expect("only UTF-8 candidates are selected")
}

/// `mkdirSync(path, { recursive: true })`; `''` fails like Node.
fn mkdir_recursive(base: &str) -> Result<(), McpClientError> {
    if base.is_empty() {
        let error = std::io::Error::from_raw_os_error(libc::ENOENT);
        return Err(McpClientError::errno(
            &error,
            "ENOENT: no such file or directory, mkdir ''".into(),
        ));
    }
    std::fs::create_dir_all(base).map_err(|error| {
        let message = super::protocol::node_fs_message(&error, "mkdir", Path::new(base));
        McpClientError::errno(&error, message)
    })
}

/// `mkdtempSync(prefix)`: six random `[A-Za-z0-9]` characters, mode 0700.
fn make_temp_dir(prefix: &str) -> std::io::Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    loop {
        let random = uuid::Uuid::new_v4();
        let suffix: String = random.as_bytes()[..6]
            .iter()
            .map(|byte| char::from(ALPHABET[usize::from(*byte) % ALPHABET.len()]))
            .collect();
        let path = format!("{prefix}{suffix}");
        let mut builder = std::fs::DirBuilder::new();
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&path) {
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
                    epoch: 0,
                    reconnect_in_flight: false,
                    per_session_dir: None,
                    cleanups: Vec::new(),
                }),
            }),
            tls_config: None,
        }
    }

    /// Trust exactly `roots` for `https:` MCP transports (tests use the
    /// fixture's throwaway CA, like `HttpHookExecutor::with_roots`). The
    /// default `new()` keeps the bundled Mozilla roots, so a fixture CA
    /// never becomes a production trust source.
    #[doc(hidden)]
    pub fn with_tls_roots(mut self, roots: rustls::RootCertStore) -> Self {
        self.tls_config = Some(Arc::new(crate::runtime::model::http::rooted_client_config(
            roots,
        )));
        self
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
                    // The attempt's generation is fixed here, under the lock
                    // that publishes it, not when the task is first polled.
                    state.epoch += 1;
                    let epoch = state.epoch;
                    state.status = McpStatus::Connecting;
                    let client = self.clone();
                    let mine = receiver.clone();
                    tokio::spawn(async move {
                        let result = client.run_connect(epoch).await;
                        if result.is_err() {
                            // Forget only this attempt, never a newer one.
                            let mut state = client.state();
                            if state
                                .connect
                                .as_ref()
                                .is_some_and(|current| current.same_channel(&mine))
                            {
                                state.connect = None;
                            }
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

    fn aborted(&self) -> McpClientError {
        McpClientError::client(
            McpClientErrorCode::HandshakeFailed,
            format!(
                "MCP handshake aborted: client closed during connect (server={})",
                self.id()
            ),
            self.id(),
        )
    }

    async fn run_connect(&self, epoch: u64) -> Result<(), McpClientError> {
        // Superseded before the task first ran: spawn nothing.
        if self.state().epoch != epoch {
            return Err(self.aborted());
        }
        // `buildTransport()` throws outside the handshake try: unwrapped,
        // status stays `connecting`.
        let mut dir = None;
        let wire = self.build_transport(&mut dir)?;
        let (close_finished, failed_close) = oneshot::channel();
        let mut dir = dir.map(|path| PerSessionDirectory { path, failed_close });
        let handshake_ms = self
            .inner
            .options
            .handshake_timeout_ms
            .unwrap_or(DEFAULT_HANDSHAKE_TIMEOUT_MS);
        let mut handshake = tokio::spawn(async move {
            match wire {
                Ok(wire) => SdkClient::connect(wire, close_finished).await,
                Err(error) => Err(error),
            }
        });
        let outcome = match tokio::time::timeout(node_delay(handshake_ms), &mut handshake).await {
            Err(_) => {
                // gate1: the handshake keeps running after the timeout (the
                // baseline's race loser is not cancelled). This attempt has
                // already failed, so a connection that still lands is owned
                // by nobody: close it so its child process is reaped.
                let late_dir = dir.take();
                self.state().spawn_cleanup(
                    async move {
                        if let Ok(Ok(late)) = handshake.await {
                            late.close().await;
                        }
                        if let Some(dir) = late_dir {
                            dir.remove().await;
                        }
                    },
                    false,
                );
                Err(McpClientError::client(
                    McpClientErrorCode::HandshakeFailed,
                    format!(
                        "MCP handshake timed out after {}ms (server={})",
                        js_value::number_string(handshake_ms),
                        self.id()
                    ),
                    self.id(),
                ))
            }
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
        if state.epoch != epoch {
            // gate2 P1-1 / #361: `close()` ran during this handshake. The
            // baseline still installs the connection (and a racing `start()`
            // could install a second one); here the late connection is closed
            // and the attempt fails, so at most one connection is ever live.
            state.spawn_cleanup(
                async move {
                    if let Ok(client) = outcome {
                        client.close().await;
                    }
                    if let Some(dir) = dir {
                        dir.remove().await;
                    }
                },
                true,
            );
            return Err(self.aborted());
        }
        // Installed under the epoch-checked lock, so `close()` either takes
        // it or this attempt was superseded and removed it above.
        if dir.is_some() {
            // A previous failed (non-timeout) attempt may still own a
            // directory and a child in its close grace period. TS overwrites
            // the directory slot and leaks it; retain cleanup ownership and
            // wait for that attempt's original background close.
            if let Some(old) = std::mem::replace(&mut state.per_session_dir, dir) {
                state.spawn_cleanup(old.remove(), true);
            }
        }
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
    fn build_transport(
        &self,
        per_session_dir: &mut Option<PathBuf>,
    ) -> Result<Result<Arc<Wire>, McpClientError>, McpClientError> {
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
                        None => mcp_session_tmpdir(|key| std::env::var_os(key)),
                    };
                    mkdir_recursive(&base)?;
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
                    *per_session_dir = Some(PathBuf::from(dir));
                }
                Ok(Wire::spawn_stdio(
                    command,
                    &args,
                    env.as_deref().unwrap_or_default(),
                    cwd.as_deref(),
                ))
            }
            McpTransportSpec::StreamableHttp { url, headers }
            | McpTransportSpec::Sse { url, headers } => {
                let kind = if matches!(spec.transport, McpTransportSpec::Sse { .. }) {
                    HttpKind::LegacySse
                } else {
                    HttpKind::Streamable
                };
                let url = parse_endpoint(url, &spec.id)?;
                let options = &self.inner.options;
                let config = self.tls_config.clone().unwrap_or_else(|| {
                    Arc::new(crate::runtime::model::http::webpki_client_config())
                });
                Ok(Ok(Wire::http(HttpLink::with_config(
                    kind,
                    url,
                    headers.clone().unwrap_or_default(),
                    options
                        .call_timeout_ms
                        .unwrap_or_else(default_call_timeout_ms),
                    options
                        .handshake_timeout_ms
                        .unwrap_or(DEFAULT_GET_TIMEOUT_MS),
                    (*config).clone(),
                    options.proxy.as_ref(),
                ))))
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
        let server_id = self.id().to_owned();
        let result = self
            .call_with_reconnect(|client| {
                let server_id = server_id.clone();
                async move {
                    match client {
                        Some(client) => client.list_tools(timeout, &server_id).await,
                        None => Err(null_property("listTools")),
                    }
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
                let options = &options;
                async move {
                    match client {
                        Some(client) => {
                            client
                                .call_tool(tool_name, arguments, timeout, options.signal.as_ref())
                                .await
                        }
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
        let error = match call(current.clone()).await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        if is_session_expired_message(&error.message) {
            let claimed = {
                let mut state = self.state();
                !std::mem::replace(&mut state.reconnect_in_flight, true)
            };
            if claimed {
                // Reset even when the caller drops this future mid-reconnect.
                struct Reset<'a>(&'a McpClient);
                impl Drop for Reset<'_> {
                    fn drop(&mut self) {
                        self.0.state().reconnect_in_flight = false;
                    }
                }
                let _reset = Reset(self);
                self.reconnect().await?;
                let current = self.state().client.clone();
                return call(current).await;
            }
        }
        if error.name == "McpClientError" {
            return Err(error);
        }
        let id = self.id();
        if error.code == Some(McpErrorCode::Number(-32001.0)) || is_timeout_message(&error.message)
        {
            self.recycle_transport_after_timeout(current.as_ref());
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
    fn recycle_transport_after_timeout(&self, timed_out: Option<&Arc<SdkClient>>) {
        let mut state = self.state();
        // A second timeout from an already retired connection owns neither
        // the new client nor its published handshake slot.
        if !state
            .client
            .as_ref()
            .zip(timed_out)
            .is_some_and(|(current, timed_out)| Arc::ptr_eq(current, timed_out))
        {
            return;
        }
        state.connect = None;
        state.list_tools_cache = None;
        state.status = McpStatus::Error;
        state.retire_connection();
    }

    /// Close the old connection before publishing a replacement attempt.
    async fn reconnect(&self) -> Result<(), McpClientError> {
        let done = {
            let mut state = self.state();
            state.status = McpStatus::Connecting;
            state.list_tools_cache = None;
            state.connect = None;
            state.retire_connection()
        };
        await_cleanup(done).await;
        self.start().await
    }

    /// Retire the current connection and await its already-retired cleanup.
    /// A timed-out handshake still running is joined only by final dispose;
    /// ordinary close retains the SDK's nonwaiting timeout behaviour.
    pub async fn close(&self) {
        let (epoch, cleanups) = {
            let mut state = self.state();
            state.epoch += 1;
            state.list_tools_cache = None;
            // A future start owns a new epoch; pending handshakes retain the
            // existing supersession contract and are not awaited here.
            state.connect = None;
            state.retire_connection();
            (
                state.epoch,
                state
                    .cleanups
                    .iter()
                    .filter(|cleanup| cleanup.wait_on_close)
                    .map(|cleanup| cleanup.done.clone())
                    .collect::<Vec<_>>(),
            )
        };
        // These are only cloned tickets. The tasks remain in State even if
        // this caller is cancelled, so a later close/dispose can wait again.
        for done in cleanups {
            await_cleanup(done).await;
        }
        let mut state = self.state();
        if state.epoch == epoch {
            state.status = McpStatus::Idle;
        }
    }

    /// Registry disposal also joins cleanup of already-timed-out handshakes.
    /// Keep the tasks in State while awaiting cloned completion tickets.
    pub(crate) async fn dispose(&self) {
        self.close().await;
        let cleanups = self
            .state()
            .cleanups
            .iter()
            .map(|cleanup| cleanup.done.clone())
            .collect::<Vec<_>>();
        for done in cleanups {
            await_cleanup(done).await;
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

/// SDK output validation's exact RPC error projection, shared with oracle replay.
pub(super) fn validate_structured_content(
    schema: &JsValue,
    data: &JsValue,
) -> Result<(), McpClientError> {
    use super::output_schema::ValidationError;
    let message = match super::output_schema::validate(schema, data) {
        Ok(None) => return Ok(()),
        Ok(Some(message)) => {
            format!("Structured content does not match the tool's output schema: {message}")
        }
        Err(ValidationError::Threw) => format!(
            "Failed to validate structured content: {}",
            CallStackExceeded::MESSAGE
        ),
        Err(ValidationError::Unsupported(message)) => {
            format!("Failed to validate structured content: {message}")
        }
    };
    Err(McpClientError::rpc(-32602.0, &message))
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
    fn mcp_session_tmpdir_skips_non_utf8_like_before() {
        use std::os::unix::ffi::OsStringExt;
        let env = |pairs: Vec<(&'static str, std::ffi::OsString)>| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.clone())
            }
        };
        let bad = std::ffi::OsString::from_vec(b"/tmp/\xff".to_vec());
        assert_eq!(
            mcp_session_tmpdir(env(vec![("TMPDIR", bad.clone()), ("TMP", "/x".into())])),
            "/x"
        );
        assert_eq!(mcp_session_tmpdir(env(vec![("TMPDIR", bad)])), "/tmp");
        assert_eq!(
            mcp_session_tmpdir(env(vec![("TMPDIR", "".into()), ("TEMP", "/t/".into())])),
            "/t"
        );
        assert_eq!(
            mcp_session_tmpdir(env(vec![("TMPDIR", "/a/b".into())])),
            "/a/b"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn close_leaves_pending_handshake_cleanup_for_dispose() {
        let client = McpClient::new(
            McpServerSpec {
                id: "unused".into(),
                transport: McpTransportSpec::Stdio {
                    command: "unused".into(),
                    args: None,
                    env: None,
                    cwd: None,
                    per_session: false,
                },
                call_timeout_ms: None,
            },
            McpClientOptions::default(),
        );
        let (gate, release) = oneshot::channel();
        let done = client.state().spawn_cleanup(
            async move {
                let _ = release.await;
            },
            false,
        );
        tokio::time::timeout(Duration::from_millis(1), client.close())
            .await
            .expect("ordinary close must not await the pending handshake");
        assert!(!*done.borrow());
        let mut dispose = Box::pin(client.dispose());
        tokio::select! {
            biased;
            () = &mut dispose => panic!("dispose must join the pending cleanup"),
            () = tokio::task::yield_now() => {}
        }
        // Finish close's own cleanup before polling again. Otherwise the
        // empty retired connection could hide a dispose that skips this gate.
        let closing = client
            .state()
            .cleanups
            .iter()
            .filter(|cleanup| cleanup.wait_on_close)
            .map(|cleanup| cleanup.done.clone())
            .collect::<Vec<_>>();
        for done in closing {
            await_cleanup(done).await;
        }
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(dispose.as_mut().poll(cx)))
                .await
                .is_pending(),
            "dispose must still await the gated pending handshake"
        );
        gate.send(()).unwrap();
        dispose.await;
        assert!(*done.borrow());
    }

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
    fn mkdir_recursive_empty_fails_like_node() {
        let error = mkdir_recursive("").unwrap_err();
        assert_eq!(
            error.message(),
            "ENOENT: no such file or directory, mkdir ''"
        );
    }

    #[test]
    fn temp_dirs_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("pdrs-mkdtemp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&base).unwrap();
        let dir = make_temp_dir(&format!("{}/p-", base.display())).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        std::fs::remove_dir_all(&base).unwrap();
        assert_eq!(mode, 0o700);
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
}
