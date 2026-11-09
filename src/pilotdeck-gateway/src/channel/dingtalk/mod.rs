//! DingTalk channel: native Stream HTTP/WS transport, A7 turns, existing IM helpers.
pub mod mapper;
pub mod render;
pub mod stream;
#[cfg(test)]
mod tests;
use super::{
    http_client::HttpClient,
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    im_interaction::InteractionPort,
    protocol::*,
};
pub use mapper::{DingTalkSessionMapper, DingTalkSessionMapperState};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Notify, watch};
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn shown(s: &JsString) -> String {
    String::from_utf16_lossy(s.units())
}
fn is(v: &JsValue, k: &str, s: &str) -> bool {
    v.get(k)
        .and_then(JsValue::as_string)
        .is_some_and(|v| v.equals_str(s))
}
fn js(v: Option<&JsValue>) -> JsString {
    super::sms::js_string_of(
        v.filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .unwrap_or(&JsValue::String("".into())),
    )
}
fn obj(rows: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in rows {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
#[derive(Default)]
pub struct DingTalkOptions {
    pub client_id: Option<JsString>,
    pub client_secret: Option<JsString>,
    pub mapper: DingTalkSessionMapper,
    pub env: Option<super::sms::EnvLookup>,
    /// Default production endpoint; override only for loopback transport tests.
    pub stream_endpoint: Option<String>,
    /// Tests preserve origin validation on the original webhook, then route to a loopback mock.
    pub webhook_route: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
}
impl DingTalkOptions {
    pub fn from_object(o: &JsObject) -> Self {
        Self {
            client_id: o.get("clientId").map(|v| js(Some(v))),
            client_secret: o.get("clientSecret").map(|v| js(Some(v))),
            ..Default::default()
        }
    }
}
struct Shared {
    id: String,
    secret: String,
    endpoint: String,
    route: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
    mapper: Mutex<DingTalkSessionMapper>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    webhooks: Mutex<(HashMap<JsString, JsString>, VecDeque<JsString>)>,
    seen: Mutex<(HashSet<JsString>, VecDeque<JsString>)>,
    closed: AtomicBool,
    active_chats: Arc<Mutex<HashSet<JsString>>>,
    active_tasks: AtomicUsize,
    idle: Notify,
    http: HttpClient,
}
impl Shared {
    fn log(&self, level: &str, text: String) {
        let l = lock(&self.deps).log.clone();
        if let Some(l) = l {
            l(level, &text)
        }
    }
    fn finish_task(&self) {
        if self.active_tasks.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_waiters();
        }
    }
    async fn send_reply(&self, chat: &JsString, text: JsString) -> bool {
        let Some(url) = lock(&self.webhooks).0.get(chat).cloned() else {
            self.log(
                "warn",
                format!(
                    "dingtalk: no sessionWebhook for chat {}, cannot send",
                    shown(chat)
                ),
            );
            return false;
        };
        let url = shown(&url);
        if !url.starts_with("https://api.dingtalk.com/") {
            self.log(
                "warn",
                format!(
                    "dingtalk: sessionWebhook for {} failed origin check",
                    shown(chat)
                ),
            );
            return false;
        }
        let route = self.route.as_ref().map(|r| r(&url)).unwrap_or(url);
        let payload = obj([
            ("msgtype", "markdown".into()),
            (
                "markdown",
                obj([
                    ("title", "Reply".into()),
                    (
                        "text",
                        JsValue::String(JsString::from_utf16(
                            text.units()[..text.units().len().min(20_000)].to_vec(),
                        )),
                    ),
                ]),
            ),
        ]);
        let body = match stringify_json(&payload) {
            Ok(Some(s)) => s,
            _ => return false,
        };
        match self
            .http
            .request_unbounded("POST", &route, Some("application/json"), &body)
            .await
        {
            Ok(r) if r.is_ok() => true,
            Ok(r) => {
                self.log(
                    "error",
                    format!(
                        "dingtalk: sendReply HTTP {}: {}",
                        r.status,
                        String::from_utf8_lossy(&r.body)
                            .chars()
                            .take(200)
                            .collect::<String>()
                    ),
                );
                false
            }
            Err(e) => {
                self.log("error", format!("dingtalk: sendReply failed: {e}"));
                false
            }
        }
    }
    fn downstream(self: &Arc<Self>, msg: JsValue) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let topic = js(msg.get("headers").and_then(|h| h.get("topic")));
        if !topic.is_empty() && !topic.equals_str(stream::TOPIC_ROBOT) {
            return;
        }
        let Some(raw) = msg.get("data").and_then(JsValue::as_string) else {
            return;
        };
        let Ok(data) = parse_json(&shown(raw)) else {
            return;
        };
        let source_id = [
            data.get("msgId"),
            data.get("messageId"),
            msg.get("headers").and_then(|h| h.get("messageId")),
        ]
        .into_iter()
        .flatten()
        .find(|v| !matches!(v, JsValue::Null | JsValue::Undefined));
        let id = source_id
            .map(|v| js(Some(v)))
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string().into());
        {
            let mut seen = lock(&self.seen);
            if seen.0.contains(&id) {
                return;
            }
            seen.0.insert(id.clone());
            seen.1.push_back(id);
            if seen.0.len() > 2000 {
                if let Some(first) = seen.1.pop_front() {
                    if !first.is_empty() {
                        seen.0.remove(&first);
                    }
                }
            }
        }
        let text = extract_text(&data);
        if text.is_empty() {
            return;
        }
        let conversation = js(data.get("conversationId"));
        let chat = if conversation.is_empty() {
            js(data.get("senderId"))
        } else {
            conversation
        };
        if chat.is_empty() {
            return;
        }
        let url = js(data.get("sessionWebhook"));
        if shown(&url).starts_with("https://api.dingtalk.com/") {
            let mut w = lock(&self.webhooks);
            if w.0.len() >= 500 {
                if let Some(k) = w.1.pop_front() {
                    if !k.is_empty() {
                        w.0.remove(&k);
                    }
                }
            }
            if !w.0.contains_key(&chat) {
                w.1.push_back(chat.clone());
            }
            w.0.insert(chat.clone(), url);
        }
        self.active_tasks.fetch_add(1, Ordering::SeqCst);
        let done = TaskDone(self.clone());
        let shared = self.clone();
        tokio::spawn(async move {
            let _done = done;
            shared.incoming(chat, text).await;
        });
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
                Err(e) => self.log("error", format!("dingtalk: elicitation answer error: {e}")),
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
                Err(e) => self.log("error", format!("dingtalk: permission answer error: {e}")),
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
                    format!("dingtalk: chat {id} already active, skipping"),
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
                channel_key: ChannelKey::new("dingtalk"),
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
                if let Some(fragment) = render::render_dingtalk_event(&event) {
                    reply.extend_from_slice(fragment.units());
                }
            }
        }
        if let Some(error) = failure {
            self.log("error", format!("dingtalk: submitTurn error: {error}"));
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
}

