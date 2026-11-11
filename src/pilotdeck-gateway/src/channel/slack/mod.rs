//! A8.3 slack channel: port of `src/adapters/channel/slack/SlackChannel.ts`
//! on the #1327 lifecycle. The TS adapter speaks Socket Mode through
//! `@slack/bolt`, whose surface the baseline only touches in seven members —
//! the #1786 stub-contract gold master (`tests/fixtures/channel-slack/`)
//! pins exactly that wire shape, so this port implements the client side of
//! the contract natively: `apps.connections.open` → WS connect →
//! `auth.test`, inbound `message` frames, `chat.postMessage` replies, and the
//! client-initiated `close(1000)` on stop.
//!
//! Native production mounting reads the common A7 `deps.turn_gateway` and
//! shares its interaction coordinator; constructor gateway injection remains
//! available for the original isolated replay tests.
//!
//! The baseline's `@slack/bolt not installed` branch has no Rust counterpart
//! (the module is always present — a product enhancement, registered in the
//! parity ledger as A8.3-CH-SLACK-001, 范围 item 3).
pub mod mapper;
pub mod render;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicU64, Ordering},
};

use futures_util::{SinkExt, StreamExt};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

use super::protocol::{
    BoxFuture, ChannelAdapter, ChannelError, ChannelHandle, ChannelKey, ChannelStartDeps,
};
use super::sms::{
    AnswerGuard, BeginAnswer, DecisionReply, GatewayRef, ImElicitationHelper, ImPermissionHelper,
    InitialPromptGuard, PermissionAnswer, PermissionDecision, PermissionState, PromptGuard,
    SubmitTurnRequest, elicitation as sms_elicitation, permission as sms_permission,
};

pub use super::sms::EnvLookup;
pub use mapper::{MappedSession, SlackSessionMapper, SlackSessionMapperState};

/// `MAX_MESSAGE_LENGTH` (`SlackChannel.ts:16`).
const MAX_MESSAGE_LENGTH: usize = 39_000;
/// The SDK REST base the stub contract speaks; production Slack traffic.
const DEFAULT_BASE_URL: &str = "https://slack.com";
/// The stub contract's WS failure text: the committed `bolt-stub.mjs`
/// rejects `start()` with `new Error("websocket error")`, so the native
/// client reports the same bytes (only observable when the endpoint refuses
/// the socket — no golden cell reaches it).
const WEBSOCKET_ERROR: &str = "Error: websocket error";

/// The socket-mode session a live `this.app` holds: the WS send half, used
/// only by `stop()`'s `close(1000)`. `None` until the socket opens — a failed
/// `connections.open` or handshake leaves `app` empty here, where the TS
/// instance keeps `this.app` set after `new BoltApp(...)` threw; the
/// divergence is unobservable without a socket (no inbound frames reach
/// `sendReply`) and is registered in A8.3-CH-SLACK-001 范围 item 6. That item
/// reached its decision point with the #2301 production wiring
/// (`production_loader_mounts` + `create_startup_configured`): the R1
/// correction keeps the divergence as-is — the wiring has no retry / re-entry
/// path, so the two states still cannot be told apart.
struct LiveApp {
    sink: Option<
        futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    >,
}

struct SlackRuntime {
    mapper: SlackSessionMapper,
    elicitation: ImElicitationHelper,
    active_chats: Arc<StdMutex<HashSet<JsString>>>,
    /// `this.app`: non-null from the `new BoltApp(...)` assignment until the
    /// real handle's `stop()` clears it.
    app: Option<LiveApp>,
    /// `this.botUserId`: the `auth.test` `user_id`, `null` before and after.
    bot_user_id: JsValue,
}

#[derive(Clone)]
struct Boundaries {
    logger: Option<super::protocol::ChannelLog>,
    gateway: Option<GatewayRef>,
}

struct SlackShared {
    generation: AtomicU64,
    stopping: tokio::sync::watch::Sender<bool>,
    readers: StdMutex<Vec<tokio::task::JoinHandle<()>>>,
    boundaries: StdMutex<Boundaries>,
    runtime: tokio::sync::Mutex<SlackRuntime>,
    /// The permission state machine behind its own `std` mutex, the #1790
    /// shape the sms adapter uses: short synchronous sections, never held
    /// across an await, so the `Drop` guards can release on panic/cancel.
    permissions: PermissionState,
    /// The resolved `this.botToken` / `this.appToken` (constructor `??` chain
    /// already applied — the values are immutable afterwards).
    bot_token: JsValue,
    app_token: JsValue,
    /// The REST origin: `https://slack.com` in production, the loopback
    /// mock in tests.
    base_url: JsString,
    http: super::http_client::HttpClient,
}

impl SlackShared {
    fn log(&self, level: &str, message: String) {
        let logger = self
            .boundaries
            .lock()
            .expect("#1344 boundaries mutex")
            .logger
            .clone();
        if let Some(logger) = logger {
            logger(level, &message);
        }
    }
}

/// The TS failure returns `{ stop: async () => undefined }` — a silent handle.
struct NoopHandle;
impl ChannelHandle for NoopHandle {
    fn stop(&self, _reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}

struct SlackHandle {
    shared: Arc<SlackShared>,
}

impl ChannelHandle for SlackHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        let shared = self.shared.clone();
        let reason = reason.to_owned();
        Box::pin(async move {
            shared.log("info", format!("slack: stopping ({reason})"));
            shared.generation.fetch_add(1, Ordering::SeqCst);
            shared.stopping.send_replace(true);
            let readers = std::mem::take(&mut *shared.readers.lock().expect("Slack reader mutex"));
            // Cancel an ACK/reconnect before taking runtime: an ACK may be
            // blocked writing while holding its short-lived sink guard.
            for reader in &readers {
                reader.abort();
            }
            for reader in readers {
                let _ = reader.await;
            }
            // The sink is taken out before any `await` on it: holding the
            // runtime mutex across `send(Close)` would stall every reader
            // frame and `send_reply` gate on a peer that stopped reading
            // (a full send buffer parks the send forever).
            let sink = {
                let mut runtime = shared.runtime.lock().await;
                runtime.bot_user_id = JsValue::Null;
                runtime.app.take().and_then(|app| app.sink)
            };
            if let Some(mut sink) = sink {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    sink.send(Message::Close(Some(
                        tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: 1000.into(),
                            reason: String::new().into(),
                        },
                    ))),
                )
                .await;
            }
            Ok(())
        })
    }
}

