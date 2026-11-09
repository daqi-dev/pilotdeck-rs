//! HomeAssistant WS channel against the existing A7 TurnGateway and IM helpers.
//! Fixed baseline cc7c4e6. No additional session owner or provider SDK stub.
pub mod mapper;
pub mod render;
#[cfg(test)]
mod tests;
use super::{
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    im_interaction::{InteractionPort, NoInteraction},
    protocol::*,
};
use futures_util::{SinkExt, StreamExt};
pub use mapper::{HomeAssistantSessionMapper, HomeAssistantSessionMapperState};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpStream,
    sync::{Notify, oneshot, watch},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
type Sink = futures_util::stream::SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
const AUTH_TIMEOUT: Duration = Duration::from_secs(20);
const SERVICE_TIMEOUT: Duration = Duration::from_secs(10);
const RECONNECT_DELAY: Duration = Duration::from_secs(5);
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn object(rows: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in rows {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn string(v: &JsValue) -> Option<JsString> {
    v.as_string().cloned()
}
fn is(v: &JsValue, key: &str, value: &str) -> bool {
    v.get(key)
        .and_then(JsValue::as_string)
        .is_some_and(|v| v.equals_str(value))
}
fn shown(s: &JsString) -> String {
    String::from_utf16_lossy(s.units())
}
pub struct HomeAssistantOptions {
    pub url: Option<JsString>,
    pub token: Option<JsString>,
    pub watch_prefixes: Vec<JsString>,
    pub notification_title: Option<JsString>,
    pub mapper: HomeAssistantSessionMapper,
    pub env: Option<super::sms::EnvLookup>,
    pub now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
}
impl Default for HomeAssistantOptions {
    fn default() -> Self {
        Self {
            url: None,
            token: None,
            watch_prefixes: vec![],
            notification_title: None,
            mapper: Default::default(),
            env: None,
            now_ms: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            }),
        }
    }
}
impl HomeAssistantOptions {
    pub fn from_object(options: &JsObject) -> Self {
        Self {
            url: options.get("url").and_then(string),
            token: options.get("token").and_then(string),
            watch_prefixes: options
                .get("watchPrefixes")
                .and_then(|v| match v {
                    JsValue::Array(a) => Some(a),
                    _ => None,
                })
                .map(|a| a.iter().filter_map(string).collect())
                .unwrap_or_default(),
            notification_title: options.get("notificationTitle").and_then(string),
            ..Default::default()
        }
    }
}
pub fn http_to_ws(base: &str) -> Result<String, ChannelError> {
    let input = if base.starts_with("http") {
        base.to_owned()
    } else {
        format!("http://{base}")
    };
    let u = url::Url::parse(&input).map_err(|e| ChannelError::error(e.to_string()))?;
    let protocol = if u.scheme() == "https" { "wss" } else { "ws" };
    let host = u
        .host_str()
        .ok_or_else(|| ChannelError::error("Invalid URL"))?;
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.into()
    };
    let authority = match u.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };
    Ok(format!(
        "{protocol}://{authority}{}/api/websocket",
        u.path().strip_suffix('/').unwrap_or(u.path())
    ))
}
struct Shared {
    url: String,
    token: Option<JsString>,
    prefixes: Vec<JsString>,
    title: Option<JsString>,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    mapper: Mutex<HomeAssistantSessionMapper>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    writer: tokio::sync::Mutex<Option<Sink>>,
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    auth: Mutex<Option<oneshot::Sender<bool>>>,
    id: AtomicU64,
    closed: AtomicBool,
    ready: AtomicBool,
    active_chats: Arc<Mutex<HashSet<JsString>>>,
    active_tasks: AtomicUsize,
    idle: Notify,
}
impl Shared {
    fn log(&self, level: &str, message: String) {
        let log = lock(&self.deps).log.clone();
        if let Some(log) = log {
            log(level, &message);
        }
    }
    fn fail_pending(&self) {
        for (_, done) in std::mem::take(&mut *lock(&self.pending)) {
            let _ = done.send(false);
        }
    }
    fn settle_auth(&self, ok: bool) {
        if let Some(done) = lock(&self.auth).take() {
            let _ = done.send(ok);
        }
    }
    async fn send(&self, message: JsValue) -> bool {
        let Ok(Some(text)) = stringify_json(&message) else {
            return false;
        };
        let mut writer = self.writer.lock().await;
        let Some(writer) = writer.as_mut() else {
            return false;
        };
        match tokio::time::timeout(SERVICE_TIMEOUT, writer.send(Message::Text(text.into()))).await {
            Ok(Ok(())) => true,
            other => {
                self.log("error", format!("homeassistant: send failed: {other:?}"));
                false
            }
        }
    }
    async fn send_reply(self: &Arc<Self>, chat: &JsString, text: JsString) -> bool {
        if self.closed.load(Ordering::SeqCst) || self.writer.lock().await.is_none() {
            self.log(
                "warn",
                format!(
                    "homeassistant: not connected, cannot send to {}",
                    shown(chat)
                ),
            );
            return false;
        }
        let id = self.id.fetch_add(1, Ordering::SeqCst);
        let (done, result) = oneshot::channel();
        lock(&self.pending).insert(id, done);
        let _pending = PendingReply {
            shared: Arc::downgrade(self),
            id,
        };
        let title = self.title.clone().unwrap_or_else(|| {
            let mut u: Vec<u16> = "Gateway · ".encode_utf16().collect();
            u.extend_from_slice(chat.units());
            JsString::from_utf16(u)
        });
        let message = object([
            ("id", JsValue::Number(id as f64)),
            ("type", "call_service".into()),
            ("domain", "persistent_notification".into()),
            ("service", "create".into()),
            (
                "service_data",
                object([
                    ("title", JsValue::String(title)),
                    ("message", JsValue::String(text)),
                    (
                        "notification_id",
                        JsValue::String(format!("gw_{}", (self.now_ms)()).into()),
                    ),
                ]),
            ),
        ]);
        if !self.send(message).await {
            lock(&self.pending).remove(&id);
            return false;
        }
        let delivered = tokio::time::timeout(SERVICE_TIMEOUT, result)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false);
        lock(&self.pending).remove(&id);
        delivered
    }
    fn finish_task(&self) {
        if self.active_tasks.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_waiters();
        }
    }
    async fn incoming(self: Arc<Self>, chat: JsString, text: JsString) {
        let id = shown(&chat);
        let text_scalar = shown(&text);
        let gateway = lock(&self.deps).turn_gateway.clone();
        if self.interaction.elicitation_pending(&id) && gateway.is_some() {
            match self.interaction.elicitation_answer(&id, &text_scalar).await {
                Ok(Some(reply)) => {
                    self.send_reply(&chat, reply.into()).await;
                }
                Ok(None) => {}
                Err(e) => self.log(
                    "error",
                    format!("homeassistant: elicitation answer error: {e}"),
                ),
            }
            return;
        }
        if self.interaction.permission_pending(&id) && gateway.is_some() {
            match self.interaction.permission_answer(&id, &text_scalar).await {
                Ok(Some(answer)) => {
                    let mut release = PermissionRelease {
                        port: self.interaction.clone(),
                        chat: id.clone(),
                        token: answer.answer_token,
                        armed: true,
                    };
                    if let Some(text) = answer.text {
                        if !self.send_reply(&chat, text.into()).await {
                            return;
                        }
                        if !answer.can_advance && !answer.retry_prompt {
                            release.armed = false;
                            return;
                        }
                        if let Some(next) = self
                            .interaction
                            .permission_take_next_prompt(&id, answer.answer_token)
                        {
                            let expected = self
                                .interaction
                                .permission_prompt_request_id(&id, answer.answer_token);
                            let delivered = self.send_reply(&chat, next.into()).await;
                            self.interaction.permission_confirm_next(
                                &id,
                                delivered,
                                expected.as_deref(),
                                answer.answer_token,
                            );
                        }
                    }
                    release.armed = false;
                }
                Ok(None) => {}
                Err(e) => self.log(
                    "error",
                    format!("homeassistant: permission answer error: {e}"),
                ),
            }
            return;
        }
        enum Admission {
            Busy,
            New,
            Empty,
            Turn(mapper::MappedSession),
        }
        let admitted = {
            let mut active = lock(&self.active_chats);
            if active.contains(&chat) {
                Admission::Busy
            } else {
                let mapped = lock(&self.mapper).resolve(&chat, &text);
                if mapped.new_session && mapped.message.is_empty() {
                    Admission::New
                } else if mapped.message.is_empty() {
                    Admission::Empty
                } else {
                    active.insert(chat.clone());
                    Admission::Turn(mapped)
                }
            }
        };
        let mapped = match admitted {
            Admission::Busy => {
                self.log(
                    "info",
                    format!("homeassistant: chat {id} already active, skipping"),
                );
                return;
            }
            Admission::New => {
                self.send_reply(&chat, "已创建新会话。".into()).await;
                return;
            }
            Admission::Empty => return,
            Admission::Turn(mapped) => mapped,
        };
        let _active = super::ActiveChat::new(self.active_chats.clone(), chat.clone());
        let Some(gateway) = gateway else { return };
        let mut reply = Vec::new();
        let stream = gateway
            .submit(TurnRequest {
                session_key: mapped.session_key.clone(),
                channel_key: ChannelKey::new("homeassistant"),
                message: mapped.message,
            })
            .await;
        let mut failure = stream.as_ref().err().map(ToString::to_string);
        if let Ok(mut stream) = stream {
            loop {
                let event = match stream.next().await {
                    Ok(Some(e)) => e,
                    Ok(None) => break,
                    Err(e) => {
                        failure = Some(e.to_string());
                        break;
                    }
                };
                if is(&event, "type", "elicitation_request") {
                    let prompt = self.interaction.elicitation_capture(
                        &id,
                        &shown(&mapped.session_key),
                        &event,
                    );
                    self.send_reply(&chat, prompt.into()).await;
                    continue;
                }
                if is(&event, "type", "permission_request") {
                    if let Some(prompt) = self.interaction.permission_capture(
                        &id,
                        &shown(&mapped.session_key),
                        &event,
                    ) {
                        let delivered = self.send_reply(&chat, prompt.into()).await;
                        let request = event
                            .get("requestId")
                            .and_then(JsValue::as_string)
                            .map(shown);
                        self.interaction.permission_confirm_initial(
                            &id,
                            delivered,
                            request.as_deref(),
                        );
                    }
                    continue;
                }
                if let Some(fragment) = render::render_home_assistant_event(&event) {
                    reply.extend_from_slice(fragment.units());
                }
            }
        }
        if let Some(error) = failure {
            self.log("error", format!("homeassistant: submitTurn error: {error}"));
            reply = "处理消息时发生错误，请重试。".encode_utf16().collect();
        }
        self.interaction.elicitation_clear(&id);
        self.interaction.permission_clear_after_turn(&id);
        let reply = super::sms::trim_units(&reply);
        if !reply.is_empty() {
            self.send_reply(&chat, JsString::from_utf16(reply.to_vec()))
                .await;
        }
    }
    async fn raw(self: &Arc<Self>, raw: &str) {
        let Ok(message) = parse_json(raw) else { return };
        if is(&message, "type", "result") {
            if let Some(id) = message
                .get("id")
                .and_then(JsValue::as_number)
                .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
                && let Some(done) = lock(&self.pending).remove(&(id as u64))
            {
                let _ = done.send(message.get("success") == Some(&JsValue::Bool(true)));
            }
            return;
        }
        if is(&message, "type", "auth_required") {
            self.send(object([
                ("type", "auth".into()),
                (
                    "access_token",
                    self.token
                        .clone()
                        .map(JsValue::String)
                        .unwrap_or(JsValue::Undefined),
                ),
            ]))
            .await;
            return;
        }
        if is(&message, "type", "auth_ok") {
            let id = self.id.fetch_add(1, Ordering::SeqCst);
            self.send(object([
                ("id", JsValue::Number(id as f64)),
                ("type", "subscribe_events".into()),
                ("event_type", "state_changed".into()),
            ]))
            .await;
            self.ready.store(true, Ordering::SeqCst);
            self.settle_auth(true);
            return;
        }
        if is(&message, "type", "auth_invalid") {
            self.settle_auth(false);
            self.log(
                "error",
                format!(
                    "homeassistant: auth invalid: {}",
                    message
                        .get("message")
                        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                        .map(super::sms::js_string_of)
                        .map(|s| shown(&s))
                        .unwrap_or_else(|| "invalid token".into())
                ),
            );
            return;
        }
        if !is(&message, "type", "event") {
            return;
        }
        let Some(event) = message.get("event") else {
            return;
        };
        if !is(event, "event_type", "state_changed") {
            return;
        }
        let Some(data) = event.get("data") else {
            return;
        };
        let Some(chat) = data
            .get("entity_id")
            .and_then(JsValue::as_string)
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        if !self
            .prefixes
            .iter()
            .any(|p| chat.units().starts_with(p.units()))
        {
            return;
        }
        let undefined = JsValue::Undefined;
        let new = data
            .get("new_state")
            .and_then(|s| s.get("state"))
            .unwrap_or(&undefined);
        let old = data
            .get("old_state")
            .and_then(|s| s.get("state"))
            .unwrap_or(&undefined);
        // JS strict equality: independently decoded objects are never identical.
        if !matches!(new, JsValue::Object(_) | JsValue::Array(_)) && new == old {
            return;
        }
        let text = match new {
            JsValue::String(s) => s.clone(),
            _ => match stringify_json(new) {
                Ok(Some(s)) => s.into(),
                _ => return,
            },
        };
        if super::sms::trim_units(text.units()).is_empty() || self.closed.load(Ordering::SeqCst) {
            return;
        }
        self.active_tasks.fetch_add(1, Ordering::SeqCst);
        let shared = self.clone();
        let chat = chat.clone();
        let done = TaskDone(shared.clone());
        tokio::spawn(async move {
            let _done = done;
            shared.incoming(chat, text).await;
        });
    }
}
struct PendingReply {
    shared: std::sync::Weak<Shared>,
    id: u64,
}
impl Drop for PendingReply {
    fn drop(&mut self) {
        if let Some(shared) = self.shared.upgrade() {
            lock(&shared.pending).remove(&self.id);
        }
    }
}
struct Starting {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    armed: bool,
}
impl Drop for Starting {
    fn drop(&mut self) {
        if self.armed {
            self.shared.closed.store(true, Ordering::SeqCst);
            self.shared.fail_pending();
            self.stop.send_replace(true);
        }
    }
}

