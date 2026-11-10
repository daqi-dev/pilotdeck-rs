//! `resources/PilotDeck/src/adapters/channel/wecom-callback/` on the #1327
//! channel core: the `wecom_callback` adapter with its own HTTP listener
//! (`GET *` URL verification, `POST *` callbacks), the session mapper, the
//! event renderer and the qyapi outbound client.
//!
//! The component is unwired like the rest of A8.3: assembly passes the A7
//! inbound port, the shared interaction port and (only in tests) a qyapi base
//! URL. It owns no session and starts no TS owner.

mod crypto;
mod http;
mod mapper;
mod render;
#[cfg(test)]
mod tests;
mod xml;

use std::{
    collections::HashSet,
    convert::Infallible,
    io,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited, combinators::BoxBody};
use hyper::{Request, Response, body::Incoming, header};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use url::Url;

use super::{
    http_client::{HttpClient, encode_uri_component},
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    im_interaction::{InteractionPort, NoInteraction},
    loader::ChannelAdapterConstructor,
    protocol::{
        BoxFuture, ChannelAdapter, ChannelError, ChannelHandle, ChannelKey, ChannelLog,
        ChannelStartDeps, TurnGateway, TurnRequest,
    },
};
use crate::{
    Body, ProxyError,
    channel::wecom_callback::{
        crypto::{decrypt_wx_message, verify_msg_signature},
        http::Listener,
        mapper::{Resolved, SessionMapper},
        render::render_event,
        xml::{extract_encrypt_from_xml, xml_tag},
    },
    metadata::js_trim,
};

/// `WeComCallbackChannel.channelKey`.
pub const CHANNEL_KEY: &str = "wecom_callback";
/// `CHANNEL_LOADERS` table key whose mount constructs this adapter.
pub const LOADER_KEY: &str = "wecomCallback";
/// `const QYAPI = "https://qyapi.weixin.qq.com/cgi-bin"`.
pub const DEFAULT_QYAPI: &str = "https://qyapi.weixin.qq.com/cgi-bin";
const DEFAULT_PORT: u32 = 8780;
const TOKEN_TTL_MS: f64 = 7_000_000.0;
const MAX_MESSAGE_LENGTH: usize = 2048;
/// #1941: the `POST *` body cap. TS's `readBody` buffers the whole request
/// with no bound (`WeComCallbackChannel.ts:57-65`), so an unauthenticated
/// caller could make the process hold an arbitrary body *before* the
/// signature check runs. The value is the webhook listener's precedent
/// (`super::webhook::DEFAULT_MAX_BODY_BYTES`); the ending on a crossing body
/// is that lane's too — a zero-byte close, not a 413.
const MAX_BODY_BYTES: f64 = super::webhook::DEFAULT_MAX_BODY_BYTES;

/// `WeComCallbackChannelOptions` after the TS constructor's `String(...).trim()`
/// and `Number(port)` normalisation. `qyapi` is the production constant; the
/// test seam keeps every other step identical.
#[derive(Clone, Debug)]
pub struct WeComCallbackOptions {
    pub corp_id: String,
    pub agent_id: String,
    pub secret: String,
    pub token: String,
    pub encoding_aes_key: String,
    pub port: u32,
    pub qyapi: String,
}

impl WeComCallbackOptions {
    /// `new WeComCallbackChannel(options)` field extraction.
    pub fn from_object(options: &JsObject, qyapi: &str) -> Self {
        let mut values: [String; STRING_FIELDS.len()] = std::array::from_fn(|_| String::new());
        for (index, (name, env)) in STRING_FIELDS.iter().enumerate() {
            values[index] = config_string(options, name, env);
        }
        let [corp_id, agent_id, secret, token, encoding_aes_key] = values;
        Self {
            corp_id,
            agent_id,
            secret,
            token,
            encoding_aes_key,
            port: config_port(options),
            qyapi: qyapi.to_owned(),
        }
    }
}

