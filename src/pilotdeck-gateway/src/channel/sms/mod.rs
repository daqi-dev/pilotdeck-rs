//! A8.3 sms channel: port of `src/adapters/channel/sms/SmsChannel.ts` on the
//! #1327 lifecycle. All external services are loopback mocks; the Twilio REST
//! boundary is an injected [`sender::TwilioFactory`].
//!
//! TS `start(deps)` reads `deps.gateway`; when this unit was written #1327's
//! `ChannelStartDeps` had no gateway field, so the A7 boundary is a constructor
//! option ([`SmsChannelOptions::gateway`]). #1333's api-server port later added
//! `turn_gateway` to the deps; the sms adapter still does not read it — the
//! difference is recorded in the #1339 diffs ledger (A8.3-CH-SMS-004).
pub mod elicitation;
pub mod gateway;
#[cfg(test)]
mod head_timeout_tests;
pub mod mapper;
pub mod permission;
pub mod render;
pub mod sender;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::task::{Context, Poll};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};

pub use elicitation::ImElicitationHelper;
pub use gateway::{
    ElicitationResponse, GatewayRef, PermissionDecision, PermissionDecisionOutcome, SmsGateway,
    SubmitTurnRequest, TurnEventStream,
};
pub use mapper::{MappedSession, SmsSessionMapper, SmsSessionMapperState};
pub use permission::{
    AnswerGuard, BeginAnswer, DecisionReply, ImPermissionHelper, InitialPromptGuard,
    PermissionAnswer, PermissionState, PromptGuard,
};
pub use sender::{SmsOutgoingMessage, SmsSender, TwilioFactory};

use super::protocol::{
    BoxFuture, ChannelAdapter, ChannelError, ChannelHandle, ChannelKey, ChannelStartDeps,
};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: f64 = 8790.0;
const DEFAULT_PATH: &str = "/sms/incoming";
const MAX_BODY_BYTES: usize = 1_048_576;
/// Node v22.22.0 `http.createServer`'s default `requestTimeout` (300 s): the
/// bound on receiving a whole request from the client. The TS sms channel
/// never overrides it (`SmsChannel.ts` has no `requestTimeout`/`setTimeout`),
/// so a client that declares `Content-Length` and stops sending is answered
/// with Node's 51-byte `408 Request Timeout` + `Connection: close` once this
/// deadline expires (#1901).
///
/// Like the webhook channel's copy, it bounds the **body read only**; the
/// request head has its own deadline, [`DEFAULT_HEADERS_TIMEOUT`]. A
/// whole-handler deadline would turn the TS `null`-body hang
/// (`A8.3-CH-SMS-007`) into a Rust divergence.
const DEFAULT_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
/// Node v22.22.0 `http.Server`'s default `headersTimeout` (60 s): the bound on
/// receiving a complete request head. TS `SmsChannel.ts` calls
/// `createServer(...)` with no options, so the default applies; the probe in
/// `_tmp/20261107-issue-1955/smoke/ts-half-head.out` reads it back from the
/// live server. Without a deadline a client that wrote half a request head
/// and stopped kept the detached connection task and the socket forever —
/// unauthenticated connection exhaustion (#1955); the sibling
/// `wecom_callback` lane installs hyper's
/// `.timer(TokioTimer::new()).header_read_timeout(..)` pair on the same
/// shape (`wecom_callback/http.rs`). #2499 (#2026 decision B) replaces that
/// pair here: hyper's answer to its own head timeout is a zero-byte close at
/// the deadline itself, while Node answers with the 51-byte `408 Request
/// Timeout` + `Connection: close` (measured, 89,995 ms) landing on the 30 s
/// connections-checker grid — so this lane now owns the read side through
/// [`HeadersTimeoutGate`], like the in-repo native lane
/// (`http_proxy::NODE_HEADERS_TIMEOUT_408`, its `grid_anchor`). The
/// bounded differences that remain (the deterministic strictly-after grid
/// landing; the idle keep-alive silent close at this 60 s deadline where
/// Node's `keepAliveTimeout` closes at 5–6 s) are registered in
/// `specs/gateway-parity/diffs/A8.3-CH-SMS-017.md`.
const DEFAULT_HEADERS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Node v22.22.0 `_http_server.js`: the connections checker runs on
/// `setInterval(checkConnections, 30_000)` — the default
/// `connectionsCheckingInterval` — with its phase anchored at the server's
/// `'listening'` event, and a `headersTimeout` 408 only ever lands on the
/// tick that first finds the deadline passed. Measured on the pinned Node:
/// a zero-byte head and a half-sent head are both answered at 90.004 s, a
/// first head delayed by 35 s at 120.008 s — the first checker point
/// strictly after the deadline, never at or before it
/// (`_tmp/20261107-issue-1955/smoke/ts-zero-and-delayed.out`). The native
/// lane replicates the same grid (`http_proxy::NODE_CONNECTIONS_CHECKING_INTERVAL`,
/// probes `probe-p12a.out` etc.); this lane keeps its own constant because
/// the two ledgers (A8.3-CH-SMS vs A8.4-NATIVE-HTTP) register their
/// residuals separately.
const NODE_CONNECTIONS_CHECKING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
/// #2026 (decision C): the bound on `SmsHandle::stop()`'s drain of the
/// connections the accept loop already handed out. The TS channel blocks
/// inside its `server.close()` callback until every connection ends — with a
/// half-sent request head that is *never*, because `httpServerPreClose`
/// clears the connections checker and the stalled head no longer retires
/// (`A8.3-CH-SMS-012-R1`: 199,001 ms unresolved, a right-censored lower
/// bound). This port drains like the sibling `wecom_callback` lane
/// (`wecom_callback/http.rs`: JoinSet + `join_next`), but wrapped in a
/// `tokio::time::timeout` so the wait cannot outlive this bound. 60 s is the
/// head deadline's own magnitude: a connection stalled while reading its
/// head always ends within it, and the drain clock starts at `stop()` —
/// strictly later than the connection's own accept — so a head-phase
/// connection always reaches its natural deadline. Anything longer (a
/// stalled body read at 300 s, a handler that never resolves) is truncated
/// at the bound: the task's `JoinSet` is dropped, which aborts the remaining
/// connection tasks and closes their sockets. The bounded-ization difference
/// is registered in `specs/gateway-parity/diffs/A8.3-CH-SMS-014.md`.
const STOP_DRAIN_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);
const TWIML_OK: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Response></Response>";

/// `process.env` read through an injected lookup (the TS start reads the real
/// one; tests pin per-case values so the host environment cannot leak in).
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<JsString> + Send + Sync>;
/// The TS `unhandledRejection` crash on a non-string JSON `Body` is contained
/// in the connection task; this hook observes it (the gold master records it).
pub type UnhandledRecorder = Arc<dyn Fn(&str) + Send + Sync>;

pub struct SmsChannelOptions {
    /// `options.extra` (the loader's `cfg.extra`); `None` is `{}`.
    pub extra: Option<JsObject>,
    /// `options.mapper`; `None` is a default-constructed mapper.
    pub mapper: Option<SmsSessionMapper>,
    /// The A7 gateway the TS start deps would have carried.
    pub gateway: Option<GatewayRef>,
    /// `process.env` replacement; `None` means no variable is ever set.
    pub env: Option<EnvLookup>,
    /// `require("twilio")`; `None` is the "twilio not installed" branch.
    pub twilio_factory: Option<TwilioFactory>,
    pub record_unhandled: Option<UnhandledRecorder>,
    /// The body-read deadline, [`DEFAULT_REQUEST_TIMEOUT`] when unset. TS
    /// exposes no such option (it is Node's server default, not a channel
    /// option); the field is the Rust tests' injection point, like the
    /// webhook channel's `request_timeout`. Tests inject a short deadline;
    /// zero or sub-millisecond values are not meaningful (tokio polls the
    /// inner future first, so whether a whole body is served or 408'd would
    /// hinge on that first poll) and no caller passes one.
    ///
    /// #1955: the same injection also shortens the request-head deadline
    /// ([`DEFAULT_HEADERS_TIMEOUT`]) — the two Node server defaults are
    /// injected together so the head bound is testable without a second
    /// option field (adding one would touch every `SmsChannelOptions`
    /// literal). Production never sets this, so the heads still get Node's
    /// 60 s and the bodies Node's 300 s. A consequence worth knowing when
    /// reading the #1901 body-read replays: their injected value also caps the
    /// head, so a client that wrote a *complete* head still has to get it onto
    /// the wire inside it. Those replays send the head and the body in one
    /// write, so the head is complete on the first poll; a future case that
    /// deliberately delays its head needs the split (`A8.3-CH-SMS-012`).
    /// `Some(Duration::ZERO)` is not meaningful either: hyper reports the head
    /// timeout on the first pending poll, so such a channel serves almost
    /// nothing.
    pub request_timeout: Option<std::time::Duration>,
}

pub struct CronResultDelivery {
    pub session_key: Option<JsString>,
    pub origin_session_key: Option<JsString>,
    pub text: JsString,
    pub origin_channel_key: Option<JsString>,
}

// ---------------------------------------------------------------------------
// JS value helpers shared by the sms submodules
// ---------------------------------------------------------------------------

/// ECMAScript truthiness.
pub(crate) fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(flag) => *flag,
        JsValue::Number(number) => *number != 0.0 && !number.is_nan(),
        JsValue::String(text) => !text.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

pub(crate) fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}

/// `String(value)` (JS string coercion, UTF-16 units preserved). The
/// stack-overflow branch of the shared converter becomes TS's RangeError.
pub(crate) fn js_string_of(value: &JsValue) -> JsString {
    match crate::js_value::to_string_units(value) {
        Ok(units) => JsString::from_utf16(units),
        Err(crate::js_value::StackOverflow) => {
            panic!("RangeError: {}", crate::js_value::StackOverflow::MESSAGE)
        }
    }
}

/// `String.prototype.trim` on UTF-16 units (all ECMAScript whitespace is BMP).
pub(crate) fn trim_units(units: &[u16]) -> &[u16] {
    fn space(unit: u16) -> bool {
        matches!(
            unit,
            0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f
                | 0x3000 | 0xfeff
        )
    }
    let mut start = 0;
    let mut end = units.len();
    while start < end && space(units[start]) {
        start += 1;
    }
    while end > start && space(units[end - 1]) {
        end -= 1;
    }
    &units[start..end]
}

pub(crate) fn concat_units(parts: &[&[u16]]) -> JsString {
    let mut units = Vec::new();
    for part in parts {
        units.extend_from_slice(part);
    }
    JsString::from_utf16(units)
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `[...].join("\n")` on UTF-16 units (shared by the prompt and render text).
pub(crate) fn join_lines(lines: &[JsString]) -> Vec<u16> {
    let mut units = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        units.extend_from_slice(line.units());
    }
    units
}

// ---------------------------------------------------------------------------
// channel state
// ---------------------------------------------------------------------------

struct SmsRuntime {
    mapper: SmsSessionMapper,
    elicitation: ImElicitationHelper,
    active_chats: Arc<StdMutex<HashSet<JsString>>>,
    client: Option<Arc<dyn SmsSender>>,
    from_number: JsString,
    auth_token: JsString,
    host: JsString,
    port: f64,
    path: JsString,
    public_url: JsString,
}

#[derive(Clone)]
struct Boundaries {
    logger: Option<super::protocol::ChannelLog>,
    gateway: Option<GatewayRef>,
    record_unhandled: Option<UnhandledRecorder>,
}

struct SmsShared {
    boundaries: StdMutex<Boundaries>,
    runtime: Mutex<SmsRuntime>,
    /// #1790: the permission state machine lives behind its own `std` mutex so
    /// the cross-`await` guards can release it on panic/cancel without the
    /// async runtime lock (which a `Drop` cannot take). Every access is a
    /// short synchronous section, never held across an `await`.
    permissions: PermissionState,
    /// Node's server `requestTimeout`, applied to the webhook body read
    /// (#1901). Immutable for the channel's lifetime, so no lock.
    request_timeout: std::time::Duration,
    /// Node's server `headersTimeout`, applied to the request-head read
    /// (#1955). Immutable for the channel's lifetime, so no lock.
    headers_timeout: std::time::Duration,
    /// Connection tasks of **this** channel that have not finished,
    /// test-only bookkeeping: the drain cases wait on it until the accept
    /// loop has handed a connection to its task, so a `stop()` issued before
    /// the hand-off cannot measure the no-op window and call it a drain.
    /// Production builds neither declare the field nor pay for the counter.
    #[cfg(test)]
    live_connections: Arc<std::sync::atomic::AtomicUsize>,
    /// #2499: the connections-checker grid's interval that the head deadline's
    /// 408 lands on — Node's 30 s in production. Test-only injection: the
    /// wall-clock cases and the negative-control three-phase runs shrink it so
    /// a landing is observable without half a minute of wait per case. Behind
    /// a mutex because the tests set it through their `Arc<SmsShared>` clone
    /// between `SmsChannel::new` and `start`, when no connection task exists
    /// yet; the accept loop reads it once before its spawn. The paused-clock
    /// cases instead drive `serve_sms_connection` directly with an explicit
    /// interval.
    #[cfg(test)]
    test_grid_interval: StdMutex<std::time::Duration>,
}

