//! QQ Channel and its first-party bot gateway are native Rust; no TS helper.
pub mod gateway;
mod mapper;
pub mod render;
#[cfg(test)]
mod tests;
use crate::channel::{
    ActiveChat,
    im_interaction::{InteractionPort, NoInteraction},
    protocol::*,
    sms,
};
use gateway::{GatewayOptions, QQBotGateway};
pub use mapper::{MappedSession, QQSessionMapper, QQSessionMapperState};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
};
pub type StateChange = Arc<dyn Fn(QQSessionMapperState) + Send + Sync>;
#[derive(Default)]
pub struct QQOptions {
    pub app_id: Option<JsString>,
    pub client_secret: Option<JsString>,
    pub allow_groups: Option<Vec<JsString>>,
    pub trigger_prefixes: Option<Vec<JsString>>,
    pub max_message_length: Option<usize>,
    pub mapper: Option<QQSessionMapper>,
    pub on_state_change: Option<StateChange>,
    pub api_base: Option<String>,
    pub token_url: Option<String>,
}
struct SendContext {
    c2c: bool,
    target: JsString,
    msg_id: Option<JsString>,
}
struct Inner {
    app_id: JsString,
    client_secret: JsString,
    allow_groups: Vec<JsString>,
    trigger_prefixes: Vec<JsString>,
    max_message_length: usize,
    mapper: Mutex<QQSessionMapper>,
    on_state_change: Option<StateChange>,
    gateway: Arc<QQBotGateway>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    active: Arc<Mutex<HashSet<JsString>>>,
    running: AtomicBool,
    inbound: Mutex<Option<mpsc::Sender<JsValue>>>,
}
pub struct QQChannel {
    key: ChannelKey,
    inner: Arc<Inner>,
}
impl QQChannel {
    pub fn new(options: QQOptions) -> Self {
        Self::with_interaction(options, Arc::new(NoInteraction))
    }
    pub fn with_interaction(options: QQOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        let app_id = options
            .app_id
            .unwrap_or_else(|| std::env::var("QQ_BOT_APPID").unwrap_or_default().into());
        let client_secret = options
            .client_secret
            .unwrap_or_else(|| std::env::var("QQ_BOT_SECRET").unwrap_or_default().into());
        let allow_groups = options
            .allow_groups
            .or_else(|| {
                std::env::var("QQ_ALLOW_GROUPS")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(|s| {
                        s.split(',')
                            .map(crate::metadata::js_trim)
                            .filter(|s| !s.is_empty())
                            .map(Into::into)
                            .collect()
                    })
            })
            .unwrap_or_else(|| vec!["*".into()]);
        let gateway = GatewayOptions {
            app_id: app_id.clone(),
            client_secret: client_secret.clone(),
            api_base: options
                .api_base
                .unwrap_or_else(|| "https://api.sgroup.qq.com".into()),
            token_url: options
                .token_url
                .unwrap_or_else(|| "https://bots.qq.com/app/getAppAccessToken".into()),
        };
        Self {
            key: ChannelKey::new("qq"),
            inner: Arc::new(Inner {
                app_id,
                client_secret,
                allow_groups,
                trigger_prefixes: options
                    .trigger_prefixes
                    .unwrap_or_else(|| vec!["/ask".into(), "/chat".into()]),
                max_message_length: options.max_message_length.unwrap_or(2000),
                mapper: Mutex::new(options.mapper.unwrap_or_default()),
                on_state_change: options.on_state_change,
                gateway: Arc::new(QQBotGateway::new(gateway)),
                interaction,
                deps: Mutex::new(ChannelStartDeps::default()),
                active: Arc::default(),
                running: AtomicBool::new(false),
                inbound: Mutex::new(None),
            }),
        }
    }
}
impl Inner {
    fn log(&self, level: &str, text: &str) {
        let log = self.deps.lock().expect("qq deps").log.clone();
        if let Some(log) = log {
            log(level, text);
        }
    }
    async fn send_reply(&self, ctx: &SendContext, text: &JsString) -> bool {
        if !self.running.load(Ordering::Acquire) {
            return false;
        }
        match self
            .gateway
            .send(ctx.c2c, &ctx.target, text, ctx.msg_id.as_ref(), None)
            .await
        {
            Ok(()) => true,
            Err(e) => {
                let method = if ctx.c2c {
                    "sendC2CMessage"
                } else {
                    "sendGroupMessage"
                };
                self.log("error", &format!("qq: {method} failed: {e}"));
                false
            }
        }
    }
    async fn send_chunked(&self, ctx: &SendContext, text: &JsString) -> bool {
        let chunks = render::split_text(text, self.max_message_length);
        for (index, chunk) in chunks.iter().enumerate() {
            if !self.running.load(Ordering::Acquire) {
                return false;
            }
            if let Err(e) = self
                .gateway
                .send(
                    ctx.c2c,
                    &ctx.target,
                    chunk,
                    ctx.msg_id.as_ref(),
                    Some(index + 1),
                )
                .await
            {
                let method = if ctx.c2c {
                    "sendC2CMessage"
                } else {
                    "sendGroupMessage"
                };
                self.log("error", &format!("qq: {method} failed: {e}"));
                return false;
            }
            if chunks.len() > 1 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        true
    }
    async fn handle(self: Arc<Self>, update: JsValue) {
        let c2c = update.get("c2c").and_then(JsValue::as_bool) == Some(true);
        let Some(event) = update.get("data") else {
            return;
        };
        let user = event
            .get("author")
            .and_then(|a| a.get(if c2c { "user_openid" } else { "member_openid" }))
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        if user.is_empty() {
            return;
        }
        let group = event
            .get("group_openid")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        if !c2c
            && (group.is_empty()
                || !self
                    .allow_groups
                    .iter()
                    .any(|g| g.equals_str("*") || g == &group))
        {
            return;
        }
        let raw = event
            .get("content")
            .filter(|v| !sms::nullish(v))
            .map(sms::js_string_of)
            .unwrap_or_default();
        let raw = JsString::from_utf16(sms::trim_units(raw.units()).to_vec());
        if raw.is_empty() {
            return;
        }
        let text = if c2c {
            raw
        } else {
            let mut text = raw.clone();
            for prefix in &self.trigger_prefixes {
                if raw.units().starts_with(prefix.units()) {
                    text = JsString::from_utf16(
                        sms::trim_units(&raw.units()[prefix.units().len()..]).to_vec(),
                    );
                    break;
                }
            }
            text
        };
        if text.is_empty() {
            return;
        }
        let chat = if c2c {
            sms::concat_units(&[&"c2c:".encode_utf16().collect::<Vec<_>>(), user.units()])
        } else {
            sms::concat_units(&[group.units(), &[58], user.units()])
        };
        let mut send_ctx = SendContext {
            c2c,
            target: if c2c { user.clone() } else { group.clone() },
            msg_id: None,
        };
        let event_id = event.get("id").and_then(JsValue::as_string).cloned();
        let chat_id = String::from_utf16_lossy(chat.units());
        let input = String::from_utf16_lossy(text.units());
        let gateway = self.deps.lock().expect("qq deps").turn_gateway.clone();
        if self.interaction.elicitation_pending(&chat_id) && gateway.is_some() {
            match self.interaction.elicitation_answer(&chat_id, &input).await {
                Ok(Some(reply)) => {
                    self.send_reply(&send_ctx, &reply.into()).await;
                }
                Ok(None) => (),
                Err(e) => self.log("error", &format!("qq: elicitation answer error: {e}")),
            }
            return;
        }
        if self.interaction.permission_pending(&chat_id) && gateway.is_some() {
            match self.interaction.permission_answer(&chat_id, &input).await {
                Ok(Some(answer)) => {
                    if let Some(reply) = answer.text.filter(|s| !s.is_empty()) {
                        if !self.send_reply(&send_ctx, &reply.into()).await {
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
                            let delivered = self.send_reply(&send_ctx, &next.into()).await;
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
                Err(e) => self.log("error", &format!("qq: permission answer error: {e}")),
            }
            return;
        }
        send_ctx.msg_id = event_id;
        // Match the synchronous JS active-check/map/admit stretch. /new-only
        // replies never occupy an active-turn slot.
        let selected = {
            let mut mapper = self.mapper.lock().expect("qq mapper");
            if self.active.lock().expect("qq active").contains(&chat) {
                None
            } else {
                let mapped = if c2c {
                    MappedSession {
                        session_key: sms::concat_units(&[
                            &"qq:c2c:".encode_utf16().collect::<Vec<_>>(),
                            user.units(),
                            &":general".encode_utf16().collect::<Vec<_>>(),
                        ]),
                        command_new: false,
                        message: text.clone(),
                    }
                } else {
                    mapper.resolve(&group, &user, &text)
                };
                if mapped.command_new {
                    if let Some(callback) = &self.on_state_change {
                        callback(mapper.snapshot());
                    }
                }
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
                &format!("qq: chat {chat_id} already active, skipping"),
            );
            return;
        };
        if mapped.command_new && mapped.message.is_empty() {
            self.send_reply(&send_ctx, &"已创建新会话。".into()).await;
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
        let processed = async {
            let mut stream = gateway
                .submit(TurnRequest {
                    session_key: mapped.session_key.clone(),
                    channel_key: ChannelKey::new("qq"),
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
                    self.send_chunked(&send_ctx, &prompt.into()).await;
                    continue;
                }
                if kind.is_some_and(|s| s.equals_str("permission_request")) {
                    if let Some(prompt) = self.interaction.permission_capture(
                        &chat_id,
                        &String::from_utf16_lossy(mapped.session_key.units()),
                        &event,
                    ) {
                        let delivered = self.send_chunked(&send_ctx, &prompt.into()).await;
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
                if let Some(fragment) = render::render_qq_event(&event) {
                    result.extend_from_slice(fragment.units());
                }
            }
            Ok::<(), ChannelError>(())
        }
        .await;
        if let Err(error) = processed {
            self.log("error", &format!("qq: submitTurn error: {error}"));
            result = "处理消息时发生错误，请重试。".encode_utf16().collect();
        }
        drop(_interaction_cleanup);
        let final_text = JsString::from_utf16(sms::trim_units(&result).to_vec());
        if !final_text.is_empty() {
            self.send_chunked(&send_ctx, &final_text).await;
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
fn object_map<const N: usize>(fields: [(&str, JsValue); N]) -> JsObject {
    let mut value = JsObject::new();
    for (k, v) in fields {
        value.insert(k.into(), v);
    }
    value
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    JsValue::Object(object_map(fields))
}
struct Handle {
    inner: Arc<Inner>,
    stop: watch::Sender<bool>,
    inbox: mpsc::Sender<JsValue>,
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}
impl ChannelHandle for Handle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        self.inner.log("info", &format!("qq: stopping ({reason})"));
        {
            let mut current = self.inner.inbound.lock().expect("qq inbox");
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
                result.map_err(|e| ChannelError::error(format!("qq: worker failed: {e}")))?;
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
struct StartupGuard {
    inner: Arc<Inner>,
    armed: bool,
}
impl Drop for StartupGuard {
    fn drop(&mut self) {
        if self.armed {
            self.inner.running.store(false, Ordering::Release);
        }
    }
}
impl ChannelAdapter for QQChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let inner = self.inner.clone();
        *inner.deps.lock().expect("qq deps") = deps;
        Box::pin(async move {
            if inner.app_id.is_empty() || inner.client_secret.is_empty() {
                inner.log(
                    "info",
                    "qq: QQ_BOT_APPID or QQ_BOT_SECRET not set, QQ channel disabled.",
                );
                return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
            }
            if inner.max_message_length == 0 {
                return Err(ChannelError::error("QQ maxMessageLength must be positive"));
            }
            if inner.running.swap(true, Ordering::AcqRel) {
                return Err(ChannelError::error("qq: already started"));
            }
            let mut startup = StartupGuard {
                inner: inner.clone(),
                armed: true,
            };
            let endpoint = match inner.gateway.prepare().await {
                Ok(url) => url,
                Err(e) => {
                    inner.log("error", &format!("qq: failed to connect: {e}"));
                    return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
                }
            };
            let (stop, stopped) = watch::channel(false);
            let (tx, mut updates) = mpsc::channel(64);
            *inner.inbound.lock().expect("qq inbox") = Some(tx.clone());
            let owner = inner.clone();
            let receive_tx = tx.clone();
            let log = inner.deps.lock().expect("qq deps").log.clone();
            inner.log(
                "info",
                &format!(
                    "qq: connecting to QQ Official Bot Gateway (appId={})...",
                    String::from_utf16_lossy(inner.app_id.units())
                ),
            );
            let task = tokio::spawn(async move {
                let _running = RunningGuard(owner.clone());
                let receive = async {
                    let refresh = owner.gateway.clone().refresh_loop(stopped.clone());
                    let socket = owner
                        .gateway
                        .clone()
                        .run(endpoint, receive_tx, stopped, log);
                    tokio::join!(socket, refresh);
                };
                tokio::pin!(receive);
                let mut tasks = JoinSet::new();
                loop {
                    tokio::select! {biased;Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},Some(update)=updates.recv(),if tasks.len()<64=>{let worker=owner.clone();tasks.spawn(async move{worker.handle(update).await;});},_=&mut receive=>break,}
                }
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            });
            startup.armed = false;
            Ok(Arc::new(Handle {
                inner,
                stop,
                inbox: tx,
                task: Arc::new(tokio::sync::Mutex::new(Some(task))),
            }) as Arc<dyn ChannelHandle>)
        })
    }
}
struct Noop;
impl ChannelHandle for Noop {
    fn stop(&self, _: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}
// QQ is a strict-enabled bundle channel, not a CHANNEL_LOADERS member.
// NativeChannels owns mapper persistence and constructs this adapter with
// QQOptions + a fresh shared-coordinator interaction helper.