/// `SlackChannelOptions`: the constructor inputs. `botToken`/`appToken` are
/// the loader's option values (`undefined` when the config omits them); the
/// `?? process.env` chain runs in [`SlackChannel::new`], like the TS
/// constructor.
pub struct SlackChannelOptions {
    pub bot_token: Option<JsValue>,
    pub app_token: Option<JsValue>,
    pub mapper: Option<SlackSessionMapper>,
    /// The A7 gateway the TS start deps would have carried.
    pub gateway: Option<GatewayRef>,
    /// `process.env` replacement; `None` means no variable is ever set.
    pub env: Option<EnvLookup>,
    /// The REST origin; `None` is `https://slack.com`, followed by `/api/…`. This is
    /// the stub-contract seam the tests point at the loopback mock (the TS
    /// side's equivalent is the stub's `PDRS_SLACK_MOCK` variable).
    pub base_url: Option<JsString>,
}

pub struct SlackChannel {
    responder: Option<Arc<dyn crate::channel::bluebubbles::InteractionResponder>>,
    key: ChannelKey,
    shared: Arc<SlackShared>,
}

impl SlackChannel {
    pub fn with_responder(
        mut self,
        responder: Arc<dyn crate::channel::bluebubbles::InteractionResponder>,
    ) -> Self {
        self.responder = Some(responder);
        self
    }
    pub fn new(options: SlackChannelOptions) -> Self {
        let SlackChannelOptions {
            bot_token,
            app_token,
            mapper,
            gateway,
            env,
            base_url,
        } = options;
        let env_value = |name: &str| {
            env.as_ref()
                .and_then(|lookup| lookup(name))
                .map(JsValue::String)
                .unwrap_or(JsValue::Undefined)
        };
        let nullish = |value: &JsValue| matches!(value, JsValue::Undefined | JsValue::Null);
        // `options.botToken ?? process.env.SLACK_BOT_TOKEN` — one `??`, then
        // the stored value is used as-is (truthiness is checked in start).
        let bot_token = match bot_token {
            Some(value) if !nullish(&value) => value,
            _ => env_value("SLACK_BOT_TOKEN"),
        };
        let app_token = match app_token {
            Some(value) if !nullish(&value) => value,
            _ => env_value("SLACK_APP_TOKEN"),
        };
        Self {
            responder: None,
            key: ChannelKey::new("slack"),
            shared: Arc::new(SlackShared {
                generation: AtomicU64::new(0),
                stopping: tokio::sync::watch::channel(false).0,
                readers: StdMutex::new(Vec::new()),
                boundaries: StdMutex::new(Boundaries {
                    logger: None,
                    gateway,
                }),
                runtime: tokio::sync::Mutex::new(SlackRuntime {
                    mapper: mapper.unwrap_or_default(),
                    elicitation: ImElicitationHelper::default(),
                    active_chats: Arc::new(StdMutex::new(HashSet::new())),
                    app: None,
                    bot_user_id: JsValue::Null,
                }),
                permissions: Arc::new(StdMutex::new(ImPermissionHelper::default())),
                bot_token,
                app_token,
                base_url: base_url.unwrap_or_else(|| JsString::from(DEFAULT_BASE_URL)),
                http: super::http_client::HttpClient::new(),
            }),
        }
    }
}

impl ChannelAdapter for SlackChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }

    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let shared = self.shared.clone();
        let responder = self.responder.clone();
        let owns_transport = responder.is_some();
        Box::pin(async move {
            if let (Some(turns), Some(responder)) = (deps.turn_gateway, responder) {
                let mut boundaries = shared.boundaries.lock().expect("#1344 boundaries mutex");
                if boundaries.gateway.is_none() {
                    boundaries.gateway = Some(Arc::new(
                        super::sms::gateway::MountedSmsGateway::new(turns, responder),
                    ));
                }
            }
            if let Some(logger) = deps.log {
                shared
                    .boundaries
                    .lock()
                    .expect("#1344 boundaries mutex")
                    .logger = Some(logger);
            }
            // `if (!this.botToken)` — truthiness, so an empty string is also
            // "not set".
            if !super::sms::truthy(&shared.bot_token) {
                shared.log("error", "slack: SLACK_BOT_TOKEN not set".to_owned());
                return Ok(Arc::new(NoopHandle) as Arc<dyn ChannelHandle>);
            }
            if !super::sms::truthy(&shared.app_token) {
                shared.log(
                    "error",
                    "slack: SLACK_APP_TOKEN not set (required for Socket Mode)".to_owned(),
                );
                return Ok(Arc::new(NoopHandle) as Arc<dyn ChannelHandle>);
            }
            // The TS `try` block: `new BoltApp` (the config is already the
            // shared fields), `event`/`error` registration (the reader task),
            // `app.start()` (connections.open + WS), `auth.test`, the connected
            // log. A failure logs `slack: start failed: ${e}` and returns the
            // empty handle. TS keeps `this.app` set on the failed-start path;
            // the port leaves `runtime.app` set only when the failure landed
            // after the WS opened (auth.test and later), and empty for a
            // failed `connections.open` / handshake — the registered
            // divergence (A8.3-CH-SLACK-001 范围 item 6).
            let generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
            shared.stopping.send_replace(false);
            if let Err(error) = start_session(&shared, generation).await {
                shared.log("error", format!("slack: start failed: {error}"));
                // Native assembly owns every transport it starts. Preserve the
                // explicit legacy constructor's recorded no-op handle, while
                // its native mounted form releases a partially opened socket.
                if owns_transport {
                    SlackHandle {
                        shared: shared.clone(),
                    }
                    .stop("start-failed")
                    .await?;
                }
                return Ok(Arc::new(NoopHandle) as Arc<dyn ChannelHandle>);
            }
            Ok(Arc::new(SlackHandle { shared }) as Arc<dyn ChannelHandle>)
        })
    }
}