/// Keeps one count in the channel's own `live_connections` for the lifetime of
/// a detached connection task. Taken in the accept loop **before**
/// `tokio::spawn`, so a task that has been spawned but not polled yet is
/// already counted: `stop()` must not observe zero while such a task is still
/// queued. Per channel, not process-wide, so a parallel unit test's connection
/// cannot satisfy another test's reading (#2035).
#[cfg(test)]
struct LiveConnection(Arc<std::sync::atomic::AtomicUsize>);

#[cfg(test)]
impl LiveConnection {
    fn track(counter: &Arc<std::sync::atomic::AtomicUsize>) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(counter.clone())
    }
}

#[cfg(test)]
impl Drop for LiveConnection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

impl SmsShared {
    /// Lock the permission state, recovering a poisoned mutex: a `Drop` guard
    /// may run while a panic unwinds, and re-raising there would abort.
    fn lock_permissions(&self) -> std::sync::MutexGuard<'_, ImPermissionHelper> {
        self.permissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl SmsShared {
    fn log(&self, level: &str, message: String) {
        if crate::mutants::on("sms-log-under-lock") {
            // The pre-fix shape kept for the negative control: the callback
            // runs while the guard is alive, so a panicking logger poisons the
            // mutex for every later call.
            if let Some(logger) = self
                .boundaries
                .lock()
                .expect("#1339 boundaries mutex")
                .logger
                .clone()
            {
                logger(level, &message);
            }
            return;
        }
        // #1339 fix-r1: the injected callback runs after the guard is
        // released (`channel/server.rs` runs its callbacks outside both
        // locks); the `let` binding ends the guard before the call.
        let logger = self
            .boundaries
            .lock()
            .expect("#1339 boundaries mutex")
            .logger
            .clone();
        if let Some(logger) = logger {
            logger(level, &message);
        }
    }
}

struct ServerEntry {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

struct SmsHandle {
    shared: Arc<SmsShared>,
    server: StdMutex<Option<ServerEntry>>,
}

impl ChannelHandle for SmsHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        let shared = self.shared.clone();
        let reason = reason.to_owned();
        let server = self.server.lock().expect("#1339 stop mutex").take();
        Box::pin(async move {
            // The TS default `reason ?? "no reason"` cannot be expressed over
            // the #1327 `&str` port; the harness passes "no reason" for an
            // absent TS reason (diffs ledger).
            let logged = if crate::mutants::on("sms-stop-log-reason") {
                "no reason".to_owned()
            } else {
                reason
            };
            shared.log("info", format!("sms: stopping ({logged})"));
            if let Some(server) = server {
                let _ = server.shutdown.send(true);
                // #2026 (decision C): `server.task` does not only run the
                // accept loop — it also drains the connections it handed out,
                // bounded by [`STOP_DRAIN_LIMIT`]. Awaiting it here is what
                // makes this `stop()` wait for the drain, and the client
                // clearing below therefore lands *after* it, like the TS
                // channel's `server.close()` callback ordering
                // (`A8.3-CH-SMS-013-R1`).
                let _ = server.task.await;
            }
            shared.runtime.lock().await.client = None;
            Ok(())
        })
    }
}

/// The TS failure returns `{ stop: async () => undefined }` — a silent handle.
struct NoopHandle;
impl ChannelHandle for NoopHandle {
    fn stop(&self, _reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}

// ---------------------------------------------------------------------------
// adapter
// ---------------------------------------------------------------------------

/// The constructor inputs `start` consumes (kept separately from the runtime
/// because the mapper moves into the runtime at construction).
struct SmsStartInputs {
    extra: Option<JsObject>,
    env: Option<EnvLookup>,
    factory: Option<TwilioFactory>,
}

pub struct SmsChannel {
    key: ChannelKey,
    shared: Arc<SmsShared>,
    inputs: StdMutex<SmsStartInputs>,
}

impl SmsChannel {
    pub fn new(options: SmsChannelOptions) -> Self {
        let SmsChannelOptions {
            extra,
            mapper,
            gateway,
            env,
            twilio_factory,
            record_unhandled,
            request_timeout,
        } = options;
        Self {
            key: ChannelKey::new("sms"),
            shared: Arc::new(SmsShared {
                boundaries: StdMutex::new(Boundaries {
                    logger: None,
                    gateway,
                    record_unhandled,
                }),
                runtime: Mutex::new(SmsRuntime {
                    mapper: mapper.unwrap_or_default(),
                    elicitation: ImElicitationHelper::default(),
                    active_chats: Arc::new(StdMutex::new(HashSet::new())),
                    client: None,
                    from_number: JsString::from(""),
                    auth_token: JsString::from(""),
                    host: JsString::from(DEFAULT_HOST),
                    port: DEFAULT_PORT,
                    path: JsString::from(DEFAULT_PATH),
                    public_url: JsString::from(""),
                }),
                permissions: Arc::new(StdMutex::new(ImPermissionHelper::default())),
                request_timeout: request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
                // #1955: one injection knob for both Node server defaults; see
                // `SmsChannelOptions::request_timeout`.
                headers_timeout: request_timeout.unwrap_or(DEFAULT_HEADERS_TIMEOUT),
                #[cfg(test)]
                live_connections: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                #[cfg(test)]
                test_grid_interval: StdMutex::new(NODE_CONNECTIONS_CHECKING_INTERVAL),
            }),
            inputs: StdMutex::new(SmsStartInputs {
                extra,
                env,
                factory: twilio_factory,
            }),
        }
    }

    /// `deliverCronResult` (`ImCronDelivery.ts`): the origin session key must
    /// belong to this channel, and the chat id is parsed out of it.
    pub async fn deliver_cron_result(
        &self,
        delivery: &CronResultDelivery,
    ) -> Result<bool, ChannelError> {
        if let Some(origin) = &delivery.origin_channel_key
            && origin != self.key.as_js()
        {
            return Ok(false);
        }
        let session_key = delivery
            .origin_session_key
            .clone()
            .or_else(|| delivery.session_key.clone());
        let Some(session_key) = session_key else {
            return Ok(false);
        };
        let Some(chat_id) = parse_chat_id_from_session_key(&session_key, &self.key) else {
            return Ok(false);
        };
        // `(await sendText(chatId, text)) !== false`
        Ok(send_reply(&self.shared, &chat_id, delivery.text.clone()).await)
    }
}

impl ChannelAdapter for SmsChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }

    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let shared = self.shared.clone();
        let inputs = {
            let guard = self.inputs.lock().expect("#1339 inputs mutex");
            (
                guard.extra.clone(),
                guard.env.clone(),
                guard.factory.clone(),
            )
        };
        Box::pin(async move {
            if let Some(logger) = deps.log {
                shared
                    .boundaries
                    .lock()
                    .expect("#1339 boundaries mutex")
                    .logger = Some(logger);
            }
            let (extra, env, factory) = inputs;
            let Some(factory) = factory else {
                shared.log(
                    "error",
                    "sms: twilio not installed; run `npm install twilio`".to_owned(),
                );
                return Ok(Arc::new(NoopHandle) as Arc<dyn ChannelHandle>);
            };
            let field = |name: &str| {
                extra
                    .as_ref()
                    .and_then(|object| object.get(name))
                    .cloned()
                    .unwrap_or(JsValue::Undefined)
            };
            let env_value = |name: &str| {
                env.as_ref()
                    .and_then(|lookup| lookup(name))
                    .map(JsValue::String)
                    .unwrap_or(JsValue::Undefined)
            };
            // `String(a ?? b ?? "")` — the chain resolves, then one coercion.
            let coalesce = |values: Vec<JsValue>| {
                values
                    .into_iter()
                    .find(|value| !nullish(value))
                    .unwrap_or(JsValue::String(JsString::from("")))
            };
            let account_sid = js_string_of(&coalesce(vec![
                field("accountSid"),
                env_value("TWILIO_ACCOUNT_SID"),
            ]));
            let auth_token = js_string_of(&coalesce(vec![
                field("authToken"),
                field("apiKey"),
                env_value("TWILIO_AUTH_TOKEN"),
                field("token"),
            ]));
            let from_number = js_string_of(&coalesce(vec![
                field("phoneNumber"),
                env_value("TWILIO_PHONE_NUMBER"),
            ]));
            let host = js_string_of(&coalesce(vec![
                field("webhookHost"),
                JsValue::from(DEFAULT_HOST),
            ]));
            let port = crate::js_value::to_number(&coalesce(vec![
                field("webhookPort"),
                env_value("TWILIO_WEBHOOK_PORT"),
                JsValue::Number(DEFAULT_PORT),
            ]))
            .unwrap_or_default();
            let path = js_string_of(&coalesce(vec![
                field("webhookPath"),
                JsValue::from(DEFAULT_PATH),
            ]));
            let public_url = js_string_of(&coalesce(vec![field("publicUrl"), JsValue::from("")]));

            if account_sid.is_empty() || auth_token.is_empty() || from_number.is_empty() {
                shared.log(
                    "error",
                    "sms: missing config; need TWILIO_ACCOUNT_SID, TWILIO_AUTH_TOKEN, TWILIO_PHONE_NUMBER".to_owned(),
                );
                return Ok(Arc::new(NoopHandle) as Arc<dyn ChannelHandle>);
            }
            let client = match factory(&account_sid, &auth_token) {
                Ok(client) => client,
                Err(error) => {
                    shared.log(
                        "error",
                        format!("sms: twilio init failed: {}", error.to_js_string()),
                    );
                    return Ok(Arc::new(NoopHandle) as Arc<dyn ChannelHandle>);
                }
            };
            // The TS fields are already assigned here; a failed listen below
            // leaves the client set (cron delivery would still send).
            {
                let mut runtime = shared.runtime.lock().await;
                runtime.client = Some(client);
                runtime.from_number = from_number.clone();
                runtime.auth_token = auth_token.clone();
                runtime.host = host.clone();
                runtime.port = port;
                runtime.path = path.clone();
                runtime.public_url = public_url.clone();
            }

            // Node's `listen` rejects non-integer and out-of-range ports.
            let bad_port = port.is_nan() || port.fract() != 0.0 || !(0.0..=65535.0).contains(&port);
            let host_text = String::from_utf16_lossy(host.units());
            let listener = if bad_port {
                None
            } else {
                match bind_listener(&host_text, port as u16).await {
                    Ok(listener) => Some(listener),
                    Err(error) => {
                        shared.log(
                            "error",
                            format!(
                                "sms: HTTP server failed: {}",
                                listen_error(&error, &host_text, port)
                            ),
                        );
                        return Ok(Arc::new(NoopHandle) as Arc<dyn ChannelHandle>);
                    }
                }
            };
            if bad_port {
                shared.log(
                    "error",
                    format!(
                        "sms: HTTP server failed: RangeError [ERR_SOCKET_BAD_PORT]: options.port should be >= 0 and < 65536. Received type number ({}).",
                        crate::js_value::number_string(port)
                    ),
                );
                return Ok(Arc::new(NoopHandle) as Arc<dyn ChannelHandle>);
            }
            let listener = listener.expect("#1339 listener bound when the port is valid");
            // #2499: Node's connections checker anchors its 30 s grid at
            // the server's 'listening' event — the bind. Captured once,
            // right after the bind succeeded, so every connection's head
            // deadline lands on the same grid regardless of when it was
            // accepted (a per-connection alignment would drift the landing
            // by the accept's phase).
            let grid_anchor = tokio::time::Instant::now();
            let path_text = String::from_utf16_lossy(path.units());
            let public_text = String::from_utf16_lossy(public_url.units());
            let mut line = format!(
                "sms: Twilio webhook http://{host_text}:{}{path_text}",
                crate::js_value::number_string(port)
            );
            if !public_text.is_empty() {
                let trimmed = public_text.strip_suffix('/').unwrap_or(&public_text);
                line += &format!(" (configure Twilio URL: {trimmed}{path_text})");
            }
            shared.log("info", line);

            let (shutdown, mut shutdown_rx) = watch::channel(false);
            let server_shared = shared.clone();
            // #2499: the checker grid's interval — Node's 30 s in
            // production; the wall-clock tests inject a short one through
            // the `#[cfg(test)]` `SmsShared::test_grid_interval`.
            #[cfg(test)]
            let grid_interval = *shared
                .test_grid_interval
                .lock()
                .expect("#2499 the test grid interval is not poisoned");
            #[cfg(not(test))]
            let grid_interval = NODE_CONNECTIONS_CHECKING_INTERVAL;
            let task = tokio::spawn(async move {
                // #2026 (decision C): every connection this loop hands out
                // lands in this set, so the drain below can wait for the
                // in-flight ones — the `wecom_callback` lane's shape
                // (`wecom_callback/http.rs`).
                let mut connections = tokio::task::JoinSet::new();
                // #2257: left the loop through the closed shutdown channel.
                let mut sender_dropped = false;
                loop {
                    tokio::select! {
                        changed = shutdown_rx.changed() => match changed {
                            // A normal `stop()`: the `ServerEntry` holding the
                            // sender stays alive until its task is awaited, so
                            // the drain below runs while somebody observes it.
                            Ok(()) => break,
                            // #2257: every sender is gone. Two paths land here:
                            // a `stop()` whose async block panicked at its
                            // first log line (the #1339 logger-panic case —
                            // `server.take()` already ran, so the entry, the
                            // sender and the task handle drop together in the
                            // unwind), or a handle dropped without `stop()` at
                            // all. Nobody awaits this task, so the drain below
                            // would run unobserved — and it holds the
                            // listener's port for up to [`STOP_DRAIN_LIMIT`]
                            // (idle keep-alive connections wait out their head
                            // deadlines), so a same-port `start` fails its
                            // bind and silently degrades to a [`NoopHandle`].
                            // #2026's error-path release delay.
                            Err(_) => {
                                sender_dropped = true;
                                break;
                            }
                        },
                        accepted = listener.accept() => match accepted {
                            Ok((stream, _)) => {
                                let shared = server_shared.clone();
                                // Taken before the spawn, so a task that is
                                // queued but not polled yet is already counted.
                                #[cfg(test)]
                                let live = LiveConnection::track(&server_shared.live_connections);
                                // #2499: each connection gets its own stop
                                // receiver — the read gate disarms its head
                                // deadline when the channel shuts down
                                // (Node clears the checker in
                                // `httpServerPreClose`).
                                let stop = shutdown_rx.clone();
                                connections.spawn(async move {
                                    #[cfg(test)]
                                    let _live = live;
                                    serve_sms_connection(
                                        stream,
                                        shared,
                                        stop,
                                        grid_anchor,
                                        grid_interval,
                                    )
                                    .await;
                                });
                                // Finished connections are only reaped by
                                // `join_next`, so without this the set would
                                // grow with every webhook the process served.
                                while connections.try_join_next().is_some() {}
                            }
                            // #1339 fix-r1: a persistent accept error (fd
                            // exhaustion) must not spin the worker hot — the
                            // TS process would crash on the server 'error'
                            // event; the port backs off one tick instead.
                            Err(_) => {
                                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                                continue;
                            }
                        }
                    }
                }
                // #2026 (decision C): the drain. The TS channel blocks in
                // `server.close()` until every connection ends — unbounded,
                // and on a half-sent request head effectively forever
                // (`A8.3-CH-SMS-012-R1`). This port waits for the in-flight
                // connections, but no longer than [`STOP_DRAIN_LIMIT`]; when
                // the bound fires, the set this task still owns is dropped
                // and a dropped `JoinSet` **aborts** its remaining tasks, so
                // the channel's sockets are all closed once `stop()`
                // returns instead of outliving it. The mutation switches are
                // what prove both halves load-bearing: `sms-stop-no-drain`
                // restores the pre-#2026 detach shape (the set is forgotten,
                // so its aborting drop never runs),
                // `sms-stop-drain-unbounded` the TS unbounded shape, and
                // #2269's `sms-stop-drain-predrain-count` the r1 counting
                // shape (a pre-drain snapshot instead of the post-bound
                // read).
                if sender_dropped && !crate::mutants::on("sms-stop-drain-on-sender-drop") {
                    // #2257: the closed-channel path — skip the drain. The
                    // set's drop aborts the in-flight connection tasks and the
                    // listener is released with the task's return, so the port
                    // is free for a rebind immediately instead of after a
                    // drain nobody observes. The mutation switch restores the
                    // pre-#2257 shape (an unconditional drain on the Err path
                    // too), which the port-rebind case in
                    // `head_timeout_tests.rs` kills.
                } else if crate::mutants::on("sms-stop-no-drain") {
                    // The pre-#2026 shape: stop the accept loop and return
                    // with the connections still running. Forgetting the set
                    // leaks it, so the aborting `Drop` of a `JoinSet` never
                    // runs and the connection tasks stay detached, like the
                    // bare `tokio::spawn`s of the pre-#2026 code.
                    std::mem::forget(connections);
                } else {
                    // #2269: the r1 shape (`deab130c0`) read the count
                    // before the drain started, so every connection the
                    // drain retired was still counted — the bound case in
                    // `head_timeout_tests.rs` kills the mutant by its warn
                    // text (3 where the aborted set holds 2).
                    let pre_drain_count = crate::mutants::on("sms-stop-drain-predrain-count")
                        .then(|| connections.len());
                    let drain = async { while connections.join_next().await.is_some() {} };
                    if crate::mutants::on("sms-stop-drain-unbounded") {
                        drain.await;
                    } else if tokio::time::timeout(STOP_DRAIN_LIMIT, drain).await.is_err() {
                        // #2026 r2: read after the bound fired. Entries only
                        // leave the set through `join_next`, so this is the
                        // count the set's closing drop will abort — the
                        // connections that finished during the drain were
                        // reaped out of the set and are not counted. The
                        // residual inaccuracy is the acceptable side: a task
                        // that finished in the last poll before the timeout
                        // may still be counted (an over-report bounded by one
                        // poll window; dropping a finished task is a no-op),
                        // whereas the pre-drain snapshot r1 used counts every
                        // connection the whole drain retired. An
                        // under-report cannot happen — a live task stays in
                        // the set until it is reaped.
                        let remaining = pre_drain_count.unwrap_or_else(|| connections.len());
                        server_shared.log(
                            "warn",
                            format!(
                                "sms: stop drain exceeded the {STOP_DRAIN_LIMIT:?} bound; \
                                 {remaining} in-flight connection(s) aborted"
                            ),
                        );
                    }
                }
            });
            Ok(Arc::new(SmsHandle {
                shared,
                server: StdMutex::new(Some(ServerEntry { shutdown, task })),
            }) as Arc<dyn ChannelHandle>)
        })
    }
}

