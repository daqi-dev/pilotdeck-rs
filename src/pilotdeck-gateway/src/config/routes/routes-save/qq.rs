//! QQ official bot owner. TS QQChannel.ts and qqbot-gateway.ts at cc7c4e6.
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway};
use pilotdeck_protocol::JsString;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, watch},
    task::{JoinHandle, JoinSet},
};
mod http;
mod interaction;
#[path = "home_assistant/socket.rs"]
mod socket;
#[cfg(test)]
mod tests;
use http::Api;
use interaction::Chat;

#[derive(Clone)]
pub struct QQOptions {
    pub app_id: String,
    pub client_secret: String,
    pub allow_groups: Vec<String>,
    pub trigger_prefixes: Vec<String>,
    pub max_message_length: usize,
    pub initial_state: Value,
    pub on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    pub api_base: String,
    pub token_url: String,
}
impl Default for QQOptions {
    fn default() -> Self {
        Self {
            app_id: String::new(),
            client_secret: String::new(),
            allow_groups: vec!["*".into()],
            trigger_prefixes: vec!["/ask".into(), "/chat".into()],
            max_message_length: 2000,
            initial_state: json!({"activeByChatKey":{}}),
            on_state_change: None,
            api_base: "https://api.sgroup.qq.com".into(),
            token_url: "https://bots.qq.com/app/getAppAccessToken".into(),
        }
    }
}
#[derive(Clone)]
struct Target {
    direct: bool,
    id: String,
    msg_id: Option<String>,
}
struct Token {
    value: String,
    expires_at: f64,
    refresh_at: Option<tokio::time::Instant>,
}
pub struct QQChannel {
    options: QQOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<HashMap<String, String>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    targets: StdMutex<HashMap<String, Target>>,
    token: Mutex<Option<Token>>,
    running: AtomicBool,
    shutdown: StdMutex<Option<watch::Receiver<bool>>>,
}
pub struct QQHandle {
    shutdown: watch::Sender<bool>,
    owner: Arc<QQChannel>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl QQHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}