/// The TS constructor's `String(options.<name> ?? env ?? "").trim()` fields
/// in TS field order (WeComCallbackChannel.ts:100-104, before the port).
/// One list serves both halves of the mirror — `from_object`'s coercion and
/// #2432's constructor guard — so a sixth string field cannot be added to
/// one half and missed by the other.
const STRING_FIELDS: [(&str, &str); 5] = [
    ("corpId", "WECOM_CORP_ID"),
    ("agentId", "WECOM_AGENT_ID"),
    ("secret", "WECOM_CB_SECRET"),
    ("token", "WECOM_CB_TOKEN"),
    ("encodingAesKey", "WECOM_ENCODING_AES_KEY"),
];

fn nullish(value: Option<&JsValue>) -> bool {
    matches!(value, None | Some(JsValue::Undefined) | Some(JsValue::Null))
}

fn js_string(value: &JsValue) -> String {
    crate::js_value::to_string_units(value)
        .map(|units| String::from_utf16_lossy(&units))
        .unwrap_or_default()
}

/// `String(options[key] ?? process.env[env] ?? "")` followed by `trim()`.
fn config_string(options: &JsObject, name: &str, env: &str) -> String {
    let raw = if nullish(options.get(name)) {
        std::env::var(env).unwrap_or_default()
    } else {
        js_string(options.get(name).expect("#1336 option present"))
    };
    js_trim(&raw).to_owned()
}

/// `Number(options.port ?? process.env.WECOM_CB_PORT ?? 8780)`, then
/// `Number.isFinite(p) && p > 0 ? Math.floor(p) : DEFAULT_PORT`.
fn config_port(options: &JsObject) -> u32 {
    let number = if nullish(options.get("port")) {
        match std::env::var("WECOM_CB_PORT") {
            Ok(value) => crate::js_value::string_to_number(&value),
            Err(_) => DEFAULT_PORT as f64,
        }
    } else {
        crate::js_value::to_number(options.get("port").expect("#1336 port present"))
            .unwrap_or(f64::NAN)
    };
    if number.is_finite() && number > 0.0 {
        number.floor().max(0.0).min(u32::MAX as f64) as u32
    } else {
        DEFAULT_PORT
    }
}

/// One channel instance. The listener is created by `start`, so a disabled
/// channel constructs this without touching a socket.
pub struct WeComCallbackChannel {
    key: ChannelKey,
    runtime: Arc<Runtime>,
}

struct CachedToken {
    value: String,
    expires_at_ms: f64,
}

struct State {
    mapper: SessionMapper,
    token: Option<CachedToken>,
    active_chats: HashSet<String>,
}

/// Clears a chat's `active_chats` entry even when the detached dispatch task
/// unwinds, mirroring TS's `try { await processMessage(...) }
/// finally { activeChats.delete(chatId) }`.
struct ActiveChat {
    runtime: Arc<Runtime>,
    chat_id: String,
}

impl Drop for ActiveChat {
    fn drop(&mut self) {
        self.runtime
            .state
            .lock()
            .expect("#1336 state mutex")
            .active_chats
            .remove(&self.chat_id);
    }
}

struct Runtime {
    options: WeComCallbackOptions,
    key: ChannelKey,
    /// TS `this.gateway = deps.gateway` in `start()`: the shared A7 port.
    gateway: Mutex<Option<Arc<dyn TurnGateway>>>,
    interaction: Arc<dyn InteractionPort>,
    http: HttpClient,
    log: Mutex<Option<ChannelLog>>,
    state: Mutex<State>,
    /// Test seam: how many request heads this channel's listener has read.
    /// Per channel, so parallel tests cannot satisfy each other's barriers
    /// (the count used to be a process-global static).
    #[cfg(test)]
    accepted: http::Accepted,
}

/// The counter the listener bumps in tests; `None` in production builds, so
/// the serving path is identical and pays nothing for the seam.
#[cfg(test)]
fn accepted_seam(runtime: &Runtime) -> Option<http::Accepted> {
    Some(runtime.accepted.clone())
}

#[cfg(not(test))]
fn accepted_seam(_runtime: &Runtime) -> Option<http::Accepted> {
    None
}

/// `route`'s failure. Only a body that crossed [`MAX_BODY_BYTES`] destroys the
/// connection (#1941); every other error keeps TS's `catch` arm — log it and
/// answer `500 "error"` — because those replies can still reach the wire.
enum RouteError {
    PayloadTooLarge,
    Message(String),
}