/// Node `listen(port, host)` with the empty host: `""` is falsy for Node's
/// host check, so it binds every address — the dual-stack `::` when the
/// platform accepts IPv4 on it, else `0.0.0.0` (probed on the pinned Node
/// v22.22.0: `address()` is `::` and both 127.0.0.1 and ::1 connect).
async fn bind_listener(host: &str, port: u16) -> std::io::Result<TcpListener> {
    if host.is_empty() {
        // #1339 fix-r2 (P2): `"[::]"` is not a host Rust resolves (the
        // bracket form only parses as a socket-address string, not through
        // `ToSocketAddrs`), so the dual-stack bind has to spell `"::"`.
        //
        // #1339 fix-r3 (P2): but `TcpListener::bind(("::", port))` honours
        // the host's `net.ipv6.bindv6only`, while Node's `listen` forces
        // `ipv6Only=false`. On a host that sets it the wildcard binds but
        // serves IPv6 only, a v4 bind to the same port still succeeds, and
        // `empty-webhook-host` fails as a false positive — so verify the
        // wildcard instead of relying on the default.
        if crate::mutants::on("sms-empty-host-bind-all") {
            // The switch makes the empty-host mapping fail outright. The
            // pre-fix shape bound the empty host, whose lookup fails on macOS
            // but resolves to 127.0.0.1 on Linux — a platform-dependent kill.
            // Returning the failure keeps the control's verdict the same on
            // every platform (binding `::1` instead degraded to the positive
            // run's `0.0.0.0` fallback on hosts without IPv6, where the
            // mutant SURVIVED).
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "#1339 sms-empty-host-bind-all",
            ));
        }
        if let Ok(listener) = bind_dual_stack(port).await {
            return Ok(listener);
        }
        return TcpListener::bind(("0.0.0.0", port)).await;
    }
    TcpListener::bind((host, port)).await
}

/// #1339 fix-r3 (P2): `::` is dual-stack only while the host keeps
/// `net.ipv6.bindv6only` at 0 — Rust std does not clear `IPV6_V6ONLY` the way
/// Node's `listen` does. Verify it instead of assuming: a dual-stack wildcard
/// already owns the IPv4 port, so a v4 bind to `127.0.0.1:<assigned port>`
/// collides. If it succeeds, the wildcard serves IPv6 only and the caller
/// falls back to `0.0.0.0` (a host-dependent difference recorded in
/// `A8.3-CH-SMS-008.md`).
async fn bind_dual_stack(port: u16) -> std::io::Result<TcpListener> {
    let listener = TcpListener::bind(("::", port)).await?;
    // #1339 fix-r4 (P1): probe the port the kernel actually assigned. With
    // `port == 0` the wildcard owns an ephemeral port, so probing
    // `127.0.0.1:0` would always succeed and every `webhookPort: 0` start
    // would be demoted to the IPv4-only fallback.
    let bound = listener.local_addr()?.port();
    match v4_port_state(bound) {
        // #1339 fix-r6 (P1): a probe that could not run says nothing about the
        // host. Keep the working wildcard instead of demoting it to IPv4-only
        // and silently dropping IPv6 service on a dual-stack host.
        V4Port::Shared | V4Port::Unknown => Ok(listener),
        V4Port::Unshared => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "#1339 `::` is IPv6-only on this host",
        )),
    }
}

/// What a bind to `127.0.0.1:<port>` learned about the wildcard above.
#[derive(Debug, PartialEq, Eq)]
enum V4Port {
    /// The bind collided: something already holds `127.0.0.1:<port>` — our own
    /// `::` listener, a third party, or a socket merely bound without ever
    /// listening. The probe cannot tell them apart, and says nothing about
    /// whether anyone *serves* IPv4 there (`A8.3-CH-SMS-008-R4.md`).
    Shared,
    /// The bind succeeded: the address is free for IPv4 at the moment of the
    /// probe.
    Unshared,
    /// The probe itself could not run; the host's posture is unknown.
    Unknown,
}

/// Probe `127.0.0.1:<port>` without any socket options.
///
/// #1339 fix-r5 (P1): the probe must not go through `TcpListener::bind`. Rust
/// std sets `SO_REUSEADDR` on listeners, and on macOS/BSD a socket that sets it
/// may bind a specific address while a wildcard owns the port — so
/// `TcpListener::bind(("127.0.0.1", port))` succeeds on a *dual-stack* macOS
/// host and the caller always fell back to `0.0.0.0` (measured: a v4 socket
/// with `SO_REUSEADDR` binds, without it gets `EADDRINUSE`). `TcpSocket` sets no
/// socket options, so the collision it reports is real.
///
/// #1339 fix-r6 (P1): only `AddrInUse` means "shared". Any other failure (no
/// IPv4 stack, descriptor exhaustion, ...) is reported as [`V4Port::Unknown`]
/// rather than being read as evidence about the host. The probe cannot tell our
/// wildcard apart from a third party holding the v4 port; that misjudgement is
/// registered in `A8.3-CH-SMS-008-R2.md`.
fn v4_port_state(port: u16) -> V4Port {
    let address = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let Ok(socket) = tokio::net::TcpSocket::new_v4() else {
        return V4Port::Unknown;
    };
    classify_v4_probe(&socket.bind(address))
}

/// What the `127.0.0.1:<port>` bind result says about the `::` wildcard above.
/// A pure function on purpose (#1589 issue #6): the `Err(_) => Unknown` arm a
/// healthy host never reaches has an observable injection point here, pinned by
/// `classify_v4_probe_other_errors_stay_unknown`.
fn classify_v4_probe(result: &std::io::Result<()>) -> V4Port {
    match result {
        Ok(()) => V4Port::Unshared,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => V4Port::Shared,
        Err(_) => {
            if crate::mutants::on("sms-v4-probe-unknown-as-unshared") {
                // The fix-r5 shape: any failure that is not `EADDRINUSE` was
                // read as a free port, demoting a working wildcard to the
                // IPv4-only fallback.
                V4Port::Unshared
            } else {
                V4Port::Unknown
            }
        }
    }
}

/// Node's `Error: listen <CODE>: <message> <host>:<port>` line for the bind
/// failure (probed on the pinned Node v22.22.0 for EADDRINUSE,
/// EADDRNOTAVAIL and EACCES).
fn listen_error(error: &std::io::Error, host: &str, port: f64) -> String {
    let port_text = crate::js_value::number_string(port);
    if crate::mutants::on("sms-listen-error-node-text") {
        return format!("Error: listen {error}: {host}:{port_text}");
    }
    match error.kind() {
        std::io::ErrorKind::AddrInUse => {
            format!("Error: listen EADDRINUSE: address already in use {host}:{port_text}")
        }
        std::io::ErrorKind::AddrNotAvailable => {
            format!("Error: listen EADDRNOTAVAIL: address not available {host}:{port_text}")
        }
        std::io::ErrorKind::PermissionDenied => {
            format!("Error: listen EACCES: permission denied {host}:{port_text}")
        }
        _ => format!("Error: listen {error}: {host}:{port_text}"),
    }
}

