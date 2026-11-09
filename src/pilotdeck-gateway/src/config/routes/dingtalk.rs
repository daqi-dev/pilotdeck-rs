//! DingTalk Stream/REST adapter (DingTalkChannel.ts:20-326). The optional TS
//! SDK is replaced by its native connection, event ACK and SYSTEM wire protocol.
use super::home_assistant::{
    interaction::{Chat, ImChannel, elicitation_prompt},
    render_event,
    socket::Socket,
};
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet, VecDeque},
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
#[cfg(test)]
mod tests;
use http::Api;
const ROBOT_TOPIC: &str = "/v1.0/im/bot/messages/get";
#[derive(Clone, Debug)]
pub struct DingTalkOptions {
    pub client_id: String,
    pub client_secret: String,
    /// Explicit SDK endpoint injection, with the real production URL by default.
    pub gateway_url: String,
    /// Replaces only the transport origin after a sessionWebhook has passed the
    /// fixed api.dingtalk.com check. Application configuration leaves this None.
    pub webhook_transport_origin: Option<String>,
}
impl Default for DingTalkOptions {
    fn default() -> Self {
        Self {
            client_id: String::new(),
            client_secret: String::new(),
            gateway_url: "https://api.dingtalk.com/v1.0/gateway/connections/open".into(),
            webhook_transport_origin: None,
        }
    }
}
#[derive(Default)]
struct State {
    seen: HashSet<String>,
    seen_order: VecDeque<String>,
    webhooks: HashMap<String, String>,
    webhook_order: VecDeque<String>,
}
pub struct DingTalkChannel {
    options: DingTalkOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    state: StdMutex<State>,
    running: AtomicBool,
}
pub struct DingTalkHandle {
    owner: Arc<DingTalkChannel>,
    shutdown: watch::Sender<bool>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl DingTalkHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        *self.owner.state.lock().unwrap() = State::default();
        self.owner.chats.lock().unwrap().clear();
    }
}
impl Drop for DingTalkHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        *self.owner.state.lock().unwrap() = State::default();
    }
}
impl DingTalkChannel {
    pub fn new(mut options: DingTalkOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        mut options: DingTalkOptions, gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        options.client_id = crate::metadata::js_trim(&options.client_id).into();
        options.client_secret = crate::metadata::js_trim(&options.client_secret).into();
        Arc::new(Self {

            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            state: Default::default(),
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
    async fn connect(&self) -> Result<Socket> {
        let response=self.api.post(&self.options.gateway_url,json!({"clientId":self.options.client_id,"clientSecret":self.options.client_secret,"ua":"","subscriptions":[{"type":"EVENT","topic":"*"}]}),true).await?;
        let endpoint = response["endpoint"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| failure("DingTalk did not return a stream endpoint"))?;
        let ticket = response["ticket"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| failure("DingTalk did not return a stream ticket"))?;
        Socket::connect(&format!("{endpoint}?ticket={ticket}"), None).await
    }
    pub async fn start(self: &Arc<Self>) -> Result<DingTalkHandle> {
        if self.options.client_id.is_empty() || self.options.client_secret.is_empty() {
            return Err(failure("DINGTALK_CLIENT_ID / CLIENT_SECRET required"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("DingTalk channel already started"));
        }
        let socket = match self.connect().await {
            Ok(socket) => socket,
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        let (shutdown, stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            owner.run(socket, stopped).await;
        });
        Ok(DingTalkHandle {
            owner: self.clone(),
            shutdown,
            task: StdMutex::new(Some(task)),
        })
    }
    async fn run(self: Arc<Self>, mut socket: Socket, mut stopped: watch::Receiver<bool>) {
        let mut messages = JoinSet::new();
        let mut attempts = 0u32;
        'connections: loop {
            loop {
                let message = tokio::select! {_=stopped.changed()=>break 'connections,message=socket.next()=>message};
                let message = match message {
                    Ok(Some(message)) => message,
                    _ => break,
                };
                let mut disconnect = false;
                let ack = match message["type"].as_str() {
                    Some("EVENT") => {
                        let owner = self.clone();
                        let input = message.clone();
                        messages.spawn(async move {
                            let _ = owner.receive(input).await;
                        });
                        Some(
                            json!({"code":200,"headers":{"contentType":"application/json","messageId":message["headers"]["messageId"]},"message":"OK","data":"{\"status\":\"SUCCESS\"}"}),
                        )
                    }
                    Some("SYSTEM")
                        if message["headers"]["topic"] == "ping"
                            || message["headers"]["topic"] == "disconnect" =>
                    {
                        disconnect = message["headers"]["topic"] == "disconnect";
                        Some(
                            json!({"code":200,"headers":message["headers"],"message":"OK","data":message["data"]}),
                        )
                    }
                    _ => None,
                };
                if let Some(ack) = ack {
                    let sent = tokio::select! {_=stopped.changed()=>break 'connections,sent=socket.send(ack)=>sent};
                    if sent.is_err() {
                        break;
                    }
                }
                if disconnect {
                    socket.close().await;
                    break;
                }
                while messages.try_join_next().is_some() {}
            }
            loop {
                let jitter = f64::from(uuid::Uuid::new_v4().as_bytes()[0]) / 255.0;
                let delay = (1000u64.saturating_mul(1u64 << attempts.min(6))).min(60_000)
                    + (jitter * 1000.0) as u64;
                tokio::select! {_=stopped.changed()=>break 'connections,_=tokio::time::sleep(Duration::from_millis(delay.min(60_000)))=>{}}
                let connected = tokio::select! {_=stopped.changed()=>break 'connections,connected=self.connect()=>connected};
                match connected {
                    Ok(connected) => {
                        socket = connected;
                        attempts = 0;
                        break;
                    }
                    Err(_) => attempts = attempts.saturating_add(1),
                }
            }
        }
        messages.abort_all();
        while messages.join_next().await.is_some() {}
        socket.close().await;
    }
    async fn receive(self: &Arc<Self>, message: Value) -> Result<()> {
        let topic = message["headers"]["topic"].as_str().unwrap_or("");
        if !topic.is_empty() && topic != ROBOT_TOPIC {
            return Ok(());
        }
        let Some(raw) = message["data"].as_str() else {
            return Ok(());
        };
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            return Ok(());
        };
        let id = data
            .get("msgId")
            .filter(|v| !v.is_null())
            .or_else(|| data.get("messageId").filter(|v| !v.is_null()))
            .or_else(|| message["headers"].get("messageId").filter(|v| !v.is_null()))
            .map(js_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        {
            let mut state = self.state.lock().unwrap();
            if !state.seen.insert(id.clone()) {
                return Ok(());
            }
            state.seen_order.push_back(id);
            if state.seen.len() > 2000 {
                if let Some(first) = state.seen_order.pop_front() {
                    state.seen.remove(&first);
                }
            }
        }
        let text = extract_text(&data);
        if crate::metadata::js_trim(&text).is_empty() {
            return Ok(());
        }
        let conversation = data
            .get("conversationId")
            .filter(|v| !v.is_null())
            .map(js_string)
            .unwrap_or_default();
        let sender = data
            .get("senderId")
            .filter(|v| !v.is_null())
            .map(js_string)
            .unwrap_or_default();
        let chat = if conversation.is_empty() {
            sender
        } else {
            conversation
        };
        if chat.is_empty() {
            return Ok(());
        }
        let webhook = data
            .get("sessionWebhook")
            .filter(|v| !v.is_null())
            .map(js_string)
            .unwrap_or_default();
        if valid_webhook(&webhook) {
            self.remember_webhook(&chat, &webhook);
        }
        self.incoming(&chat, &text).await
    }
    fn remember_webhook(&self, chat: &str, url: &str) {
        let mut state = self.state.lock().unwrap();
        if state.webhooks.len() >= 500 {
            if let Some(first) = state.webhook_order.pop_front() {
                state.webhooks.remove(&first);
            }
        }
        if !state.webhooks.contains_key(chat) {
            state.webhook_order.push_back(chat.into());
        }
        state.webhooks.insert(chat.into(), url.into());
    }
    async fn incoming(self: &Arc<Self>, chat: &str, text: &str) -> Result<()> {
        if (self.as_ref() as &dyn ImChannel).answer(chat, text).await? {
            return Ok(());
        }
        let shared = self.chat(chat);
        let mut state = shared.lock().await;
        if state.is_active() {
            return Ok(());
        }
        let text = crate::metadata::js_trim(text);
        let new = text == "/new" || text.starts_with("/new ");
        let (session, message) = {
            let mut mapper = self.mapper.lock().unwrap();
            if new {
                mapper.reset("dingtalk", chat);
            }
            (
                mapper.session("dingtalk", chat),
                if new {
                    crate::metadata::js_trim(&text[4..])
                } else {
                    text
                },
            )
        };
        if new {
            self.persist_mapper();
        }
        if new && message.is_empty() {
            drop(state);
            self.send_reply(chat, "已创建新会话。").await;
            return Ok(());
        }
        if message.is_empty() {
            return Ok(());
        }
        let flag = Arc::new(AtomicBool::new(true));
        state.active = Some(flag.clone());
        drop(state);
        let _active = Active(flag);
        let sink = Sink {
            owner: self.clone(),
            chat: chat.into(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let result = self
            .gateway
            .stream_turn(
                json!({"sessionKey":session,"channelKey":"dingtalk","message":message}),
                &sink,
            )
            .await;
        self.chat(chat).lock().await.finish_turn();
        let text = if result.is_err() {
            "处理消息时发生错误，请重试。".into()
        } else {
            sink.text.lock().unwrap().clone()
        };
        if !crate::metadata::js_trim(&text).is_empty() {
            self.send_reply(chat, crate::metadata::js_trim(&text)).await;
        }
        Ok(())
    }
    async fn send_reply(&self, chat: &str, text: &str) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        let Some(webhook) = self
            .state
            .lock()
            .unwrap()
            .webhooks
            .get(chat)
            .cloned()
            .filter(|url| valid_webhook(url))
        else {
            return false;
        };
        let endpoint = if let Some(origin) = &self.options.webhook_transport_origin {
            format!(
                "{}{}",
                origin.trim_end_matches('/'),
                &webhook["https://api.dingtalk.com".len()..]
            )
        } else {
            webhook
        };
        let text = String::from_utf16_lossy(&text.encode_utf16().take(20_000).collect::<Vec<_>>());
        self.api
            .post(
                &endpoint,
                json!({"msgtype":"markdown","markdown":{"title":"Reply","text":text}}),
                false,
            )
            .await
            .is_ok()
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "dingtalk") {
            return Ok(false);
        }
        let session = delivery
            .get("originSessionKey")
            .filter(|v| !v.is_null())
            .unwrap_or(&delivery["sessionKey"]);
        let Some(chat) = session
            .as_str()
            .and_then(|session| session.strip_prefix("dingtalk:chat="))
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
impl ImChannel for DingTalkChannel {
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
struct Active(Arc<AtomicBool>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct Sink {
    owner: Arc<DingTalkChannel>,
    chat: String,
    session: String,
    text: StdMutex<String>,
}
impl ChannelEventSink for Sink {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            match event["type"].as_str() {
                Some("permission_request") => {
                    (self.owner.as_ref() as &dyn ImChannel)
                        .capture_permission(&self.chat, &self.session, event)
                        .await
                }
                Some("elicitation_request") => {
                    self.owner.chat(&self.chat).lock().await.elicitation =
                        Some((self.session.clone(), event.clone()));
                    self.owner
                        .send_reply(&self.chat, &elicitation_prompt(&event))
                        .await;
                }
                Some("tool_call_finished") if event["ok"] != true => {
                    self.text.lock().unwrap().push_str(&format!(
                        "\n⚠️ {} 执行失败\n",
                        event["toolName"]
                            .as_str()
                            .or_else(|| event["toolCallId"].as_str())
                            .unwrap_or("")
                    ))
                }
                _ => {
                    if let Some(fragment) = render_event(&event) {
                        self.text.lock().unwrap().push_str(&fragment);
                    }
                }
            }
            Ok(())
        })
    }
}
fn extract_text(data: &Value) -> String {
    let text = &data["text"];
    let raw = if let Some(content) = text.get("content") {
        if content.is_null() {
            String::new()
        } else {
            js_string(content)
        }
    } else if let Some(text) = text.as_str() {
        text.into()
    } else {
        data.get("richText")
            .filter(|v| !v.is_null())
            .or_else(|| data.get("rich_text"))
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter(|value| value.is_object() || value.is_array())
                    .filter_map(|value| value.get("text").filter(|v| !v.is_null()).map(js_string))
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default()
    };
    crate::metadata::js_trim(&raw).into()
}
fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(values) => values.iter().map(js_string).collect::<Vec<_>>().join(","),
        _ => value.to_string(),
    }
}
fn valid_webhook(url: &str) -> bool {
    url.starts_with("https://api.dingtalk.com/")
}
fn failure(message: &str) -> RouteError {
    RouteError::new("DINGTALK_CHANNEL_FAILED", message)
}
