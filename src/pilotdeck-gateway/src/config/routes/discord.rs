//! Discord channel. TS adapters/channel/discord/DiscordChannel.ts:39-262.
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use pilotdeck_protocol::JsString;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
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
// The existing checked WS framing implementation has no platform behavior.
#[path = "home_assistant/socket.rs"]
mod socket;
#[cfg(test)]
mod tests;
use http::Api;
use interaction::Chat;

#[derive(Clone, Debug)]
pub struct DiscordOptions {
    pub token: String,
    pub base_url: String,
    /// Override Gateway discovery for isolated protocol fixtures.
    pub gateway_url: Option<String>,
}
impl Default for DiscordOptions {
    fn default() -> Self {
        Self {
            token: String::new(),
            base_url: "https://discord.com/api/v10".into(),
            gateway_url: None,
        }
    }
}
pub struct DiscordChannel {
    options: DiscordOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    bot_id: StdMutex<Option<String>>,
    running: AtomicBool,
    shutdown: StdMutex<Option<watch::Receiver<bool>>>,
    typing: StdMutex<JoinSet<()>>,
}
pub struct DiscordHandle {
    shutdown: watch::Sender<bool>,
    owner: Arc<DiscordChannel>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl DiscordHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        let mut typing = std::mem::take(&mut *self.owner.typing.lock().unwrap());
        typing.abort_all();
        while typing.join_next().await.is_some() {}
        self.owner.bot_id.lock().unwrap().take();
    }
}
impl Drop for DiscordHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        self.owner.typing.lock().unwrap().abort_all();
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        self.owner.bot_id.lock().unwrap().take();
    }
}
struct Connection {
    socket: socket::Socket,
    heartbeat: Duration,
}
#[derive(Default)]
struct Resume {
    sequence: Option<i64>,
    session: Option<String>,
    url: Option<String>,
}
impl DiscordChannel {
    pub fn new(mut options: DiscordOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        mut options: DiscordOptions, gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        options.base_url = options.base_url.trim_end_matches('/').into();
        Arc::new(Self {

            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            bot_id: Default::default(),
            running: AtomicBool::new(false),
            shutdown: Default::default(),
            typing: Default::default(),        })
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
    async fn connect(&self, resume: &Resume) -> Result<Connection> {
        let endpoint = if let Some(url) = resume.url.as_ref().or(self.options.gateway_url.as_ref())
        {
            url.clone()
        } else {
            self.api
                .json(&self.options, "GET", "/gateway/bot", None)
                .await?["url"]
                .as_str()
                .ok_or_else(|| failure("Discord Gateway discovery has no URL"))?
                .into()
        };
        let mut url =
            url::Url::parse(&endpoint).map_err(|_| failure("Invalid Discord Gateway URL"))?;
        url.query_pairs_mut()
            .append_pair("v", "10")
            .append_pair("encoding", "json");
        let mut socket = socket::Socket::connect(url.as_str(), None).await?;
        let hello = tokio::time::timeout(Duration::from_secs(20), socket.next())
            .await
            .map_err(|_| failure("Discord Gateway HELLO timed out"))??
            .ok_or_else(|| failure("Discord Gateway closed before HELLO"))?;
        let millis = hello["d"]["heartbeat_interval"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0)
            .ok_or_else(|| failure("Discord Gateway invalid heartbeat interval"))?;
        if hello["op"] != 10 {
            return Err(failure("Discord Gateway expected HELLO"));
        }
        if let (Some(session), Some(sequence)) = (&resume.session, resume.sequence) {
            socket.send(json!({"op":6,"d":{"token":self.options.token,"session_id":session,"seq":sequence}})).await?;
        } else {
            socket.send(json!({"op":2,"d":{"token":self.options.token,"intents":37377,"properties":{"os":std::env::consts::OS,"browser":"PilotDeck","device":"PilotDeck"}}})).await?;
        }
        Ok(Connection {
            socket,
            heartbeat: Duration::from_millis(pilotdeck_protocol::timer_delay_ms(millis)),
        })
    }
    fn ready(&self, frame: &Value, resume: &mut Resume) {
        if let Some(seq) = frame["s"].as_i64() {
            resume.sequence = Some(seq);
        }
        if frame["t"] == "READY" {
            *self.bot_id.lock().unwrap() = frame["d"]["user"]["id"].as_str().map(str::to_owned);
            resume.session = frame["d"]["session_id"].as_str().map(str::to_owned);
            resume.url = frame["d"]["resume_gateway_url"].as_str().map(str::to_owned);
        }
    }
    pub async fn start(self: &Arc<Self>) -> Result<DiscordHandle> {
        if self.options.token.is_empty() {
            return Err(failure("DISCORD_BOT_TOKEN not set"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("Discord channel already started"));
        }
        let mut resume = Resume::default();
        let initialized = async {
            let mut connection = self.connect(&resume).await?;
            let wait = async {
                loop {
                    let frame = connection
                        .socket
                        .next()
                        .await?
                        .ok_or_else(|| failure("Discord Gateway closed before READY"))?;
                    self.ready(&frame, &mut resume);
                    match frame["op"].as_i64() {
                        Some(0) if frame["t"] == "READY" => return Ok::<_, RouteError>(()),
                        Some(1) => {
                            connection
                                .socket
                                .send(json!({"op":1,"d":resume.sequence}))
                                .await?
                        }
                        Some(9) => return Err(failure("Discord Gateway invalid session")),
                        _ => {}
                    }
                }
            };
            tokio::time::timeout(Duration::from_secs(30), wait)
                .await
                .map_err(|_| failure("Discord Gateway READY timed out"))??;
            Ok::<_, RouteError>(connection)
        }
        .await;
        let connection = match initialized {
            Ok(connection) => connection,
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        let (shutdown, mut stopped) = watch::channel(false);
        *self.shutdown.lock().unwrap() = Some(stopped.clone());
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let mut connection = connection;
            let mut messages = JoinSet::new();
            'connections: loop {
                let mut heartbeat = tokio::time::interval_at(
                    tokio::time::Instant::now() + connection.heartbeat,
                    connection.heartbeat,
                );
                heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut acknowledged = true;
                loop {
                    let frame = tokio::select! {
                        _=stopped.changed()=>{connection.socket.close().await;break 'connections;},
                        _=heartbeat.tick()=>{if !acknowledged{break;}if connection.socket.send(json!({"op":1,"d":resume.sequence})).await.is_err(){break;}acknowledged=false;continue;},
                        frame=connection.socket.next()=>frame,
                    };
                    let Ok(Some(frame)) = frame else {
                        break;
                    };
                    owner.ready(&frame, &mut resume);
                    match frame["op"].as_i64() {
                        Some(11) => acknowledged = true,
                        Some(1) => {
                            if connection
                                .socket
                                .send(json!({"op":1,"d":resume.sequence}))
                                .await
                                .is_err()
                            {
                                break;
                            }
                            acknowledged = false;
                        }
                        Some(7) => break,
                        Some(9) => {
                            if frame["d"] != true {
                                resume = Resume::default();
                            }
                            break;
                        }
                        Some(0) if frame["t"] == "MESSAGE_CREATE" => {
                            let owner = owner.clone();
                            let raw = frame["d"].clone();
                            let mut message = Box::pin(async move {
                                let _ = owner.message(raw).await;
                            });
                            let done = std::future::poll_fn(|cx| {
                                std::task::Poll::Ready(message.as_mut().poll(cx).is_ready())
                            })
                            .await;
                            if !done {
                                messages.spawn(message);
                            }
                        }
                        _ => {}
                    }
                    while messages.try_join_next().is_some() {}
                }
                connection.socket.close().await;
                loop {
                    tokio::select! {
                        _ = stopped.changed() => break 'connections,
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                    }
                    let opened = tokio::select! {
                        _ = stopped.changed() => break 'connections,
                        result = owner.connect(&resume) => result,
                    };
                    if let Ok(opened) = opened {
                        connection = opened;
                        break;
                    }
                }
            }
            messages.abort_all();
            while messages.join_next().await.is_some() {}
        });
        Ok(DiscordHandle {
            shutdown,
            owner: self.clone(),
            task: StdMutex::new(Some(task)),
        })
    }
    async fn message(self: &Arc<Self>, raw: Value) -> Result<()> {
        let Some(author) = raw.get("author").filter(|v| v.is_object()) else {
            return Ok(());
        };
        if author["bot"] == true
            || raw["system"] == true
            || raw["type"]
                .as_u64()
                .is_some_and(|kind| !matches!(kind, 0 | 19 | 20 | 23))
        {
            return Ok(());
        }
        if self
            .bot_id
            .lock()
            .unwrap()
            .as_deref()
            .is_some_and(|id| author["id"].as_str() == Some(id))
        {
            return Ok(());
        }
        let text = raw["content"].as_str().unwrap_or("");
        let text = crate::metadata::js_trim(text);
        if text.is_empty() {
            return Ok(());
        }
        let Some(chat) = raw["channel_id"].as_str().filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        if self.answer(chat, text).await? {
            return Ok(());
        }
        let shared = self.chat(chat);
        let mut state = shared.lock().await;
        if state.is_active() {
            return Ok(());
        }
        let (session, message, new) = {
            let mut mapper = self.mapper.lock().unwrap();
            let new = text == "/new" || text.starts_with("/new ");
            if new {
                mapper.reset("discord", chat);
            }
            (
                mapper.session("discord", chat),
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
            self.send_reply(chat, "已创建新会话。").await;
            return Ok(());
        }
        let flag = Arc::new(AtomicBool::new(true));
        state.active = Some(flag.clone());
        drop(state);
        let _active = Active(flag);
        let owner = self.clone();
        let typing_chat = chat.to_owned();
        {
            let mut typing = self.typing.lock().unwrap();
            while typing.try_join_next().is_some() {}
            typing.spawn(async move {
                owner.send_typing(&typing_chat).await;
            });
        }
        let sink = Sink {
            owner: self.clone(),
            chat: chat.into(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let result = self
            .gateway
            .stream_turn(
                json!({"sessionKey":session,"channelKey":"discord","message":message}),
                &sink,
            )
            .await;
        self.chat(chat).lock().await.finish_turn();
        let text = if result.is_err() {
            "处理消息时发生错误，请重试。".into()
        } else {
            sink.text.lock().unwrap().clone()
        };
        let text = crate::metadata::js_trim(&text);
        if !text.is_empty() {
            self.send_reply(chat, text).await;
        }
        Ok(())
    }
    async fn cancellable(&self, method: &str, path: &str, body: Option<String>) -> Result<Value> {
        if !self.running.load(Ordering::SeqCst) {
            return Err(failure("Discord channel stopped"));
        }
        let Some(mut stopped) = self.shutdown.lock().unwrap().clone() else {
            return Err(failure("Discord channel stopped"));
        };
        if *stopped.borrow() {
            return Err(failure("Discord channel stopped"));
        }
        tokio::select! {_=stopped.changed()=>Err(failure("Discord channel stopped")),result=self.api.request(&self.options,method,path,body)=>result}
    }
    async fn send_reply(&self, chat: &str, text: &str) -> bool {
        let path = format!("/channels/{}", http::encode_component(chat));
        let Ok(channel) = self.cancellable("GET", &path, None).await else {
            return false;
        };
        if !channel["type"]
            .as_u64()
            .is_some_and(|kind| matches!(kind, 0 | 1 | 3 | 5 | 10 | 11 | 12))
        {
            return false;
        }
        let mut ok = true;
        for text in chunk_text(text, 2000) {
            let body =
                crate::metadata::object([("content", pilotdeck_protocol::JsValue::String(text))]);
            let body = pilotdeck_protocol::stringify_json(&body).ok().flatten();
            ok &= self
                .cancellable("POST", &format!("{path}/messages"), body)
                .await
                .is_ok();
        }
        ok
    }
    async fn send_typing(&self, chat: &str) {
        let path = format!("/channels/{}", http::encode_component(chat));
        if let Ok(channel) = self.cancellable("GET", &path, None).await
            && channel["type"]
                .as_u64()
                .is_some_and(|kind| matches!(kind, 0 | 1 | 3 | 5 | 10 | 11 | 12))
        {
            let _ = self
                .cancellable("POST", &format!("{path}/typing"), None)
                .await;
        }
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "discord") {
            return Ok(false);
        }
        let Some(session) = delivery
            .get("originSessionKey")
            .filter(|v| !v.is_null())
            .unwrap_or(&delivery["sessionKey"])
            .as_str()
        else {
            return Ok(false);
        };
        let Some((chat, suffix)) = session
            .strip_prefix("discord:chat=")
            .and_then(|s| s.rsplit_once(':'))
        else {
            return Ok(false);
        };
        if chat.is_empty()
            || chat.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            || !(suffix == "general"
                || suffix.strip_prefix("s_").is_some_and(|s| {
                    s.len() == 36 && s.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
                }))
        {
            return Ok(false);
        }
        Ok(self
            .send_reply(chat, delivery["text"].as_str().unwrap_or(""))
            .await)
    }
}
struct Active(Arc<AtomicBool>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct Sink {
    owner: Arc<DiscordChannel>,
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
                        .capture_permission(&self.chat, &self.session, event)
                        .await
                }
                Some("elicitation_request") => {
                    self.owner.chat(&self.chat).lock().await.elicitation =
                        Some((self.session.clone(), event.clone()));
                    self.owner
                        .send_reply(&self.chat, &interaction::elicitation_prompt(&event))
                        .await;
                }
                Some("assistant_text_delta") => {
                    if let Some(text) = event["text"].as_str() {
                        self.text.lock().unwrap().push_str(text);
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
    RouteError::new("DISCORD_CHANNEL_FAILED", message)
}
fn chunk_text(text: &str, max: usize) -> Vec<JsString> {
    let mut rest = text.encode_utf16().collect::<Vec<_>>();
    let mut chunks = Vec::new();
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
        chunks.push(JsString::from_utf16(rest[..split].to_vec()));
        rest = rest[split..].to_vec();
        while rest.first() == Some(&10) {
            rest.remove(0);
        }
    }
    if !rest.is_empty() || chunks.is_empty() {
        chunks.push(JsString::from_utf16(rest));
    }
    chunks
}
