//! Mattermost REST/WebSocket adapter (MattermostChannel.ts:19-333).
//! The application resolves configuration and environment precedence.
use super::home_assistant::{
    interaction::{Chat, ImChannel, elicitation_prompt},
    render_event,
    socket::Socket,
};
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
#[cfg(test)]
mod tests;
use http::Api;

#[derive(Clone, Debug, Default)]
pub struct MattermostOptions {
    pub token: String,
    pub server_url: String,
    pub team_id: Option<String>,
}
pub struct MattermostChannel {
    options: MattermostOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    bot_user_id: StdMutex<Option<String>>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    running: AtomicBool,
}
pub struct MattermostHandle {
    owner: Arc<MattermostChannel>,
    shutdown: watch::Sender<bool>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl MattermostHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.owner.bot_user_id.lock().unwrap().take();
        self.owner.chats.lock().unwrap().clear();
    }
}
impl Drop for MattermostHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        self.owner.bot_user_id.lock().unwrap().take();
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl MattermostChannel {
    pub fn new(mut options: MattermostOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        mut options: MattermostOptions, gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        options.server_url = options.server_url.trim_end_matches('/').to_owned();
        Arc::new(Self {

            options,
            api: Api::new(),
            gateway,
            bot_user_id: Default::default(),
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
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
    pub async fn start(self: &Arc<Self>) -> Result<MattermostHandle> {
        if self.options.token.is_empty() || self.options.server_url.is_empty() {
            return Err(failure(
                "MATTERMOST_URL and MATTERMOST_TOKEN / server_url and token required",
            ));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("Mattermost channel already started"));
        }
        let prepare = async {
            let me = self
                .api
                .rest(&self.options, "GET", "/users/me", None)
                .await?;
            *self.bot_user_id.lock().unwrap() = me["id"].as_str().map(str::to_owned);
            let (endpoint, origin) = websocket_url(&self.options.server_url, &self.options.token)?;
            let socket = Socket::connect(&endpoint, Some(&origin)).await?;
            Ok::<_, RouteError>((socket, endpoint, origin))
        }
        .await;
        let (socket, endpoint, origin) = match prepare {
            Ok(ready) => ready,
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                self.bot_user_id.lock().unwrap().take();
                return Err(error);
            }
        };
        let (shutdown, stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            owner.run(socket, endpoint, origin, stopped).await;
        });
        Ok(MattermostHandle {
            owner: self.clone(),
            shutdown,
            task: StdMutex::new(Some(task)),
        })
    }
    async fn run(
        self: Arc<Self>,
        mut socket: Socket,
        endpoint: String,
        origin: String,
        mut stopped: watch::Receiver<bool>,
    ) {
        let mut messages = JoinSet::new();
        'connections: loop {
            loop {
                let message = tokio::select! {_ = stopped.changed()=>break 'connections,message=socket.next()=>message};
                match message {
                    Ok(Some(message)) => {
                        let owner = self.clone();
                        messages.spawn(async move {
                            let _ = owner.receive(message).await;
                        });
                    }
                    _ => break,
                }
                while messages.try_join_next().is_some() {}
            }
            loop {
                tokio::select! {_ = stopped.changed()=>break 'connections,_ = tokio::time::sleep(Duration::from_secs(4))=>{}}
                let connected = tokio::select! {_ = stopped.changed()=>break 'connections,connected=Socket::connect(&endpoint,Some(&origin))=>connected};
                if let Ok(connected) = connected {
                    socket = connected;
                    break;
                }
            }
        }
        messages.abort_all();
        while messages.join_next().await.is_some() {}
        socket.close().await;
    }
    async fn receive(self: &Arc<Self>, message: Value) -> Result<()> {
        if message["event"] != "posted" {
            return Ok(());
        }
        let Some(raw) = message["data"]["post"].as_str() else {
            return Ok(());
        };
        let Ok(post) = serde_json::from_str::<Value>(raw) else {
            return Ok(());
        };
        if post["user_id"].as_str().is_some_and(|id| {
            !id.is_empty() && self.bot_user_id.lock().unwrap().as_deref() == Some(id)
        }) {
            return Ok(());
        }
        if post["props"]["from_webhook"] == "true" || post["props"]["from_bot"] == "true" {
            return Ok(());
        }
        let Some(channel) = post["channel_id"].as_str().filter(|id| !id.is_empty()) else {
            return Ok(());
        };
        let text = post
            .get("message")
            .filter(|v| !v.is_null())
            .map(js_string)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        let text = crate::metadata::js_trim(&text);
        if text.is_empty() {
            return Ok(());
        }
        let root = post["root_id"].as_str().filter(|id| !id.is_empty());
        let chat = root
            .map(|root| format!("{channel}:{root}"))
            .unwrap_or_else(|| channel.into());
        self.incoming(&chat, text).await
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
                mapper.reset("mattermost", chat);
            }
            (
                mapper.session("mattermost", chat),
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
                json!({"sessionKey":session,"channelKey":"mattermost","message":message}),
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
        let (channel, root) = chat
            .split_once(':')
            .map(|(channel, root)| (channel, Some(root)))
            .unwrap_or((chat, None));
        let mut delivered = true;
        for chunk in chunk_text(text, 16383) {
            let mut body = json!({"channel_id":channel,"message":chunk});
            if let Some(root) = root {
                body["root_id"] = root.into();
            }
            if self
                .api
                .rest(&self.options, "POST", "/posts", Some(body))
                .await
                .is_err()
            {
                delivered = false;
            }
        }
        delivered
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::{origin_channel_matches, parse_chat_id, session_field};
        if !origin_channel_matches(delivery, "mattermost") {
            return Ok(false);
        }
        let Some(session) = session_field(delivery) else {
            return Ok(false);
        };
        let Some(chat) = parse_chat_id(session, "mattermost") else {
            return Ok(false);
        };
        Ok(self
            .send_reply(chat, delivery["text"].as_str().unwrap_or(""))
            .await)
    }
}
impl ImChannel for MattermostChannel {
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
    owner: Arc<MattermostChannel>,
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
fn websocket_url(base: &str, token: &str) -> Result<(String, String)> {
    let base = if base.starts_with("http") {
        base.to_owned()
    } else {
        format!("https://{base}")
    };
    let parsed = url::Url::parse(&base).map_err(|_| failure("Invalid Mattermost URL"))?;
    let host = &parsed[url::Position::BeforeHost..url::Position::AfterPort];
    let secure = parsed.scheme() == "https";
    let origin = format!("{}://{host}", if secure { "https" } else { "http" });
    let endpoint = format!(
        "{}://{host}{}/api/v4/websocket?token={}",
        if secure { "wss" } else { "ws" },
        parsed.path().trim_end_matches('/'),
        encode_component(token)
    );
    Ok((endpoint, origin))
}
fn encode_component(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
fn js_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                if value.is_null() {
                    String::new()
                } else {
                    js_string(value)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        _ => value.to_string(),
    }
}
fn chunk_text(text: &str, max: usize) -> Vec<String> {
    let mut rest = text.encode_utf16().collect::<Vec<_>>();
    let mut out = vec![];
    while rest.len() > max {
        let split = rest[..=max]
            .iter()
            .rposition(|unit| *unit == 10)
            .filter(|index| *index >= max.div_ceil(2))
            .or_else(|| {
                rest[..=max]
                    .iter()
                    .rposition(|unit| *unit == 32)
                    .filter(|index| *index >= max.div_ceil(2))
            })
            .unwrap_or(max);
        out.push(String::from_utf16_lossy(&rest[..split]));
        rest = rest[split..].to_vec();
        while rest.first() == Some(&10) {
            rest.remove(0);
        }
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(String::from_utf16_lossy(&rest));
    }
    out
}
fn failure(message: &str) -> RouteError {
    RouteError::new("MATTERMOST_CHANNEL_FAILED", message)
}