impl From<String> for RouteError {
    fn from(value: String) -> Self {
        Self::Message(value)
    }
}

impl Runtime {
    fn log(&self, level: &str, message: &str) {
        let log = self.log.lock().expect("#1336 logger mutex").clone();
        if let Some(log) = log {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| log(level, message)));
        }
    }

    /// TS `this.gateway`: the A7 port the enclosing `start()` supplied.
    fn gateway(&self) -> Option<Arc<dyn TurnGateway>> {
        self.gateway.lock().expect("#1336 gateway mutex").clone()
    }

    async fn start(
        self: &Arc<Self>,
        deps: ChannelStartDeps,
    ) -> Result<Arc<dyn ChannelHandle>, ChannelError> {
        // TS `start()`: `this.gateway = deps.gateway` before the config checks,
        // so a channel with missing options still holds the port.
        *self.gateway.lock().expect("#1336 gateway mutex") = deps.turn_gateway;
        *self.log.lock().expect("#1336 logger mutex") = deps.log;
        if self.options.corp_id.is_empty()
            || self.options.token.is_empty()
            || self.options.encoding_aes_key.is_empty()
        {
            self.log(
                "error",
                "wecom_callback: corpId, token, and encodingAesKey are required",
            );
            return Ok(Arc::new(NoopHandle));
        }
        if self.options.secret.is_empty() || self.options.agent_id.is_empty() {
            self.log(
                "error",
                "wecom_callback: secret and agentId are required for outbound send",
            );
            return Ok(Arc::new(NoopHandle));
        }
        match http::listen(self.clone(), self.options.port, accepted_seam(self)).await {
            Ok(listener) => {
                self.log(
                    "info",
                    &format!("wecom_callback: listening on 0.0.0.0:{}", listener.port()),
                );
                Ok(Arc::new(ListenerHandle {
                    runtime: self.clone(),
                    listener: Arc::new(Mutex::new(Some(listener))),
                }))
            }
            Err(error) => {
                self.log("error", &format!("wecom_callback: start failed: {error}"));
                Ok(Arc::new(NoopHandle))
            }
        }
    }

    // ----- HTTP face -------------------------------------------------------

    async fn handle_request(
        self: Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<Body>, io::Error> {
        match self.route(request).await {
            Ok(response) => Ok(response),
            // #1941: an oversize body ends the connection with zero response
            // bytes — the webhook listener's precedent (`channel/webhook/mod.rs`
            // catch arm returns `Err`: "TS destroyed the socket before the
            // rejection continuation runs"; 413 lives only in that lane's
            // negative-control mutant). Returning `Err` from the service is
            // what makes hyper drop the connection without writing.
            Err(RouteError::PayloadTooLarge) => Err(io::Error::other("payload too large")),
            Err(RouteError::Message(error)) => {
                self.log("error", &format!("wecom_callback: HTTP error: {error}"));
                Ok(response(500, None, b"error"))
            }
        }
    }

    async fn route(
        self: &Arc<Self>,
        request: Request<Incoming>,
    ) -> Result<Response<Body>, RouteError> {
        let method = request.method().as_str().to_owned();
        let target = request.uri().to_string();
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("undefined");
        let url = build_url(&target, host)?;
        let query = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
                .unwrap_or_default()
        };
        match method.as_str() {
            "GET" => {
                let echostr = query("echostr");
                if echostr.is_empty() {
                    return Ok(response(400, None, b"missing echostr"));
                }
                let signature = query("msg_signature");
                if !verify_msg_signature(
                    &self.options.token,
                    &query("timestamp"),
                    &query("nonce"),
                    &echostr,
                    &signature,
                ) {
                    return Ok(response(403, None, b"signature"));
                }
                let plain = decrypt_wx_message(
                    &self.options.encoding_aes_key,
                    &echostr,
                    Some(&self.options.corp_id),
                )
                .map_err(|error| error.to_string())?;
                Ok(response(
                    200,
                    Some("text/plain; charset=utf-8"),
                    plain.as_bytes(),
                ))
            }
            "POST" => {
                // #1941: bound the body while it is read. `Limited` reports
                // the frame that crosses the cap as `LengthLimitError`; a body
                // of exactly `MAX_BODY_BYTES` still passes, the same boundary
                // as `webhook::read_request_body`'s `total > max`. The limit
                // has to sit here, before verification: the signed material
                // (`Encrypt`) is inside the body, so the body must be read
                // before the signature can be checked at all.
                let bytes = match Limited::new(request.into_body(), MAX_BODY_BYTES as usize)
                    .collect()
                    .await
                {
                    Ok(collected) => collected.to_bytes(),
                    Err(error) if error.downcast_ref::<LengthLimitError>().is_some() => {
                        return Err(RouteError::PayloadTooLarge);
                    }
                    Err(error) => return Err(RouteError::Message(format!("Error: {error}"))),
                };
                let body = String::from_utf8_lossy(&bytes).into_owned();
                let signature = query("msg_signature");
                let Some(encrypt) =
                    extract_encrypt_from_xml(&body).filter(|value| !value.is_empty())
                else {
                    return Ok(response(400, None, b"no Encrypt"));
                };
                if !verify_msg_signature(
                    &self.options.token,
                    &query("timestamp"),
                    &query("nonce"),
                    &encrypt,
                    &signature,
                ) {
                    return Ok(response(403, None, b"signature"));
                }
                let inner = decrypt_wx_message(
                    &self.options.encoding_aes_key,
                    &encrypt,
                    Some(&self.options.corp_id),
                )
                .map_err(|error| error.to_string())?;
                let runtime = self.clone();
                tokio::spawn(async move {
                    runtime.dispatch_inbound_xml(inner).await;
                });
                Ok(response(200, Some("text/plain"), b"success"))
            }
            _ => Ok(response(405, None, b"")),
        }
    }

    // ----- inbound dispatch ------------------------------------------------

    async fn dispatch_inbound_xml(self: Arc<Self>, xml: String) {
        let msg_type = xml_tag(&xml, "MsgType").unwrap_or_else(|| "text".to_owned());
        let Some(from_user) = xml_tag(&xml, "FromUserName") else {
            return;
        };
        let chat_id = from_user;
        if chat_id.is_empty() {
            return;
        }
        if msg_type != "text" {
            return;
        }
        let Some(content) = xml_tag(&xml, "Content") else {
            return;
        };
        if js_trim(&content).is_empty() {
            return;
        }
        let has_gateway = self.gateway().is_some();

        // TS evaluates `hasPending(chatId) && this.gateway`: the helper call
        // happens even without a gateway.
        if self.interaction.elicitation_pending(&chat_id) && has_gateway {
            match self
                .interaction
                .elicitation_answer(&chat_id, &content)
                .await
            {
                Ok(Some(confirmation)) => {
                    let _ = self.send_reply(&chat_id, &confirmation).await;
                }
                Ok(None) => {}
                Err(error) => self.log(
                    "error",
                    &format!("wecom_callback: elicitation answer error: {error}"),
                ),
            }
            return;
        }

        if self.interaction.permission_pending(&chat_id) && has_gateway {
            match self.interaction.permission_answer(&chat_id, &content).await {
                Ok(answer) => {
                    if let Some(answer) = answer
                        && let Some(reply) = answer.text.as_ref().filter(|text| !text.is_empty())
                    {
                        let delivered = self.send_reply(&chat_id, reply).await;
                        if !delivered {
                            self.interaction
                                .permission_release_answer(&chat_id, answer.answer_token);
                            return;
                        }
                        if !answer.can_advance && !answer.retry_prompt {
                            return;
                        }
                        if let Some(next_prompt) = self
                            .interaction
                            .permission_take_next_prompt(&chat_id, answer.answer_token)
                        {
                            let request_id = self
                                .interaction
                                .permission_prompt_request_id(&chat_id, answer.answer_token);
                            let delivered = self.send_reply(&chat_id, &next_prompt).await;
                            self.interaction.permission_confirm_next(
                                &chat_id,
                                delivered,
                                request_id.as_deref(),
                                answer.answer_token,
                            );
                        }
                    }
                }
                Err(error) => {
                    self.log(
                        "error",
                        &format!("wecom_callback: permission answer error: {error}"),
                    );
                }
            }
            return;
        }

        // TS runs `has()` → `mapper.resolve()` → `add()` inside one synchronous
        // block, so no second callback for the same chat can slip between the
        // check and the insert. Taking the lock twice (as this used to) left
        // that window open: on the multi-threaded runtime two concurrent POSTs
        // for one chat both saw "not active" and each dispatched a turn. One
        // lock scope reproduces the JS run-to-completion block; the early
        // returns keep the TS ordering (`has` before `resolve`).
        enum Gate {
            Active,
            Notify,
            Dispatch(Resolved),
        }
        let gate = {
            let mut state = self.state.lock().expect("#1336 state mutex");
            if state.active_chats.contains(&chat_id) {
                Gate::Active
            } else {
                let resolved = state.mapper.resolve(&chat_id, &content);
                if resolved.is_new && resolved.message.is_empty() {
                    Gate::Notify
                } else if resolved.message.is_empty() {
                    return;
                } else {
                    state.active_chats.insert(chat_id.clone());
                    Gate::Dispatch(resolved)
                }
            }
        };
        let resolved = match gate {
            Gate::Active => {
                self.log(
                    "info",
                    &format!("wecom_callback: chat {chat_id} already active, skipping"),
                );
                return;
            }
            Gate::Notify => {
                // TS awaits the notice without the chat added, so a message
                // arriving meanwhile still dispatches; release the lock first.
                let _ = self.send_reply(&chat_id, "已创建新会话。").await;
                return;
            }
            Gate::Dispatch(resolved) => resolved,
        };

        // RAII: a panic in a dynamically dispatched port (`submit`/`next`, or
        // any `InteractionPort` call) unwinds this detached task without
        // reaching a trailing `remove`, which would leave the chat "active" and
        // silently drop every later message until the process restarts.
        let _active = ActiveChat {
            runtime: self.clone(),
            chat_id: chat_id.clone(),
        };
        self.process_message(&chat_id, &resolved.session_key, &resolved.message)
            .await;
    }

    async fn process_message(self: &Arc<Self>, chat_id: &str, session_key: &str, message: &str) {
        let Some(gateway) = self.gateway() else {
            return;
        };
        let mut reply_text = String::new();
        let mut failure = None;
        match gateway
            .submit(TurnRequest {
                session_key: JsString::from(session_key),
                channel_key: self.key.clone(),
                message: JsString::from(message),
            })
            .await
        {
            Err(error) => failure = Some(error),
            Ok(mut events) => loop {
                match events.next().await {
                    Ok(None) => break,
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                    Ok(Some(event)) => {
                        let event_type = event
                            .get("type")
                            .and_then(JsValue::as_string)
                            .map(|value| String::from_utf16_lossy(value.units()))
                            .unwrap_or_default();
                        if event_type == "elicitation_request" {
                            let question =
                                self.interaction
                                    .elicitation_capture(chat_id, session_key, &event);
                            let _ = self.send_reply(chat_id, &question).await;
                        } else if event_type == "permission_request" {
                            let question =
                                self.interaction
                                    .permission_capture(chat_id, session_key, &event);
                            if let Some(question) = question {
                                let delivered = self.send_reply(chat_id, &question).await;
                                self.interaction.permission_confirm_initial(
                                    chat_id,
                                    delivered,
                                    request_id(&event).as_deref(),
                                );
                            }
                        } else if let Some(fragment) = render_event(&event) {
                            reply_text.push_str(&fragment);
                        }
                    }
                }
            },
        }
        if let Some(error) = failure {
            self.log(
                "error",
                &format!("wecom_callback: submitTurn error: {error}"),
            );
            reply_text = "处理消息时发生错误，请重试。".to_owned();
        }
        self.interaction.elicitation_clear(chat_id);
        self.interaction.permission_clear_after_turn(chat_id);
        let final_text = js_trim(&reply_text).to_owned();
        if !final_text.is_empty() {
            let _ = self.send_reply(chat_id, &final_text).await;
        }
    }

    // ----- qyapi outbound --------------------------------------------------

    async fn get_access_token(self: &Arc<Self>) -> Result<String, String> {
        let now = now_ms();
        {
            let state = self.state.lock().expect("#1336 state mutex");
            if let Some(token) = &state.token
                && !token.value.is_empty()
                && now < token.expires_at_ms
            {
                return Ok(token.value.clone());
            }
        }
        let url = format!(
            "{}/gettoken?corpid={}&corpsecret={}",
            self.options.qyapi,
            encode_uri_component(&self.options.corp_id),
            encode_uri_component(&self.options.secret)
        );
        let response = self
            .http
            .request("GET", &url, None, "", Duration::from_secs(20))
            .await?;
        let data = pilotdeck_protocol::parse_json(&String::from_utf8_lossy(&response.body))
            .map_err(|error| format!("SyntaxError: {error}"))?;
        if !response.is_ok() || !errcode_ok(data.get("errcode")) {
            let message = data
                .get("errmsg")
                .filter(|value| !nullish(Some(value)))
                .map(|value| crate::js_value::display(value).unwrap_or_default())
                .unwrap_or_else(|| format!("gettoken failed ({})", response.status));
            return Err(format!("Error: {message}"));
        }
        let Some(token) = data
            .get("access_token")
            .and_then(JsValue::as_string)
            .filter(|value| !value.is_empty())
            .map(|value| String::from_utf16_lossy(value.units()))
        else {
            return Err("Error: gettoken: no access_token".to_owned());
        };
        let expires_in = match data.get("expires_in") {
            Some(JsValue::Number(value)) => *value,
            _ => 7200.0,
        };
        let expires_at_ms = now + (expires_in * 1000.0 - 60_000.0).min(TOKEN_TTL_MS);
        self.state.lock().expect("#1336 state mutex").token = Some(CachedToken {
            value: token.clone(),
            expires_at_ms,
        });
        Ok(token)
    }

    /// `sendReply`: always resolves; failures are logged, never propagated.
    async fn send_reply(self: &Arc<Self>, chat_id: &str, text: &str) -> bool {
        let token = match self.get_access_token().await {
            Ok(token) => token,
            Err(error) => {
                self.log(
                    "error",
                    &format!("wecom_callback: sendReply error: {error}"),
                );
                return false;
            }
        };
        let url = format!(
            "{}/message/send?access_token={}",
            self.options.qyapi,
            encode_uri_component(&token)
        );
        let body = message_body(&self.options.agent_id, chat_id, text);
        let response = match self
            .http
            .request(
                "POST",
                &url,
                Some("application/json"),
                &body,
                Duration::from_secs(60),
            )
            .await
        {
            Ok(response) => response,
            Err(error) => {
                self.log(
                    "error",
                    &format!("wecom_callback: sendReply error: {error}"),
                );
                return false;
            }
        };
        let data = pilotdeck_protocol::parse_json(&String::from_utf8_lossy(&response.body))
            .unwrap_or_else(|_| JsValue::Object(JsObject::new()));
        let errcode = data.get("errcode").cloned();
        if response.is_ok() && errcode_ok(errcode.as_ref()) {
            return true;
        }
        let error = data
            .get("errmsg")
            .filter(|value| !nullish(Some(value)))
            .map(|value| crate::js_value::display(value).unwrap_or_default())
            .unwrap_or_else(|| response.status_text.to_owned());
        self.log(
            "error",
            &format!(
                "wecom_callback: sendReply failed (errcode={}): {error}",
                js_display(errcode.as_ref())
            ),
        );
        if matches!(errcode, Some(JsValue::Number(value)) if value == 40014.0 || value == 42001.0) {
            self.state.lock().expect("#1336 state mutex").token = None;
        }
        false
    }
}