// ---------------------------------------------------------------------------
// webhook
// ---------------------------------------------------------------------------

/// A request handling failure that destroys the connection without any
/// response byte, like the TS `req.destroy()` payload guard.
#[derive(Debug)]
struct DestroyConnection;
impl std::fmt::Display for DestroyConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("payload too large")
    }
}
impl std::error::Error for DestroyConnection {}

fn plain_response(status: u16, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
        .expect("#1339 static response parts")
}

/// [`plain_response`] / [`twiml_response`] carry a known length, so boxing
/// them keeps Node's `content-length` framing (unlike
/// `native_http::streamed`, which hyper frames chunked).
fn sized(response: Response<Full<Bytes>>) -> Response<crate::Body> {
    response.map(|body| {
        body.map_err(|never: std::convert::Infallible| match never {})
            .boxed()
    })
}

fn twiml_response() -> Response<Full<Bytes>> {
    let body = if crate::mutants::on("sms-twiml-body") {
        "<Response/>"
    } else {
        TWIML_OK
    };
    Response::builder()
        .status(200)
        .header("content-type", "text/xml")
        .body(Full::new(Bytes::copy_from_slice(body.as_bytes())))
        .expect("#1339 static response parts")
}

/// Node answers every HTTP/1.0 request with an `HTTP/1.1` status line,
/// `Connection: close` and a close-delimited body (no `content-length`), even
/// when the request asks to keep alive — measured on the pinned Node v22.22.0
/// (`_tmp/20261108-sms-residual/probe-sms-http10.out`, the #1913 probe), the
/// same shape the webhook lane measured for its own handler in #1684. Pinning
/// the response version to `HTTP_10` keeps hyper's `fix_keep_alive` in its
/// HTTP/1.0 branch, so the socket closes instead of staying open waiting out
/// the keep-alive the request carried; hyper's `HTTP/1.0` status line against
/// Node's `HTTP/1.1` is the accepted-diff entry A8.3-CH-SMS-015, the same
/// class as the webhook lane's A8.3-CH-WEBHOOK-004. The body is also made
/// unsized: this channel answers with known-length bodies ([`sized`]), which
/// hyper frames with `content-length` on any version, while Node's 1.0
/// replies are close-delimited — with an unknown size hyper cannot chunk a
/// 1.0 connection, so it writes the body and closes, matching Node's bytes.
fn finalize(mut response: Response<crate::Body>, version: hyper::Version) -> Response<crate::Body> {
    if version != hyper::Version::HTTP_10 {
        return response;
    }
    *response.version_mut() = hyper::Version::HTTP_10;
    let body = std::mem::replace(response.body_mut(), crate::native_http::streamed(None));
    *response.body_mut() = Unsized(body).boxed();
    response
}

/// A body whose size hyper cannot know (#1913): everything else delegates, so
/// the frames and the end-of-stream flag are unchanged.
struct Unsized(crate::Body);

impl hyper::body::Body for Unsized {
    type Data = Bytes;
    type Error = crate::ProxyError;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        std::pin::Pin::new(&mut self.get_mut().0).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        hyper::body::SizeHint::default()
    }
}

// ---------------------------------------------------------------------------
// #2499: the request-head deadline's read gate (Node's headersTimeout 408
// and its 30 s connections-checker grid landing), per #2026 decision B.
// ---------------------------------------------------------------------------

/// #2499: the instant the sms lane's head-deadline 408 fires — the first
/// connections-checker grid point strictly *after* the deadline
/// ([`NODE_CONNECTIONS_CHECKING_INTERVAL`] ticks from the listener's bind,
/// [`HeadersTimeoutGate::grid_anchor`]). The checker's comparison is
/// strictly greater (`ConnectionsList::Expired` expires when
/// `last_message_start_ < now - timeout`, node_http_parser v22.22.0
/// disassembly at 0xda3db0: `b.cs` skips the expiry on equality — the same
/// recomputation the native lane keeps in `http_proxy.rs`
/// `headers_deadline_landing`), so a deadline that lands exactly on a grid
/// point survives that tick and fires at the next one: 整除进一.
fn headers_deadline_landing(
    deadline: tokio::time::Instant,
    anchor: tokio::time::Instant,
    interval: std::time::Duration,
) -> tokio::time::Instant {
    if crate::mutants::on("sms-headers-grid-off") {
        // The pre-#2499 behavior the negative control kills: firing at the
        // deadline itself, up to one checker interval before Node's next
        // tick would.
        return deadline;
    }
    let elapsed = deadline.saturating_duration_since(anchor).as_nanos();
    let ticks = if crate::mutants::on("sms-headers-grid-inclusive") {
        // The inclusive-grid shape the negative control kills: an on-grid
        // deadline firing at its own grid point, one checker interval
        // before Node's strictly-greater comparison lets it.
        elapsed.div_ceil(interval.as_nanos())
    } else {
        elapsed / interval.as_nanos() + 1
    };
    let ticks = u32::try_from(ticks).expect("#2499 the checker grid stays within u32 ticks");
    anchor + interval * ticks
}

/// #2499: upper bound on the bytes a next request head may park in
/// [`HeadersTimeoutGate::withheld_head`] while the previous response still
/// streams — the native lane's `WITHHELD_HEAD_CAP` shape: once such bytes
/// reach hyper's read buffer its mid-message read parks on the non-empty
/// buffer without polling io again, so the gate (and the parked head
/// deadline) would never be polled again and the connection would hang.
/// 32 KB is far above any real webhook head and small enough that no
/// client can pin an unbounded buffer for the length of a response.
const WITHHELD_HEAD_CAP: usize = 32 * 1024;

/// #2499: the request-head deadline's read gate — `http_proxy.rs`'s
/// `UpgradeReadGate` trimmed to this lane. It wraps the raw stream hyper
/// serves from and owns the head deadline instead of hyper's
/// `header_read_timeout` (that one closes silently at the deadline, before
/// the checker-grid landing, and Node's answer is the 51-byte 408,
/// `user_dialog::NODE_408_RESPONSE`). Per request head: the deadline arms at
/// the connection's start and again at every response finish (a standby
/// window), re-anchors at the awaited head's first byte (Node's
/// `on_message_begin` sets `last_message_start_`), and disarms the moment
/// the awaited head parsed (the [`Self::disarm_after`] watermark) or the
/// stop watch fired (Node clears the checker in `httpServerPreClose`). At
/// the landing the gate writes the 408 straight onto the socket and reports
/// EOF to hyper — except for a zero-byte **follow-up** head on a connection
/// that already served a response, which Node leaves to the 5 s
/// `keepAliveTimeout` silent close; this lane does not replicate that idle
/// close (the registered residual), so it keeps its own deadline but stays
/// byte-silent on that one path.
struct HeadersTimeoutGate<S> {
    stream: S,
    /// #2499: Node's `headersTimeout`, or `None` when the
    /// `sms-header-read-no-timeout` mutation removed the deadline — the
    /// switch's #2499 rewire of the pre-#2499 `.header_read_timeout(..)`
    /// builder arm.
    headers_timeout: Option<std::time::Duration>,
    /// #2499: the listener's bind instant — the phase anchor of Node's
    /// connections checker; `serve_sms_connection` captures it once right
    /// after the bind.
    grid_anchor: tokio::time::Instant,
    /// #2499: the checker grid's interval — Node's 30 s in production; the
    /// wall-clock tests inject a short one (`SmsShared::test_grid_interval`)
    /// so a landing is observable without half a minute of wait.
    grid_interval: std::time::Duration,
    /// #2499: heads this connection has parsed — the service closure bumps
    /// it per parsed head.
    served_heads: Arc<AtomicUsize>,
    /// #2499: response-finish signal, sent by each [`RearmBody`] completion
    /// with the finish instant. Node leaves no headers deadline between
    /// requests (`on_message_complete` clears `last_message_start_`); this
    /// lane arms at the finish and re-anchors at the next head's first byte.
    /// The instant matters because the bump is only a signal consumed at the
    /// gate's next poll — hyper parks its read once and never re-polls it
    /// until the parked sleep or the client's bytes wake the task, so the
    /// standby window would otherwise anchor at the consumption instant, one
    /// grid step past the finish it belongs to.
    rearm: watch::Receiver<Option<tokio::time::Instant>>,
    /// #2499: 1 while a parsed request's response has not finished.
    in_flight: Arc<AtomicUsize>,
    /// #2499: request-body bytes the connection still owes the handler
    /// (from the parsed request's `Content-Length`; a chunked body stays
    /// "open", `i64::MAX` — the native lane's sentinel). Only the bytes
    /// past them can start the *next* head: Node's
    /// `last_message_start_` is set at `on_message_begin`, not by body
    /// bytes.
    body_remaining: Arc<AtomicI64>,
    /// #2499: `served_heads` at deadline-arm time: the head that completes
    /// after an arming is the one that disarms it.
    disarm_after: usize,
    /// #2499: the awaited request head has received its first byte — the
    /// armed deadline is that head's own and later dribble segments never
    /// defer it.
    awaited_head_started: bool,
    /// #2499: hyper has written bytes for a response that has not finished
    /// — Node's `_headerSent` mirror: an expiry with the in-flight
    /// response's head on the wire destroys silently, no 408.
    response_head_written: bool,
    /// #2499: the channel's shutdown watch — a stop disarms the deadline and
    /// blocks re-arming. A dropped sender keeps its last value (`false`),
    /// which is correct for the #2257 closed-channel paths: their
    /// connections end through the abort, never through a 408.
    stop: watch::Receiver<bool>,
    /// #2499: when the currently awaited head's deadline fires; `None` once
    /// disarmed.
    headers_deadline: Option<tokio::time::Instant>,
    /// The parked deadline sleep, polled under the socket read's waker so a
    /// silent head still wakes at its landing.
    headers_sleep: Option<Pin<Box<tokio::time::Sleep>>>,
    /// #2499: Node's 408 bytes still to write, and how much of them was
    /// written; the read side reports EOF and hyper's own writes are refused
    /// once they are on the socket.
    timeout_408: Option<&'static [u8]>,
    timeout_408_written: usize,
    timeout_408_closed: bool,
    /// #2499: bytes of a next request head that arrived while the previous
    /// response was still streaming, parked gate-side instead of hyper's
    /// read buffer — see [`WITHHELD_HEAD_CAP`].
    withheld_head: Vec<u8>,
    /// #2499: peer EOF seen while withholding — parked with the bytes and
    /// reported to hyper only when they are, the same way hyper parks a
    /// mid-stream EOF behind a non-empty read buffer. A half-closed client
    /// still gets its response written first.
    withheld_eof: bool,
}

impl<S> HeadersTimeoutGate<S> {
    #[allow(clippy::too_many_arguments)] // the head deadline state machine's inputs
    fn new(
        stream: S,
        headers_timeout: Option<std::time::Duration>,
        grid_anchor: tokio::time::Instant,
        grid_interval: std::time::Duration,
        served_heads: Arc<AtomicUsize>,
        rearm: watch::Receiver<Option<tokio::time::Instant>>,
        in_flight: Arc<AtomicUsize>,
        body_remaining: Arc<AtomicI64>,
        stop: watch::Receiver<bool>,
    ) -> Self {
        // #2499: the connection-establishment anchor — Node arms
        // `last_message_start_` at the accept; the gate constructs at the
        // connection's first poll, right after it, and the awaited first
        // head's first byte re-anchors the deadline.
        let headers_deadline = headers_timeout.map(|timeout| {
            headers_deadline_landing(
                tokio::time::Instant::now() + timeout,
                grid_anchor,
                grid_interval,
            )
        });
        Self {
            stream,
            headers_timeout,
            grid_anchor,
            grid_interval,
            served_heads,
            rearm,
            in_flight,
            body_remaining,
            disarm_after: 0,
            awaited_head_started: false,
            response_head_written: false,
            stop,
            headers_sleep: headers_deadline
                .map(|deadline| Box::pin(tokio::time::sleep_until(deadline))),
            headers_deadline,
            timeout_408: None,
            timeout_408_written: 0,
            timeout_408_closed: false,
            withheld_head: Vec::new(),
            withheld_eof: false,
        }
    }

