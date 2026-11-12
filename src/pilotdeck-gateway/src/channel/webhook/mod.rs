//! `src/adapters/channel/webhook/WebhookChannel.ts` (baseline cc7c4e6): the
//! channel with its own HTTP listener (`/health`, `POST /webhooks/:name`),
//! mounted on the #1327 channel protocol core. The listener reuses
//! [`crate::http_proxy::serve_connection_native_with_shutdown`], so the
//! Node close/keep-alive semantics and the frame-level presentation diffs
//! (header-name case, chunk-size case, the bare 400's `content-length: 0`)
//! carry over from the A8.4-NATIVE-HTTP-001 ledger family.
//!
//! The body read carries Node's default `requestTimeout` (v22.22.0: 300 s,
//! [`DEFAULT_REQUEST_TIMEOUT`]) as its only deadline: a request whose body
//! never completes is answered with a 408 and the connection closes, so
//! `stop()` stays bounded (probe
//! `_tmp/20261026-issue-1334/fix-r1/node-probe/probe4.out`). The deadline
//! covers the body read only, never the whole handler: a JSON body of
//! literal `null` never responds while the client stays connected (the TS
//! baseline, where the voided rejection writes no response, #1557), and a
//! client FIN now reclaims the connection instead of hanging `stop()`
//! forever (#1534 P0).
//!
//! The 408's frame-level presentation is registered as the draft (unapproved)
//! ledger entry `A8.3-CH-WEBHOOK-001` (`specs/gateway-parity/diffs/`): hyper
//! frames that 408's unknown-length empty body as `Transfer-Encoding:
//! chunked` + `0\r\n\r\n` where Node's raw `_http_server.js`
//! `requestTimeoutResponse` is 51 unframed bytes
//! (`crate::user_dialog::NODE_408_RESPONSE`) — the same hyper-public-API
//! presentation-diff class the A8.4-NATIVE-HTTP-001 entry registered for the
//! native lane. That entry states 「未批准，非已接受差异」: it is a record of
//! the pinned bytes, **not** an accepted diff, and it confers no grading
//! exemption until a human approves it after two non-author reviews.
//!
//! The remaining channel-specific residuals are *not registered* in the
//! gateway-parity diff ledger — adding an accepted entry needs human approval
//! and two non-author reviews, so these stay pending until then: the
//! failed-start error log carries the Rust bind error where TS prints Node's
//! (`Error: listen EADDRINUSE ...`), the message pipeline carries reply texts
//! as Rust `String`s (lone surrogates become U+FFFD; truncation stays
//! UTF-16-unit based), body text is Node `toString("utf8")` semantics, header
//! values are Latin-1 decoded like Node's parser, an invalid port is rejected
//! up front where Node throws inside `listen`, and the 408 above fires at
//! exactly its deadline where Node lands request timeouts on the 30 s
//! connections-checker grid (the same residual class the
//! A8.4-NATIVE-HTTP-002-R2 entry registered for the native lane's
//! headersTimeout).
mod hmac;
mod mapper;
mod render;
mod status;
#[cfg(test)]
mod tests;

pub use mapper::{MappedTurn, WebhookCommand, WebhookSessionMapper, WebhookSessionMapperState};

use std::{
    collections::{HashMap, HashSet},
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use http_body_util::BodyExt;
use hyper::body::Incoming;
use pilotdeck_protocol::{
    JsObject, JsString, JsValue, number::number_to_string, parse_json, stringify_json,
};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
};

use super::{
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    loader::js_truthy,
    protocol::{
        BoxFuture, ChannelAdapter, ChannelError, ChannelHandle, ChannelKey, ChannelLog,
        ChannelStartDeps, TurnGateway, TurnRequest,
    },
};
use crate::{Body, http_proxy, js_value, native_http};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: f64 = 8643.0;
const DEFAULT_RATE_LIMIT: f64 = 30.0;
/// #1941: also the `wecom_callback` listener's `POST *` cap; both lanes keep
/// the value in one place so the two body limits cannot drift.
pub(crate) const DEFAULT_MAX_BODY_BYTES: f64 = 1_048_576.0;
const INSECURE_NO_AUTH: &str = "__INSECURE_NO_AUTH__";
/// `checkRateLimit`'s sliding window, seconds.
const RATE_LIMIT_WINDOW: f64 = 60.0;
/// `pruneSeenDeliveries`/`pruneDeliveryInfo` cutoff, seconds.
const PRUNE_CUTOFF: f64 = 3600.0;
/// Node `http.createServer`'s default `requestTimeout` (v22.22.0: 300000 ms):
/// the bound on receiving the whole request from the client. Here it bounds
/// the body read only — the head is already bounded by the shared lane's
/// `headersTimeout`, and a whole-handler deadline would turn the TS `null`
/// -body hang (#1557) into a Rust divergence.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// `Date.now() / 1000` as the channel reads it; direct tests inject a fake.
pub type Now = Arc<dyn Fn() -> f64 + Send + Sync>;

fn default_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(seconds_from_duration)
        .unwrap_or_default()
}

/// `Date.now() / 1000`, evaluated on an explicit instant so the precision
/// contract can be asserted on fixed inputs.
///
/// `Date.now()` is integer milliseconds, so the quotient carries millisecond
/// precision and nothing finer: two deliveries that arrive inside the same
/// millisecond share a fallback id and therefore a dedup key. `as_secs_f64()`
/// would keep sub-millisecond bits and let those two requests produce
/// different keys, so quantize to the TS resolution.
fn seconds_from_duration(since: std::time::Duration) -> f64 {
    since.as_millis() as f64 / 1000.0
}

