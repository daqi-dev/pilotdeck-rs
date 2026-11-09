//! Telegram channel on the native A7 turn port. Bot API HTTP replaces grammy;
//! the API root is injectable so all acceptance traffic can stay on loopback.
//! Webhook mode sets the remote webhook like the baseline; the embedding routes
//! decoded updates through `accept_webhook_update` (this module opens no listener).
mod mapper;
pub mod render;
#[cfg(test)]
mod tests;
use crate::channel::im_cron::{CronResultDelivery, deliver_chat_cron_result};
use crate::channel::sms;
use crate::channel::{
    ActiveChat,
    http_client::HttpClient,
    im_interaction::{InteractionPort, NoInteraction},
    protocol::*,
};
pub use mapper::{MappedSession, TelegramSessionMapper, TelegramSessionMapperState};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};

#[derive(Default)]
pub struct TelegramOptions {
    pub token: Option<JsString>,
    pub webhook_url: Option<JsString>,
    pub mapper: Option<TelegramSessionMapper>,
    /// Production default is https://api.telegram.org; tests inject loopback.
    pub api_root: Option<String>,
}
struct Inner {
    token: JsString,
    webhook_url: Option<JsString>,
    api_root: String,
    http: HttpClient,
    mapper: Mutex<TelegramSessionMapper>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    active: Arc<Mutex<HashSet<JsString>>>,
    running: AtomicBool,
    inbound: Mutex<Option<mpsc::Sender<JsValue>>>,
}
pub struct TelegramChannel {
    key: ChannelKey,
    inner: Arc<Inner>,
}
impl TelegramChannel {
    pub fn new(options: TelegramOptions) -> Self {
        Self::with_interaction(options, Arc::new(NoInteraction))
    }
    pub fn with_interaction(
        options: TelegramOptions,
        interaction: Arc<dyn InteractionPort>,
    ) -> Self {
        Self {
            key: ChannelKey::new("telegram"),
            inner: Arc::new(Inner {
                token: options.token.unwrap_or_else(|| {
                    std::env::var("TELEGRAM_BOT_TOKEN")
                        .unwrap_or_default()
                        .into()
                }),
                webhook_url: options.webhook_url,
                api_root: options
                    .api_root
                    .unwrap_or_else(|| "https://api.telegram.org".into())
                    .trim_end_matches('/')
                    .into(),
                http: HttpClient::new(),
                mapper: Mutex::new(options.mapper.unwrap_or_default()),
                interaction,
                deps: Mutex::new(ChannelStartDeps::default()),
                active: Arc::default(),
                running: AtomicBool::new(false),
                inbound: Mutex::new(None),
            }),
        }
    }
    /// Webhook routing and polling use the same owned task/admission path.
    pub async fn accept_webhook_update(&self, update: JsValue) -> Result<(), ChannelError> {
        let sender = self
            .inner
            .inbound
            .lock()
            .expect("telegram inbox")
            .clone()
            .ok_or_else(|| ChannelError::error("telegram: not started"))?;
        sender
            .send(update)
            .await
            .map_err(|_| ChannelError::error("telegram: stopped"))
    }
}
impl Inner {
    fn log(&self, level: &str, message: &str) {
        let logger = self.deps.lock().expect("telegram deps").log.clone();
        if let Some(log) = logger {
            log(level, message);
        }
    }
    async fn api(&self, method: &str, body: JsValue) -> Result<JsValue, ChannelError> {
        let token = self
            .token
            .to_utf8()
            .map_err(|_| ChannelError::error("telegram: invalid token"))?;
        let body = stringify_json(&body)
            .map_err(|e| ChannelError::error(e.to_string()))?
            .unwrap_or_else(|| "{}".into());
        let url = format!("{}/bot{token}/{method}", self.api_root);
        let response = self
            .http
            .request(
                "POST",
                &url,
                Some("application/json"),
                &body,
                Duration::from_secs(60),
            )
            .await
            .map_err(ChannelError::error)?;
        let value = parse_json(&String::from_utf8_lossy(&response.body))
            .map_err(|e| ChannelError::error(e.to_string()))?;
        if !response.is_ok() || value.get("ok").and_then(JsValue::as_bool) != Some(true) {
            return Err(ChannelError::error(
                value
                    .get("description")
                    .and_then(JsValue::as_string)
                    .map(|s| String::from_utf16_lossy(s.units()))
                    .unwrap_or_else(|| "Telegram API request failed".into()),
            ));
        }
        Ok(value.get("result").cloned().unwrap_or(JsValue::Undefined))
    }
    async fn send_reply(&self, chat: &JsString, text: &JsString) -> bool {
        if !self.running.load(Ordering::Acquire) {
            return false;
        }
        let mut ok = true;
        for chunk in render::chunk_text(text, 4096) {
            if let Err(error) = self
                .api(
                    "sendMessage",
                    object([
                        ("chat_id", JsValue::String(chat.clone())),
                        ("text", JsValue::String(chunk)),
                    ]),
                )
                .await
            {
                self.log("error", &format!("telegram: sendMessage failed: {error}"));
                ok = false;
            }
        }
        ok
    }
    async fn handle(self: Arc<Self>, update: JsValue) {
        let Some(message) = update.get("message") else {
            return;
        };
        let Some(text) = message
            .get("text")
            .and_then(JsValue::as_string)
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        let Some(chat) = message.get("chat").and_then(|c| c.get("id")) else {
            return;
        };
        let chat = sms::js_string_of(chat);
        let chat_id = String::from_utf16_lossy(chat.units());
        let input = String::from_utf16_lossy(text.units());
        let gateway = self
            .deps
            .lock()
            .expect("telegram deps")
            .turn_gateway
            .clone();
        if self.interaction.elicitation_pending(&chat_id) && gateway.is_some() {
            match self.interaction.elicitation_answer(&chat_id, &input).await {
                Ok(Some(reply)) => {
                    self.send_reply(&chat, &reply.into()).await;
                }
                Ok(None) => (),
                Err(e) => self.log("error", &format!("telegram: elicitation answer error: {e}")),
            }
            return;
        }
        if self.interaction.permission_pending(&chat_id) && gateway.is_some() {
            match self.interaction.permission_answer(&chat_id, &input).await {
                Ok(Some(answer)) => {
                    if let Some(reply) = answer.text.filter(|s| !s.is_empty()) {
                        if !self.send_reply(&chat, &reply.into()).await {
                            self.interaction
                                .permission_release_answer(&chat_id, answer.answer_token);
                            return;
                        }
                        if !answer.can_advance && !answer.retry_prompt {
                            return;
                        }
                        if let Some(next) = self
                            .interaction
                            .permission_take_next_prompt(&chat_id, answer.answer_token)
                        {
                            let request = self
                                .interaction
                                .permission_prompt_request_id(&chat_id, answer.answer_token);
                            let delivered = self.send_reply(&chat, &next.into()).await;
                            self.interaction.permission_confirm_next(
                                &chat_id,
                                delivered,
                                request.as_deref(),
                                answer.answer_token,
                            );
                        }
                    }
                }
                Ok(None) => (),
                Err(e) => self.log("error", &format!("telegram: permission answer error: {e}")),
            }
            return;
        }
        // Match the synchronous JS active-check/map/admit stretch. /new-only
        // replies never occupy an active-turn slot.
        let selected = {
            let mut mapper = self.mapper.lock().expect("telegram mapper");
            if self.active.lock().expect("telegram active").contains(&chat) {
                None
            } else {
                let mapped = mapper.resolve(&chat, text);
                let guard = if mapped.message.is_empty() {
                    None
                } else {
                    Some(
                        ActiveChat::admit(self.active.clone(), chat.clone())
                            .expect("serialized admission"),
                    )
                };
                Some((mapped, guard))
            }
        };
        let Some((mapped, _active)) = selected else {
            self.log(
                "info",
                &format!("telegram: chat {chat_id} already active, skipping"),
            );
            return;
        };
        if mapped.command_new && mapped.message.is_empty() {
            self.send_reply(&chat, &"已创建新会话。".into()).await;
            return;
        }
        if mapped.message.is_empty() {
            return;
        }
        let Some(gateway) = gateway else { return };
        let _interaction_cleanup = InteractionCleanup {
            inner: self.clone(),
            chat: chat_id.clone(),
        };
        let mut result = Vec::new();
        let processed = {
            let processed = async {
                let mut stream = gateway
                    .submit(TurnRequest {
                        session_key: mapped.session_key.clone(),
                        channel_key: ChannelKey::new("telegram"),
                        message: mapped.message,
                    })
                    .await?;
                while let Some(event) = stream.next().await? {
                    let kind = event.get("type").and_then(JsValue::as_string);
                    if kind.is_some_and(|s| s.equals_str("elicitation_request")) {
                        let prompt = self.interaction.elicitation_capture(
                            &chat_id,
                            &String::from_utf16_lossy(mapped.session_key.units()),
                            &event,
                        );
                        self.send_reply(&chat, &prompt.into()).await;
                        continue;
                    }
                    if kind.is_some_and(|s| s.equals_str("permission_request")) {
                        if let Some(prompt) = self.interaction.permission_capture(
                            &chat_id,
                            &String::from_utf16_lossy(mapped.session_key.units()),
                            &event,
                        ) {
                            let delivered = self.send_reply(&chat, &prompt.into()).await;
                            let request = event
                                .get("requestId")
                                .and_then(JsValue::as_string)
                                .map(|s| String::from_utf16_lossy(s.units()));
                            self.interaction.permission_confirm_initial(
                                &chat_id,
                                delivered,
                                request.as_deref(),
                            );
                        }
                        continue;
                    }
                    if let Some(fragment) = render::render_telegram_event(&event) {
                        result.extend_from_slice(fragment.units());
                    }
                }
                Ok::<(), ChannelError>(())
            };
            let typing = self.api(
                "sendChatAction",
                object([
                    ("chat_id", JsValue::String(chat.clone())),
                    ("action", "typing".into()),
                ]),
            );
            tokio::pin!(processed, typing);
            let mut typing_done = false;
            // This owned future is dropped with the turn; there is no detached
            // task whose cancellation could outlive stop's actual child drain.
            loop {
                tokio::select! { biased;
                    _ = &mut typing, if !typing_done => { typing_done=true; },
                    output = &mut processed => break output,
                }
            }
        };
        if let Err(error) = processed {
            self.log("error", &format!("telegram: submitTurn error: {error}"));
            result = "处理消息时发生错误，请重试。".encode_utf16().collect();
        }
        drop(_interaction_cleanup);
        let final_text = JsString::from_utf16(sms::trim_units(&result).to_vec());
        if !final_text.is_empty() {
            self.send_reply(&chat, &final_text).await;
        }
    }
}
struct InteractionCleanup {
    inner: Arc<Inner>,
    chat: String,
}
impl Drop for InteractionCleanup {
    fn drop(&mut self) {
        self.inner.interaction.elicitation_clear(&self.chat);
        self.inner
            .interaction
            .permission_clear_after_turn(&self.chat);
    }
}
struct RunningGuard(Arc<Inner>);
impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut value = JsObject::new();
    for (k, v) in fields {
        value.insert(k.into(), v);
    }
    JsValue::Object(value)
}
struct Handle {
    inner: Arc<Inner>,
    stop: watch::Sender<bool>,
    inbox: mpsc::Sender<JsValue>,
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}
impl ChannelHandle for Handle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        self.inner
            .log("info", &format!("telegram: stopping ({reason})"));
        {
            let mut current = self.inner.inbound.lock().expect("telegram inbox");
            if current
                .as_ref()
                .is_some_and(|sender| sender.same_channel(&self.inbox))
            {
                current.take();
            }
        }
        let _ = self.stop.send(true);
        let task = self.task.clone();
        Box::pin(async move {
            let mut task = task.lock().await;
            if let Some(worker) = task.as_mut() {
                let result = worker.await;
                task.take();
                result.map_err(|e| ChannelError::error(format!("telegram: worker failed: {e}")))?;
            }
            Ok(())
        })
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}
impl ChannelAdapter for TelegramChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let inner = self.inner.clone();
        *inner.deps.lock().expect("telegram deps") = deps;
        Box::pin(async move {
            if inner.token.is_empty() {
                inner.log("error", "telegram: TELEGRAM_BOT_TOKEN not set");
                return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
            }
            if inner.running.swap(true, Ordering::AcqRel) {
                return Err(ChannelError::error("telegram: already started"));
            }
            let setup = async {
                if let Some(url) = inner.webhook_url.as_ref().filter(|u| !u.is_empty()) {
                    inner
                        .api(
                            "setWebhook",
                            object([("url", JsValue::String(url.clone()))]),
                        )
                        .await?;
                    inner.log(
                        "info",
                        &format!(
                            "telegram: webhook mode at {}",
                            String::from_utf16_lossy(url.units())
                        ),
                    );
                } else {
                    inner.api("deleteWebhook", object([])).await?;
                    inner.log("info", "telegram: long-polling started");
                }
                let me = inner.api("getMe", object([])).await?;
                let name = me
                    .get("username")
                    .map(sms::js_string_of)
                    .unwrap_or_else(|| "undefined".into());
                inner.log(
                    "info",
                    &format!(
                        "telegram: connected as @{}",
                        String::from_utf16_lossy(name.units())
                    ),
                );
                Ok::<(), ChannelError>(())
            }
            .await;
            if let Err(error) = setup {
                inner.running.store(false, Ordering::Release);
                inner.log("error", &format!("telegram: start failed: {error}"));
                return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
            }
            let (stop, mut stopped) = watch::channel(false);
            let (tx, mut updates) = mpsc::channel(64);
            *inner.inbound.lock().expect("telegram inbox") = Some(tx.clone());
            let owner = inner.clone();
            let task = tokio::spawn(async move {
                let _running = RunningGuard(owner.clone());
                let mut tasks = JoinSet::new();
                let mut offset = 0f64;
                let webhook = owner.webhook_url.as_ref().is_some_and(|u| !u.is_empty());
                let mut poll = Box::pin(owner.api(
                    "getUpdates",
                    object([
                        ("offset", JsValue::Number(offset)),
                        ("timeout", JsValue::Number(30.0)),
                        ("allowed_updates", JsValue::Array(vec!["message".into()])),
                    ]),
                ));
                loop {
                    tokio::select! {
                        biased;
                        _ = stopped.changed() => break,
                        Some(_) = tasks.join_next(), if !tasks.is_empty() => {},
                        Some(update) = updates.recv() => { let worker = owner.clone(); tasks.spawn(async move { worker.handle(update).await; }); },
                        result = &mut poll, if !webhook => {
                            match result {
                                Ok(JsValue::Array(ref rows)) => for row in rows {
                                    if let Some(JsValue::Number(id)) = row.get("update_id") { offset = offset.max(*id + 1.0); }
                                    let worker = owner.clone(); let row=row.clone(); tasks.spawn(async move { worker.handle(row).await; });
                                },
                                Ok(_) => (), Err(error) => { owner.log("error", &format!("telegram: bot error: {error}")); tokio::select! { _ = stopped.changed() => break, _ = tokio::time::sleep(Duration::from_secs(1)) => {} } },
                            }
                            poll = Box::pin(owner.api("getUpdates", object([("offset", JsValue::Number(offset)), ("timeout", JsValue::Number(30.0)), ("allowed_updates", JsValue::Array(vec!["message".into()]))])));
                        }
                    }
                }
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            });
            Ok(Arc::new(Handle {
                inner,
                stop,
                inbox: tx,
                task: Arc::new(tokio::sync::Mutex::new(Some(task))),
            }) as Arc<dyn ChannelHandle>)
        })
    }
    fn deliver_cron_result(&self, delivery: &CronResultDelivery) -> BoxFuture<bool> {
        let inner = self.inner.clone();
        deliver_chat_cron_result(delivery, &self.key, move |chat, text| {
            Box::pin(async move { inner.send_reply(&chat, &text).await })
        })
    }
}
struct Noop;
impl ChannelHandle for Noop {
    fn stop(&self, _: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}
pub fn register(mounts: &mut crate::channel::loader::ChannelLoaderMounts) {
    register_with_interaction(mounts, Arc::new(NoInteraction));
}
/// Composition supplies a fresh helper per adapter/reload, backed by the
/// common gateway coordinator rather than a second hidden session owner.
pub fn register_with_interaction(
    mounts: &mut crate::channel::loader::ChannelLoaderMounts,
    interaction: Arc<dyn InteractionPort>,
) {
    mounts.register(
        "telegram",
        Arc::new(move |options: &JsObject| {
            Ok(Arc::new(TelegramChannel::with_interaction(
                TelegramOptions {
                    token: options
                        .get("token")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of),
                    webhook_url: options
                        .get("webhookUrl")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of),
                    ..Default::default()
                },
                interaction.clone(),
            )) as Arc<dyn ChannelAdapter>)
        }),
    );
}