fn request_id(event: &JsValue) -> Option<String> {
    event
        .get("requestId")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
}

fn errcode_ok(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => true,
        Some(JsValue::Number(number)) => *number == 0.0,
        _ => false,
    }
}

fn js_display(value: Option<&JsValue>) -> String {
    match value {
        None | Some(JsValue::Undefined) => "undefined".to_owned(),
        Some(JsValue::Null) => "null".to_owned(),
        Some(value) => crate::js_value::display(value).unwrap_or_else(|_| "undefined".to_owned()),
    }
}

/// `JSON.stringify({touser, msgtype: "text", agentid: Number(agentId), text:{content}})`
/// with the JS `slice(0, 2048)` UTF-16 truncation.
fn message_body(agent_id: &str, chat_id: &str, text: &str) -> String {
    let units: Vec<u16> = text.encode_utf16().take(MAX_MESSAGE_LENGTH).collect();
    let mut content = JsObject::new();
    content.insert(
        "content".into(),
        JsValue::String(JsString::from_utf16(units)),
    );
    let agent =
        crate::js_value::to_number(&JsValue::String(JsString::from(agent_id))).unwrap_or(f64::NAN);
    let mut body = JsObject::new();
    body.insert("touser".into(), JsValue::String(JsString::from(chat_id)));
    body.insert("msgtype".into(), JsValue::String(JsString::from("text")));
    body.insert("agentid".into(), JsValue::Number(agent));
    body.insert("text".into(), JsValue::Object(content));
    pilotdeck_protocol::stringify_json(&JsValue::Object(body))
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as f64)
        .unwrap_or(0.0)
}