/// `WebhookChannelOptions`, the constructor's input after the TS `??` and
/// `Number()` coercions have run.
#[derive(Default)]
pub struct WebhookChannelOptions {
    pub port: Option<f64>,
    pub host: Option<String>,
    pub secret: Option<String>,
    pub routes: Option<JsObject>,
    pub rate_limit: Option<f64>,
    pub max_body_bytes: Option<f64>,
    pub mapper: Option<WebhookSessionMapper>,
    pub now: Option<Now>,
    /// The body-read deadline, [`DEFAULT_REQUEST_TIMEOUT`] when unset. TS
    /// exposes no such option (it is Node's server default, not a channel
    /// option); the field is the Rust tests' injection point, like `now`.
    pub request_timeout: Option<Duration>,
    /// Rust test seam (#2047): called once per connection the accept loop has
    /// taken out of the kernel backlog, before the connection task starts.
    /// `stop()` breaks the accept loop and drops the listener, and a socket
    /// still in the backlog at that moment is reset by the kernel — so a test
    /// that wants to observe the body-read deadline on its own connection has
    /// to know the connection was accepted first. Production leaves this
    /// `None`: no callback, no behavior change (the hook never touches the
    /// accept/stop paths, it only reports that `accept()` returned).
    pub accepted: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// The channel's mutable lifetime state, all behind one lock like the TS
/// instance fields. Nothing here is ever held across an await.
#[derive(Default)]
struct State {
    gateway: Option<Arc<dyn TurnGateway>>,
    logger: Option<ChannelLog>,
    server: Option<StopSlot>,
    bound: Option<std::net::SocketAddr>,
    delivery_info: HashMap<String, JsObject>,
    delivery_info_created: HashMap<String, f64>,
    seen_deliveries: HashMap<String, f64>,
    rate_counts: HashMap<String, Vec<f64>>,
    active_chats: Arc<Mutex<HashSet<String>>>,
}

struct StopSlot {
    stop: watch::Sender<bool>,
    accept: JoinHandle<io::Result<()>>,
}

struct Ctx {
    key: ChannelKey,
    host: String,
    port: f64,
    routes: JsObject,
    global_secret: String,
    rate_limit: f64,
    max_body_bytes: f64,
    request_timeout: Duration,
    now: Now,
    /// #2047 test observation point, `None` in production.
    accepted: Option<Arc<dyn Fn() + Send + Sync>>,
    mapper: Mutex<WebhookSessionMapper>,
    state: Mutex<State>,
}

pub struct WebhookChannel {
    ctx: Arc<Ctx>,
}

impl WebhookChannel {
    pub fn new(options: WebhookChannelOptions) -> Self {
        Self {
            ctx: Arc::new(Ctx {
                key: ChannelKey::new("webhook"),
                host: options.host.unwrap_or_else(|| DEFAULT_HOST.to_owned()),
                port: options.port.unwrap_or(DEFAULT_PORT),
                routes: options.routes.unwrap_or_default(),
                global_secret: options.secret.unwrap_or_default(),
                rate_limit: options.rate_limit.unwrap_or(DEFAULT_RATE_LIMIT),
                max_body_bytes: options.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES),
                request_timeout: options.request_timeout.unwrap_or(DEFAULT_REQUEST_TIMEOUT),
                now: options.now.unwrap_or_else(|| Arc::new(default_now)),
                accepted: options.accepted,
                mapper: Mutex::new(options.mapper.unwrap_or_else(|| {
                    WebhookSessionMapper::new(WebhookSessionMapper::random_uuid())
                })),
                state: Mutex::new(State::default()),
            }),
        }
    }

    /// `new WebhookChannel(options)`: the loader's option object, with the TS
    /// nullish-then-`Number()` coercions applied field by field.
    pub fn from_options(options: &JsObject) -> Self {
        let field = |name: &str| options.get(name).filter(|value| !nullish(value));
        let number =
            |name: &str| field(name).map(|value| js_value::to_number(value).unwrap_or(f64::NAN));
        Self::new(WebhookChannelOptions {
            port: number("port"),
            host: field("host").map(value_to_display_string),
            secret: field("secret").map(value_to_display_string),
            routes: field("routes").and_then(JsValue::as_object).cloned(),
            rate_limit: number("rateLimit"),
            max_body_bytes: number("maxBodyBytes"),
            mapper: None,
            now: None,
            request_timeout: None,
            accepted: None,
        })
    }

    /// The address the current listener is bound to, for tests and the host
    /// assembly. TS tests reach the port through the options; the Rust tree
    /// has no reflection on the Node `Server` object.
    pub fn bound_addr(&self) -> Option<std::net::SocketAddr> {
        self.ctx
            .state
            .lock()
            .expect("#1334 webhook state mutex")
            .bound
    }

    /// Inherent stop with the TS optional reason, used by the trait method and
    /// direct tests: `undefined` logs `no reason`.
    pub fn stop_with_reason(&self, reason: Option<&str>) -> BoxFuture<Result<(), ChannelError>> {
        let ctx = self.ctx.clone();
        let reason = reason
            .map(str::to_owned)
            .unwrap_or_else(|| "no reason".to_owned());
        Box::pin(async move {
            log(&ctx, "info", &format!("webhook: stopping ({reason})"));
            if crate::mutants::on("webhook-stop-keeps-listener") {
                // The stop still resolves, but the listener keeps serving.
                return Ok(());
            }
            let slot = {
                let mut state = ctx.state.lock().expect("#1334 webhook state mutex");
                state.bound = None;
                state.server.take()
            };
            let Some(slot) = slot else {
                return Ok(());
            };
            // `server.close()`: stop accepting, then wait for the served
            // connections, which `serve_connection_native_with_shutdown`
            // closes at their idle deadline or in-flight completion.
            let _ = slot.stop.send(true);
            slot.accept
                .await
                .map_err(|error| ChannelError::error(format!("webhook: stop failed: {error}")))?
                .map_err(|error| ChannelError::error(format!("webhook: stop failed: {error}")))
        })
    }
}

/// Loader mount: builds the adapter from the `webhook` loader option object
/// (`build_options` already forwarded `extra.port` / `extra.secret`;
/// [`WebhookChannel::from_options`] applies the TS constructor's
/// nullish-then-`Number()` coercions field by field). #2301 fix-r1: the TS
/// constructor's `Number(options.port ?? DEFAULT_PORT)` throws on a failed
/// toPrimitive, the loader's catch records the key and continues, and the
/// channel never enters the startup array — the mount applies the same check
/// at construction (`rateLimit` / `maxBodyBytes` never travel through the
/// loader literal, so `port` is the only coercion the loader can observe).
pub fn register(mounts: &mut super::loader::ChannelLoaderMounts) {
    mounts.register(
        "webhook",
        Arc::new(|options: &JsObject| {
            if let Some(port) = options.get("port").filter(|value| !nullish(value))
                && let Some(error) = crate::js_value::number_conversion_error(port)
            {
                return Err(ChannelError::Plain(error));
            }
            Ok(Arc::new(WebhookChannel::from_options(options)) as Arc<dyn ChannelAdapter>)
        }),
    );
}

fn log(ctx: &Ctx, level: &str, text: &str) {
    let logger = ctx
        .state
        .lock()
        .expect("#1334 webhook state mutex")
        .logger
        .clone();
    if let Some(logger) = logger {
        logger(level, text);
    }
}

/// `String(value)` for template interpolation; a too-deep array join keeps
/// the JS RangeError text.
fn value_to_display_string(value: &JsValue) -> String {
    js_value::display(value).unwrap_or_else(|_| js_value::StackOverflow::MESSAGE.to_owned())
}

/// `text.slice(0, count)`: UTF-16 code units, never the char count.
fn js_slice_prefix(text: &str, count: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    let end = count.min(units.len());
    String::from_utf16_lossy(&units[..end])
}

fn nullish(value: &JsValue) -> bool {
    matches!(value, JsValue::Undefined | JsValue::Null)
}