/// One JSON POST through the shared outbound client; the body string is the
/// stub contract's own `JSON.stringify` text, so key order is byte-compared.
async fn post_json(shared: &SlackShared, path: &str, body: &JsValue) -> Result<JsValue, String> {
    let text = stringify_json(body)
        .map_err(|_| "SyntaxError: JSON stringify failure".to_owned())?
        .ok_or("JSON.stringify returned undefined")?;
    let url = endpoint_url(&shared.base_url, path);
    let token = super::sms::js_string_of(body.get("token").unwrap_or(&shared.bot_token));
    let token = String::from_utf16_lossy(token.units());
    let response = shared
        .http
        .request_bearer(
            "POST",
            &url,
            Some("application/json"),
            &token,
            &text,
            super::http_client::UNDICI_DEFAULT_TIMEOUT,
        )
        .await?;
    let payload = String::from_utf8_lossy(&response.body);
    pilotdeck_protocol::parse_json(&payload)
        .map_err(|_| "SyntaxError: JSON parse failure".to_owned())
}

/// `app.start()` + `auth.test`: the exact TS order — connections.open, WS
/// open (handlers already registered), auth.test, the connected log.
async fn start_session(shared: &Arc<SlackShared>, generation: u64) -> Result<(), String> {
    if !is_current_transport(shared, generation) {
        return Err("Slack transport stopped".into());
    }
    // `app.start()`: POST `/api/apps.connections.open` with `{"token":appToken}`.
    let mut open_body = JsObject::new();
    open_body.insert("token".into(), shared.app_token.clone());
    let open = post_json(
        shared,
        "/api/apps.connections.open",
        &JsValue::Object(open_body),
    )
    .await?;
    if !super::sms::truthy(&open.get("ok").cloned().unwrap_or(JsValue::Undefined)) {
        return Err(format!(
            "Error: {}",
            String::from_utf16_lossy(
                super::sms::js_string_of(&open.get("error").cloned().unwrap_or(JsValue::Undefined))
                    .units()
            )
        ));
    }
    let url = super::sms::js_string_of(&open.get("url").cloned().unwrap_or(JsValue::Undefined));
    // The socket opens with the message/error handlers already registered —
    // a frame that lands mid-`auth.test` dispatches with `botUserId = null`.
    // The handshake is deadline-bounded: a black-holed WS endpoint would
    // otherwise park `start()` forever (the TS `ws` handshake bound is
    // SDK-internal and unmeasured — A8.3-CH-SLACK-001 item 2 — so the
    // repo-measured JS exchange bound is used; no oracle case reaches it).
    let (ws, _response) = tokio::time::timeout(
        super::http_client::UNDICI_DEFAULT_TIMEOUT,
        connect_async(String::from_utf16_lossy(url.units())),
    )
    .await
    .map_err(|_| WEBSOCKET_ERROR.to_owned())?
    .map_err(|_| WEBSOCKET_ERROR.to_owned())?;
    let (sink, stream) = ws.split();
    {
        let mut runtime = shared.runtime.lock().await;
        if !is_current_transport(shared, generation) {
            return Err("Slack transport stopped".into());
        }
        runtime.app = Some(LiveApp { sink: Some(sink) });
    }
    let (ready, ready_rx) = tokio::sync::watch::channel(false);
    spawn_reader(shared.clone(), stream, generation, ready_rx);
    // `app.client.auth.test({token})`: POST `/api/auth.test`.
    let mut auth_body = JsObject::new();
    auth_body.insert("token".into(), shared.bot_token.clone());
    let auth = post_json(shared, "/api/auth.test", &JsValue::Object(auth_body)).await?;
    if !super::sms::truthy(&auth.get("ok").cloned().unwrap_or(JsValue::Undefined)) {
        return Err(format!(
            "Error: {}",
            String::from_utf16_lossy(
                super::sms::js_string_of(&auth.get("error").cloned().unwrap_or(JsValue::Undefined))
                    .units()
            )
        ));
    }
    let user_id = auth.get("user_id").cloned().unwrap_or(JsValue::Undefined);
    let connected_as = auth
        .get("user")
        .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
        .cloned()
        .unwrap_or_else(|| user_id.clone());
    {
        let mut runtime = shared.runtime.lock().await;
        if !is_current_transport(shared, generation) {
            return Err("Slack transport stopped".into());
        }
        runtime.bot_user_id = user_id;
    }
    ready.send_replace(true);
    shared.log(
        "info",
        format!(
            "slack: Socket Mode connected as {}",
            String::from_utf16_lossy(super::sms::js_string_of(&connected_as).units())
        ),
    );
    Ok(())
}