    /// #2499: arm the headers deadline for a request head — at each response
    /// finish (a standby window, `started == false`, whose first byte
    /// re-anchors at the true request start) and at a head's first byte
    /// (`started == true`: the deadline is that head's own). `from` is the
    /// window's anchor — the finish instant for a standby window (the bump
    /// is only consumed at this gate's next poll, which can sit later than
    /// the finish itself), now for a first byte.
    fn arm_headers_deadline(&mut self, started: bool, from: tokio::time::Instant) {
        let Some(timeout) = self.headers_timeout else {
            return;
        };
        if *self.stop.borrow_and_update() {
            // #2499: a stop disarms the deadline and blocks re-arming, like
            // Node clearing its connections checker in `server.close()`.
            return;
        }
        let landing =
            headers_deadline_landing(from + timeout, self.grid_anchor, self.grid_interval);
        self.headers_deadline = Some(landing);
        self.headers_sleep = Some(Box::pin(tokio::time::sleep_until(landing)));
        self.disarm_after = self.served_heads.load(Ordering::Relaxed);
        self.awaited_head_started = started;
    }

    /// #2499: a request head's first byte just arrived (not a body segment):
    /// arm the head's own deadline. Two shapes reach here — a standby
    /// window's awaited head, for the connection's *first* head too
    /// (measured: a first head delayed 35 s is answered at 120.008 s, not at
    /// connect + 60 s), and a head already arriving while its predecessor's
    /// response streams (the withheld path).
    fn arm_started_head(&mut self, cx: &mut Context<'_>, head_start: bool) {
        if !head_start || self.awaited_head_started {
            return;
        }
        if crate::mutants::on("sms-headers-first-byte-anchor-off") {
            // The pre-#2499 behavior the negative control kills: the
            // deadline never re-anchors at the head's first byte, so a
            // delayed first head stays timed from the connection's start.
            return;
        }
        let served = self.served_heads.load(Ordering::Relaxed) > self.disarm_after;
        let standby = self.headers_deadline.is_some();
        if standby || served {
            self.arm_headers_deadline(true, tokio::time::Instant::now());
            // Park the fresh deadline under this poll's waker straight away:
            // while the previous response streams, this gate's own poll is
            // the only place the deadline will ever be consulted.
            if let Some(sleep) = self.headers_sleep.as_mut() {
                let _ = sleep.as_mut().poll(cx);
            }
        }
    }

    /// #2499: count `read` bytes against the awaited request's body, so the
    /// reader can tell where the body ends and the next head starts.
    fn consume_request_body(&self, read: usize) {
        let remaining = self.body_remaining.load(Ordering::Relaxed);
        if remaining > 0 {
            self.body_remaining
                .fetch_sub(std::cmp::min(remaining, read as i64), Ordering::Relaxed);
        }
    }

    /// #2499: a request is in flight whose body is fully read, so a read now
    /// can only serve the *next* request's head — bytes hyper cannot parse
    /// until the response finishes, parked gate-side instead (see
    /// [`WITHHELD_HEAD_CAP`]).
    fn withholding_mid_stream(&self) -> bool {
        self.headers_timeout.is_some()
            && self.in_flight.load(Ordering::Relaxed) > 0
            && self.body_remaining.load(Ordering::Relaxed) <= 0
    }
}

impl<S: AsyncWrite + Unpin> HeadersTimeoutGate<S> {
    /// Write Node's 408 on the task that owns the socket, then report EOF.
    /// The bytes stay in the kernel buffer exactly like Node's
    /// `socket.write(response)` followed by `destroy()`.
    fn poll_headers_timeout_408(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        while let Some(raw) = self.timeout_408 {
            match Pin::new(&mut self.stream).poll_write(cx, &raw[self.timeout_408_written..]) {
                Poll::Ready(Ok(0)) => break,
                Poll::Ready(Ok(written)) => {
                    self.timeout_408_written += written;
                    if self.timeout_408_written == raw.len() {
                        break;
                    }
                }
                Poll::Ready(Err(_)) => break,
                // `poll_write` registered the write waker; self-waking here
                // would spin the executor until the socket drains.
                Poll::Pending => return Poll::Pending,
            }
        }
        self.timeout_408 = None;
        self.timeout_408_closed = true;
        Poll::Ready(Ok(()))
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> AsyncRead for HeadersTimeoutGate<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if self.timeout_408_closed {
            return Poll::Ready(Ok(()));
        }
        if self.timeout_408.is_some() {
            return self.poll_headers_timeout_408(cx);
        }
        // #2499: no in-flight response means no response head is on the
        // wire — an expiry from here on writes the 408 (Node's
        // `socketOnError` answers while no in-flight `ServerResponse` has
        // sent its head); while one is streaming, poll_write has set the
        // flag and the expiry closes silently instead.
        if self.in_flight.load(Ordering::Relaxed) == 0 {
            self.response_head_written = false;
        }
        // #2499: each response finish re-arms the deadline for the next
        // request head. The bump is only a signal — the instant is read
        // here — so a bump consumed late still lands after every reachable
        // 408. A head already mid-arrival keeps its own deadline: the
        // response ending does not move Node's `last_message_start_`.
        // `served` splits that head's two shapes: one whose bytes are
        // parked gate-side (the withheld path — or still unread) has not
        // moved the watermark, so its first byte's deadline rides out the
        // response; one that already parsed has left
        // `awaited_head_started` stale (the disarm below would have
        // cleared it, but hyper parks on the service, not the io, between
        // the parse and the response's end), and this lane owns the idle
        // close hyper's `header_read_timeout` used to make — the standby
        // window this arms is that close (Node's own idle close is the
        // A8.3-CH-SMS-017 residual, 5–6 s there).
        if self.rearm.has_changed().unwrap_or(false) {
            let finished = *self.rearm.borrow_and_update();
            let started_head_unserved = self.awaited_head_started
                && self.served_heads.load(Ordering::Relaxed) <= self.disarm_after;
            if let Some(finished) = finished
                && !started_head_unserved
            {
                self.arm_headers_deadline(false, finished);
            }
        }
        if let Some(deadline) = self.headers_deadline {
            // #2499: the armed deadline belongs to one specific head.
            // `served`: that head parsed — the disarm watermark passing —
            // so the deadline disarms and the next read starts the next
            // head's own window. `busy`: a standby window (the anchored head
            // has no bytes yet) while another request is still in flight —
            // a pipelined head that parsed before the response-finish re-arm
            // was consumed. A *started* head's deadline rides out the
            // previous response's stream: Node's checker stays active
            // through it.
            let served = self.served_heads.load(Ordering::Relaxed) > self.disarm_after;
            let busy = self.in_flight.load(Ordering::Relaxed) > 0 && !self.awaited_head_started;
            if (*self.stop.borrow_and_update() && !crate::mutants::on("sms-headers-stop-no-disarm"))
                || (served || busy)
            {
                self.headers_deadline = None;
                self.awaited_head_started = false;
            } else {
                let sleep = self
                    .headers_sleep
                    .get_or_insert_with(|| Box::pin(tokio::time::sleep_until(deadline)));
                let _ = sleep.as_mut().poll(cx);
                if tokio::time::Instant::now() >= deadline {
                    // #2499: the three expiries. A head that has received
                    // bytes (a half-sent head, or a follow-up head that
                    // started), and a zero-byte **first** head, are answered
                    // with Node's 51-byte 408 (measured: 89,995 ms and
                    // 90,004 ms). A zero-byte follow-up head on a connection
                    // that already served a response is Node's
                    // `keepAliveTimeout` silent close — not replicated here
                    // (the registered residual), so it stays byte-silent.
                    // An in-flight response that already wrote its head
                    // destroys silently (`response_head_written` above).
                    let follow_up_idle =
                        !self.awaited_head_started && self.served_heads.load(Ordering::Relaxed) > 0;
                    if !self.response_head_written
                        && (!follow_up_idle || crate::mutants::on("sms-headers-idle-followup-408"))
                        && !crate::mutants::on("sms-headers-408-bytes-off")
                    {
                        self.timeout_408 = Some(crate::user_dialog::NODE_408_RESPONSE);
                        self.timeout_408_written = 0;
                        return self.poll_headers_timeout_408(cx);
                    }
                    self.timeout_408_closed = true;
                    return Poll::Ready(Ok(()));
                }
            }
        }
        // #2499: a next head's bytes arriving while the response streams —
        // parked gate-side (hyper would stop polling io once they reach its
        // read buffer, stranding the parked deadline without a poll to fire
        // in), anchored at their first byte, and re-fed once the response
        // finishes.
        if self.withholding_mid_stream() {
            let mut scratch = [0u8; 4096];
            let mut arrived = 0usize;
            loop {
                if self.withheld_head.len() >= WITHHELD_HEAD_CAP {
                    break;
                }
                let mut read_buf = ReadBuf::new(&mut scratch);
                match Pin::new(&mut self.stream).poll_read(cx, &mut read_buf) {
                    Poll::Ready(Ok(())) => {
                        let n = read_buf.filled().len();
                        if n == 0 {
                            // Peer EOF parks with the bytes: hyper today
                            // parks a mid-stream EOF behind a non-empty read
                            // buffer the same way, and a half-closed client
                            // still gets its response.
                            self.withheld_eof = true;
                            break;
                        }
                        self.withheld_head.extend_from_slice(&scratch[..n]);
                        arrived += n;
                    }
                    Poll::Ready(Err(error)) => return Poll::Ready(Err(error)),
                    Poll::Pending => break,
                }
            }
            if arrived > 0 {
                // The withheld bytes are all next-head bytes (the request's
                // own body is fully read), so the first of them is the
                // head's first byte — Node's `on_message_begin`.
                self.arm_started_head(cx, true);
            }
            if self.withheld_head.len() >= WITHHELD_HEAD_CAP {
                // At the cap the gate no longer polls the socket, so
                // nothing else would register a waker for the parked head
                // deadline — poll the sleep here.
                if let Some(sleep) = self.headers_sleep.as_mut() {
                    let _ = sleep.as_mut().poll(cx);
                }
            }
            return Poll::Pending;
        }
        // The response finished: hand the withheld head bytes to hyper's
        // parse path, one buffer at a time — the parse loop re-polls for
        // the rest.
        if !self.withheld_head.is_empty() {
            let n = std::cmp::min(buf.remaining(), self.withheld_head.len());
            buf.put_slice(&self.withheld_head[..n]);
            if n == self.withheld_head.len() {
                self.withheld_head.clear();
            } else {
                self.withheld_head.drain(..n);
            }
            // #2499 fix-r1 (interleave B): a request pipelined whole — head
            // and body — while the previous response streamed parks both in
            // the withheld buffer. The head's first chunk re-feeds before the
            // service stores the new `Content-Length` (hyper parses that
            // chunk first), but the *body* chunks re-feed after it, and
            // without this count they never pass the fresh-read accounting
            // below — the stored length lingers and suppresses the successor
            // head's first-byte re-anchor, closing it on the standby
            // landing instead. Same read as the fresh path: the parked
            // bytes past the awaited body can only be the next head's.
            let body_before = self.body_remaining.load(Ordering::Relaxed);
            self.consume_request_body(n);
            let head_start = body_before <= 0 || n as i64 > body_before;
            self.arm_started_head(cx, head_start);
            return Poll::Ready(Ok(()));
        }
        if self.withheld_eof {
            return Poll::Ready(Ok(()));
        }
        let filled = buf.filled().len();
        match Pin::new(&mut self.stream).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                if buf.filled().len() > filled {
                    let read = buf.filled().len() - filled;
                    let body_before = self.body_remaining.load(Ordering::Relaxed);
                    self.consume_request_body(read);
                    // #2499: only the bytes past the awaited request's body
                    // can start its successor's head — the body's own
                    // segments never anchor a deadline.
                    let head_start = body_before <= 0 || read as i64 > body_before;
                    self.arm_started_head(cx, head_start);
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for HeadersTimeoutGate<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        // #2499: Node's 408 already terminated this connection; finish the
        // terminal write, then refuse every hyper byte so none of its own
        // encoding follows it.
        if self.timeout_408.is_some() {
            if self.poll_headers_timeout_408(cx).is_pending() {
                return Poll::Pending;
            }
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "connection closed",
            )));
        }
        if self.timeout_408_closed {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "connection closed",
            )));
        }
        // #2499: hyper wrote response bytes — Node's `_headerSent` mirror:
        // hyper never writes a response's body ahead of its head, so any
        // successful write means the in-flight response's head is on the
        // wire, and a head-deadline expiry from here on closes silently.
        match Pin::new(&mut self.stream).poll_write(cx, buf) {
            Poll::Ready(Ok(written)) => {
                if written > 0 {
                    self.response_head_written = true;
                }
                Poll::Ready(Ok(written))
            }
            other => other,
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(cx)
    }
}