impl ChannelAdapter for WebhookChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.ctx.key
    }

    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        {
            let mut state = self.ctx.state.lock().expect("#1334 webhook state mutex");
            state.gateway = deps.turn_gateway;
            state.logger = deps.log;
        }
        // The TS synchronous loop over `Object.entries(this.routes)`: a route
        // without any secret resolves start with a no-op handle.
        for (name, route) in self.ctx.routes.own_entries() {
            let has_secret = route
                .get("secret")
                .is_some_and(|value| js_truthy(Some(value)))
                || !self.ctx.global_secret.is_empty();
            if !has_secret {
                let name = String::from_utf16_lossy(name.units());
                log(
                    &self.ctx,
                    "error",
                    &format!(
                        "webhook: route '{name}' has no HMAC secret. Set 'secret' on the route \
                         or globally. For testing without auth, set secret to \
                         '{INSECURE_NO_AUTH}'."
                    ),
                );
                let ctx = self.ctx.clone();
                return Box::pin(async {
                    Ok(Arc::new(WebhookHandle { ctx }) as Arc<dyn ChannelHandle>)
                });
            }
        }
        let ctx = self.ctx.clone();
        Box::pin(async move {
            let route_names = ctx
                .routes
                .own_entries()
                .iter()
                .map(|(name, _)| String::from_utf16_lossy(name.units()))
                .collect::<Vec<_>>()
                .join(", ");
            let route_names = if route_names.is_empty() {
                "(none configured)".to_owned()
            } else {
                route_names
            };
            match start_listener(&ctx).await {
                Ok((bound, stop, accept)) => {
                    {
                        let mut state = ctx.state.lock().expect("#1334 webhook state mutex");
                        state.bound = Some(bound);
                        state.server = Some(StopSlot { stop, accept });
                    }
                    log(
                        &ctx,
                        "info",
                        &format!(
                            "webhook: listening on http://{}:{} — routes: {route_names}",
                            ctx.host,
                            number_to_string(ctx.port)
                        ),
                    );
                }
                Err(error) => {
                    log(&ctx, "error", &format!("webhook: failed to start: {error}"));
                }
            }
            Ok(Arc::new(WebhookHandle { ctx }) as Arc<dyn ChannelHandle>)
        })
    }

    fn deliver_cron_result(&self, delivery: &CronResultDelivery) -> BoxFuture<bool> {
        let ctx = self.ctx.clone();
        let delivery = delivery.clone();
        Box::pin(async move {
            let send = {
                let ctx = ctx.clone();
                move |chat_id: JsString, text: JsString| {
                    let ctx = ctx.clone();
                    let chat_id = String::from_utf16_lossy(chat_id.units());
                    let text = String::from_utf16_lossy(text.units());
                    Box::pin(async move { deliver_reply(&ctx, &chat_id, &text).await })
                        as BoxFuture<bool>
                }
            };
            deliver_chat_cron_result(&delivery, &ctx.key, send).await
        })
    }
}

async fn start_listener(
    ctx: &Arc<Ctx>,
) -> io::Result<(
    std::net::SocketAddr,
    watch::Sender<bool>,
    JoinHandle<io::Result<()>>,
)> {
    let port = ctx.port;
    if !port.is_finite() || port.fract() != 0.0 || !(0.0..=65535.0).contains(&port) {
        return Err(io::Error::other(format!(
            "port must be an integer in 0..=65535, got {}",
            number_to_string(port)
        )));
    }
    let listener = TcpListener::bind((ctx.host.as_str(), port as u16)).await?;
    let bound = listener
        .local_addr()
        .map_err(|error| io::Error::other(format!("local address: {error}")))?;
    let (stop, stop_rx) = watch::channel(false);
    let accept = tokio::spawn(accept_loop(listener, ctx.clone(), stop_rx));
    Ok((bound, stop, accept))
}

async fn accept_loop(
    listener: TcpListener,
    ctx: Arc<Ctx>,
    mut stop: watch::Receiver<bool>,
) -> io::Result<()> {
    let mut connections: Vec<JoinHandle<()>> = Vec::new();
    loop {
        let socket = tokio::select! {
            biased;
            changed = stop.changed() => {
                match changed {
                    Ok(()) => break,
                    // The slot is dropped without a stop signal: nothing can
                    // restart the accept loop, so treat it as closed.
                    Err(_) => break,
                }
            }
            accepted = listener.accept() => match accepted {
                Ok((socket, _)) => socket,
                // Node emits the accept error on the server; without a
                // listener the process dies. Here the loop pauses briefly so
                // a persistent error cannot spin the core.
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        let stop_signal = stop.clone();
        let connection_ctx = ctx.clone();
        // #2047: the socket is out of the kernel backlog now, so this is the
        // instant after which dropping the listener can no longer reset it.
        // Production passes `None`.
        if let Some(accepted) = &ctx.accepted {
            accepted();
        }
        // #1534 P0: duplicate the accepted socket so the literal-null branch
        // can read the client's FIN itself. hyper 1.7.0's
        // `mid_message_detect_eof` returns `Pending` whenever its `read_buf`
        // still holds pipelined bytes, so after a follow-up request the
        // connection never observes the FIN and the `pending()` request
        // future has no other wake source; `stop()` then waits on it forever.
        let (socket, client_probe) = match duplicate_socket(socket) {
            Ok((socket, probe)) => (socket, Some(Arc::new(probe))),
            Err(error) => {
                log(
                    &ctx,
                    "error",
                    &format!("webhook: cannot duplicate the accepted socket: {error}"),
                );
                continue;
            }
        };
        let served = tokio::spawn(async move {
            let handler = {
                let ctx = connection_ctx.clone();
                move |request: hyper::Request<Incoming>,
                      _lease: Option<crate::http_proxy::ReadLease>| {
                    let ctx = ctx.clone();
                    let client_probe = client_probe.clone();
                    async move { handle_request(ctx, request, client_probe).await }
                }
            };
            // #1292 added the test-only `stop_observed` counter and the
            // `grid_anchor` deadline origin; the webhook listener needs
            // neither (its 5 s header deadline is not part of the parity grid).
            let _ = http_proxy::serve_connection_native_with_shutdown(
                socket,
                handler,
                stop_signal,
                None,
                None,
            )
            .await;
        });
        connections.push(served);
        connections.retain(|handle| !handle.is_finished());
    }
    // Closing one fd cannot disable a Linux listening socket while a forked
    // child still holds its CLOEXEC duplicate before exec. Disable the shared
    // socket at the existing listener-close point, then drain accepted peers.
    #[cfg(target_os = "linux")]
    let shutdown = socket2::SockRef::from(&listener).shutdown(std::net::Shutdown::Both);
    drop(listener);
    for connection in connections {
        let _ = connection.await;
    }
    #[cfg(target_os = "linux")]
    shutdown?;
    Ok(())
}

struct WebhookHandle {
    ctx: Arc<Ctx>,
}

impl ChannelHandle for WebhookHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        let ctx = self.ctx.clone();
        let reason = reason.to_owned();
        Box::pin(async move {
            let channel = WebhookChannel { ctx };
            channel.stop_with_reason(Some(&reason)).await
        })
    }
}