/// The WS read loop: one text frame `{"type":"message","payload":{"event":…}}`
/// dispatches `handle_message` as its own task (the async listener the stub
/// registers — a frame is never blocked by a previous frame's turn). The
/// error handler contract is the stub's: a transport failure reports
/// `Error: websocket error` through `app.error`.
fn spawn_reader(
    shared: Arc<SlackShared>,
    mut stream: futures_util::stream::SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    generation: u64,
    mut ready: tokio::sync::watch::Receiver<bool>,
) {
    let owner = shared.clone();
    let task = tokio::spawn(async move {
        let mut stop = shared.stopping.subscribe();
        loop {
            let frame =
                tokio::select! {biased;_=stop.changed()=>{return},frame=stream.next()=>frame};
            let Some(frame) = frame else { break };
            if !is_current_transport(&shared, generation) {
                return;
            }

            match frame {
                Ok(Message::Text(text)) => {
                    let Ok(value) = pilotdeck_protocol::parse_json(text.as_str()) else {
                        continue;
                    };
                    // The Node client stops dispatching once its close
                    // handshake began (the `ws` closing state): a frame that
                    // lands on the stopped socket is dropped, so `stop` then
                    // `ws-send` stays silent.
                    let stopped = {
                        let runtime = shared.runtime.lock().await;
                        runtime.app.is_none()
                    };
                    if stopped {
                        continue;
                    }
                    let kind = value.get("type").and_then(JsValue::as_string);
                    if kind.is_some_and(|k| k.equals_str("disconnect"))
                        && !value
                            .get("reason")
                            .and_then(JsValue::as_string)
                            .is_some_and(|r| r.equals_str("warning"))
                    {
                        break;
                    }
                    let official = kind.is_some_and(|kind| kind.equals_str("events_api"));
                    if !official && !kind.is_some_and(|kind| kind.equals_str("message")) {
                        continue;
                    }
                    let event = value
                        .get("payload")
                        .and_then(|payload| payload.get("event"))
                        .cloned()
                        .unwrap_or(JsValue::Undefined);
                    if official {
                        let Some(envelope) = value.get("envelope_id").and_then(JsValue::as_string)
                        else {
                            continue;
                        };
                        let mut ack = JsObject::new();
                        ack.insert("envelope_id".into(), JsValue::String(envelope.clone()));
                        let encoded = stringify_json(&JsValue::Object(ack))
                            .ok()
                            .flatten()
                            .expect("Slack envelope ACK");
                        let mut runtime = shared.runtime.lock().await;
                        let Some(app) = runtime.app.as_mut() else {
                            continue;
                        };
                        let Some(sink) = app.sink.as_mut() else {
                            continue;
                        };
                        if sink.send(Message::Text(encoded.into())).await.is_err() {
                            continue;
                        }
                        drop(runtime);
                        if !event
                            .get("type")
                            .and_then(JsValue::as_string)
                            .is_some_and(|kind| kind.equals_str("message"))
                        {
                            continue;
                        }
                    }
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        if let Err(error) = handle_message(&shared, event).await {
                            shared.log(
                                "error",
                                format!("slack: message handler error: {}", error.to_js_string()),
                            );
                        }
                    });
                }
                Ok(Message::Close(_)) => break,
                Ok(_) => {}
                Err(_) => {
                    shared.log("error", format!("slack: bolt error: {WEBSOCKET_ERROR}"));
                    break;
                }
            }
        }
        // Authentication can still be finishing when a peer closes. A failed
        // start never enters auto-reconnect; an owned stop invalidates first.
        while !*ready.borrow() {
            tokio::select! {biased;_=stop.changed()=>return,result=ready.changed()=>{if result.is_err(){return;}}}
        }
        reconnect(shared, generation).await;
    });
    let mut readers = owner.readers.lock().expect("Slack reader mutex");
    readers.retain(|task| !task.is_finished());
    if is_current_transport(&owner, generation) {
        readers.push(task);
    } else {
        task.abort();
    }
}
fn is_current_transport(shared: &SlackShared, generation: u64) -> bool {
    shared.generation.load(Ordering::SeqCst) == generation && !*shared.stopping.borrow()
}
fn reconnect(
    shared: Arc<SlackShared>,
    generation: u64,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        let mut stop = shared.stopping.subscribe();
        let mut backoff = std::time::Duration::from_millis(100);
        while is_current_transport(&shared, generation) {
            tokio::select! {biased;_=stop.changed()=>return,_=tokio::time::sleep(backoff)=>{}}
            if !is_current_transport(&shared, generation) {
                return;
            }
            // The same owner and mapper/helpers survive a transient disconnect.
            let result = tokio::select! {biased;_=stop.changed()=>return,result=start_session(&shared,generation)=>result};
            match result {
                Ok(()) => return,
                Err(error) => {
                    // A reconnect that opened a socket but failed auth must
                    // not leave its reader behind while the owner retries.
                    let orphans = {
                        let mut readers = shared.readers.lock().expect("Slack reader mutex");
                        let mut retained = Vec::new();
                        let mut orphans = Vec::new();
                        for task in readers.drain(..) {
                            if task.id() == tokio::task::id() {
                                retained.push(task)
                            } else {
                                orphans.push(task)
                            }
                        }
                        *readers = retained;
                        orphans
                    };
                    for task in &orphans {
                        task.abort();
                    }
                    for task in orphans {
                        let _ = task.await;
                    }
                    if is_current_transport(&shared, generation) {
                        shared.runtime.lock().await.app.take();
                    }
                    shared.log("warn", format!("slack: reconnect failed: {error}"));
                }
            }
            backoff = (backoff * 2).min(std::time::Duration::from_secs(5));
        }
    })
}

fn boundaries(shared: &SlackShared) -> Boundaries {
    shared
        .boundaries
        .lock()
        .expect("#1344 boundaries mutex")
        .clone()
}