pub fn extract_text(data: &JsValue) -> JsString {
    let t = data.get("text");
    let value = if let Some(content) = t.and_then(|t| t.as_object()).and_then(|o| o.get("content"))
    {
        js(Some(content))
    } else if let Some(JsValue::String(t)) = t {
        t.clone()
    } else if let Some(JsValue::Array(parts)) = data
        .get("richText")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .or_else(|| data.get("rich_text"))
    {
        let mut u = Vec::new();
        for p in parts {
            if p.as_object().is_none() {
                continue;
            }
            let s = js(p.get("text"));
            if s.is_empty() {
                continue;
            }
            if !u.is_empty() {
                u.push(32)
            }
            u.extend_from_slice(s.units())
        }
        JsString::from_utf16(u)
    } else {
        "".into()
    };
    JsString::from_utf16(super::sms::trim_units(value.units()).to_vec())
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

pub struct DingTalkChannel {
    key: ChannelKey,
    shared: Arc<Shared>,
}
impl DingTalkChannel {
    pub fn new(options: DingTalkOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        let env = |k: &str| {
            options.env.as_ref().and_then(|e| e(k)).or_else(|| {
                if options.env.is_none() {
                    std::env::var(k).ok().map(Into::into)
                } else {
                    None
                }
            })
        };
        let id = options
            .client_id
            .clone()
            .or_else(|| env("DINGTALK_CLIENT_ID"))
            .unwrap_or_else(|| "".into());
        let secret = options
            .client_secret
            .clone()
            .or_else(|| env("DINGTALK_CLIENT_SECRET"))
            .unwrap_or_else(|| "".into());
        Self {
            key: ChannelKey::new("dingtalk"),
            shared: Arc::new(Shared {
                id: shown(&JsString::from_utf16(
                    super::sms::trim_units(id.units()).to_vec(),
                )),
                secret: shown(&JsString::from_utf16(
                    super::sms::trim_units(secret.units()).to_vec(),
                )),
                endpoint: options
                    .stream_endpoint
                    .unwrap_or_else(|| stream::GATEWAY_URL.into()),
                route: options.webhook_route,
                mapper: Mutex::new(options.mapper),
                interaction,
                deps: Mutex::new(Default::default()),
                webhooks: Mutex::new(Default::default()),
                seen: Mutex::new(Default::default()),
                closed: AtomicBool::new(true),
                active_chats: Arc::new(Mutex::new(HashSet::new())),
                active_tasks: AtomicUsize::new(0),
                idle: Notify::new(),
                http: HttpClient::new(),
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
        self.stop.send_replace(true);
        *lock(&self.shared.webhooks) = Default::default();
        *lock(&self.shared.seen) = Default::default();
        let shared = self.shared.clone();
        let task = self.task.clone();
        let reason = reason.to_owned();
        Box::pin(async move {
            shared.log("info", format!("dingtalk: stopping ({reason})"));
            if let Some(task) = task.lock().await.take() {
                let _ = task.await;
            }
            *lock(&shared.webhooks) = Default::default();
            *lock(&shared.seen) = Default::default();
            Ok(())
        })
    }
}
impl ChannelAdapter for DingTalkChannel {
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
            if shared.id.is_empty() || shared.secret.is_empty() {
                shared.log(
                    "error",
                    "dingtalk: clientId and clientSecret are required".into(),
                );
                return Ok(Arc::new(Empty) as Arc<dyn ChannelHandle>);
            }
            let socket = match stream::connect(&shared.endpoint, &shared.id, &shared.secret).await {
                Ok(s) => s,
                Err(e) => {
                    shared.log("error", format!("dingtalk: start failed: {e}"));
                    return Ok(Arc::new(Empty) as Arc<dyn ChannelHandle>);
                }
            };
            shared.closed.store(false, Ordering::SeqCst);
            let (stop, stopped) = watch::channel(false);
            let weak = Arc::downgrade(&shared);
            let endpoint = shared.endpoint.clone();
            let id = shared.id.clone();
            let secret = shared.secret.clone();
            let task = tokio::spawn(stream::run(
                socket,
                endpoint,
                id,
                secret,
                stopped,
                Arc::new(move |msg| {
                    if let Some(shared) = weak.upgrade() {
                        shared.downstream(msg)
                    }
                }),
            ));
            shared.log("info", "dingtalk: connected via Stream mode".into());
            Ok(Arc::new(Handle {
                shared,
                stop,
                task: Arc::new(tokio::sync::Mutex::new(Some(task))),
            }) as Arc<dyn ChannelHandle>)
        })
    }
    fn deliver_cron_result(&self, delivery: &CronResultDelivery) -> BoxFuture<bool> {
        let shared = self.shared.clone();
        deliver_chat_cron_result(delivery, &self.key, move |chat, text| {
            Box::pin(async move { shared.send_reply(&chat, text).await })
        })
    }
}