/// #1534: duplicate the accepted socket so the literal-`null` branch can read
/// the client's FIN itself — hyper 1.7.0's `mid_message_detect_eof` returns
/// `Pending` while its `read_buf` holds pipelined bytes. `try_clone` is
/// `dup(2)`, which shares the nonblocking open file description, so both
/// halves stay usable by tokio.
fn duplicate_socket(socket: TcpStream) -> io::Result<(TcpStream, TcpStream)> {
    let std_socket = socket.into_std()?;
    let std_probe = std_socket.try_clone()?;
    Ok((
        TcpStream::from_std(std_socket)?,
        TcpStream::from_std(std_probe)?,
    ))
}

async fn handle_request(
    ctx: Arc<Ctx>,
    request: hyper::Request<Incoming>,
    client_probe: Option<Arc<TcpStream>>,
) -> Result<hyper::Response<Body>, io::Error> {
    let version = request.version();
    Ok(finalize(
        route_request(ctx, request, client_probe).await?,
        version,
    ))
}

async fn route_request(
    ctx: Arc<Ctx>,
    request: hyper::Request<Incoming>,
    client_probe: Option<Arc<TcpStream>>,
) -> Result<hyper::Response<Body>, io::Error> {
    if let Some(rejection) = native_http::node_parser_admission(&request)? {
        return Ok(rejection);
    }
    let keep_alive = native_http::node_keep_alive(&request);
    // `new URL(req.url ?? "/", "http://<host header>")`. The WHATWG base host
    // never changes the pathname of an origin- or absolute-form target.
    let target = if request.uri().scheme().is_some() {
        request.uri().to_string()
    } else {
        request
            .uri()
            .path_and_query()
            .map_or_else(|| "/".to_owned(), |value| value.as_str().to_owned())
    };
    let path = native_http::whatwg_path(&target);
    if path == "/health" {
        let mut body = JsObject::new();
        body.insert("status".into(), JsValue::String("ok".into()));
        body.insert("platform".into(), JsValue::String("webhook".into()));
        return Ok(json_response(
            hyper::StatusCode::OK,
            &JsValue::Object(body),
            keep_alive,
        ));
    }
    if let Some(route_name) = path
        .strip_prefix("/webhooks/")
        .filter(|name| !name.is_empty() && !name.contains('/'))
        && (request.method() == hyper::Method::POST
            || crate::mutants::on("webhook-route-method-off"))
    {
        let route_name = route_name.to_owned();
        return handle_webhook(ctx, request, route_name, keep_alive, client_probe).await;
    }
    // `res.statusCode = 404; res.end("Not Found")` — no content-type.
    Ok(native_http::node_plain_response(
        hyper::StatusCode::NOT_FOUND,
        Some("Not Found".to_owned()),
        keep_alive,
    ))
}

/// Node answers every HTTP/1.0 request with a `Connection: close` response on
/// a closed socket, even one asking to keep alive; an explicit 1.0 response
/// version keeps hyper's `fix_keep_alive` in its HTTP/1.0 branch — the same
/// #1209 trick, whose hyper `HTTP/1.0` status line against Node's `HTTP/1.1`
/// is the accepted-diff entry A8.3-CH-WEBHOOK-004 (approved by its R1
/// correction, `A8.3-CH-WEBHOOK-004-R1`), the channel's own
/// record of the A8.4-NATIVE-HTTP-001 item 4 class. Without it `fix_keep_alive`
/// runs on the response as built (1.1), sees the request's keep-alive and
/// overwrites this channel's `Connection: close`, so the socket stays open —
/// the #1684 probe's divergence. The status line does not distinguish the two
/// paths: `enforce_version` rewrites it to `HTTP/1.0` for a 1.0 request either
/// way, which is why the `webhook-http10-finalize-off` mutation is caught by
/// the framing/close assertions rather than by the status-line assertion.
///
/// The body is also made unsized: Node's HTTP/1.0 replies are close-delimited
/// (no `content-length`, no chunking — the recorded `http10` cell), while
/// hyper frames a known-length body with `content-length` on any version.
/// With an unknown size hyper cannot chunk a 1.0 connection, so it writes the
/// body and closes, matching Node's bytes.
/// Sibling: this is the one lane list — the same lanes are tabulated in
/// `A8.3-CH-WEBHOOK-004`「范围」. The 1.0 version switch is kept in sync with
/// the `finalize` helpers in `src/module_host.rs`,
/// `src/module_host/approvals.rs` and `src/api_web.rs`, and with the inline 1.0
/// branches in `src/user_dialog/http_store.rs` and `src/native_http.rs`.
/// `src/channel/api_server/` (shared `src/http_proxy.rs` transport) has no
/// local 1.0 pin and no 1.0 golden cell — its 1.0 keep-alive and status line
/// are untested and tracked in #1688; the `sms` lane has none yet (#1913).
/// The unsized wrapper is webhook-only because this channel answers with
/// known-length bodies: `module_host`, `module_host/approvals` and `api_web`
/// reply from `streamed` (unknown size), which a 1.0 connection already frames
/// by closing, but a `Full` body would get a `content-length` from hyper that
/// Node's 1.0 replies do not carry. The lanes that still build a `Full` body
/// (`user_dialog/http_store.rs`, `native_http::handler_response_with_length`,
/// module_host's provider value body) have that framing difference unmeasured
/// per lane and tracked in #1947.
fn finalize(mut response: hyper::Response<Body>, version: hyper::Version) -> hyper::Response<Body> {
    if version != hyper::Version::HTTP_10 || crate::mutants::on("webhook-http10-finalize-off") {
        return response;
    }
    *response.version_mut() = hyper::Version::HTTP_10;
    if !crate::mutants::on("webhook-http10-framing-off") {
        let body = std::mem::replace(response.body_mut(), native_http::streamed(None));
        *response.body_mut() = Unsized(body).boxed();
    }
    response
}

/// A body whose size hyper cannot know (#1684): everything else delegates, so
/// the frames and the end-of-stream flag are unchanged.
struct Unsized(Body);