/// The event is `any` on the wire: absent/`null`/non-object fields all mean
/// the same thing they do in TS (`event.bot_id` on a non-object would throw;
/// the recorded events are always objects).
fn event_field<'a>(event: &'a JsValue, name: &str) -> Option<&'a JsValue> {
    event.get(name)
}

/// `ctx.threadTs ? `${channelId}:${threadTs}` : channelId` — the thread-
/// qualified chat id behind the `slack-thread-key-off` control. The two call
/// sites differ in TS: `handleSlackMessage` falls back to the bare id when
/// `threadTs === ts` (the thread root keeps its own session under the bare
/// key), while `processMessage` qualifies unconditionally, so a root message
/// clears the turn's elicitation/permission state under the qualified key.
fn thread_chat_id(channel_id: &JsString, thread_ts: &JsValue) -> Option<JsString> {
    if !super::sms::truthy(thread_ts) || crate::mutants::on("slack-thread-key-off") {
        return None;
    }
    Some(super::sms::concat_units(&[
        channel_id.units(),
        ":".encode_utf16().collect::<Vec<u16>>().as_slice(),
        super::sms::js_string_of(thread_ts).units(),
    ]))
}

/// `handleSlackMessage` (`SlackChannel.ts:102`): filters, mention strip, the
/// thread-qualified chat id, the pending elicitation/permission answers, the
/// active-chat admission and the mapper dispatch.
async fn handle_message(shared: &Arc<SlackShared>, event: JsValue) -> Result<(), ChannelError> {
    if matches!(event, JsValue::Undefined | JsValue::Null) {
        return Ok(());
    }
    // `event.bot_id || event.subtype === "bot_message"` — the discriminator
    // the fixture's 判别力正控 removes on the TS side (`bot-id` filter).
    let subtype = event_field(&event, "subtype");
    let is_subtype = |name: &str| {
        subtype
            .and_then(JsValue::as_string)
            .is_some_and(|value| value.equals_str(name))
    };
    if !crate::mutants::on("slack-bot-id-filter-off")
        && (super::sms::truthy(
            &event_field(&event, "bot_id")
                .cloned()
                .unwrap_or(JsValue::Undefined),
        ) || is_subtype("bot_message"))
    {
        return Ok(());
    }
    if is_subtype("message_changed") || is_subtype("message_deleted") {
        return Ok(());
    }
    let user_id = event_field(&event, "user")
        .cloned()
        .unwrap_or(JsValue::Undefined);
    let bot_user_id = {
        let runtime = shared.runtime.lock().await;
        runtime.bot_user_id.clone()
    };
    // `userId && this.botUserId && userId === this.botUserId`
    if super::sms::truthy(&user_id) && super::sms::truthy(&bot_user_id) && user_id == bot_user_id {
        return Ok(());
    }
    let channel_id = event_field(&event, "channel")
        .cloned()
        .unwrap_or(JsValue::Undefined);
    if !super::sms::truthy(&channel_id) {
        return Ok(());
    }
    let channel_id = super::sms::js_string_of(&channel_id);
    // `String(event.text ?? "").replace(/<@[^>]+>/g, "").trim()`
    let raw_text = event_field(&event, "text")
        .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
        .cloned()
        .unwrap_or(JsValue::String(JsString::from("")));
    let text = {
        let stripped = if crate::mutants::on("slack-mention-strip-off") {
            super::sms::js_string_of(&raw_text).units().to_vec()
        } else {
            strip_mentions(super::sms::js_string_of(&raw_text).units())
        };
        super::sms::trim_units(&stripped).to_vec()
    };
    let text = JsString::from_utf16(text);
    // `(event.thread_ts as string | undefined) ?? undefined` — `null` is
    // normalized to `undefined`; the raw value is kept for the strict `!==`.
    let thread_ts = event_field(&event, "thread_ts")
        .filter(|value| !matches!(value, JsValue::Undefined | JsValue::Null))
        .cloned()
        .unwrap_or(JsValue::Undefined);
    let ts = event_field(&event, "ts")
        .cloned()
        .unwrap_or(JsValue::Undefined);
    // `${channelId}:${threadTs}` when the thread root differs from this ts.
    let chat_id = if thread_ts != ts {
        thread_chat_id(&channel_id, &thread_ts)
    } else {
        None
    }
    .unwrap_or_else(|| channel_id.clone());
    if text.units().is_empty() {
        return Ok(());
    }

    // The pending elicitation answer.
    let pending_elicitation = {
        let mut runtime = shared.runtime.lock().await;
        if runtime.elicitation.has_pending(&chat_id) && boundaries(shared).gateway.is_some() {
            runtime.elicitation.take(&chat_id)
        } else {
            None
        }
    };
    if let Some(entry) = pending_elicitation {
        let gateway = boundaries(shared)
            .gateway
            .expect("#1344 checked before the elicitation take");
        let trimmed = JsString::from_utf16(super::sms::trim_units(text.units()).to_vec());
        let (response, confirmation) = if trimmed == JsString::from("0") {
            (
                sms_elicitation::cancelled_response(&entry),
                Some(JsString::from("已取消。")),
            )
        } else {
            (sms_elicitation::answered_response(&entry, &trimmed), None)
        };
        match gateway.respond_elicitation(response).await {
            Err(error) => shared.log(
                "error",
                format!("slack: elicitation answer error: {}", error.to_js_string()),
            ),
            Ok(()) => {
                if let Some(confirmation) = confirmation {
                    send_reply(shared, &channel_id, &thread_ts, confirmation).await;
                }
            }
        }
        return Ok(());
    }

    // The pending permission answer.
    let began = {
        let mut permissions = shared
            .permissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
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
                deliver_answer(shared, &chat_id, &channel_id, &thread_ts, answer).await;
            }
            BeginAnswer::Decide {
                entry,
                deny,
                remember,
                answer_token,
                generation,
                shifted_rest,
            } => {
                let mut guard = AnswerGuard::new(
                    shared.permissions.clone(),
                    chat_id.clone(),
                    generation,
                    entry.clone(),
                    shifted_rest.clone(),
                );
                let gateway = boundaries(shared)
                    .gateway
                    .expect("#1344 checked before begin");
                let decision = PermissionDecision {
                    session_key: entry.session_key.clone(),
                    request_id: entry.request_id.clone(),
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
                        shared
                            .permissions
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .fail_answer(&chat_id, generation, entry, shifted_rest);
                        guard.settle();
                        shared.log(
                            "error",
                            format!("slack: permission answer error: {}", error.to_js_string()),
                        );
                    }
                    Ok(outcome) => {
                        let answer = shared
                            .permissions
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .finish_answer(
                                &chat_id,
                                answer_token,
                                generation,
                                reply,
                                shifted_rest,
                                outcome,
                            );
                        guard.settle();
                        if let Some(answer) = answer {
                            deliver_answer(shared, &chat_id, &channel_id, &thread_ts, answer).await;
                        }
                    }
                }
            }
        }
        return Ok(());
    }

    // `activeChats.has` → resolve → add in one lock section (the sms #1339
    // fix-r1 shape; TS runs the same stretch synchronously).
    let dedupe_off = crate::mutants::on("slack-active-dedupe-off");
    let (already_active, outcome) = {
        let mut runtime = shared.runtime.lock().await;
        let already_active = runtime
            .active_chats
            .lock()
            .expect("#1344 active chats mutex")
            .contains(&chat_id);
        let outcome = if already_active && !dedupe_off {
            None
        } else {
            let mapped = runtime.mapper.resolve(&chat_id, &text);
            if mapped.command_new && mapped.message.is_empty() {
                Some(DispatchOutcome::NewChat)
            } else if mapped.message.is_empty() {
                Some(DispatchOutcome::EmptyMessage)
            } else {
                let active = runtime.active_chats.clone();
                active
                    .lock()
                    .expect("#1344 active chats mutex")
                    .insert(chat_id.clone());
                let guard = super::ActiveChat::new(active, chat_id.clone());
                Some(DispatchOutcome::Dispatch(mapped, guard))
            }
        };
        (already_active, outcome)
    };
    if already_active {
        shared.log(
            "info",
            format!(
                "slack: chat {} already active, skipping",
                String::from_utf16_lossy(chat_id.units())
            ),
        );
    }
    match outcome {
        None => Ok(()),
        Some(DispatchOutcome::NewChat) => {
            send_reply(
                shared,
                &channel_id,
                &thread_ts,
                JsString::from("已创建新会话。"),
            )
            .await;
            Ok(())
        }
        Some(DispatchOutcome::EmptyMessage) => Ok(()),
        Some(DispatchOutcome::Dispatch(mapped, _guard)) => {
            process_message(
                shared,
                &channel_id,
                &thread_ts,
                &mapped.session_key,
                &mapped.message,
            )
            .await
        }
    }
}