/// #2499: the response-finish signal the read gate re-arms at. Node arms the
/// next head's `headersTimeout` window when the response finishes; the
/// wrapper sends the finish instant and clears `in_flight` when the body's
/// last frame was polled — or when the body was dropped unfinished (hyper
/// drops it when the client or an error ends the connection), so the gate
/// never waits on a signal that can no longer come. The instant, not just a
/// bump: the gate consumes the signal at its next poll, which can sit later
/// than the finish itself, and the standby window it arms anchors at the
/// finish — a generation bump would drift the window's landing to the
/// consumption's grid step.
struct RearmBody {
    inner: crate::Body,
    rearm: watch::Sender<Option<tokio::time::Instant>>,
    in_flight: Arc<AtomicUsize>,
    released: bool,
}

impl RearmBody {
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.in_flight.store(0, Ordering::Relaxed);
        self.rearm.send_replace(Some(tokio::time::Instant::now()));
    }
}

impl hyper::body::Body for RearmBody {
    type Data = Bytes;
    type Error = crate::ProxyError;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        match std::pin::Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(None) => {
                this.release();
                Poll::Ready(None)
            }
            other => other,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for RearmBody {
    fn drop(&mut self) {
        self.release();
    }
}

/// #2499: the awaited request's remaining body bytes — from the parsed
/// request's `Content-Length`; a chunked body stays "open" (`i64::MAX`, the
/// native lane's sentinel: no mid-stream parking for a body whose length the
/// reader cannot know up front), and no framing at all means an empty body.
fn request_content_length(request: &Request<Incoming>) -> i64 {
    if request
        .headers()
        .contains_key(hyper::header::TRANSFER_ENCODING)
    {
        if crate::mutants::on("sms-headers-chunked-body-withheld") {
            // The pre-fix-r1 sentinel the negative control kills: −1 reads
            // as "body over" in `withholding_mid_stream`'s `<= 0`, so the
            // gate parks the chunked body's bytes as if they were the next
            // request's head and the handler starves into the 300 s body
            // 408 (fix-r1 review boundary#1/concurrency#1).
            return -1;
        }
        return i64::MAX;
    }
    match request
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
    {
        Some(length) if length >= 0 => length,
        _ => 0,
    }
}

/// #2499: one sms connection's serving — the accept loop's per-connection
/// body, extracted so the paused-clock tests can drive the read gate with a
/// duplex directly. The deadline state (the gate) and the handler wiring
/// (the service) live together here: the service closure bumps
/// `served_heads` per parsed head and wraps every response body in
/// [`RearmBody`], so the gate can disarm at the awaited head's parse,
/// re-arm at the response's finish, and anchor a head already arriving
/// mid-response.
async fn serve_sms_connection<S>(
    stream: S,
    shared: Arc<SmsShared>,
    stop: watch::Receiver<bool>,
    grid_anchor: tokio::time::Instant,
    grid_interval: std::time::Duration,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // #1955/#2499: the mutation switch proves the deadline is
    // load-bearing — with it the gate runs deadline-less, the pre-#1955
    // unbounded-connection shape. #2020 registered it in
    // `tests/fixtures/channel-sms-head-timeout/negative_controls.py`
    // (the `channel-sms` table's single `channel::sms::tests::` prefix
    // cannot reach this module); #2499 rewired it from hyper's
    // `.header_read_timeout(..)` arm to the gate's own deadline.
    let headers_timeout = if crate::mutants::on("sms-header-read-no-timeout") {
        None
    } else {
        Some(shared.headers_timeout)
    };
    let (rearm, rearm_rx) = watch::channel(None::<tokio::time::Instant>);
    let served_heads = Arc::new(AtomicUsize::new(0));
    let in_flight = Arc::new(AtomicUsize::new(0));
    let body_remaining = Arc::new(AtomicI64::new(0));
    let service_shared = shared.clone();
    let service_served_heads = served_heads.clone();
    let service_in_flight = in_flight.clone();
    let service_body_remaining = body_remaining.clone();
    let service_rearm = rearm.clone();
    let service = hyper::service::service_fn(move |request: Request<Incoming>| {
        let shared = service_shared.clone();
        let served_heads = service_served_heads.clone();
        let in_flight = service_in_flight.clone();
        let body_remaining = service_body_remaining.clone();
        let rearm = service_rearm.clone();
        async move {
            served_heads.fetch_add(1, Ordering::Relaxed);
            in_flight.store(1, Ordering::Relaxed);
            // #2499: the awaited head parsed — its deadline disarms at
            // the next gate poll (the watermark passing), and the
            // request's own body bytes are counted so the reader can
            // tell them from a *next* head's first byte.
            body_remaining.store(request_content_length(&request), Ordering::Relaxed);
            match handle_request(request, shared, body_remaining).await {
                Ok(response) => Ok(response.map(|body| {
                    crate::Body::new(RearmBody {
                        inner: body,
                        rearm,
                        in_flight,
                        released: false,
                    })
                })),
                Err(destroy) => {
                    // The destroy paths (a payload over budget, a null
                    // JSON body) never build a response body, so the
                    // in-flight half clears here instead.
                    in_flight.store(0, Ordering::Relaxed);
                    Err(destroy)
                }
            }
        }
    });
    let gate = HeadersTimeoutGate::new(
        stream,
        headers_timeout,
        grid_anchor,
        grid_interval,
        served_heads,
        rearm_rx,
        in_flight,
        body_remaining,
        stop,
    );
    // #2499: the builder runs bare — hyper's own `header_read_timeout`
    // would close silently at the deadline itself, before the checker-grid
    // landing, and its `Timer` is what arms that path.
    let builder = hyper::server::conn::http1::Builder::new();
    let _ = builder.serve_connection(TokioIo::new(gate), service).await;
}

async fn handle_request(
    request: Request<Incoming>,
    shared: Arc<SmsShared>,
    body_remaining: Arc<AtomicI64>,
) -> Result<Response<crate::Body>, DestroyConnection> {
    let version = request.version();
    route_request(request, shared, body_remaining)
        .await
        .map(|response| finalize(response, version))
}

async fn route_request(
    request: Request<Incoming>,
    shared: Arc<SmsShared>,
    body_remaining: Arc<AtomicI64>,
) -> Result<Response<crate::Body>, DestroyConnection> {
    // #2152: the TS channel is a bare `http.createServer` (SmsChannel.ts:89,
    // no options), so llhttp / `_http_server.js` reject a bad method, a
    // non-ASCII or malformed target, an over-budget header block and an
    // HTTP/1.1 request without `Host` *before* the handler runs — the same
    // gate the webhook and api-server channels already run (#1683). The
    // rejection flows through the same `finalize` as every other response.
    // The gate's `Err` is the zero-byte socket close a CONNECT to a plain
    // `http.Server` performs — the same wire outcome as the payload guard,
    // so it maps explicitly onto the connection destroy hyper already
    // performs for this error type (the serve result is discarded); the
    // mapping stays at the call site instead of a blanket `From` so a future
    // io error in here cannot silently become a connection destroy.
    if let Some(rejection) =
        crate::native_http::node_parser_admission(&request).map_err(|_| DestroyConnection)?
    {
        return Ok(rejection);
    }
    let (path, port, auth_token, host, public_url) = {
        let runtime = shared.runtime.lock().await;
        (
            runtime.path.clone(),
            runtime.port,
            runtime.auth_token.clone(),
            runtime.host.clone(),
            runtime.public_url.clone(),
        )
    };
    let path_text = String::from_utf16_lossy(path.units());
    if request.uri().path() != path_text || request.method() != hyper::http::Method::POST {
        return Ok(sized(plain_response(404, "Not Found")));
    }

    // Header reads happen before `into_body` consumes the request.
    let content_type = request
        .headers()
        .get("content-type")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .unwrap_or_default();
    let signature = request
        .headers()
        .get("x-twilio-signature")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .unwrap_or_default();
    // #1901: Node's `requestTimeout` bounds receiving the whole request. The
    // TS sms channel inherits the 300 s default from `http.createServer`, so a
    // client that declares `Content-Length` and then stops sending is answered
    // with `408 Request Timeout` + `Connection: close` instead of holding the
    // connection task open forever.
    let body_read = async move {
        let mut incoming = request.into_body();
        let mut body: Vec<u8> = Vec::new();
        let mut read_error: Option<String> = None;
        loop {
            match incoming.frame().await {
                Some(Ok(frame)) => {
                    if let Ok(bytes) = frame.into_data() {
                        body.extend_from_slice(&bytes);
                        if body.len() > MAX_BODY_BYTES {
                            // `reject(new Error("payload too large")); req.destroy()`
                            // — the destroy wins, zero response bytes.
                            return Err(DestroyConnection);
                        }
                    }
                }
                Some(Err(error)) => {
                    read_error = Some(error.to_string());
                    break;
                }
                None => break,
            }
        }
        Ok((body, read_error))
    };
    let (body, read_error) = if crate::mutants::on("sms-body-read-no-timeout") {
        // The pre-fix behavior the negative control kills: no deadline on the
        // body read, so a partial body holds the connection task forever.
        body_read.await?
    } else {
        match tokio::time::timeout(shared.request_timeout, body_read).await {
            Ok(result) => result?,
            // The wire answer Node's `requestTimeoutResponse` writes: a 408
            // with `Connection: close` and no body (shared with the webhook
            // channel so the two lanes cannot drift).
            Err(_) => return Ok(crate::channel::webhook::node_request_timeout_response()),
        }
    };
    // #2499 fix-r1 (interleave A): the handler has finished reading the
    // awaited request's body, so hyper has consumed all of it — any
    // positive remainder is bytes it read ahead into its own buffer
    // together with the head (or parked head bytes re-fed past the store),
    // which never crossed the gate's fresh-read accounting. Left in place
    // it would read as "the body is still coming" and skip the successor
    // head's first-byte re-anchor, closing a follow-up head on the
    // standby landing instead of its own.
    body_remaining.store(0, Ordering::Relaxed);
    if let Some(error) = read_error {
        // #1339 fix-r1: probed on the pinned Node v22.22.0
        // (probes/truncated-body.txt, 3/3 deterministic) — the handler's
        // `Bad Request: <e>` body never reaches the wire because the
        // truncated request kills the socket first; the observable bytes
        // are the bare 400 with an empty body.
        let body = if crate::mutants::on("sms-body-error-text") {
            format!("Bad Request: Error: {error}")
        } else {
            String::new()
        };
        return Ok(sized(plain_response(400, &body)));
    }

    let params = if content_type.contains("application/json") {
        match pilotdeck_protocol::parse_json(&String::from_utf8_lossy(&body)) {
            Ok(value) => match &value {
                JsValue::Object(object) => object.clone(),
                JsValue::Null if !crate::mutants::on("sms-json-null-body") => {
                    // #1339 fix-r1: literal JSON `null` — on TS the socket
                    // hangs (probes/null-body.txt: no response byte, the
                    // TypeError never reaches the wire), so the oracle
                    // cannot pin it. Rust reports the exact TypeError and
                    // destroys the connection; the hang-vs-destroy deviation
                    // is ledgered in A8.3-CH-SMS-007. Inside the signature
                    // gate `Object.keys(params)` throws first.
                    let message = if !auth_token.is_empty() && !signature.is_empty() {
                        "TypeError: Cannot convert undefined or null to object"
                    } else {
                        "TypeError: Cannot read properties of null (reading 'Body')"
                    };
                    report_unhandled(&shared, message);
                    return Err(DestroyConnection);
                }
                _ => JsObject::new(),
            },
            Err(_) => return Ok(sized(plain_response(400, "Bad Request"))),
        }
    } else {
        parse_form_urlencoded(&body)
    };

    let host_text = String::from_utf16_lossy(host.units());
    let public_text = String::from_utf16_lossy(public_url.units());
    // #1339 fix-r1: the recorded `X-Twilio-Signature` values were computed
    // by the TS-side generator over this exact URL, so the branch choice is
    // part of the pinned bytes (mutant: ignore `publicUrl`).
    let full_url = if !public_text.is_empty() && !crate::mutants::on("sms-signature-public-url") {
        format!(
            "{}{}",
            public_text.strip_suffix('/').unwrap_or(&public_text),
            path_text
        )
    } else {
        format!(
            "http://{host_text}:{}{path_text}",
            crate::js_value::number_string(port)
        )
    };

    if !auth_token.is_empty()
        && !signature.is_empty()
        && !crate::mutants::on("sms-signature-gate-off")
        && !validate_twilio_signature(&full_url, &params, &signature, &auth_token)
    {
        shared.log("warn", "sms: invalid Twilio signature".to_owned());
        return Ok(sized(plain_response(401, "Unauthorized")));
    }

    // `params.Body ?? ""` / `params.From ?? ""` keep raw wire values.
    let nullish_or = |name: &str| {
        params
            .get(name)
            .filter(|value| !nullish(value))
            .cloned()
            .unwrap_or(JsValue::String(JsString::from("")))
    };
    let body_value = nullish_or("Body");
    let from_value = nullish_or("From");

    let response = sized(twiml_response());
    if !truthy(&from_value) && !crate::mutants::on("sms-empty-from-dispatch") {
        return Ok(response);
    }
    let text = if let JsValue::String(text) = &body_value {
        if trim_units(text.units()).is_empty() {
            return Ok(response);
        }
        text.clone()
    } else {
        // `body.trim()` on a non-string is the TS unhandled rejection; the
        // 200 was already written. The Rust port contains it in the
        // connection task and reports it through the recorder.
        report_unhandled(&shared, "TypeError: body.trim is not a function");
        return Ok(response);
    };

    let chat_id = js_string_of(&from_value);
    let spawned = shared.clone();
    tokio::spawn(async move {
        // `void this.handleIncoming(...).catch(e => log)`
        if let Err(error) = handle_incoming(&spawned, chat_id, text).await {
            spawned.log(
                "error",
                format!("sms: handleIncoming error: {}", error.to_js_string()),
            );
        }
    });
    Ok(response)
}

/// WHATWG percent-decoding of one form component: `+` becomes a space, `%XX`
/// with two hex digits decodes, anything else stays (invalid escapes keep
/// their bytes, so invalid UTF-8 becomes U+FFFD at the string boundary).
fn decode_form_component(raw: &[u8]) -> Vec<u8> {
    fn hex_value(byte: u8) -> u8 {
        match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            b'A'..=b'F' => byte - b'A' + 10,
            _ => 0,
        }
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        match raw[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < raw.len()
                && raw[index + 1].is_ascii_hexdigit()
                && raw[index + 2].is_ascii_hexdigit() =>
            {
                out.push(hex_value(raw[index + 1]) * 16 + hex_value(raw[index + 2]));
                index += 3;
            }
            _ => {
                out.push(raw[index]);
                index += 1;
            }
        }
    }
    out
}

