//! Feishu webhook text chain (FeishuChannel.ts:306-668,824-876,1041-1174).
//! Stream transport follows the fixed SDK protobuf protocol. Live posts use
//! real REST send/edit with text fallback and bounded streamed segments.
use super::home_assistant::interaction::{Chat, ImChannel, elicitation_prompt};

pub fn handle_inbound_text(text: &str, reply: impl FnMut(&str)) -> bool {
    super::channel_commands::dispatch_feishu_inbound_system_command(text, reply)
}

use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{
    ChannelEventSink, ChannelGateway, ChannelReply, ChannelSessionState, CommandContext,
    execute_channel_command,
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, watch},
    task::JoinSet,
};
mod http;
mod inbound;
mod live;
mod stream;
#[cfg(test)]
mod tests;
use http::Api;
use inbound::{Inbound, Parsed};
#[derive(Clone, Debug)]
pub struct FeishuOptions {
    pub app_id: String,
    pub app_secret: String,
    pub encrypt_key: Option<String>,
    pub verify_token: Option<String>,
    pub connection_mode: String,
    pub domain_name: String,
    pub permission_mode: Option<String>,
    pub pilot_home: PathBuf,
    pub api_base: String,
    pub turn_timeout_ms: u64,
}
impl Default for FeishuOptions {
    fn default() -> Self {
        Self {
            app_id: String::new(),
            app_secret: String::new(),
            encrypt_key: None,
            verify_token: None,
            connection_mode: "stream".into(),
            domain_name: "feishu".into(),
            permission_mode: None,
            pilot_home: PathBuf::new(),
            api_base: "https://open.feishu.cn/open-apis".into(),
            turn_timeout_ms: 600_000,
        }
    }
}
pub trait FeishuStateChange: Send + Sync {
    fn changed(&self, state: Value);
}
#[derive(Default)]
struct TokenCache {
    value: Option<String>,
    expires_at: u64,
}
#[derive(Clone)]
struct Turn {
    session: String,
    message: String,
    project: Option<String>,
    message_id: Option<String>,
    generation: u64,
    attachments: Vec<Value>,
}
#[derive(Default)]
struct Queue {
    generation: u64,
    pending: VecDeque<Turn>,
    running: bool,
    active_session: Option<String>,
    active_run_id: Option<String>,
    cancel: Option<watch::Sender<bool>>,
}
#[derive(Default)]
struct Seen {
    ids: HashSet<String>,
    order: VecDeque<String>,
}
pub struct FeishuChannel {
    options: FeishuOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: Mutex<ChannelSessionState>,
    changed: Option<Arc<dyn FeishuStateChange>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    queues: StdMutex<HashMap<String, Queue>>,
    inbound: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
    seen: StdMutex<Seen>,
    token: Mutex<TokenCache>,
    running: AtomicBool,
    work: StdMutex<JoinSet<()>>,
    stream_stop: StdMutex<Option<watch::Sender<bool>>>,
    stream_task: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    batches: StdMutex<HashMap<String, stream::Batch>>,
}
pub struct FeishuHandle {
    owner: Arc<FeishuChannel>,
}
impl FeishuHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.owner.stop_stream();
        let stream = self.owner.stream_task.lock().unwrap().take();
        if let Some(stream) = stream {
            let _ = stream.await;
        }
        self.owner.cancel_all();
        let mut work = std::mem::take(&mut *self.owner.work.lock().unwrap());
        work.abort_all();
        while work.join_next().await.is_some() {}
        self.owner.chats.lock().unwrap().clear();
    }
}
impl Drop for FeishuHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.owner.stop_stream();
        self.owner.cancel_all();
        self.owner.work.lock().unwrap().abort_all();
    }
}
impl FeishuChannel {
    pub fn new(options: FeishuOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_state(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_state(
        options: FeishuOptions,
        gateway: Arc<dyn ChannelGateway>,
        state: ChannelSessionState,
        changed: Option<Arc<dyn FeishuStateChange>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            options,
            api: Api::new(),
            gateway,
            mapper: Mutex::new(state),
            changed,
            chats: Default::default(),
            queues: Default::default(),
            inbound: Default::default(),
            seen: Default::default(),
            token: Default::default(),
            running: AtomicBool::new(false),
            work: StdMutex::new(JoinSet::new()),
            stream_stop: Default::default(),
            stream_task: Default::default(),
            batches: Default::default(),
        })
    }
    pub async fn start(self: &Arc<Self>) -> Result<FeishuHandle> {
        if self.options.app_id.is_empty() || self.options.app_secret.is_empty() {
            return Err(failure("Feishu app_id/app_secret required"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("Feishu channel already started"));
        }
        if self.options.connection_mode == "stream" {
            // The SDK start schedules its connection without waiting for the
            // handshake. A transport failure leaves the webhook route usable.
            let (stop, stopped) = watch::channel(false);
            *self.stream_stop.lock().unwrap() = Some(stop);
            *self.stream_task.lock().unwrap() =
                Some(tokio::spawn(stream::run(self.clone(), stopped)));
        }
        Ok(FeishuHandle {
            owner: self.clone(),
        })
    }
    fn stop_stream(&self) {
        if let Some(stop) = self.stream_stop.lock().unwrap().take() {
            stop.send_replace(true);
        }
    }
    fn chat(&self, chat: &str) -> Arc<Mutex<Chat>> {
        self.chats
            .lock()
            .unwrap()
            .entry(chat.into())
            .or_default()
            .clone()
    }
    fn current(&self, chat: &str, generation: u64) -> bool {
        self.running.load(Ordering::SeqCst)
            && self
                .queues
                .lock()
                .unwrap()
                .get(chat)
                .is_some_and(|queue| queue.generation == generation)
    }
    fn cancel_all(&self) {
        self.batches.lock().unwrap().clear();
        for queue in self.queues.lock().unwrap().values_mut() {
            if let Some(cancel) = queue.cancel.take() {
                cancel.send_replace(true);
            }
            queue.pending.clear();
        }
    }
    pub async fn snapshot(&self) -> Value {
        self.mapper.lock().await.snapshot()
    }
    fn persist(&self, state: &ChannelSessionState) {
        if let Some(changed) = &self.changed {
            changed.changed(state.snapshot());
        }
    }
    pub async fn webhook(self: &Arc<Self>, body: &str) -> (u16, Value) {
        if !self.running.load(Ordering::SeqCst) {
            return (503, json!({"error":"feishu_not_started"}));
        }
        let input = match inbound::parse(body, &self.options) {
            Parsed::Challenge(challenge) => return (200, json!({"challenge":challenge})),
            Parsed::Ignore => {
                if let Some(media) = stream::webhook_media(body, &self.options) {
                    return self.accept_media(media);
                }
                return (200, json!({"ok":true}));
            }
            Parsed::Message(input) => input,
        };
        self.accept(input)
    }
    fn accept_media(self: &Arc<Self>, input: stream::MediaInput) -> (u16, Value) {
        if !self.running.load(Ordering::SeqCst) {
            return (503, json!({"error":"feishu_not_started"}));
        }
        {
            let mut seen = self.seen.lock().unwrap();
            if !seen.ids.insert(input.event_id.clone()) {
                return (200, json!({"ok":true,"deduped":true}));
            }
            seen.order.push_back(input.event_id.clone());
            if seen.ids.len() > 2000 {
                if let Some(first) = seen.order.pop_front() {
                    seen.ids.remove(&first);
                }
            }
        }
        stream::enqueue(self, stream::BatchInput::Media(input));
        (200, json!({"ok":true}))
    }
    fn accept(self: &Arc<Self>, input: Inbound) -> (u16, Value) {
        if !self.running.load(Ordering::SeqCst) {
            return (503, json!({"error":"feishu_not_started"}));
        }
        {
            let mut seen = self.seen.lock().unwrap();
            if !seen.ids.insert(input.event_id.clone()) {
                return (200, json!({"ok":true,"deduped":true}));
            }
            seen.order.push_back(input.event_id.clone());
            if seen.ids.len() > 2000 {
                if let Some(first) = seen.order.pop_front() {
                    seen.ids.remove(&first);
                }
            }
        }
        stream::enqueue(self, stream::BatchInput::Text(input));
        (200, json!({"ok":true}))
    }
    async fn incoming(self: &Arc<Self>, input: Inbound) -> Result<()> {
        self.incoming_attachments(input, Vec::new()).await
    }
    async fn incoming_attachments(
        self: &Arc<Self>,
        input: Inbound,
        attachments: Vec<Value>,
    ) -> Result<()> {
        if (self.as_ref() as &dyn ImChannel)
            .answer(&input.chat, &input.text)
            .await?
        {
            return Ok(());
        }
        let serial = self
            .inbound
            .lock()
            .unwrap()
            .entry(input.chat.clone())
            .or_default()
            .clone();
        let _serial = serial.lock().await;
        if (self.as_ref() as &dyn ImChannel)
            .answer(&input.chat, &input.text)
            .await?
        {
            return Ok(());
        }
        let text = crate::metadata::js_trim(&input.text);
        if text.is_empty() {
            return Ok(());
        }
        let new = text == "/new" || text.starts_with("/new ");
        let mut mapper = self.mapper.lock().await;
        let previous = mapper.session("feishu", &input.chat);
        if new {
            if let Some(batch) = self.batches.lock().unwrap().get_mut(&input.chat) {
                batch.messages.clear();
            }
            mapper.reset("feishu", &input.chat);
            self.persist(&mapper);
            let active = {
                let mut queues = self.queues.lock().unwrap();
                let queue = queues.entry(input.chat.clone()).or_default();
                queue.generation += 1;
                queue.pending.clear();
                if let Some(cancel) = queue.cancel.take() {
                    cancel.send_replace(true);
                }
                (
                    queue.active_session.take().unwrap_or(previous),
                    queue.active_run_id.take(),
                )
            };
            self.chats.lock().unwrap().remove(&input.chat);
            let mut params = json!({"sessionKey":active.0});
            if let Some(run) = active.1 {
                params["runId"] = run.into();
            }
            let _ = self.gateway.request("abort_turn", params).await;
            if crate::metadata::js_trim(&text[4..]).is_empty() {
                drop(mapper);
                self.send_reply(&input.chat, "已创建新会话。").await;
                return Ok(());
            }
        }
        if text.starts_with('/') {
            let before = mapper.snapshot();
            let reply = Reply {
                owner: self.clone(),
                chat: input.chat.clone(),
            };
            let mut context = CommandContext {
                gateway: self.gateway.as_ref(),
                reply: &reply,
                state: &mut mapper,
                channel: "feishu",
                chat: &input.chat,
                home: &self.options.pilot_home,
            };
            if execute_channel_command(text, &mut context).await? {
                if before != mapper.snapshot() {
                    self.persist(&mapper);
                }
                return Ok(());
            }
        }
        let turn = Turn {
            session: mapper.session("feishu", &input.chat),
            message: if new {
                crate::metadata::js_trim(&text[4..]).into()
            } else {
                text.into()
            },
            project: mapper.project(&input.chat).map(str::to_owned),
            message_id: input.message_id,
            generation: 0,
            attachments,
        };
        drop(mapper);
        let start = {
            let mut queues = self.queues.lock().unwrap();
            let queue = queues.entry(input.chat.clone()).or_default();
            let mut turn = turn;
            turn.generation = queue.generation;
            if queue.running {
                queue.pending.push_back(turn);
                while queue.pending.len() > 20 {
                    queue.pending.pop_front();
                }
                None
            } else {
                queue.running = true;
                Some(turn)
            }
        };
        if let Some(turn) = start {
            let owner = self.clone();
            let chat = input.chat;
            let mut work = self.work.lock().unwrap();
            while work.try_join_next().is_some() {}
            work.spawn(async move {
                owner.run_queue(chat, turn).await;
            });
        }
        Ok(())
    }
    async fn run_queue(self: Arc<Self>, chat: String, mut turn: Turn) {
        loop {
            self.process_turn(&chat, &turn).await;
            let next = {
                let mut queues = self.queues.lock().unwrap();
                let queue = queues.entry(chat.clone()).or_default();
                if let Some(next) = queue.pending.pop_front() {
                    Some(next)
                } else {
                    queue.running = false;
                    queue.active_session = None;
                    queue.active_run_id = None;
                    queue.cancel = None;
                    None
                }
            };
            let Some(next) = next else {
                break;
            };
            turn = next;
        }
    }
    async fn process_turn(self: &Arc<Self>, chat: &str, turn: &Turn) {
        if !self.current(chat, turn.generation) {
            return;
        }
        let reaction = if let Some(message) = &turn.message_id {
            self.add_reaction(message).await
        } else {
            None
        };
        let (cancel, mut stopped) = watch::channel(false);
        {
            let mut queues = self.queues.lock().unwrap();
            let queue = queues.entry(chat.into()).or_default();
            if queue.generation != turn.generation {
                return;
            }
            queue.active_session = Some(turn.session.clone());
            queue.cancel = Some(cancel);
        }
        let sink = Sink {
            owner: self.clone(),
            chat: chat.into(),
            turn: turn.clone(),
            live: Mutex::new(live::Live::default()),
        };
        let mut params = json!({"sessionKey":turn.session,"channelKey":"feishu","message":turn.message,"allowPlanModeTools":false,"timeoutMs":self.options.turn_timeout_ms});
        if let Some(project) = &turn.project {
            params["projectKey"] = project.clone().into();
        }
        if !turn.attachments.is_empty() {
            params["attachments"] = turn.attachments.clone().into();
        }
        if let Some(mode) = &self.options.permission_mode {
            params["mode"] = mode.clone().into();
            params["basePermissionMode"] = mode.clone().into();
        }
        let operation = self.gateway.stream_turn(params, &sink);
        tokio::pin!(operation);
        let deadline =
            tokio::time::sleep(Duration::from_millis(if self.options.turn_timeout_ms > 0 {
                self.options.turn_timeout_ms
            } else {
                u64::MAX / 2
            }));
        tokio::pin!(deadline);
        let mut live_tick = tokio::time::interval(Duration::from_millis(100));
        let result = loop {
            tokio::select! {
                _ = stopped.changed() => break None,
                _ = &mut deadline => break Some(Err(())),
                result = &mut operation => break Some(Ok(result)),
                _ = live_tick.tick() => {
                    if self.current(chat,turn.generation) { sink.live.lock().await.tick(self,chat).await; }
                }
            }
        };
        if self.current(chat, turn.generation) {
            match result {
                Some(Err(())) => {
                    let run = self
                        .queues
                        .lock()
                        .unwrap()
                        .get(chat)
                        .and_then(|queue| queue.active_run_id.clone());
                    let mut params = json!({"sessionKey":turn.session,"reason":"system:timeout"});
                    if let Some(run) = run {
                        params["runId"] = run.into();
                    }
                    let _ = self.gateway.request("abort_turn", params).await;
                    sink.live
                        .lock()
                        .await
                        .append(self, chat, "处理超时，请重新发送或稍后重试。")
                        .await;
                }
                Some(Ok(Err(_))) => {
                    sink.live
                        .lock()
                        .await
                        .append(self, chat, "Failed to process this message. Please retry.")
                        .await
                }
                _ => {}
            }
            self.chat(chat).lock().await.finish_turn();
            sink.live.lock().await.finish(self, chat).await;
        }
        if let (Some(message), Some(reaction)) = (&turn.message_id, reaction) {
            let _ = self
                .rest(
                    "DELETE",
                    &format!(
                        "/im/v1/messages/{}/reactions/{}",
                        http::encode(message),
                        http::encode(&reaction)
                    ),
                    None,
                )
                .await;
        }
    }
    async fn token(&self) -> Result<String> {
        let mut cache = self.token.lock().await;
        if cache.expires_at.saturating_sub(300_000) > now_ms() {
            if let Some(token) = &cache.value {
                return Ok(token.clone());
            }
        }
        let (_, raw) = self
            .api
            .call(
                "POST",
                &format!(
                    "{}/auth/v3/tenant_access_token/internal",
                    self.options.api_base
                ),
                None,
                Some(json!({"app_id":self.options.app_id,"app_secret":self.options.app_secret})),
            )
            .await?;
        if raw["code"] != 0 {
            return Err(failure("Feishu tenant token rejected"));
        }
        let token = raw["tenant_access_token"]
            .as_str()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| failure("Feishu tenant token missing"))?
            .to_owned();
        cache.value = Some(token.clone());
        cache.expires_at = now_ms()
            .saturating_add((raw["expire"].as_f64().unwrap_or(7200.0) * 1000.0).max(0.0) as u64);
        Ok(token)
    }
    async fn rest(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let token = self.token().await?;
        let (ok, raw) = self
            .api
            .call(
                method,
                &format!("{}{path}", self.options.api_base),
                Some(&token),
                body,
            )
            .await?;
        if !ok || raw.get("code").is_some_and(|code| code != 0) {
            if raw["code"] == 99991663 || raw["code"] == 99991664 {
                self.token.lock().await.value = None;
            }
            return Err(failure("Feishu API rejected request"));
        }
        Ok(raw)
    }
    async fn add_reaction(&self, message: &str) -> Option<String> {
        self.rest(
            "POST",
            &format!("/im/v1/messages/{message}/reactions"),
            Some(json!({"reaction_type":{"emoji_type":"OnIt"}})),
        )
        .await
        .ok()
        .and_then(|raw| raw["data"]["reaction_id"].as_str().map(str::to_owned))
    }
    async fn send_reply(&self, chat: &str, text: &str) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        self.rest("POST","/im/v1/messages?receive_id_type=chat_id",Some(json!({"receive_id":chat,"msg_type":"text","content":json!({"text":text}).to_string()}))).await.is_ok()
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "feishu") {
            return Ok(false);
        }
        let session = delivery
            .get("originSessionKey")
            .filter(|v| !v.is_null())
            .unwrap_or(&delivery["sessionKey"]);
        let Some(chat) = session
            .as_str()
            .and_then(|session| session.strip_prefix("feishu:chat="))
            .and_then(|suffix| suffix.rsplit_once(':'))
            .filter(|(chat, suffix)| {
                !chat.is_empty()
                    && !chat.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
                    && (*suffix == "general"
                        || suffix.strip_prefix("s_").is_some_and(|id| {
                            id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
                        }))
            })
            .map(|(chat, _)| chat)
        else {
            return Ok(false);
        };
        Ok(self
            .send_reply(chat, delivery["text"].as_str().unwrap_or(""))
            .await)
    }
}
impl ImChannel for FeishuChannel {
    fn gateway(&self) -> &dyn ChannelGateway {
        self.gateway.as_ref()
    }
    fn chat(&self, chat: &str) -> Arc<Mutex<Chat>> {
        self.chat(chat)
    }
    fn send_reply<'a>(
        &'a self,
        chat: &'a str,
        text: &'a str,
    ) -> super::home_assistant::interaction::ReplyCall<'a> {
        Box::pin(self.send_reply(chat, text))
    }
}
struct Reply {
    owner: Arc<FeishuChannel>,
    chat: String,
}
impl ChannelReply for Reply {
    fn reply<'a>(&'a self, text: &'a str) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            if self.owner.send_reply(&self.chat, text).await {
                Ok(())
            } else {
                Err(failure("Feishu command reply failed"))
            }
        })
    }
}
struct Sink {
    owner: Arc<FeishuChannel>,
    chat: String,
    turn: Turn,
    live: Mutex<live::Live>,
}
impl ChannelEventSink for Sink {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            if !self.owner.current(&self.chat, self.turn.generation) {
                return Err(failure("Feishu stale turn generation"));
            }
            match event["type"].as_str() {
                Some("turn_started") => {
                    self.live.lock().await.activity("thinking");
                    if let Some(queue) = self.owner.queues.lock().unwrap().get_mut(&self.chat) {
                        queue.active_run_id = event["runId"].as_str().map(str::to_owned);
                    }
                }
                Some("permission_request") => {
                    self.live.lock().await.pause();
                    (self.owner.as_ref() as &dyn ImChannel)
                        .capture_permission(&self.chat, &self.turn.session, event)
                        .await
                }
                Some("elicitation_request") => {
                    self.live.lock().await.pause();
                    self.owner.chat(&self.chat).lock().await.elicitation =
                        Some((self.turn.session.clone(), event.clone()));
                    self.owner
                        .send_reply(&self.chat, &elicitation_prompt(&event))
                        .await;
                }
                Some("assistant_text_delta") => {
                    if let Some(text) = event["text"].as_str() {
                        self.live
                            .lock()
                            .await
                            .append(&self.owner, &self.chat, text)
                            .await;
                    }
                }
                Some("assistant_attachment") => {
                    self.live
                        .lock()
                        .await
                        .boundary(&self.owner, &self.chat)
                        .await;
                    stream::send_attachment(&self.owner, &self.chat, &event["attachment"]).await;
                }
                Some("tool_call_started") => {
                    self.live
                        .lock()
                        .await
                        .boundary(&self.owner, &self.chat)
                        .await;
                    self.live.lock().await.activity("tool");
                }
                Some("assistant_thinking_delta" | "model_request_started") => {
                    self.live.lock().await.activity("thinking")
                }
                Some("tool_call_finished") if event["ok"] != true => {
                    let name = event["toolName"]
                        .as_str()
                        .or_else(|| event["toolCallId"].as_str())
                        .unwrap_or("");
                    if name != "send_attachment" {
                        self.live
                            .lock()
                            .await
                            .append(&self.owner, &self.chat, &format!("\n⚠️ {name} failed\n"))
                            .await;
                    }
                }
                Some("error") => {
                    let text = match event["code"].as_str() {
                        Some("agent_aborted") => "处理已中止，请重新发送或稍后重试。".into(),
                        Some("turn_timeout") => "处理超时，请重新发送或稍后重试。".into(),
                        _ => format!("\n❌ {}\n", event["message"].as_str().unwrap_or("")),
                    };
                    self.live
                        .lock()
                        .await
                        .append(&self.owner, &self.chat, &text)
                        .await;
                }
                _ => {}
            }
            Ok(())
        })
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn failure(message: &str) -> RouteError {
    RouteError::new("FEISHU_CHANNEL_FAILED", message)
}