enum DispatchOutcome {
    NewChat,
    EmptyMessage,
    Dispatch(MappedSession, super::ActiveChat<JsString>),
}

/// The post-answer part of the TS permission branch: confirmation send, the
/// non-advancing statuses and the next queued prompt (the sms `deliver_answer`
/// shape; slack carries the `{channelId, threadTs}` send context instead of
/// the chat id).
async fn deliver_answer(
    shared: &Arc<SlackShared>,
    chat_id: &JsString,
    channel_id: &JsString,
    thread_ts: &JsValue,
    answer: PermissionAnswer,
) {
    let mut guard = PromptGuard::new(
        shared.permissions.clone(),
        chat_id.clone(),
        answer.answer_token,
    );
    if let Some(text) = answer.text.clone() {
        let delivered = send_reply(shared, channel_id, thread_ts, text).await;
        if !delivered {
            shared
                .permissions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recover_answer(chat_id, answer.answer_token);
            guard.settle();
            return;
        }
        if !answer.can_advance && !answer.retry_prompt {
            guard.settle();
            return;
        }
        let next_prompt = shared
            .permissions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take_next_prompt(chat_id, answer.answer_token);
        if let Some(prompt) = next_prompt {
            let expected = shared
                .permissions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_prompt_request_id(chat_id, answer.answer_token);
            let delivered = send_reply(shared, channel_id, thread_ts, prompt).await;
            shared
                .permissions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .confirm_next_prompt(chat_id, delivered, expected.as_ref(), answer.answer_token);
        }
    }
    guard.settle();
}