/// `Object.fromEntries(new URLSearchParams(body))`: last duplicate wins, the
/// first position is kept; empty `&` segments are skipped.
fn parse_form_urlencoded(body: &[u8]) -> JsObject {
    let mut object = JsObject::new();
    for pair in body.split(|byte| *byte == b'&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = match pair.iter().position(|byte| *byte == b'=') {
            Some(at) => (&pair[..at], &pair[at + 1..]),
            None => (pair, &pair[..0]),
        };
        let name =
            JsString::from(String::from_utf8_lossy(&decode_form_component(name)).into_owned());
        let value = JsValue::String(JsString::from(
            String::from_utf8_lossy(&decode_form_component(value)).into_owned(),
        ));
        object.insert(name, value);
    }
    object
}

/// `validateTwilioSignature`: sorted keys, `url + k + v` HMAC-SHA1 base64.
fn validate_twilio_signature(
    full_url: &str,
    params: &JsObject,
    signature: &str,
    auth_token: &JsString,
) -> bool {
    let mut entries: Vec<(&JsString, &JsValue)> = params.own_entries();
    // `Object.keys(params).sort()` — UTF-16 code unit order.
    entries.sort_by_key(|(a, _)| *a);
    // #1339 fix-r1 mutant: reversed key order — the recorded oracle
    // signatures only replay while the port keeps the sort.
    if crate::mutants::on("sms-signature-key-order") {
        entries.reverse();
    }
    let mut data = full_url.to_owned();
    for (key, value) in entries {
        data.push_str(&String::from_utf16_lossy(key.units()));
        data.push_str(&String::from_utf16_lossy(js_string_of(value).units()));
    }
    let token = String::from_utf16_lossy(auth_token.units());
    let digest = base64::engine::general_purpose::STANDARD
        .encode(hmac_sha1(token.as_bytes(), data.as_bytes()));
    // `Buffer.from(hmac)` vs `Buffer.from(signature)`: UTF-8 bytes of both
    // base64 strings, equal length first, like `timingSafeEqual`.
    let (a, b) = (digest.as_bytes(), signature.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// HMAC-SHA1 with the shared digest crate (block size 64).
fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..20].copy_from_slice(&Sha1::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha1::new();
    let ipad: Vec<u8> = block.iter().map(|byte| byte ^ 0x36).collect();
    inner.update(&ipad);
    inner.update(message);
    let inner_digest = inner.finalize();
    let mut outer = Sha1::new();
    let opad: Vec<u8> = block.iter().map(|byte| byte ^ 0x5c).collect();
    outer.update(&opad);
    outer.update(inner_digest);
    outer.finalize().into()
}

// ---------------------------------------------------------------------------
// inbound message processing (`handleIncoming` / `processMessage`)
// ---------------------------------------------------------------------------

fn boundaries(shared: &SmsShared) -> Boundaries {
    shared
        .boundaries
        .lock()
        .expect("#1339 boundaries mutex")
        .clone()
}

/// Report through the unhandled recorder with the callback outside the
/// boundaries lock, like `SmsShared::log`.
fn report_unhandled(shared: &SmsShared, message: &str) {
    let recorder = shared
        .boundaries
        .lock()
        .expect("#1339 boundaries mutex")
        .record_unhandled
        .clone();
    if let Some(recorder) = recorder {
        recorder(message);
    }
}

async fn send_reply(shared: &SmsShared, chat_id: &JsString, text: JsString) -> bool {
    let sender = {
        let runtime = shared.runtime.lock().await;
        runtime
            .client
            .clone()
            .map(|client| (client, runtime.from_number.clone()))
    };
    let Some((client, from)) = sender else {
        return false;
    };
    let message = SmsOutgoingMessage {
        body: text,
        from,
        to: chat_id.clone(),
    };
    match client.send(message).await {
        Ok(()) => true,
        Err(error) => {
            shared.log(
                "error",
                format!("sms: sendMessage failed: {}", error.to_js_string()),
            );
            false
        }
    }
}

async fn handle_incoming(
    shared: &Arc<SmsShared>,
    chat_id: JsString,
    text: JsString,
) -> Result<(), ChannelError> {
    // #1589 (issue #1): the switch restores the pre-fix two-section shape —
    // `hasPending` in one lock section, `take` in another — with one
    // `yield_now()` between them so the concurrent case interleaves
    // deterministically. It exists only to make the old shape judgeable red
    // (`sms_elicit_lock_split`); it is not a production path.
    let pending_elicitation = if crate::mutants::on("sms-elicit-lock-split") {
        let has_pending = {
            let runtime = shared.runtime.lock().await;
            runtime.elicitation.has_pending(&chat_id) && boundaries(shared).gateway.is_some()
        };
        tokio::task::yield_now().await;
        let mut runtime = shared.runtime.lock().await;
        if has_pending {
            // A `clear` that landed between the sections empties the entry;
            // the stale check lets the message fall through to the turn.
            runtime.elicitation.take(&chat_id)
        } else {
            None
        }
    } else {
        // #1339 fix-r1: `hasPending` + `take` in one lock section — TS runs the
        // same stretch synchronously, and two sections would let another
        // webhook turn slip a `clear` between the check and the take.
        {
            let mut runtime = shared.runtime.lock().await;
            if runtime.elicitation.has_pending(&chat_id) && boundaries(shared).gateway.is_some() {
                runtime.elicitation.take(&chat_id)
            } else {
                None
            }
        }
    };
    if let Some(entry) = pending_elicitation {
        let trimmed = trim_units(text.units());
        let (response, confirmation) = if trimmed == utf16("0") {
            let text = if crate::mutants::on("sms-elicit-cancel-text") {
                "已取消"
            } else {
                "已取消。"
            };
            (
                elicitation::cancelled_response(&entry),
                Some(JsString::from(text)),
            )
        } else {
            (
                elicitation::answered_response(&entry, &JsString::from_utf16(trimmed.to_vec())),
                None,
            )
        };
        let gateway = boundaries(shared)
            .gateway
            .expect("#1339 checked before take");
        match gateway.respond_elicitation(response).await {
            Err(error) => shared.log(
                "error",
                format!("sms: elicitation answer error: {}", error.to_js_string()),
            ),
            Ok(()) => {
                if let Some(confirmation) = confirmation {
                    send_reply(shared, &chat_id, confirmation).await;
                }
            }
        }
        return Ok(());
    }

    // #1589 (issue #1): same two-section restoration as the elicitation
    // switch above — not a production path, only the negative control's
    // observable (`sms_permission_lock_split`). #1790: the permission state is
    // its own `std` mutex, so the sections are synchronous.
    let began = if crate::mutants::on("sms-permission-lock-split") {
        let has_pending = {
            let permissions = shared.lock_permissions();
            permissions.has_pending(&chat_id) && boundaries(shared).gateway.is_some()
        };
        tokio::task::yield_now().await;
        let mut permissions = shared.lock_permissions();
        if !has_pending {
            None
        } else {
            match permissions.begin_answer(&chat_id, &text) {
                // A `clear` that landed between the sections emptied the
                // state; the stale check lets the message fall through to
                // the turn instead of the early `None` return.
                BeginAnswer::None => None,
                answer => Some(answer),
            }
        }
    } else {
        // #1339 fix-r1: same fusion for `hasPending` + `beginAnswer`.
        let mut permissions = shared.lock_permissions();
        if permissions.has_pending(&chat_id) && boundaries(shared).gateway.is_some() {
            Some(permissions.begin_answer(&chat_id, &text))
        } else {
            None
        }
    };
    if let Some(begin) = began {
        match begin {
            BeginAnswer::None => (),
            BeginAnswer::Status(answer) => {
                deliver_answer(shared, &chat_id, answer).await;
            }
            BeginAnswer::Decide {
                entry,
                deny,
                remember,
                answer_token,
                generation,
                shifted_rest,
            } => {
                // #1790: own the `answering`/`in_flight` release across the
                // decide await; a gateway panic or a cancelled/timed-out task
                // then unwinds through the guard instead of wedging the chat.
                // Armed before the `expect` below so no fallible statement sits
                // between `begin_answer` arming the flags and this guard.
                let mut guard = AnswerGuard::new(
                    shared.permissions.clone(),
                    chat_id.clone(),
                    generation,
                    entry.clone(),
                    shifted_rest.clone(),
                );
                let gateway = boundaries(shared)
                    .gateway
                    .expect("#1339 checked before begin");
                // #1339 fix-r1: a missing event `requestId` stays real
                // `undefined` (the JSON record then omits the key), not the
                // string "undefined".
                let request_id = entry.request_id.clone();
                let request_id = if crate::mutants::on("sms-permission-request-id-undefined")
                    && request_id.is_none()
                {
                    Some(JsString::from("undefined"))
                } else {
                    request_id
                };
                let decision = PermissionDecision {
                    session_key: entry.session_key.clone(),
                    request_id,
                    deny,
                    remember,
                };
                let reply = if deny {
                    DecisionReply::Deny
                } else if remember {
                    DecisionReply::AllowSession
                } else {
                    DecisionReply::AllowOnce
                };
                match gateway.permission_decide(decision).await {
                    Err(error) => {
                        shared.lock_permissions().fail_answer(
                            &chat_id,
                            generation,
                            entry,
                            shifted_rest,
                        );
                        guard.settle();
                        shared.log(
                            "error",
                            format!("sms: permission answer error: {}", error.to_js_string()),
                        );
                    }
                    Ok(outcome) => {
                        let answer = shared.lock_permissions().finish_answer(
                            &chat_id,
                            answer_token,
                            generation,
                            reply,
                            shifted_rest,
                            outcome,
                        );
                        guard.settle();
                        if let Some(answer) = answer {
                            deliver_answer(shared, &chat_id, answer).await;
                        }
                    }
                }
            }
        }
        return Ok(());
    }

    enum ActiveOutcome {
        Skip,
        NewChat,
        EmptyMessage,
        /// The admitted turn carries its own `Drop` guard: the slot is taken
        /// and guarded in one synchronous section, so nothing between the
        /// `insert` and the guard (the already-active log, the match dispatch)
        /// can panic with the chat left marked active.
        Dispatch(MappedSession, super::ActiveChat<JsString>),
    }
    /// The mapper→outcome mapping both lock shapes share (#1589 issue #1 kept
    /// it one function so the switch branch cannot drift from the fused path).
    fn dispatch_outcome(
        runtime: &mut SmsRuntime,
        chat_id: &JsString,
        text: &JsString,
    ) -> ActiveOutcome {
        let mapped = runtime.mapper.resolve(chat_id, text);
        if mapped.command_new && mapped.message.is_empty() {
            ActiveOutcome::NewChat
        } else if mapped.message.is_empty() {
            // TS: `if (!mapped.message) return;`
            ActiveOutcome::EmptyMessage
        } else {
            // #1738: the set itself keeps a std mutex (never held across an
            // await) so the synchronous `Drop` guard can release the slot.
            // The guard is built in the same section as the `insert`: the
            // caller's already-active log runs before the match arm, and a
            // panic there would otherwise leak the admission.
            let active = runtime.active_chats.clone();
            active
                .lock()
                .expect("#1738 sms active chats mutex")
                .insert(chat_id.clone());
            let guard = super::ActiveChat::new(active, chat_id.clone());
            ActiveOutcome::Dispatch(mapped, guard)
        }
    }
    // #1589 (issue #1): same two-section restoration — `contains` in one lock
    // section, `resolve` + `insert` in another — not a production path, only
    // the negative control's observable (`sms_active_insert_split`). The
    // dedupe switch is read once here so both shapes share it.
    let dedupe_off = crate::mutants::on("sms-active-dedupe-off");
    let (already_active, outcome) = if crate::mutants::on("sms-active-insert-split") {
        let already_active = {
            let runtime = shared.runtime.lock().await;
            runtime
                .active_chats
                .lock()
                .expect("#1738 sms active chats mutex")
                .contains(&chat_id)
        };
        tokio::task::yield_now().await;
        let mut runtime = shared.runtime.lock().await;
        let outcome = if already_active && !dedupe_off {
            ActiveOutcome::Skip
        } else {
            dispatch_outcome(&mut runtime, &chat_id, &text)
        };
        (already_active, outcome)
    } else {
        // #1339 fix-r1: `activeChats.has` → `sessionMapper.resolve` → `add` in
        // one lock section — TS runs the same stretch synchronously; separate
        // sections could let a parallel webhook for the same chat resolve twice
        // and double-dispatch. The "already active" log and the `/new` reply
        // stay outside the lock.
        let mut runtime = shared.runtime.lock().await;
        let already_active = runtime
            .active_chats
            .lock()
            .expect("#1738 sms active chats mutex")
            .contains(&chat_id);
        let outcome = if already_active && !dedupe_off {
            ActiveOutcome::Skip
        } else {
            dispatch_outcome(&mut runtime, &chat_id, &text)
        };
        (already_active, outcome)
    };
    if already_active {
        shared.log(
            "info",
            format!(
                "sms: chat {} already active, skipping",
                String::from_utf16_lossy(chat_id.units())
            ),
        );
    }
    match outcome {
        ActiveOutcome::Skip => Ok(()),
        ActiveOutcome::NewChat => {
            send_reply(shared, &chat_id, JsString::from("已创建新会话。")).await;
            Ok(())
        }
        ActiveOutcome::EmptyMessage => Ok(()),
        // #1738: the `Drop` guard replaces the paired insert/remove so a panic
        // in `process_message` (spawned fire-and-forget, so only the task dies)
        // still releases the slot — the TS `finally { delete }`. It was built
        // by `dispatch_outcome` on the dispatch path only: a non-dispatch
        // outcome never admitted, so its drop must not release another turn's
        // slot. Bound here so it outlives the `.await`.
        ActiveOutcome::Dispatch(mapped, _guard) => {
            process_message(shared, &chat_id, &mapped.session_key, &mapped.message).await
        }
    }
}

/// The post-answer part of the TS permission branch: confirmation send,
/// non-advancing status handling, and the next queued prompt.
async fn deliver_answer(shared: &Arc<SmsShared>, chat_id: &JsString, answer: PermissionAnswer) {
    // #1790: after `finish_answer` the answer lock is still held across both
    // sends below (the confirmation and, when queued, the next prompt), and
    // this branch never reaches `clear_after_turn`. One guard covers the whole
    // tail: a panic, cancellation or timeout retries the queued prompt or
    // settles the answer (`release_answer` + the queued-head promotion in
    // `recover_answer`) instead of leaving `answering` set forever — which
    // drops every later message as `BeginAnswer::None`.
    let mut guard = PromptGuard::new(
        shared.permissions.clone(),
        chat_id.clone(),
        answer.answer_token,
    );
    if let Some(text) = answer.text.clone() {
        let delivered = send_reply(shared, chat_id, text).await;
        if !delivered {
            // Same recovery as the guard: an undelivered confirmation with a
            // permission captured meanwhile is queued in `pending` with no
            // prompt, and the bare `release_answer` would bail on the non-empty
            // queue and leave `answering` set for good. With an empty queue the
            // two calls are identical, so the TS-mirrored bytes of this branch
            // are unchanged (`permission-confirmation-send-fail`).
            shared
                .lock_permissions()
                .recover_answer(chat_id, answer.answer_token);
            guard.settle();
            return;
        }
        if !answer.can_advance && !answer.retry_prompt {
            guard.settle();
            return;
        }
        let next_prompt = shared
            .lock_permissions()
            .take_next_prompt(chat_id, answer.answer_token);
        if let Some(prompt) = next_prompt {
            let expected = shared
                .lock_permissions()
                .get_prompt_request_id(chat_id, answer.answer_token);
            let delivered = send_reply(shared, chat_id, prompt).await;
            shared.lock_permissions().confirm_next_prompt(
                chat_id,
                delivered,
                expected.as_ref(),
                answer.answer_token,
            );
        }
    }
    guard.settle();
}

async fn process_message(
    shared: &Arc<SmsShared>,
    chat_id: &JsString,
    session_key: &JsString,
    message: &JsString,
) -> Result<(), ChannelError> {
    let gateway = boundaries(shared).gateway;
    if gateway.is_none() {
        // TS `this.gateway.submitTurn(...)` with no gateway throws into
        // handleIncoming's catch; the log line is identical.
        return Err(ChannelError::Plain(
            "TypeError: Cannot read properties of undefined (reading 'submitTurn')".to_owned(),
        ));
    }
    let gateway = gateway.expect("#1339 checked above");
    let mut reply_units: Vec<u16> = Vec::new();
    let request = SubmitTurnRequest {
        session_key: session_key.clone(),
        channel_key: JsString::from("sms"),
        message: message.clone(),
    };
    let mut turn_error = None;
    let mut stream = match gateway.submit_turn(request).await {
        Err(error) => {
            turn_error = Some(error.to_js_string());
            None
        }
        Ok(stream) => Some(stream),
    };
    if let Some(stream) = stream.as_mut() {
        loop {
            let event = match stream.next_event().await {
                None => break,
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    turn_error = Some(error.to_js_string());
                    break;
                }
            };
            let kind = event.get("type").and_then(JsValue::as_string);
            let is = |name: &str| kind.as_ref().is_some_and(|kind| kind.equals_str(name));
            if is("elicitation_request") {
                let question = {
                    let mut runtime = shared.runtime.lock().await;
                    runtime.elicitation.capture(chat_id, session_key, &event)
                };
                send_reply(shared, chat_id, question).await;
                continue;
            }
            if is("permission_request") {
                // #1790: `js_string_of` can panic on pathological nesting, and
                // `capture` has already armed `answering`/`initial_prompt_pending`
                // by then. Convert first (`capture` performs the same conversion
                // on the same value), so no fallible statement sits between the
                // flags and the guard.
                let expected = event.get("requestId").and_then(permission::optional_string);
                let question = shared
                    .lock_permissions()
                    .capture(chat_id, session_key, &event);
                if let Some(question) = question {
                    // #1790: own the `answering`/`initial_prompt_pending` set
                    // by `capture` across the prompt send; a panic or
                    // cancellation then records the not-delivered retry
                    // instead of wedging the chat in "权限提示发送中".
                    let mut guard = InitialPromptGuard::new(
                        shared.permissions.clone(),
                        chat_id.clone(),
                        expected.clone(),
                    );
                    let delivered = send_reply(shared, chat_id, question).await;
                    shared.lock_permissions().confirm_initial_prompt(
                        chat_id,
                        delivered,
                        expected.as_ref(),
                    );
                    guard.settle();
                }
                continue;
            }
            if let Some(fragment) = render::render_sms_event(&event) {
                reply_units.extend_from_slice(fragment.units());
            }
        }
    }
    if let Some(error) = turn_error {
        shared.log("error", format!("sms: submitTurn error: {error}"));
        reply_units = if crate::mutants::on("sms-error-reply-text") {
            utf16("出错了。")
        } else {
            utf16("处理消息时发生错误，请重试。")
        };
    }
    {
        let mut runtime = shared.runtime.lock().await;
        runtime.elicitation.clear(chat_id);
    }
    shared.lock_permissions().clear_after_turn(chat_id);
    let final_text = trim_units(&reply_units);
    if !final_text.is_empty() {
        send_reply(shared, chat_id, JsString::from_utf16(final_text.to_vec())).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// cron session-key parsing (`ImCronDelivery.ts`)
// ---------------------------------------------------------------------------

/// `parseChatIdFromSessionKey`: `<key>:chat=<chatId>:(general|s_<uuid36>)`.
/// The TS regex `.` excludes `\n\r  `, so the chat id may not
/// contain any of those. Returns the raw units, lone surrogates included.
pub fn parse_chat_id_from_session_key(
    session_key: &JsString,
    channel_key: &ChannelKey,
) -> Option<JsString> {
    let mut prefix = channel_key.as_js().units().to_vec();
    prefix.extend_from_slice(&utf16(":chat="));
    let units = session_key.units();
    if units.len() < prefix.len() || units[..prefix.len()] != prefix[..] {
        return None;
    }
    let suffix = &units[prefix.len()..];
    let excluded = |unit: u16| matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029);
    // The anchored `$` makes the tail deterministic: `:general`, or `:s_` +
    // 36 `[0-9a-fA-F-]` units (39 in all), at the very end. The tails cannot
    // both match (`:` is not a tail character class member), so the greedy
    // `(.+)` split point is unambiguous.
    let general = utf16(":general");
    let chat_units = if suffix.ends_with(general.as_slice()) && suffix.len() >= general.len() {
        &suffix[..suffix.len() - general.len()]
    } else if suffix.len() >= 39
        && suffix[suffix.len() - 39] == u16::from(b':')
        && suffix[suffix.len() - 38] == u16::from(b's')
        && suffix[suffix.len() - 37] == u16::from(b'_')
        && suffix[suffix.len() - 36..].iter().all(|&unit| {
            (u16::from(b'0')..=u16::from(b'9')).contains(&unit)
                || (u16::from(b'a')..=u16::from(b'f')).contains(&unit)
                || (u16::from(b'A')..=u16::from(b'F')).contains(&unit)
                || unit == u16::from(b'-')
        })
    {
        &suffix[..suffix.len() - 39]
    } else {
        return None;
    };
    // `chatId ? chatId : undefined` — the empty string is falsy.
    if chat_units.is_empty() || chat_units.iter().any(|&unit| excluded(unit)) {
        return None;
    }
    Some(JsString::from_utf16(chat_units.to_vec()))
}

/// Loader mount: builds the adapter from the `sms` loader option object.
pub fn register(mounts: &mut super::loader::ChannelLoaderMounts) {
    mounts.register(
        "sms",
        Arc::new(|options: &JsObject| {
            Ok(Arc::new(SmsChannel::new(SmsChannelOptions {
                extra: options.get("extra").and_then(JsValue::as_object).cloned(),
                mapper: None,
                gateway: None,
                env: Some(Arc::new(|name| {
                    std::env::var(name).ok().map(JsString::from)
                })),
                twilio_factory: None,
                record_unhandled: None,
                request_timeout: None,
            })) as Arc<dyn ChannelAdapter>)
        }),
    );
}

/// Raw-socket HTTP probe client used by the replay tests (`Connection: close`,
/// full response bytes, minimal status/content-type/body parse). `half_close`
/// mirrors the generator's `socket.end()`: the write side finishes after the
/// (possibly truncated) request so the response can still be read.
#[cfg(test)]
pub(crate) async fn raw_request(
    port: u16,
    raw: &[u8],
    half_close: bool,
) -> Result<serde_json::Value, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|error| error.to_string())?;
    // A destroyed connection (oversized payload) surfaces as a write or read
    // error with zero response bytes, same as the probe client's `closed`.
    let _ = stream.write_all(raw).await;
    if half_close {
        let _ = stream.shutdown().await;
    }
    let mut bytes = Vec::new();
    let _ = stream.read_to_end(&mut bytes).await;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.is_empty() {
        return Ok(serde_json::json!({"closed": true}));
    }
    let split = text
        .find("\r\n\r\n")
        .ok_or("#1339 response without header end")?;
    let (head, body) = text.split_at(split);
    let body = &body[4..];
    let status: u16 = head
        .split("\r\n")
        .next()
        .ok_or("#1339 empty response head")?
        .split(' ')
        .nth(1)
        .ok_or("#1339 status line")?
        .parse()
        .map_err(|_| "#1339 status code")?;
    // The probe's `/^content-type:\s*(.*)$/im`: the header name matches
    // case-insensitively, the value is captured verbatim.
    let content_type = head.split("\r\n").skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("content-type") {
            return None;
        }
        Some(value.trim_start().to_owned())
    });
    Ok(serde_json::json!({"status": status, "contentType": content_type, "body": body}))
}
