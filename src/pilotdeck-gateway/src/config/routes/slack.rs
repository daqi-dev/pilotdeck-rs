//! Slack Socket Mode (`SlackChannel.ts:44-258`). HTTP and WebSocket transport
//! are native; the owner never reads credential files or environment variables.
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Mutex, watch},
    task::{JoinHandle, JoinSet},
};
mod http;
mod interaction;
mod socket;
#[cfg(test)]
mod tests;
use http::Api;
use interaction::Chat;

#[derive(Clone, Debug)]
pub struct SlackOptions {
    /// Application resolves options.botToken ?? SLACK_BOT_TOKEN.
    pub bot_token: String,
    /// Application resolves options.appToken ?? SLACK_APP_TOKEN.
    pub app_token: String,
    pub base_url: String,
}
impl Default for SlackOptions {
    fn default() -> Self {
        Self {
            bot_token: String::new(),
            app_token: String::new(),
            base_url: "https://slack.com/api/".into(),
        }
    }
}
pub struct SlackChannel {
    options: SlackOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    bot_user: StdMutex<Option<String>>,
    running: AtomicBool,
}
pub struct SlackHandle {
    shutdown: watch::Sender<bool>,
    owner: Arc<SlackChannel>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl SlackHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        *self.owner.bot_user.lock().unwrap() = None;
    }
}
impl Drop for SlackHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        *self.owner.bot_user.lock().unwrap() = None;
    }
}
#[derive(Clone)]
struct SendContext {
    channel: String,
    thread: Option<String>,
}
impl SendContext {
    fn interaction_chat(&self) -> String {
        self.thread
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|thread| format!("{}:{thread}", self.channel))
            .unwrap_or_else(|| self.channel.clone())
    }
}
impl SlackChannel {
    pub fn new(options: SlackOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        options: SlackOptions, gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        Arc::new(Self {

            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            bot_user: Default::default(),
            running: AtomicBool::new(false),        })
    }
    fn persist_mapper(&self) {
        ChannelSessionState::persist_channel_mapper(
            &self.mapper.lock().unwrap(),
            &self.on_state_change,
        );
    }
    fn chat(&self, id: &str) -> Arc<Mutex<Chat>> {
        self.chats
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .clone()
    }
    async fn call(&self, method: &str, params: Value, app: bool) -> Result<Value> {
        self.api.call(&self.options, method, params, app).await
    }
    async fn connect(&self) -> Result<socket::Socket> {
        let response = self.call("apps.connections.open", json!({}), true).await?;
        let url = response["url"]
            .as_str()
            .ok_or_else(|| failure("Slack returned no Socket Mode URL"))?;
        socket::Socket::connect(url).await
    }
    pub async fn start(self: &Arc<Self>) -> Result<SlackHandle> {
        if self.options.bot_token.is_empty() {
            return Err(failure("SLACK_BOT_TOKEN not set"));
        }
        if self.options.app_token.is_empty() {
            return Err(failure(
                "SLACK_APP_TOKEN not set (required for Socket Mode)",
            ));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("Slack channel already started"));
        }
        let start = async {
            let socket = self.connect().await?;
            let auth = self
                .call("auth.test", json!({"token":self.options.bot_token}), false)
                .await?;
            *self.bot_user.lock().unwrap() = auth["user_id"].as_str().map(str::to_owned);
            Ok::<_, RouteError>(socket)
        }
        .await;
        let socket = match start {
            Ok(socket) => socket,
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        let (shutdown, mut stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let mut socket = Some(socket);
            let mut messages = JoinSet::new();
            loop {
                let mut connection = match socket.take() {
                    Some(socket) => socket,
                    None => {
                        tokio::select! {_ = stopped.changed()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
                        match tokio::select! {_ = stopped.changed()=>break,result=owner.connect()=>result}
                        {
                            Ok(socket) => socket,
                            Err(_) => continue,
                        }
                    }
                };
                loop {
                    let event = tokio::select! {_ = stopped.changed()=>{connection.close().await;break},event=connection.next()=>event};
                    let Ok(Some(event)) = event else { break };
                    if let Some(id) = event["envelope_id"].as_str() {
                        let ack = connection.send(json!({"envelope_id":id}));
                        if tokio::select! {_ = stopped.changed()=>false,result=ack=>result.is_ok()}
                            == false
                        {
                            break;
                        }
                    }
                    if event["type"] == "disconnect" {
                        break;
                    }
                    if event["type"] == "events_api"
                        && event["payload"]["event"]["type"] == "message"
                    {
                        let owner = owner.clone();
                        let event = event["payload"]["event"].clone();
                        messages.spawn(async move {
                            let _ = owner.message(event).await;
                        });
                        tokio::task::yield_now().await;
                    }
                    while messages.try_join_next().is_some() {}
                }
                if *stopped.borrow() {
                    break;
                }
            }
            messages.abort_all();
            while messages.join_next().await.is_some() {}
        });
        Ok(SlackHandle {
            shutdown,
            owner: self.clone(),
            task: StdMutex::new(Some(task)),
        })
    }
    async fn message(self: &Arc<Self>, event: Value) -> Result<()> {
        if event.is_null()
            || truthy(&event["bot_id"])
            || matches!(
                event["subtype"].as_str(),
                Some("bot_message" | "message_changed" | "message_deleted")
            )
        {
            return Ok(());
        }
        if let Some(user) = event["user"].as_str().filter(|s| !s.is_empty()) {
            if self.bot_user.lock().unwrap().as_deref() == Some(user) {
                return Ok(());
            }
        }
        let Some(channel) = event["channel"].as_str().filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let text = event["text"].as_str().unwrap_or("");
        let text = regex::Regex::new(r"<@[^>]+>")
            .unwrap()
            .replace_all(text, "");
        let text = crate::metadata::js_trim(&text);
        if text.is_empty() {
            return Ok(());
        }
        let thread = event["thread_ts"].as_str().map(str::to_owned);
        let chat = thread
            .as_deref()
            .filter(|thread| !thread.is_empty() && Some(*thread) != event["ts"].as_str())
            .map(|thread| format!("{channel}:{thread}"))
            .unwrap_or_else(|| channel.into());
        let context = SendContext {
            channel: channel.into(),
            thread,
        };
        if self.answer(&chat, &context, text).await? {
            return Ok(());
        }
        let shared = self.chat(&chat);
        let mut state = shared.lock().await;
        if state.is_active() {
            return Ok(());
        }
        let (session, message, new) = {
            let mut mapper = self.mapper.lock().unwrap();
            let new = text == "/new" || text.starts_with("/new ");
            if new {
                mapper.reset("slack", &chat);
            }
            (
                mapper.session("slack", &chat),
                if new {
                    crate::metadata::js_trim(&text[4..]).to_owned()
                } else {
                    text.into()
                },
                new,
            )
        };
        if new {
            self.persist_mapper();
        }
        if new && message.is_empty() {
            drop(state);
            self.send_reply(&context, "已创建新会话。").await;
            return Ok(());
        }
        if message.is_empty() {
            return Ok(());
        }
        let flag = Arc::new(AtomicBool::new(true));
        state.active = Some(flag.clone());
        drop(state);
        let _active = Active(flag);
        let interaction_chat = context.interaction_chat();
        let sink = Sink {
            owner: self.clone(),
            context: context.clone(),
            chat: interaction_chat.clone(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let result = self
            .gateway
            .stream_turn(
                json!({"sessionKey":session,"channelKey":"slack","message":message}),
                &sink,
            )
            .await;
        self.chat(&interaction_chat).lock().await.finish_turn();
        let text = if result.is_err() {
            "处理消息时发生错误，请重试。".into()
        } else {
            sink.text.lock().unwrap().clone()
        };
        let text = crate::metadata::js_trim(&text);
        if !text.is_empty() {
            self.send_reply(&context, text).await;
        }
        Ok(())
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::{origin_channel_matches, parse_chat_id, session_field};
        if !origin_channel_matches(delivery, "slack") {
            return Ok(false);
        }
        let Some(session) = session_field(delivery) else {
            return Ok(false);
        };
        let Some(chat_key) = parse_chat_id(session, "slack") else {
            return Ok(false);
        };
        let (channel, thread) = chat_key
            .split_once(':')
            .map(|(channel, thread)| (channel.to_owned(), Some(thread.to_owned())))
            .unwrap_or_else(|| (chat_key.to_owned(), None));
        let context = SendContext { channel, thread };
        Ok(self
            .send_reply(
                &context,
                delivery["text"].as_str().unwrap_or(""),
            )
            .await)
    }
    async fn send_reply(&self, context: &SendContext, text: &str) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        let mut success = true;
        for text in chunk_text(&format_mrkdwn(text), 39000) {
            if self
                .api
                .send_message(
                    &self.options,
                    &context.channel,
                    context.thread.as_deref(),
                    text,
                )
                .await
                .is_err()
            {
                success = false;
            }
        }
        success
    }
}
struct Active(Arc<AtomicBool>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct Sink {
    owner: Arc<SlackChannel>,
    context: SendContext,
    chat: String,
    session: String,
    text: StdMutex<String>,
}
impl ChannelEventSink for Sink {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            match event["type"].as_str() {
                Some("permission_request") => {
                    self.owner
                        .capture_permission(&self.chat, &self.context, &self.session, event)
                        .await
                }
                Some("elicitation_request") => {
                    self.owner.chat(&self.chat).lock().await.elicitation =
                        Some((self.session.clone(), event.clone()));
                    self.owner
                        .send_reply(&self.context, &interaction::elicitation_prompt(&event))
                        .await;
                }
                Some("assistant_text_delta") => {
                    if let Some(text) = event["text"].as_str() {
                        self.text.lock().unwrap().push_str(text)
                    }
                }
                Some("tool_call_finished") if event["ok"] != true => {
                    self.text.lock().unwrap().push_str(&format!(
                        "\n⚠️ {} failed\n",
                        event["toolName"]
                            .as_str()
                            .or_else(|| event["toolCallId"].as_str())
                            .unwrap_or("")
                    ))
                }
                Some("error") => self.text.lock().unwrap().push_str(&format!(
                    "\n❌ {}\n",
                    event["message"].as_str().unwrap_or("")
                )),
                _ => {}
            }
            Ok(())
        })
    }
}
fn failure(message: &str) -> RouteError {
    RouteError::new("SLACK_CHANNEL_FAILED", message)
}
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(v) => *v,
        Value::Number(v) => v.as_f64() != Some(0.0),
        Value::String(v) => !v.is_empty(),
        _ => true,
    }
}
fn format_mrkdwn(text: &str) -> String {
    let text = regex::Regex::new(r"\*\*(.+?)\*\*")
        .unwrap()
        .replace_all(text, "*$1*");
    regex::Regex::new(r"\[([^\]]+)\]\(([^)]+)\)")
        .unwrap()
        .replace_all(&text, "<$2|$1>")
        .into_owned()
}
fn chunk_text(text: &str, max: usize) -> Vec<pilotdeck_protocol::JsString> {
    let mut rest = text.encode_utf16().collect::<Vec<_>>();
    let mut result = Vec::new();
    while rest.len() > max {
        let split = rest[..=max]
            .iter()
            .rposition(|c| *c == 10)
            .filter(|i| *i >= max / 2)
            .or_else(|| {
                rest[..=max]
                    .iter()
                    .rposition(|c| *c == 32)
                    .filter(|i| *i >= max / 2)
            })
            .unwrap_or(max);
        result.push(pilotdeck_protocol::JsString::from_utf16(
            rest[..split].to_vec(),
        ));
        rest = rest[split..].to_vec();
        while rest.first() == Some(&10) {
            rest.remove(0);
        }
    }
    if !rest.is_empty() || result.is_empty() {
        result.push(pilotdeck_protocol::JsString::from_utf16(rest));
    }
    result
}