/// `processMessage` (`SlackChannel.ts:182`). The TS chat id here is
/// `ctx.threadTs ? channelId:threadTs : channelId` — no `!== ts` guard, so a
/// root message (thread_ts === ts) clears elicitation state under the
/// thread-qualified key while its turn ran under the bare channel id.
async fn process_message(
    shared: &Arc<SlackShared>,
    channel_id: &JsString,
    thread_ts: &JsValue,
    session_key: &JsString,
    message: &JsString,
) -> Result<(), ChannelError> {
    let gateway = boundaries(shared).gateway;
    if gateway.is_none() {
        return Ok(());
    }
    let gateway = gateway.expect("#1344 checked above");
    let chat_id = thread_chat_id(channel_id, thread_ts).unwrap_or_else(|| channel_id.clone());
    let mut reply_units: Vec<u16> = Vec::new();
    let request = SubmitTurnRequest {
        session_key: session_key.clone(),
        channel_key: JsString::from("slack"),
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
                    runtime.elicitation.capture(&chat_id, session_key, &event)
                };
                send_reply(shared, channel_id, thread_ts, question).await;
                continue;
            }
            if is("permission_request") {
                let expected = event
                    .get("requestId")
                    .and_then(sms_permission::optional_string);
                let question = shared
                    .permissions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .capture(&chat_id, session_key, &event);
                if let Some(question) = question {
                    let mut guard = InitialPromptGuard::new(
                        shared.permissions.clone(),
                        chat_id.clone(),
                        expected.clone(),
                    );
                    let delivered = send_reply(shared, channel_id, thread_ts, question).await;
                    shared
                        .permissions
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .confirm_initial_prompt(&chat_id, delivered, expected.as_ref());
                    guard.settle();
                }
                continue;
            }
            if let Some(fragment) = render::render_slack_event(&event) {
                reply_units.extend_from_slice(fragment.units());
            }
        }
    }
    if let Some(error) = turn_error {
        shared.log("error", format!("slack: submitTurn error: {error}"));
        reply_units = if crate::mutants::on("slack-error-reply-text") {
            "出错了。".encode_utf16().collect::<Vec<u16>>()
        } else {
            "处理消息时发生错误，请重试。"
                .encode_utf16()
                .collect::<Vec<u16>>()
        };
    }
    {
        let mut runtime = shared.runtime.lock().await;
        runtime.elicitation.clear(&chat_id);
    }
    shared
        .permissions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear_after_turn(&chat_id);
    let final_text = super::sms::trim_units(&reply_units);
    if !final_text.is_empty() {
        send_reply(
            shared,
            channel_id,
            thread_ts,
            JsString::from_utf16(final_text.to_vec()),
        )
        .await;
    }
    Ok(())
}

/// `sendReply` (`SlackChannel.ts:223`): `false` once `app` is `null`, else a
/// postMessage per chunk with `delivered` set false by any failure.
async fn send_reply(
    shared: &Arc<SlackShared>,
    channel_id: &JsString,
    thread_ts: &JsValue,
    text: JsString,
) -> bool {
    let app_live = {
        let runtime = shared.runtime.lock().await;
        runtime.app.is_some() || crate::mutants::on("slack-stopped-send-still-sends")
    };
    if !app_live {
        return false;
    }
    let mut delivered = true;
    let formatted = format_slack_mrkdwn(&text);
    for chunk in chunk_text(formatted.units(), MAX_MESSAGE_LENGTH) {
        // `{channel, text, mrkdwn, ...(threadTs ? {thread_ts} : {})}` — the
        // exact key order the stub contract's wire comparison pins.
        let mut body = JsObject::new();
        body.insert("channel".into(), JsValue::String(channel_id.clone()));
        body.insert("text".into(), JsValue::String(JsString::from_utf16(chunk)));
        body.insert("mrkdwn".into(), JsValue::Bool(true));
        if super::sms::truthy(thread_ts) && !crate::mutants::on("slack-post-message-thread-off") {
            body.insert(
                "thread_ts".into(),
                JsValue::String(super::sms::js_string_of(thread_ts)),
            );
        }
        if let Err(error) = post_message(shared, &JsValue::Object(body)).await {
            shared.log("error", format!("slack: postMessage failed: {error}"));
            delivered = false;
        }
    }
    delivered
}

/// `app.client.chat.postMessage`: POST `/api/chat.postMessage`; `ok:false`
/// throws `Error: <error>` like the stub.
async fn post_message(shared: &SlackShared, body: &JsValue) -> Result<JsValue, String> {
    let payload = post_json(shared, "/api/chat.postMessage", body).await?;
    if !super::sms::truthy(&payload.get("ok").cloned().unwrap_or(JsValue::Undefined)) {
        return Err(format!(
            "Error: {}",
            String::from_utf16_lossy(
                super::sms::js_string_of(
                    &payload.get("error").cloned().unwrap_or(JsValue::Undefined)
                )
                .units()
            )
        ));
    }
    Ok(payload)
}

/// `formatSlackMrkdwn` (`SlackChannel.ts:248`): `**b**` → `*b*`, then
/// `[t](u)` → `<u|t>`. Both regexes run without the `s` flag, so the bold
/// middle may not contain a line terminator; the scanners below keep the
/// lazy/greedy semantics on UTF-16 units.
pub(crate) fn format_slack_mrkdwn(content: &JsString) -> JsString {
    let units = content.units();
    if crate::mutants::on("slack-mrkdwn-off") {
        return content.clone();
    }
    if units.is_empty() {
        return content.clone();
    }
    let bolded = replace_bold(units);
    let linked = replace_link(&bolded);
    JsString::from_utf16(linked)
}

/// `/\*\*(.+?)\*\*/g`: at each `**`, the shortest non-empty middle without a
/// line terminator, else the `**` is literal and the scan moves on.
fn replace_bold(units: &[u16]) -> Vec<u16> {
    let terminator = |unit: u16| matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029);
    let star = u16::from(b'*');
    let mut out = Vec::new();
    let mut index = 0;
    while index < units.len() {
        if units[index] == star && index + 1 < units.len() && units[index + 1] == star {
            let mut scan = index + 2;
            let mut closing = None;
            while scan < units.len() {
                if terminator(units[scan]) {
                    break;
                }
                if units[scan] == star
                    && scan + 1 < units.len()
                    && units[scan + 1] == star
                    && scan > index + 2
                {
                    closing = Some(scan);
                    break;
                }
                scan += 1;
            }
            if let Some(close) = closing {
                out.push(star);
                out.extend_from_slice(&units[index + 2..close]);
                out.push(star);
                index = close + 2;
                continue;
            }
        }
        out.push(units[index]);
        index += 1;
    }
    out
}

