//! A8.3 sms channel: port of `src/adapters/channel/sms/SmsChannel.ts` on the
//! #1327 lifecycle. All external services are loopback mocks; the Twilio REST
//! boundary is an injected [`sender::TwilioFactory`].
//!
//! TS `start(deps)` reads `deps.gateway`; #1327's `ChannelStartDeps` has no
//! gateway field (the A7 inbound port is a later unit), so the gateway is a
//! constructor option instead — recorded in the #1339 diffs ledger.
pub mod elicitation;
pub mod gateway;
pub mod mapper;
pub mod permission;
pub mod render;
pub mod sender;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use sha1::{Digest, Sha1};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};

pub use elicitation::ImElicitationHelper;
pub use gateway::{
    ElicitationResponse, GatewayRef, PermissionDecision, PermissionDecisionOutcome, SmsGateway,
    SubmitTurnRequest, TurnEventStream,
};
pub use mapper::{MappedSession, SmsSessionMapper, SmsSessionMapperState};
pub use permission::{BeginAnswer, DecisionReply, ImPermissionHelper, PermissionAnswer};
pub use sender::{SmsOutgoingMessage, SmsSender, TwilioFactory};

use super::protocol::{
    BoxFuture, ChannelAdapter, ChannelError, ChannelHandle, ChannelKey, ChannelStartDeps,
};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: f64 = 8790.0;
const DEFAULT_PATH: &str = "/sms/incoming";
const MAX_BODY_BYTES: usize = 1_048_576;
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
    permissions: ImPermissionHelper,
    active_chats: HashSet<JsString>,
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
                    permissions: ImPermissionHelper::default(),
                    active_chats: HashSet::new(),
                    client: None,
                    from_number: JsString::from(""),
                    auth_token: JsString::from(""),
                    host: JsString::from(DEFAULT_HOST),
                    port: DEFAULT_PORT,
                    path: JsString::from(DEFAULT_PATH),
                    public_url: JsString::from(""),
                }),
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
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = shutdown_rx.changed() => break,
                        accepted = listener.accept() => match accepted {
                            Ok((stream, _)) => {
                                let shared = server_shared.clone();
                                tokio::spawn(async move {
                                    let service = hyper::service::service_fn(
                                        move |request: Request<Incoming>| {
                                            let shared = shared.clone();
                                            async move { handle_request(request, shared).await }
                                        },
                                    );
                                    let _ = hyper::server::conn::http1::Builder::new()
                                        .serve_connection(TokioIo::new(stream), service)
                                        .await;
                                });
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
    match socket.bind(address) {
        Ok(()) => V4Port::Unshared,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => V4Port::Shared,
        Err(_) => V4Port::Unknown,
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

async fn handle_request(
    request: Request<Incoming>,
    shared: Arc<SmsShared>,
) -> Result<Response<Full<Bytes>>, DestroyConnection> {
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
        return Ok(plain_response(404, "Not Found"));
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
        return Ok(plain_response(400, &body));
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
            Err(_) => return Ok(plain_response(400, "Bad Request")),
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
        return Ok(plain_response(401, "Unauthorized"));
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

    let response = twiml_response();
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
    // #1339 fix-r1: `hasPending` + `take` in one lock section — TS runs the
    // same stretch synchronously, and two sections would let another
    // webhook turn slip a `clear` between the check and the take.
    let pending_elicitation = {
        let mut runtime = shared.runtime.lock().await;
        if runtime.elicitation.has_pending(&chat_id) && boundaries(shared).gateway.is_some() {
            runtime.elicitation.take(&chat_id)
        } else {
            None
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

    // #1339 fix-r1: same fusion for `hasPending` + `beginAnswer`.
    let began = {
        let mut runtime = shared.runtime.lock().await;
        if runtime.permissions.has_pending(&chat_id) && boundaries(shared).gateway.is_some() {
            Some(runtime.permissions.begin_answer(&chat_id, &text))
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
                        {
                            let mut runtime = shared.runtime.lock().await;
                            runtime.permissions.fail_answer(
                                &chat_id,
                                generation,
                                entry,
                                shifted_rest,
                            );
                        }
                        shared.log(
                            "error",
                            format!("sms: permission answer error: {}", error.to_js_string()),
                        );
                    }
                    Ok(outcome) => {
                        let answer = {
                            let mut runtime = shared.runtime.lock().await;
                            runtime.permissions.finish_answer(
                                &chat_id,
                                answer_token,
                                generation,
                                reply,
                                shifted_rest,
                                outcome,
                            )
                        };
                        if let Some(answer) = answer {
                            deliver_answer(shared, &chat_id, answer).await;
                        }
                    }
                }
            }
        }
        return Ok(());
    }

    // #1339 fix-r1: `activeChats.has` → `sessionMapper.resolve` → `add` in
    // one lock section — TS runs the same stretch synchronously; separate
    // sections could let a parallel webhook for the same chat resolve twice
    // and double-dispatch. The "already active" log and the `/new` reply
    // stay outside the lock.
    enum ActiveOutcome {
        Skip,
        NewChat,
        EmptyMessage,
        Dispatch(MappedSession),
    }
    let (already_active, outcome) = {
        let mut runtime = shared.runtime.lock().await;
        let already_active = runtime.active_chats.contains(&chat_id);
        if already_active && !crate::mutants::on("sms-active-dedupe-off") {
            (already_active, ActiveOutcome::Skip)
        } else {
            let mapped = runtime.mapper.resolve(&chat_id, &text);
            let outcome = if mapped.command_new && mapped.message.is_empty() {
                ActiveOutcome::NewChat
            } else if mapped.message.is_empty() {
                // TS: `if (!mapped.message) return;`
                ActiveOutcome::EmptyMessage
            } else {
                runtime.active_chats.insert(chat_id.clone());
                ActiveOutcome::Dispatch(mapped)
            };
            (already_active, outcome)
        }
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
        ActiveOutcome::Dispatch(mapped) => {
            let result =
                process_message(shared, &chat_id, &mapped.session_key, &mapped.message).await;
            {
                let mut runtime = shared.runtime.lock().await;
                runtime.active_chats.remove(&chat_id);
            }
            result
        }
    }
}

/// The post-answer part of the TS permission branch: confirmation send,
/// non-advancing status handling, and the next queued prompt.
async fn deliver_answer(shared: &Arc<SmsShared>, chat_id: &JsString, answer: PermissionAnswer) {
    if let Some(text) = answer.text.clone() {
        let delivered = send_reply(shared, chat_id, text).await;
        if !delivered {
            let mut runtime = shared.runtime.lock().await;
            runtime
                .permissions
                .release_answer(chat_id, answer.answer_token);
            return;
        }
        if !answer.can_advance && !answer.retry_prompt {
            return;
        }
        let next_prompt = {
            let mut runtime = shared.runtime.lock().await;
            runtime
                .permissions
                .take_next_prompt(chat_id, answer.answer_token)
        };
        if let Some(prompt) = next_prompt {
            let expected = {
                let runtime = shared.runtime.lock().await;
                runtime
                    .permissions
                    .get_prompt_request_id(chat_id, answer.answer_token)
            };
            let delivered = send_reply(shared, chat_id, prompt).await;
            let mut runtime = shared.runtime.lock().await;
            runtime.permissions.confirm_next_prompt(
                chat_id,
                delivered,
                expected.as_ref(),
                answer.answer_token,
            );
        }
    }
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
                let question = {
                    let mut runtime = shared.runtime.lock().await;
                    runtime.permissions.capture(chat_id, session_key, &event)
                };
                if let Some(question) = question {
                    let delivered = send_reply(shared, chat_id, question).await;
                    let expected = event
                        .get("requestId")
                        .cloned()
                        .and_then(|value| match value {
                            JsValue::Undefined | JsValue::Null => None,
                            value => Some(js_string_of(&value)),
                        });
                    let mut runtime = shared.runtime.lock().await;
                    runtime.permissions.confirm_initial_prompt(
                        chat_id,
                        delivered,
                        expected.as_ref(),
                    );
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
        runtime.permissions.clear_after_turn(chat_id);
    }
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