impl Drop for QQHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
#[derive(Default)]
struct Resume {
    sequence: Option<i64>,
    session: Option<String>,
}
impl QQChannel {
    pub fn new(mut options: QQOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        options.api_base = options.api_base.trim_end_matches('/').into();
        let mapper = options.initial_state["activeByChatKey"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect();
        Arc::new(Self {
            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            chats: Default::default(),
            targets: Default::default(),
            token: Default::default(),
            running: AtomicBool::new(false),
            shutdown: Default::default(),
        })
    }
    pub fn snapshot(&self) -> Value {
        json!({"activeByChatKey":*self.mapper.lock().unwrap()})
    }
    fn chat(&self, id: &str) -> Arc<Mutex<Chat>> {
        self.chats
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .clone()
    }
    async fn access_token(&self) -> Result<String> {
        let mut stored = self.token.lock().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0;
        if let Some(token) = stored.as_ref()
            && now < token.expires_at - 120_000.0
        {
            return Ok(token.value.clone());
        }
        let value = self
            .api
            .request(
                "POST",
                &self.options.token_url,
                None,
                Some(
                    json!({"appId":self.options.app_id,"clientSecret":self.options.client_secret})
                        .to_string(),
                ),
            )
            .await?;
        let token = value["access_token"]
            .as_str()
            .ok_or_else(|| failure("QQ token response has no access_token"))?
            .to_owned();
        let expires = pilotdeck_protocol::parse_json(&value["expires_in"].to_string())
            .ok()
            .and_then(|v| pilotdeck_protocol::js_value::to_number(&v).ok())
            .unwrap_or(f64::NAN);
        let refresh = pilotdeck_protocol::timer_delay_ms((expires - 300.0) * 1000.0);
        *stored = Some(Token {
            value: token.clone(),
            expires_at: now + expires * 1000.0,
            refresh_at: Some(tokio::time::Instant::now() + Duration::from_millis(refresh)),
        });
        Ok(token)
    }
    async fn connect(&self) -> Result<socket::Socket> {
        let token = self.access_token().await?;
        let value = self
            .api
            .request(
                "GET",
                &format!("{}/gateway", self.options.api_base),
                Some(&format!("QQBot {token}")),
                None,
            )
            .await?;
        let endpoint = value["url"]
            .as_str()
            .ok_or_else(|| failure("QQ Gateway response has no url"))?;
        socket::Socket::connect(endpoint, None).await
    }
    async fn identify(&self, socket: &mut socket::Socket, resume: &Resume) -> Result<()> {
        let token = self.access_token().await?;
        if let Some(session) = &resume.session {
            socket.send(json!({"op":6,"d":{"token":format!("QQBot {token}"),"session_id":session,"seq":resume.sequence}})).await
        } else {
            socket.send(json!({"op":2,"d":{"token":format!("QQBot {token}"),"intents":33554432,"shard":[0,1],"properties":{"$os":"linux","$browser":"pilotdeck","$device":"pilotdeck"}}})).await
        }
    }
    pub async fn start(self: &Arc<Self>) -> Result<QQHandle> {
        if self.options.app_id.is_empty() || self.options.client_secret.is_empty() {
            return Err(failure("QQ_BOT_APPID or QQ_BOT_SECRET not set"));
        }
        if self.options.max_message_length == 0 {
            return Err(failure("QQ maxMessageLength must be positive"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("QQ channel already started"));
        }
        let socket = match self.connect().await {
            Ok(socket) => socket,
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        let (shutdown, mut stopped) = watch::channel(false);
        *self.shutdown.lock().unwrap() = Some(stopped.clone());
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let mut socket = socket;
            let mut resume = Resume::default();
            let mut messages = JoinSet::new();
            'connections: loop {
                let mut heartbeat_ms = 41250;
                let mut heartbeat: Option<tokio::time::Interval> = None;
                let mut reidentify: Option<tokio::time::Instant> = None;
                loop {
                    let refresh_at = owner.token.lock().await.as_ref().and_then(|t| t.refresh_at);
                    let frame = tokio::select! {
                        _=stopped.changed()=>{socket.close().await;break 'connections;},
                        _=async{if let Some(interval)=heartbeat.as_mut(){interval.tick().await;}else{std::future::pending::<()>().await;}}=>{if socket.send(json!({"op":1,"d":resume.sequence})).await.is_err(){break;}continue;},
                        _=async{if let Some(at)=reidentify{tokio::time::sleep_until(at).await;}else{std::future::pending::<()>().await;}}=>{reidentify=None;if owner.identify(&mut socket,&resume).await.is_err(){break;}continue;},
                        _=async{if let Some(at)=refresh_at{tokio::time::sleep_until(at).await;}else{std::future::pending::<()>().await;}}=>{if let Some(token)=owner.token.lock().await.as_mut(){token.refresh_at=None;}let _=owner.access_token().await;continue;},
                        frame=socket.next()=>frame,
                    };
                    let Ok(Some(frame)) = frame else {
                        break;
                    };
                    if let Some(seq) = frame["s"].as_i64() {
                        resume.sequence = Some(seq);
                    }
                    match frame["op"].as_i64() {
                        Some(10) => {
                            heartbeat_ms = pilotdeck_protocol::timer_delay_ms(
                                frame["d"]["heartbeat_interval"].as_f64().unwrap_or(41250.0),
                            );
                            if owner.identify(&mut socket, &resume).await.is_err() {
                                break;
                            }
                        }
                        Some(7) => break,
                        Some(9) => {
                            resume = Resume::default();
                            reidentify = Some(tokio::time::Instant::now() + Duration::from_secs(2));
                        }
                        Some(0) => match frame["t"].as_str() {
                            Some("READY" | "RESUMED") => {
                                if frame["t"] == "READY" {
                                    resume.session =
                                        frame["d"]["session_id"].as_str().map(str::to_owned);
                                }
                                heartbeat = Some(tokio::time::interval_at(
                                    tokio::time::Instant::now()
                                        + Duration::from_millis(heartbeat_ms),
                                    Duration::from_millis(heartbeat_ms),
                                ));
                            }
                            Some(kind @ ("GROUP_AT_MESSAGE_CREATE" | "C2C_MESSAGE_CREATE")) => {
                                let direct = kind == "C2C_MESSAGE_CREATE";
                                let owner = owner.clone();
                                let event = frame["d"].clone();
                                let mut message = Box::pin(async move {
                                    let _ = owner.message(direct, event).await;
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
                        },
                        _ => {}
                    }
                    while messages.try_join_next().is_some() {}
                }
                socket.close().await;
                loop {
                    tokio::select! {_=stopped.changed()=>break 'connections,_=tokio::time::sleep(Duration::from_secs(5))=>{}}
                    let opened = tokio::select! {_=stopped.changed()=>break 'connections,result=owner.connect()=>result};
                    if let Ok(opened) = opened {
                        socket = opened;
                        break;
                    }
                }
            }
            messages.abort_all();
            while messages.join_next().await.is_some() {}
        });
        Ok(QQHandle {
            shutdown,
            owner: self.clone(),
            task: StdMutex::new(Some(task)),
        })
    }
    async fn message(self: &Arc<Self>, direct: bool, event: Value) -> Result<()> {
        let user = event["author"][if direct {
            "user_openid"
        } else {
            "member_openid"
        }]
        .as_str()
        .unwrap_or("");
        if user.is_empty() {
            return Ok(());
        }
        let group = event["group_openid"].as_str().unwrap_or("");
        if !direct
            && (group.is_empty()
                || !self
                    .options
                    .allow_groups
                    .iter()
                    .any(|v| v == "*" || v == group))
        {
            return Ok(());
        }
        let raw = crate::metadata::js_trim(event["content"].as_str().unwrap_or(""));
        let text = if direct {
            raw
        } else {
            self.options
                .trigger_prefixes
                .iter()
                .find_map(|prefix| raw.strip_prefix(prefix).map(crate::metadata::js_trim))
                .unwrap_or(raw)
        };
        if text.is_empty() {
            return Ok(());
        }
        let chat = if direct {
            format!("c2c:{user}")
        } else {
            format!("{group}:{user}")
        };
        let target = Target {
            direct,
            id: if direct { user } else { group }.into(),
            msg_id: event["id"]
                .as_str()
                .filter(|v| !v.is_empty())
                .map(str::to_owned),
        };
        // Answers use the original target, but never its passive reply msg_id.
        if self.answer(&chat, text).await? {
            return Ok(());
        }
        let shared = self.chat(&chat);
        let mut state = shared.lock().await;
        if state.is_active() {
            return Ok(());
        }
        self.targets.lock().unwrap().insert(chat.clone(), target);
        let (session, message, new) = if direct {
            (format!("qq:c2c:{user}:general"), text.to_owned(), false)
        } else {
            let mut mapper = self.mapper.lock().unwrap();
            let new = text == "/new" || text.starts_with("/new ");
            if new {
                mapper.insert(
                    chat.clone(),
                    format!("qq:group={group}:user={user}:s_{}", uuid::Uuid::new_v4()),
                );
            }
            (
                mapper
                    .get(&chat)
                    .cloned()
                    .unwrap_or_else(|| format!("qq:group={group}:user={user}:general")),
                if new {
                    crate::metadata::js_trim(&text[4..]).into()
                } else {
                    text.into()
                },
                new,
            )
        };
        if new && let Some(changed) = &self.options.on_state_change {
            changed(self.snapshot());
        }
        if new && message.is_empty() {
            drop(state);
            let target = self.targets.lock().unwrap().get(&chat).cloned();
            if let Some(target) = target {
                self.send(
                    &target,
                    "已创建新会话。".into(),
                    target.msg_id.as_deref(),
                    1,
                )
                .await;
            }
            return Ok(());
        }
        let flag = Arc::new(AtomicBool::new(true));
        state.active = Some(flag.clone());
        drop(state);
        let _active = Active(flag);
        let sink = Sink {
            owner: self.clone(),
            chat: chat.clone(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let result = self
            .gateway
            .stream_turn(
                json!({"sessionKey":session,"channelKey":"qq","message":message}),
                &sink,
            )
            .await;
        self.chat(&chat).lock().await.finish_turn();
        let text = if result.is_err() {
            "处理消息时发生错误，请重试。".into()
        } else {
            sink.text.lock().unwrap().clone()
        };
        let text = crate::metadata::js_trim(&text);
        if !text.is_empty() {
            self.send_initial_reply(&chat, text).await;
        }
        Ok(())
    }
    async fn send(
        &self,
        target: &Target,
        text: JsString,
        msg_id: Option<&str>,
        seq: usize,
    ) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        let Some(mut stopped) = self.shutdown.lock().unwrap().clone() else {
            return false;
        };
        if *stopped.borrow() {
            return false;
        }
        let send = async {
            let token = self.access_token().await?;
            let mut body = pilotdeck_protocol::JsObject::new();
            body.insert("content".into(), pilotdeck_protocol::JsValue::String(text));
            body.insert("msg_type".into(), pilotdeck_protocol::JsValue::Number(0.0));
            if let Some(id) = msg_id {
                body.insert("msg_id".into(), id.into());
                body.insert(
                    "msg_seq".into(),
                    pilotdeck_protocol::JsValue::Number(seq as f64),
                );
            }
            let body =
                pilotdeck_protocol::stringify_json(&pilotdeck_protocol::JsValue::Object(body))
                    .map_err(|_| failure("Invalid QQ message JSON"))?
                    .unwrap();
            self.api
                .request(
                    "POST",
                    &format!(
                        "{}/v2/{}/{}/messages",
                        self.options.api_base,
                        if target.direct { "users" } else { "groups" },
                        target.id
                    ),
                    Some(&format!("QQBot {token}")),
                    Some(body),
                )
                .await
        };
        tokio::select! {_=stopped.changed()=>false,result=send=>result.is_ok()}
    }
    async fn send_reply(&self, chat: &str, text: &str) -> bool {
        let target = self.targets.lock().unwrap().get(chat).cloned();
        let Some(target) = target else {
            return false;
        };
        self.send(&target, text.into(), None, 1).await
    }
    async fn send_initial_reply(&self, chat: &str, text: &str) -> bool {
        let target = self.targets.lock().unwrap().get(chat).cloned();
        let Some(target) = target else {
            return false;
        };
        let chunks = split_text(text, self.options.max_message_length);
        let many = chunks.len() > 1;
        for (i, text) in chunks.into_iter().enumerate() {
            if !self
                .send(&target, text, target.msg_id.as_deref(), i + 1)
                .await
            {
                return false;
            }
            if many {
                let Some(mut stopped) = self.shutdown.lock().unwrap().clone() else {
                    return false;
                };
                tokio::select! {_=stopped.changed()=>return false,_=tokio::time::sleep(Duration::from_millis(500))=>{}}
            }
        }
        true
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::{origin_channel_matches, session_field};
        if !origin_channel_matches(delivery, "qq") {
            return Ok(false);
        }
        let Some(session) = session_field(delivery) else {
            return Ok(false);
        };
        let chat = if let Some(user) = session
            .strip_prefix("qq:c2c:")
            .and_then(|rest| rest.strip_suffix(":general"))
        {
            format!("c2c:{user}")
        } else if let Some(rest) = session.strip_prefix("qq:group=") {
            let Some((group, tail)) = rest.split_once(":user=") else {
                return Ok(false);
            };
            let Some(user) = tail.split(':').next().filter(|user| !user.is_empty()) else {
                return Ok(false);
            };
            format!("{group}:{user}")
        } else {
            return Ok(false);
        };
        Ok(self
            .send_initial_reply(&chat, delivery["text"].as_str().unwrap_or(""))
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
    owner: Arc<QQChannel>,
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
                        .send_initial_reply(&self.chat, &interaction::elicitation_prompt(&event))
                        .await;
                }
                Some("assistant_text_delta") => {
                    if let Some(text) = event["text"].as_str() {
                        self.text.lock().unwrap().push_str(text);
                    }
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
fn split_text(text: &str, max: usize) -> Vec<JsString> {
    let mut rest = text.encode_utf16().collect::<Vec<_>>();
    let mut chunks = Vec::new();
    while rest.len() > max {
        let split = rest[..=max]
            .iter()
            .rposition(|c| *c == 10)
            .filter(|i| (*i as f64) >= max as f64 * 0.3)
            .unwrap_or(max);
        chunks.push(JsString::from_utf16(rest[..split].to_vec()));
        rest = rest[split..].to_vec();
        if rest.first() == Some(&10) {
            rest.remove(0);
        }
    }
    if !rest.is_empty() || chunks.is_empty() {
        chunks.push(JsString::from_utf16(rest));
    }
    chunks
}
fn failure(message: &str) -> RouteError {
    RouteError::new("QQ_CHANNEL_FAILED", message)
}