impl hyper::body::Body for Unsized {
    type Data = bytes::Bytes;
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

/// The response of `sendJson`: `JSON.stringify` under `application/json`.
fn json_response(
    status: hyper::StatusCode,
    body: &JsValue,
    keep_alive: bool,
) -> hyper::Response<Body> {
    let text = stringify_json(body).ok().flatten().unwrap_or_default();
    native_http::handler_response_with_length(status, "application/json", text, keep_alive)
}

/// `createWebhookErrorBody` with the webhook defaults (`scope: "http"`,
/// `source: "webhook"`).
fn webhook_http_error_body(
    event: &str,
    message: &str,
    code: &str,
    status: u16,
    type_name: Option<&str>,
    user_hint: &str,
) -> JsValue {
    let code = if crate::mutants::on("webhook-error-body-code") {
        format!("mutant_{code}")
    } else {
        code.to_owned()
    };
    status::create_agent_status_http_error_body(status::HttpErrorBodyInput {
        event: event.into(),
        event_name: event.into(),
        message: message.into(),
        code: Some(JsValue::String(code.into())),
        status: Some(status),
        r#type: type_name.map(JsValue::from),
        user_hint: Some(user_hint.into()),
        scope: "http",
        detail: None,
    })
}

/// A Node header read: joined duplicates, Latin-1 decoded like the parser.
fn header_string(headers: &hyper::HeaderMap, name: &str) -> Option<String> {
    headers.get(name)?;
    let values = headers.get_all(name);
    Some(
        values
            .iter()
            .map(|value| {
                value
                    .as_bytes()
                    .iter()
                    .map(|&byte| byte as char)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// `readRequestBody`: accumulate the request body, rejecting the moment the
/// running size crosses `max` (a NaN max never rejects, like the JS compare).
async fn read_request_body(body: Incoming, max: f64) -> io::Result<String> {
    let mut body = body;
    let mut total = 0.0f64;
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| io::Error::other("request body error"))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        total += data.len() as f64;
        if total > max {
            return Err(io::Error::other("payload too large"));
        }
        bytes.extend_from_slice(&data);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Node v22.22.0 `_http_server.js` `requestTimeoutResponse`: what the default
/// `requestTimeout` writes before closing a socket whose request body never
/// completes (probe `_tmp/20261026-issue-1334/fix-r1/node-probe/probe4.out`).
/// The body is close-terminated (`Connection: close`, no `content-length`);
/// hyper's public API frames an unknown-length HTTP/1.1 body as chunked, so
/// the wire is Node's 51 bytes plus the chunked framing. That residual is
/// registered per lane: `A8.3-CH-WEBHOOK-001` for the webhook listener and
/// `A8.3-CH-SMS-010` for the sms listener, whose bare `http1::Builder` also
/// writes lowercase header names and an implicit `date`.
pub(crate) fn node_request_timeout_response() -> hyper::Response<Body> {
    let mut response = hyper::Response::new(native_http::streamed(None));
    *response.status_mut() = hyper::StatusCode::REQUEST_TIMEOUT;
    response.headers_mut().insert(
        hyper::header::CONNECTION,
        "close".parse().expect("#1334 header"),
    );
    response
}

async fn handle_webhook(
    ctx: Arc<Ctx>,
    request: hyper::Request<Incoming>,
    route_name: String,
    keep_alive: bool,
    client_probe: Option<Arc<TcpStream>>,
) -> Result<hyper::Response<Body>, io::Error> {
    let headers = request.headers().clone();
    // `const route = this.routes[routeName]` walks the prototype chain, so a
    // name that is no own key can still resolve (#1682). An own key always
    // wins, including a falsy one (`{"main": null}` still answers 404).
    let Some(route) = ctx
        .routes
        .get(&route_name)
        .cloned()
        .or_else(|| prototype_route(&route_name).map(JsValue::Object))
    else {
        return Ok(unknown_route_response(&route_name, keep_alive));
    };
    if !js_truthy(Some(&route)) {
        return Ok(unknown_route_response(&route_name, keep_alive));
    }

    let now = (ctx.now)();
    if !crate::mutants::on("webhook-rate-limit-off") && !check_rate_limit(&ctx, &route_name, now) {
        return Ok(json_response(
            hyper::StatusCode::TOO_MANY_REQUESTS,
            &webhook_http_error_body(
                "webhook_rate_limited",
                "Rate limit exceeded",
                "rate_limit_exceeded",
                429,
                Some("rate_limit_error"),
                "Wait for the webhook rate limit window to reset, then retry.",
            ),
            keep_alive,
        ));
    }

    let body_read = read_request_body(request.into_body(), ctx.max_body_bytes);
    let body_text = if crate::mutants::on("webhook-body-read-no-timeout") {
        // The pre-fix behavior the negative control kills: no deadline on the
        // body read, so a partial body hangs the connection and `stop()`.
        body_read.await
    } else {
        match tokio::time::timeout(ctx.request_timeout, body_read).await {
            Ok(result) => result,
            Err(_) => return Ok(node_request_timeout_response()),
        }
    };
    let body_text = match body_text {
        Ok(text) => text,
        // The catch arm's 413 never reaches the wire: TS destroyed the socket
        // before the rejection continuation runs, so the response is the
        // zero-byte close this service error produces.
        Err(_) => {
            if crate::mutants::on("webhook-oversize-answers") {
                // Negative control: #1334 the oversize request closes without
                // a response. Answering instead of closing must turn the
                // `too-large` cell red.
                return Ok(json_response(
                    hyper::StatusCode::PAYLOAD_TOO_LARGE,
                    &JsValue::String("payload too large".to_owned().into()),
                    keep_alive,
                ));
            }
            return Err(io::Error::other("webhook payload too large"));
        }
    };

    let secret = route
        .get("secret")
        .filter(|value| js_truthy(Some(value)))
        .map(value_to_display_string)
        .unwrap_or_else(|| ctx.global_secret.clone());
    if secret != INSECURE_NO_AUTH {
        let signature = header_string(&headers, "x-hub-signature-256")
            .or_else(|| header_string(&headers, "x-signature-256"))
            .unwrap_or_default();
        if !hmac::verify_hmac(&body_text, &secret, &signature) {
            return Ok(json_response(
                hyper::StatusCode::UNAUTHORIZED,
                &webhook_http_error_body(
                    "webhook_invalid_signature",
                    "Invalid signature",
                    "invalid_signature",
                    401,
                    None,
                    "Check the webhook signing secret and signature header.",
                ),
                keep_alive,
            ));
        }
    }

    let body = match parse_json(&body_text) {
        Ok(value) => value,
        Err(_) => {
            let mut raw = JsObject::new();
            raw.insert("raw".into(), JsValue::String(body_text.into()));
            JsValue::Object(raw)
        }
    };

    let delivery_id = header_string(&headers, "x-delivery-id")
        .or_else(|| header_string(&headers, "x-github-delivery"))
        .unwrap_or_else(|| {
            if crate::mutants::on("webhook-delivery-id-fallback") {
                route_name.clone()
            } else {
                format!("{route_name}-{}", number_to_string(now))
            }
        });
    let duplicate = {
        let mut state = ctx.state.lock().expect("#1334 webhook state mutex");
        if state.seen_deliveries.contains_key(&delivery_id)
            && !crate::mutants::on("webhook-dedup-off")
        {
            true
        } else {
            state.seen_deliveries.insert(delivery_id.clone(), now);
            prune_seen_deliveries(&mut state, now);
            false
        }
    };
    if duplicate {
        let mut body = JsObject::new();
        body.insert("status".into(), JsValue::String("duplicate".into()));
        body.insert("delivery_id".into(), JsValue::String(delivery_id.into()));
        return Ok(json_response(
            hyper::StatusCode::OK,
            &JsValue::Object(body),
            keep_alive,
        ));
    }

    // `extractText` dereferences `body` on its first line; a JSON body of
    // literal `null` throws there, the voided rejection goes unhandled, and
    // no response is ever written. The socket hangs indefinitely while the
    // client stays connected, and so does `stop()`: Node's `requestTimeout`
    // (300 s) only covers a body that never completes, and this four-byte
    // body is already complete, so TS `server.close()` waits on the same
    // socket this future waits on (issue #1557).
    //
    // Measured on the fixed TS baseline (cc7c4e6, Node v22.22.0) through the
    // real `WebhookChannel.start()`: no response at 20 s, no keep-alive close
    // at 7 s, `handle.stop()` still pending 18 s in, process alive with the
    // listening port already refused (evidence
    // `_tmp/20261105-issue-1534/ts-probe/probeB-app-shutdown.mjs`,
    // `probeC-channel-isolated.mjs`, `probeD-long.mjs`). This is a replicated
    // baseline defect, not a Rust deviation, so the "no response while the
    // client is connected" behavior stays: responding would diverge from TS.
    // The `webhook-null-body-responds` mutant exists only so the negative
    // control can turn the hang into a 400 and watch the `noResponse` oracle
    // cell go red; it never runs in the shipped path.
    //
    // #1534 P0: staying pending is correct only *while the client is still
    // connected*. A client FIN must reclaim the connection, and hyper 1.7.0
    // will not read it: `mid_message_detect_eof` (`proto/h1/conn.rs`) returns
    // `Pending` whenever `read_buf` is non-empty, so a pipelined follow-up
    // request left in the buffer means the connection never observes the FIN,
    // the `pending()` below has no other wake source, and `stop()` waits on
    // this connection forever (measured: Rust hung, the same bytes/timing on
    // TS reclaimed the socket in +1 ms — `_tmp/20261106-1534-p0/verdict.md`).
    // The duplicated socket reaches EOF on that FIN without touching hyper's
    // buffered bytes (they belong to a request this connection will never
    // serve), which ends the connection exactly as TS `server.close()`
    // reclaims the same socket. There is no timeout: EOF is a real signal,
    // and while the client stays connected the read simply stays pending, so
    // the probeC/D baseline equivalence is unchanged. The probe spends task
    // budget per read so a client that streams without ever closing cannot
    // starve the runtime. The
    // `webhook-null-body-probe-off` mutant disables the probe so the negative
    // control can reproduce the hang.
    let text = if nullish(&body) {
        if crate::mutants::on("webhook-null-body-responds") {
            return Ok(json_response(
                hyper::StatusCode::BAD_REQUEST,
                &JsValue::Null,
                keep_alive,
            ));
        }
        if !crate::mutants::on("webhook-null-body-probe-off")
            && let Some(probe) = client_probe
        {
            let mut discard = [0u8; 4096];
            loop {
                probe.readable().await?;
                match probe.try_read(&mut discard) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "webhook: client closed after a literal-null body",
                        ));
                    }
                    Ok(_) => {
                        // Neither `readable()` nor `try_read()` draws on the
                        // task budget (tokio 1.48 `Registration::readiness` and
                        // `Registration::try_io` call no `coop::poll_proceed`),
                        // so a client that keeps this socket saturated would
                        // otherwise spin this task without ever yielding to the
                        // scheduler: measured on a current-thread runtime, a
                        // 10 ms ticker fell from 175 ticks/2 s to 17 with one
                        // stream and to ~1 tick/s with 32
                        // (`_tmp/20261106-issue-1534/coop-probe/`). Spend one
                        // unit per read so the loop yields once the budget runs
                        // out, exactly as hyper's own reads do.
                        tokio::task::coop::consume_budget().await;
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        return std::future::pending().await;
    } else {
        extract_text(&body)
    };
    // TS: `if (!text)`. Among strings only `""` is falsy (`"0"` and `"false"`
    // are truthy), and `null`/`undefined` already arrive as `None`; so filter
    // the empty string here. `webhook-empty-text-accepted` restores the
    // pre-#1681 behavior so the golden empty-string cell can turn red.
    let Some(text) =
        text.filter(|text| !text.is_empty() || crate::mutants::on("webhook-empty-text-accepted"))
    else {
        let mut body = JsObject::new();
        body.insert("status".into(), JsValue::String("ignored".into()));
        body.insert("reason".into(), JsValue::String("no text content".into()));
        return Ok(json_response(
            hyper::StatusCode::OK,
            &JsValue::Object(body),
            keep_alive,
        ));
    };

    let chat_id = format!("webhook:{route_name}:{delivery_id}");
    {
        let mut state = ctx.state.lock().expect("#1334 webhook state mutex");
        let mut info = JsObject::new();
        info.insert(
            "deliver".into(),
            route
                .get("deliver")
                .filter(|value| !nullish(value))
                .cloned()
                .unwrap_or_else(|| JsValue::String("log".into())),
        );
        info.insert("route".into(), JsValue::String(route_name.into()));
        // `...body`: the parsed value's own enumerable keys, arrays spread
        // as their index keys.
        match &body {
            JsValue::Object(object) => {
                for (key, value) in object.own_entries() {
                    info.insert(key.clone(), value.clone());
                }
            }
            JsValue::Array(items) => {
                for (index, value) in items.iter().enumerate() {
                    info.insert(index.to_string().into(), value.clone());
                }
            }
            _ => {}
        }
        state.delivery_info.insert(chat_id.clone(), info);
        state.delivery_info_created.insert(chat_id.clone(), now);
        prune_delivery_info(&mut state, now);
    }

    let mut accepted = JsObject::new();
    accepted.insert("status".into(), JsValue::String("accepted".into()));
    accepted.insert("delivery_id".into(), JsValue::String(delivery_id.into()));
    let response = json_response(
        hyper::StatusCode::OK,
        &JsValue::Object(accepted),
        keep_alive,
    );
    tokio::spawn(async move {
        if let Err(error) = handle_incoming(&ctx, &chat_id, text).await {
            log(
                &ctx,
                "error",
                &format!("webhook: handleIncoming error: {}", error.to_js_string()),
            );
        }
    });
    Ok(response)
}

/// What `this.routes[routeName]` resolves to when only the prototype chain has
/// it: an empty route object, i.e. every field `undefined`.
///
/// `this.routes` is `options.routes ?? {}` (`WebhookChannel.ts`), a plain
/// record whose `[[Prototype]]` is `Object.prototype`, so
/// `this.routes[routeName]` resolves every own property of `Object.prototype`
/// — the eleven methods to a function, and the `__proto__` accessor to
/// `Object.prototype` itself (not `null`; #1682 measured it). All of them are
/// truthy and none carries `secret` or `deliver`, so TS treats such a request
/// exactly like one for a route object with no fields — rate limit, body read,
/// HMAC against the **global** secret (`route.secret || this.globalSecret`),
/// `deliver ?? "log"`, then `accepted`. Rust's `JsObject` lookup is own-key
/// only, so without this fallback every one of them answered 404.
///
/// The name set is not restated here: it is the shared JS-semantics predicate
/// `config::builtin_property`, and `tests::object_prototype_names_match_the_measured_v8_set`
/// pins that predicate to the V8 set measured on the baseline. The oracle case
/// `object-prototype-route-names` (`tests/fixtures/webhook-channel/`) records
/// the end-to-end behaviour on the real TS source.
///
/// `webhook-prototype-routes-off` restores the pre-#1682 own-key-only lookup,
/// so the `object-prototype-route-names` cell turns red.
fn prototype_route(route_name: &str) -> Option<JsObject> {
    let inherited = crate::config::builtin_property(&JsString::from(route_name));
    (inherited && !crate::mutants::on("webhook-prototype-routes-off")).then(JsObject::new)
}

fn unknown_route_response(route_name: &str, keep_alive: bool) -> hyper::Response<Body> {
    json_response(
        hyper::StatusCode::NOT_FOUND,
        &webhook_http_error_body(
            "webhook_unknown_route",
            &format!("Unknown route: {route_name}"),
            "unknown_route",
            404,
            None,
            "Check the webhook route name and retry.",
        ),
        keep_alive,
    )
}

/// `extractText`, in the TS statement order.
fn extract_text(body: &JsValue) -> Option<String> {
    let fields: &[&str] = if crate::mutants::on("webhook-extract-order") {
        &["message", "content", "body"]
    } else {
        &["text", "message", "content", "body"]
    };
    for field in fields {
        if let Some(text) = body.get(field).and_then(JsValue::as_string) {
            return Some(String::from_utf16_lossy(text.units()));
        }
    }
    let action = body.get("action");
    let issue = body.get("issue");
    if js_truthy(action)
        && js_truthy(issue)
        && issue
            .and_then(|issue| issue.get("body"))
            .and_then(JsValue::as_string)
            .is_some()
    {
        let action = value_to_display_string(action.expect("#1334 truthy action"));
        let title = issue
            .and_then(|issue| issue.get("title"))
            .map(value_to_display_string)
            .unwrap_or_else(|| "undefined".to_owned());
        let issue_body = issue
            .and_then(|issue| issue.get("body"))
            .and_then(JsValue::as_string)
            .map(|text| String::from_utf16_lossy(text.units()))
            .unwrap_or_default();
        return Some(format!("GitHub {action}: {title}\n{issue_body}"));
    }
    if js_truthy(action) && js_truthy(body.get("pull_request")) {
        let pull = body.get("pull_request").expect("#1334 truthy pull request");
        let action = value_to_display_string(action.expect("#1334 truthy action"));
        let title = pull
            .get("title")
            .map(value_to_display_string)
            .unwrap_or_else(|| "undefined".to_owned());
        let pull_body = match pull.get("body") {
            None | Some(JsValue::Undefined) | Some(JsValue::Null) => String::new(),
            Some(value) => value_to_display_string(value),
        };
        return Some(format!("GitHub PR {action}: {title}\n{pull_body}"));
    }
    let comment = body.get("comment");
    if js_truthy(comment)
        && comment
            .and_then(|comment| comment.get("body"))
            .and_then(JsValue::as_string)
            .is_some()
    {
        let body = comment
            .and_then(|comment| comment.get("body"))
            .and_then(JsValue::as_string)
            .map(|text| String::from_utf16_lossy(text.units()))
            .unwrap_or_default();
        return Some(format!("Comment: {body}"));
    }
    let serialized = stringify_json(body).ok().flatten()?;
    if serialized.encode_utf16().count() > 5 && serialized != "{}" {
        Some(serialized)
    } else {
        None
    }
}

/// `checkRateLimit`: a rejected request does not touch the stored timestamps,
/// because TS filters a local copy before the length test.
fn check_rate_limit(ctx: &Ctx, route_name: &str, now: f64) -> bool {
    let mut state = ctx.state.lock().expect("#1334 webhook state mutex");
    let mut timestamps = state
        .rate_counts
        .get(route_name)
        .cloned()
        .unwrap_or_default();
    if crate::mutants::on("webhook-rate-window") {
        timestamps.retain(|&stamp| stamp >= now - RATE_LIMIT_WINDOW);
    } else {
        timestamps.retain(|&stamp| stamp > now - RATE_LIMIT_WINDOW);
    }
    if timestamps.len() as f64 >= ctx.rate_limit {
        return false;
    }
    timestamps.push(now);
    state.rate_counts.insert(route_name.to_owned(), timestamps);
    true
}

fn prune_seen_deliveries(state: &mut State, now: f64) {
    let cutoff = now - PRUNE_CUTOFF;
    if crate::mutants::on("webhook-prune-boundary") {
        state.seen_deliveries.retain(|_, stamp| *stamp > cutoff);
    } else {
        state.seen_deliveries.retain(|_, stamp| *stamp >= cutoff);
    }
}

fn prune_delivery_info(state: &mut State, now: f64) {
    let cutoff = now - PRUNE_CUTOFF;
    let stale: Vec<String> = state
        .delivery_info_created
        .iter()
        .filter(|(_, created)| **created < cutoff)
        .map(|(chat_id, _)| chat_id.clone())
        .collect();
    for chat_id in stale {
        state.delivery_info_created.remove(&chat_id);
        state.delivery_info.remove(&chat_id);
    }
}

/// `handleIncoming`, the fire-and-forget turn pipeline.
async fn handle_incoming(ctx: &Ctx, chat_id: &str, text: String) -> Result<(), ChannelError> {
    let active_chats = {
        let state = ctx.state.lock().expect("#1334 webhook state mutex");
        state.active_chats.clone()
    };
    let already_active = active_chats
        .lock()
        .expect("#1738 webhook active chats mutex")
        .contains(chat_id);
    if already_active {
        log(
            ctx,
            "info",
            &format!("webhook: chat {chat_id} already active, skipping"),
        );
        return Ok(());
    }
    let mapped = ctx
        .mapper
        .lock()
        .expect("#1334 mapper mutex")
        .resolve(chat_id, &text);
    if mapped.command == Some(WebhookCommand::New) && mapped.message.is_empty() {
        let message = if crate::mutants::on("webhook-new-session-message") {
            "会话已重置。"
        } else {
            "已创建新会话。"
        };
        deliver_reply(ctx, chat_id, message).await;
        return Ok(());
    }
    if mapped.message.is_empty() {
        return Ok(());
    }
    // #1738: the `Drop` guard replaces the paired insert/remove so a panic in
    // `process_message` (spawned fire-and-forget, so only the task dies) still
    // releases the slot — the TS `finally { delete }` both baselines have. The
    // guard must outlive the `.await` below, hence the named binding.
    //
    // Admission is this `insert`, not the `contains` above: the two are
    // separate lock sections with `mapper.resolve` and the `/new` reply in
    // between, so two concurrent turns of one chat can both pass the check —
    // and the first to finish would then drop the slot the other still holds.
    // TS cannot race here (one event loop), and its `add` sits at this same
    // point (after the `/new` and empty-message early returns), so the order
    // stays; only the decision is taken from the insert itself.
    //
    // Deliberately unlike `api_server::admit_chat`, which admits *before*
    // `resolve`: admitting here any earlier would hold the chat across the
    // `/new` reply, where TS has not added it yet. The residual window — a
    // turn rejected here has already run `resolve`, so a `/new` in it may
    // have re-pointed the chat's session — is pre-existing and tracked in
    // #1744.
    let Some(_guard) = super::ActiveChat::admit(active_chats, chat_id.to_owned()) else {
        log(
            ctx,
            "info",
            &format!("webhook: chat {chat_id} already active, skipping"),
        );
        return Ok(());
    };
    process_message(ctx, chat_id, &mapped.session_key, &mapped.message).await
}

/// `processMessage`: submit the turn, render the visible fragments, deliver
/// the trimmed reply.
async fn process_message(
    ctx: &Ctx,
    chat_id: &str,
    session_key: &str,
    message: &str,
) -> Result<(), ChannelError> {
    let gateway = {
        let state = ctx.state.lock().expect("#1334 webhook state mutex");
        state.gateway.clone()
    };
    let Some(gateway) = gateway else {
        let message = if crate::mutants::on("webhook-gateway-missing-hint") {
            "Gateway absent"
        } else {
            "Gateway not ready"
        };
        deliver_status_reply(
            ctx,
            chat_id,
            "gateway_unavailable",
            message,
            "gateway_unavailable",
            "preflight",
            "Start or reconnect the PilotDeck gateway, then retry.",
        )
        .await;
        return Ok(());
    };

    let mut reply_text = String::new();
    // The TS flag lives outside the loop: a visible failure keeps later
    // `error` events out of the reply.
    let mut has_visible_failure_status = false;
    // TS awaits `gateway.submitTurn` inside the same try as the per-turn
    // iteration, so a rejected submit lands in the same error path.
    let outcome: Result<(), ChannelError> = match gateway
        .submit(TurnRequest {
            session_key: JsString::from(session_key),
            channel_key: ctx.key.clone(),
            message: JsString::from(message),
        })
        .await
    {
        Err(error) => Err(error),
        Ok(mut stream) => loop {
            match stream.next().await {
                Ok(Some(event)) => {
                    if is_visible_failure_gateway_event(&event) {
                        has_visible_failure_status = true;
                    }
                    let is_error = event
                        .get("type")
                        .is_some_and(|kind| kind == &JsValue::String("error".into()));
                    if is_error && has_visible_failure_status {
                        continue;
                    }
                    if let Some(fragment) = render::render_webhook_event(&event) {
                        reply_text.push_str(&fragment);
                    }
                }
                Ok(None) => break Ok(()),
                Err(error) => break Err(error),
            }
        },
    };
    if let Err(error) = outcome {
        log(
            ctx,
            "error",
            &format!("webhook: submitTurn error: {}", error.to_js_string()),
        );
        let status_event = webhook_status_event(
            "channel_submit_failed",
            "Failed to process this message. Please retry.",
            "channel_submit_failed",
            "channel",
            "PilotDeck failed before this webhook turn could finish. Retry the delivery; if it \
             repeats, check webhook and gateway logs.",
        );
        reply_text = render::render_webhook_event(&status_event)
            .unwrap_or_else(|| "Failed to process this message. Please retry.".to_owned());
    }
    let final_text = crate::metadata::js_trim(&reply_text).to_owned();
    if !final_text.is_empty() {
        deliver_reply(ctx, chat_id, &final_text).await;
    }
    Ok(())
}

/// `isVisibleFailureGatewayEvent`.
fn is_visible_failure_gateway_event(event: &JsValue) -> bool {
    let is_agent_status = event
        .get("type")
        .is_some_and(|kind| kind == &JsValue::String("agent_status".into()));
    is_agent_status && status::is_visible_failure_status_detail(event.get("detail"))
}

/// `createWebhookStatusEvent` for the status replies.
fn webhook_status_event(
    event: &str,
    message: &str,
    code: &str,
    scope: &'static str,
    user_hint: &str,
) -> JsValue {
    let detail = status::create_visible_error_status_detail(status::StatusDetailInput {
        message: message.into(),
        message_i18n: None,
        code: Some(code.into()),
        severity: "error",
        user_hint: Some(user_hint.into()),
        user_hint_i18n: None,
        scope,
        source: "webhook",
        detail: None,
    });
    let mut object = JsObject::new();
    object.insert("type".into(), JsValue::String("agent_status".into()));
    object.insert("event".into(), JsValue::String(event.into()));
    object.insert("detail".into(), JsValue::Object(detail));
    JsValue::Object(object)
}

/// `deliverStatusReply`.
async fn deliver_status_reply(
    ctx: &Ctx,
    chat_id: &str,
    event: &str,
    message: &str,
    code: &str,
    scope: &'static str,
    user_hint: &str,
) {
    let status_event = webhook_status_event(event, message, code, scope, user_hint);
    let text = render::render_webhook_event(&status_event)
        .map(|rendered| crate::metadata::js_trim(&rendered).to_owned())
        .filter(|trimmed| !trimmed.is_empty())
        .unwrap_or_else(|| message.to_owned());
    deliver_reply(ctx, chat_id, &text).await;
}

/// `deliverReply`: the log deliverer and the "anything else is not taken"
/// boolean, with the TS slice bounds.
async fn deliver_reply(ctx: &Ctx, chat_id: &str, text: &str) -> bool {
    let deliver = {
        let state = ctx.state.lock().expect("#1334 webhook state mutex");
        state
            .delivery_info
            .get(chat_id)
            .and_then(|info| info.get("deliver").cloned())
    };
    let deliver = deliver.filter(|value| !nullish(value));
    let deliver_type = deliver
        .as_ref()
        .map(value_to_display_string)
        .unwrap_or_else(|| "log".to_owned());
    if deliver_type == "log" {
        log(
            ctx,
            "info",
            &format!(
                "webhook: response for {chat_id}: {}",
                js_slice_prefix(
                    text,
                    if crate::mutants::on("webhook-log-truncation-200") {
                        250
                    } else {
                        200
                    }
                )
            ),
        );
        true
    } else {
        log(
            ctx,
            "info",
            &format!(
                "webhook: deliver type '{deliver_type}' for {chat_id}: {}",
                js_slice_prefix(
                    text,
                    if crate::mutants::on("webhook-log-truncation-100") {
                        150
                    } else {
                        100
                    }
                )
            ),
        );
        false
    }
}