struct PermissionRelease {
    port: Arc<dyn InteractionPort>,
    chat: String,
    token: Option<u64>,
    armed: bool,
}
impl Drop for PermissionRelease {
    fn drop(&mut self) {
        if self.armed {
            self.port.permission_release_answer(&self.chat, self.token);
        }
    }
}
struct TaskDone(Arc<Shared>);
impl Drop for TaskDone {
    fn drop(&mut self) {
        self.0.finish_task();
    }
}
pub struct HomeAssistantChannel {
    key: ChannelKey,
    shared: Arc<Shared>,
}
impl HomeAssistantChannel {
    pub fn new(options: HomeAssistantOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        let lookup = |key: &str| {
            options.env.as_ref().and_then(|get| get(key)).or_else(|| {
                if options.env.is_none() {
                    std::env::var(key).ok().map(Into::into)
                } else {
                    None
                }
            })
        };
        let url = options
            .url
            .clone()
            .or_else(|| lookup("HASS_URL"))
            .unwrap_or_else(|| "http://127.0.0.1:8123".into());
        let token = options.token.clone().or_else(|| lookup("HASS_TOKEN"));
        Self {
            key: ChannelKey::new("homeassistant"),
            shared: Arc::new(Shared {
                url: shown(&url),
                token,
                prefixes: if options.watch_prefixes.is_empty() {
                    vec!["conversation.".into()]
                } else {
                    options.watch_prefixes
                },
                title: options.notification_title,
                now_ms: options.now_ms,
                mapper: Mutex::new(options.mapper),
                interaction,
                deps: Mutex::new(Default::default()),
                writer: tokio::sync::Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
                auth: Mutex::new(None),
                id: AtomicU64::new(1),
                closed: AtomicBool::new(true),
                ready: AtomicBool::new(false),
                active_chats: Arc::new(Mutex::new(HashSet::new())),
                active_tasks: AtomicUsize::new(0),
                idle: Notify::new(),
            }),
        }
    }
    pub fn active_tasks(&self) -> usize {
        self.shared.active_tasks.load(Ordering::SeqCst)
    }
    /// Separate from TS stop: call after the shared A7 owner has drained turns.
    pub fn drain(&self) -> BoxFuture<()> {
        let shared = self.shared.clone();
        Box::pin(async move {
            loop {
                let notified = shared.idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if shared.active_tasks.load(Ordering::SeqCst) == 0 {
                    return;
                }
                notified.await;
            }
        })
    }
}
pub fn no_interaction() -> Arc<dyn InteractionPort> {
    Arc::new(NoInteraction)
}
struct Empty;
impl ChannelHandle for Empty {
    fn stop(&self, _: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}
struct Handle {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}
impl ChannelHandle for Handle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.shared.ready.store(false, Ordering::SeqCst);
        self.shared.fail_pending();
        self.stop.send_replace(true);
        let shared = self.shared.clone();
        let task = self.task.clone();
        let reason = reason.to_owned();
        Box::pin(async move {
            shared.log("info", format!("homeassistant: stopping ({reason})"));
            if let Some(task) = task.lock().await.take() {
                let _ = task.await;
            }
            Ok(())
        })
    }
}
impl ChannelAdapter for HomeAssistantChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        *lock(&self.shared.deps) = deps;
        let shared = self.shared.clone();
        Box::pin(async move {
            if shared.token.as_ref().is_none_or(JsString::is_empty) {
                shared.log("error", "homeassistant: HASS_TOKEN not set".into());
                return Ok(Arc::new(Empty) as Arc<dyn ChannelHandle>);
            }
            let url = http_to_ws(&shared.url)?;
            shared.closed.store(false, Ordering::SeqCst);
            shared.ready.store(false, Ordering::SeqCst);
            let (auth, wait) = oneshot::channel();
            *lock(&shared.auth) = Some(auth);
            let (stop, mut stopped) = watch::channel(false);
            let reader_shared = shared.clone();
            let endpoint = url.clone();
            let task = tokio::spawn(async move {
                loop {
                    let connection = tokio::select! {biased;_=stopped.wait_for(|s|*s)=>break,result=connect_async(&endpoint)=>result};
                    match connection {
                        Ok((socket, _)) => {
                            let (sink, mut read) = socket.split();
                            *reader_shared.writer.lock().await = Some(sink);
                            loop {
                                let next = tokio::select! {biased;_=stopped.wait_for(|s|*s)=>break,item=read.next()=>item};
                                match next {
                                    Some(Ok(Message::Text(raw))) => reader_shared.raw(&raw).await,
                                    Some(Ok(Message::Binary(raw))) => {
                                        reader_shared.raw(&String::from_utf8_lossy(&raw)).await
                                    }
                                    Some(Ok(Message::Ping(bytes))) => {
                                        if let Some(sink) =
                                            reader_shared.writer.lock().await.as_mut()
                                        {
                                            let _ = sink.send(Message::Pong(bytes)).await;
                                        }
                                    }
                                    Some(Ok(Message::Close(_))) | None => break,
                                    Some(Err(e)) => {
                                        reader_shared
                                            .log("error", format!("homeassistant: ws error: {e}"));
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        Err(e) => reader_shared
                            .log("error", format!("homeassistant: connect failed: {e}")),
                    }
                    reader_shared.fail_pending();
                    reader_shared.settle_auth(false);
                    if let Some(mut sink) = reader_shared.writer.lock().await.take() {
                        let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
                    }
                    if reader_shared.closed.load(Ordering::SeqCst)
                        || !reader_shared.ready.load(Ordering::SeqCst)
                    {
                        break;
                    }
                    tokio::select! {biased;_=stopped.wait_for(|s|*s)=>break,_=tokio::time::sleep(RECONNECT_DELAY)=>{}}
                }
                reader_shared.fail_pending();
                reader_shared.settle_auth(false);
                if let Some(mut sink) = reader_shared.writer.lock().await.take() {
                    let _ = tokio::time::timeout(Duration::from_secs(1), sink.close()).await;
                }
            });
            let task = Arc::new(tokio::sync::Mutex::new(Some(task)));
            let mut starting = Starting {
                shared: shared.clone(),
                stop: stop.clone(),
                armed: true,
            };
            let handle = Handle {
                shared: shared.clone(),
                stop,
                task,
            };
            if tokio::time::timeout(AUTH_TIMEOUT, wait)
                .await
                .ok()
                .and_then(Result::ok)
                != Some(true)
            {
                shared.log(
                    "error",
                    "homeassistant: WebSocket auth failed or timed out".into(),
                );
                shared.closed.store(true, Ordering::SeqCst);
                shared.ready.store(false, Ordering::SeqCst);
                shared.fail_pending();
                handle.stop.send_replace(true);
                if let Some(task) = handle.task.lock().await.take() {
                    let _ = task.await;
                }
                return Ok(Arc::new(Empty) as Arc<dyn ChannelHandle>);
            }
            shared.log("info", format!("homeassistant: WebSocket {url}"));
            starting.armed = false;
            Ok(Arc::new(handle) as Arc<dyn ChannelHandle>)
        })
    }
    fn deliver_cron_result(&self, delivery: &CronResultDelivery) -> BoxFuture<bool> {
        let shared = self.shared.clone();
        deliver_chat_cron_result(delivery, &self.key, move |chat, text| {
            Box::pin(async move { shared.send_reply(&chat, text).await })
        })
    }
}