/// `/\[([^\]]+)\]\(([^)]+)\)/g`: `[label](url)` → `<url|label>`. The label
/// may not contain `]` (newlines are fine), the url may not contain `)`, both
/// need at least one character.
fn replace_link(units: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < units.len() {
        if units[index] == u16::from(b'[')
            && let Some((label_end, url_end)) = link_at(units, index)
        {
            out.push(u16::from(b'<'));
            out.extend_from_slice(&units[label_end + 2..url_end]);
            out.push(u16::from(b'|'));
            out.extend_from_slice(&units[index + 1..label_end]);
            out.push(u16::from(b'>'));
            index = url_end + 1;
            continue;
        }
        out.push(units[index]);
        index += 1;
    }
    out
}

/// The `[` at `index` closes as `[label](url)`; returns the `]` position and
/// the `)` position. The label may contain anything except `]` (including
/// newlines and further `[`), the url anything except `)`, both at least one
/// character.
fn link_at(units: &[u16], index: usize) -> Option<(usize, usize)> {
    let mut scan = index + 1;
    while scan < units.len() {
        if units[scan] != u16::from(b']') {
            scan += 1;
            continue;
        }
        if scan == index + 1 {
            return None;
        }
        if scan + 1 >= units.len() || units[scan + 1] != u16::from(b'(') {
            return None;
        }
        let url_start = scan + 2;
        let mut url_scan = url_start;
        while url_scan < units.len() && units[url_scan] != u16::from(b')') {
            url_scan += 1;
        }
        if url_scan == units.len() || url_scan == url_start {
            return None;
        }
        return Some((scan, url_scan));
    }
    None
}

/// `chunkText` (`SlackChannel.ts:256`): split at the last `\n`, then the last
/// space, both only past the half-way mark, else hard at `max`; the rest is
/// stripped of leading newlines.
pub(crate) fn chunk_text(units: &[u16], max: usize) -> Vec<Vec<u16>> {
    if units.len() <= max || crate::mutants::on("slack-chunk-off") {
        return vec![units.to_vec()];
    }
    let mut out = Vec::new();
    let mut rest = units;
    let last_index_of = |units: &[u16], needle: u16, from: usize| -> Option<usize> {
        let bound = from.min(units.len().saturating_sub(1));
        (0..=bound)
            .rev()
            .find(|&position| units[position] == needle)
    };
    while rest.len() > max {
        let mut split = last_index_of(rest, u16::from(b'\n'), max);
        if split.is_none_or(|position| position < max / 2) {
            split = last_index_of(rest, u16::from(b' '), max);
        }
        let split = split.filter(|position| *position >= max / 2).unwrap_or(max);
        out.push(rest[..split].to_vec());
        let tail = &rest[split..];
        let mut start = 0;
        while start < tail.len() && tail[start] == u16::from(b'\n') {
            start += 1;
        }
        rest = &tail[start..];
    }
    if !rest.is_empty() {
        out.push(rest.to_vec());
    }
    out
}

/// `/<@[^>]+>/g`: `<@…>` mention runs, `>` excluded from the middle, at least
/// one character inside.
fn strip_mentions(units: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < units.len() {
        if units[index] == u16::from(b'<')
            && index + 1 < units.len()
            && units[index + 1] == u16::from(b'@')
        {
            let mut scan = index + 2;
            let mut closed = None;
            while scan < units.len() {
                if units[scan] == u16::from(b'>') {
                    closed = Some(scan);
                    break;
                }
                scan += 1;
            }
            if let Some(close) = closed
                && close > index + 2
            {
                index = close + 1;
                continue;
            }
        }
        out.push(units[index]);
        index += 1;
    }
    out
}

/// `slack::register`: the loader-table mount (the sms precedent). The
/// production assembly calls it through `production_loader_mounts`
/// (`create_startup_configured`, #2301); the wiring correction is registered
/// in the parity ledger as A8.3-CH-SLACK-001-R1.
pub fn register(mounts: &mut super::loader::ChannelLoaderMounts) {
    mounts.register(
        "slack",
        Arc::new(|options: &JsObject| {
            Ok(Arc::new(SlackChannel::new(SlackChannelOptions {
                bot_token: options.get("botToken").cloned(),
                app_token: options.get("appToken").cloned(),
                mapper: None,
                gateway: None,
                env: Some(Arc::new(|name| {
                    std::env::var(name).ok().map(JsString::from)
                })),
                base_url: None,
            })) as Arc<dyn ChannelAdapter>)
        }),
    );
}

pub fn register_native(
    mounts: &mut super::loader::ChannelLoaderMounts,
    responder: Arc<dyn crate::channel::bluebubbles::InteractionResponder>,
) {
    register_native_inner(mounts, responder, None)
}
pub fn register_native_with_base(
    mounts: &mut super::loader::ChannelLoaderMounts,
    responder: Arc<dyn crate::channel::bluebubbles::InteractionResponder>,
    base: String,
) {
    register_native_inner(mounts, responder, Some(base.into()))
}
fn register_native_inner(
    mounts: &mut super::loader::ChannelLoaderMounts,
    responder: Arc<dyn crate::channel::bluebubbles::InteractionResponder>,
    base: Option<JsString>,
) {
    mounts.register(
        "slack",
        Arc::new(move |options: &JsObject| {
            Ok(Arc::new(
                SlackChannel::new(SlackChannelOptions {
                    bot_token: options.get("botToken").cloned(),
                    app_token: options.get("appToken").cloned(),
                    mapper: None,
                    gateway: None,
                    env: Some(Arc::new(|name| {
                        std::env::var(name).ok().map(JsString::from)
                    })),
                    base_url: base.clone(),
                })
                .with_responder(responder.clone()),
            ) as Arc<dyn ChannelAdapter>)
        }),
    );
}

fn endpoint_url(base: &JsString, path: &str) -> String {
    format!("{}{path}", String::from_utf16_lossy(base.units()))
}
#[cfg(test)]
mod native_tests;