fn build_url(target: &str, host: &str) -> Result<Url, String> {
    let parsed = if target.starts_with("http://") || target.starts_with("https://") {
        Url::parse(target)
    } else if target == "*" {
        Url::parse(&format!("http://{host}/*"))
    } else if target.starts_with('/') {
        Url::parse(&format!("http://{host}{target}"))
    } else {
        Url::parse(&format!("http://{host}/{target}"))
    };
    parsed.map_err(|_| "TypeError: Invalid URL".to_owned())
}

fn response(status: u16, content_type: Option<&str>, body: &[u8]) -> Response<Body> {
    let mut builder = Response::builder()
        .status(status)
        .header(header::DATE, httpdate::fmt_http_date(SystemTime::now()));
    if let Some(content_type) = content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    builder
        .body(full(Bytes::copy_from_slice(body)))
        .expect("#1336 response builder")
}

fn full(body: Bytes) -> Body {
    BoxBody::new(Full::new(body).map_err(|never: Infallible| -> ProxyError { match never {} }))
}

// ----- adapters ------------------------------------------------------------

impl WeComCallbackChannel {
    pub fn new(options: WeComCallbackOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        Self::with_uuid(options, interaction, Arc::new(random_uuid))
    }

    /// The TS constructor accepts `options.mapper`; the Rust side exposes the
    /// mapper's uuid seam instead, used by the parity tests for deterministic
    /// `s_<uuid>` session keys.
    pub(crate) fn with_uuid(
        options: WeComCallbackOptions,
        interaction: Arc<dyn InteractionPort>,
        uuid: mapper::Uuid,
    ) -> Self {
        let runtime = Arc::new(Runtime {
            options,
            key: ChannelKey::new(CHANNEL_KEY),
            gateway: Mutex::new(None),
            interaction,
            http: HttpClient::new(),
            log: Mutex::new(None),
            state: Mutex::new(State {
                mapper: SessionMapper::new(uuid),
                token: None,
                active_chats: HashSet::new(),
            }),
            #[cfg(test)]
            accepted: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        Self {
            key: ChannelKey::new(CHANNEL_KEY),
            runtime,
        }
    }

    /// Builds the adapter from the #1327 loader's option object.
    pub fn from_object(
        options: &JsObject,
        interaction: Arc<dyn InteractionPort>,
        qyapi: &str,
    ) -> Self {
        Self::new(
            WeComCallbackOptions::from_object(options, qyapi),
            interaction,
        )
    }

    /// `ChannelLoaderMounts` mount for `wecomCallback`. Assembly supplies the
    /// interaction port here and the single shared A7 port through
    /// `ChannelStartDeps.turn_gateway`, like every other A8.3 adapter; no
    /// per-channel session owner is created. #2301 fix-r1: the TS constructor's
    /// `Number(options.port ?? process.env.WECOM_CB_PORT ?? DEFAULT_PORT)`
    /// throws on a failed toPrimitive and the loader's catch records the key
    /// and continues, so the mount surfaces the same TypeError at construction
    /// instead of letting `config_port` fold it into the default port.
    /// #2432: the five `String(options.<field> ?? env ?? "").trim()` coercions
    /// run before the port (WeComCallbackChannel.ts:100-104 field order), and
    /// a value whose `String()` throws surfaces the same way — the guard walks
    /// the same `validate_coercion` the port's `Number()` shares, so an own
    /// `toString`, a prototype-carried one, a null-terminating chain and a
    /// too-deep array join all report the TS error text. The field list is
    /// [`STRING_FIELDS`], the same one `from_object` coerces.
    pub fn constructor(interaction: Arc<dyn InteractionPort>) -> ChannelAdapterConstructor {
        Self::constructor_inner(interaction, DEFAULT_QYAPI.to_owned())
    }
    pub(crate) fn constructor_with_base(
        interaction: Arc<dyn InteractionPort>,
        qyapi: String,
    ) -> ChannelAdapterConstructor {
        Self::constructor_inner(interaction, qyapi)
    }
    fn constructor_inner(
        interaction: Arc<dyn InteractionPort>,
        qyapi: String,
    ) -> ChannelAdapterConstructor {
        Arc::new(move |options: &JsObject| {
            for (field, _) in STRING_FIELDS {
                if let Some(value) = options.get(field).filter(|value| !nullish(Some(value)))
                    && let Err(error) = crate::js_value::string_coercible_checked(value)
                {
                    return Err(ChannelError::Plain(format!(
                        "{}: {}",
                        error.name(),
                        error.message()
                    )));
                }
            }
            if let Some(port) = options.get("port").filter(|value| !nullish(Some(value)))
                && let Some(error) = crate::js_value::number_conversion_error(port)
            {
                return Err(ChannelError::Plain(error));
            }
            Ok(
                Arc::new(Self::from_object(options, interaction.clone(), &qyapi))
                    as Arc<dyn ChannelAdapter>,
            )
        })
    }
}

fn random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

impl ChannelAdapter for WeComCallbackChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }

    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let runtime = self.runtime.clone();
        Box::pin(async move { runtime.start(deps).await })
    }

    /// `WeComCallbackChannel.deliverCronResult`:
    /// `deliverChatCronResult(delivery, this.channelKey, (chatId, text) => this.sendReply(chatId, text))`.
    fn deliver_cron_result(&self, delivery: &CronResultDelivery) -> BoxFuture<bool> {
        let runtime = self.runtime.clone();
        let key = self.key.clone();
        let delivery = delivery.clone();
        Box::pin(async move {
            let send = {
                let runtime = runtime.clone();
                move |chat_id: JsString, text: JsString| {
                    let runtime = runtime.clone();
                    let chat_id = String::from_utf16_lossy(chat_id.units());
                    let text = String::from_utf16_lossy(text.units());
                    Box::pin(async move { runtime.send_reply(&chat_id, &text).await })
                        as BoxFuture<bool>
                }
            };
            deliver_chat_cron_result(&delivery, &key, send).await
        })
    }
}

struct NoopHandle;

impl ChannelHandle for NoopHandle {
    fn stop(&self, _reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}

struct ListenerHandle {
    runtime: Arc<Runtime>,
    listener: Arc<Mutex<Option<Listener>>>,
}

impl ChannelHandle for ListenerHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        let reason = reason.to_owned();
        let runtime = self.runtime.clone();
        let listener = self.listener.clone();
        Box::pin(async move {
            runtime.log("info", &format!("wecom_callback: stopping ({reason})"));
            let listener = listener.lock().expect("#1336 listener handle").take();
            if let Some(listener) = listener {
                listener.stop().await;
            }
            Ok(())
        })
    }
}

/// Default interaction port used by the loader mount. Kept public so the
/// assembly can substitute the shared helper implementation later.
pub fn no_interaction() -> Arc<dyn InteractionPort> {
    Arc::new(NoInteraction)
}
