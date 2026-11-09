//! WeCom intelligent bot long connection. TS WeComChannel.ts:198-1330 (cc7c4e6).
//! Configuration/environment resolution belongs to the application.

pub fn handle_inbound_text(text: &str, reply: impl FnMut(&str)) -> bool {
    super::channel_commands::dispatch_wecom_inbound_system_command(text, reply)
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
    time::Duration,
};
use tokio::{
    sync::{Mutex, mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
    time::Instant,
};
mod interaction;
mod media;
#[path = "home_assistant/socket.rs"]
mod socket;
#[cfg(test)]
mod tests;
use interaction::Chat;
#[derive(Clone)]
pub struct WeComOptions {
    pub bot_key: String,
    pub bot_secret: String,
    pub websocket_url: String,
    pub dm_policy: String,
    pub allow_from: Vec<String>,
    pub group_policy: String,
    pub group_allow_from: Vec<String>,
    pub groups: Value,
    pub group_sessions_per_user: bool,
    pub text_batch_delay_ms: u64,
    pub text_batch_split_delay_ms: u64,
    pub pilot_home: PathBuf,
}
impl Default for WeComOptions {
    fn default() -> Self {
        Self {
            bot_key: String::new(),
            bot_secret: String::new(),
            websocket_url: "wss://openws.work.weixin.qq.com".into(),
            dm_policy: "pairing".into(),
            allow_from: vec![],
            group_policy: "pairing".into(),
            group_allow_from: vec![],
            groups: json!({}),
            group_sessions_per_user: true,
            text_batch_delay_ms: 600,
            text_batch_split_delay_ms: 2000,
            pilot_home: PathBuf::new(),
        }
    }
}
#[derive(Clone)]
struct Target {
    chat: String,
    group: bool,
    reply: String,
}
#[derive(Clone)]
struct Input {
    target: Target,
    sender: String,
    key: String,
    text: String,
    attachments: Vec<Value>,
}
struct Batch {
    input: Input,
    due: Instant,
}
struct Rpc {
    id: String,
    cmd: String,
    body: Value,
    text: Option<pilotdeck_protocol::JsString>,
    answer: oneshot::Sender<Result<Value>>,
}
struct RpcPending {
    answer: oneshot::Sender<Result<Value>>,
    deadline: Instant,
}
pub struct WeComChannel {
    options: WeComOptions,
    gateway: Arc<dyn ChannelGateway>,
    device: String,
    media: media::Http,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    active: StdMutex<HashSet<String>>,
    targets: StdMutex<HashMap<String, Target>>,
    replies: StdMutex<VecDeque<(String, String)>>,
    last_replies: StdMutex<VecDeque<(String, String)>>,
    seen: StdMutex<VecDeque<(String, Instant)>>,
    batches: StdMutex<HashMap<String, Batch>>,
    running: AtomicBool,
    connected: AtomicBool,
    rpc: StdMutex<Option<mpsc::UnboundedSender<Rpc>>>,
}
pub struct WeComHandle {
    shutdown: watch::Sender<bool>,
    owner: Arc<WeComChannel>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl WeComHandle {
    pub async fn close(&self) {
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}
impl Drop for WeComHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        self.owner.clear();
    }
}
impl WeComChannel {
    pub fn new(options: WeComOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, json!({}), None)
    }
    pub fn with_mapper(
        options: WeComOptions,
        gateway: Arc<dyn ChannelGateway>,
        saved: Value,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        let mut state = ChannelSessionState::from_value(&saved);
        state.project_by_chat_id = saved["projectByScopeKey"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.into())))
            .collect();
        Arc::new(Self {
            options,
            gateway,
            device: uuid::Uuid::new_v4().simple().to_string(),
            media: media::Http::new(),
            mapper: StdMutex::new(state),
            on_state_change,
            chats: Default::default(),
            active: Default::default(),
            targets: Default::default(),
            replies: Default::default(),
            last_replies: Default::default(),
            seen: Default::default(),
            batches: Default::default(),
            running: AtomicBool::new(false),
            connected: AtomicBool::new(false),
            rpc: Default::default(),
        })
    }
    pub fn snapshot(&self) -> Value {
        let s = self.mapper.lock().unwrap();
        json!({"activeByChatId":s.active_by_chat_id,"projectByScopeKey":s.project_by_chat_id})
    }
    fn changed(&self) {
        if let Some(f) = &self.on_state_change {
            f(self.snapshot());
        }
    }
    fn chat(&self, key: &str) -> Arc<Mutex<Chat>> {
        self.chats
            .lock()
            .unwrap()
            .entry(key.into())
            .or_default()
            .clone()
    }
    fn clear(&self) {
        self.running.store(false, Ordering::SeqCst);
        self.connected.store(false, Ordering::SeqCst);
        self.rpc.lock().unwrap().take();
        self.replies.lock().unwrap().clear();
        self.last_replies.lock().unwrap().clear();
        self.seen.lock().unwrap().clear();
        self.batches.lock().unwrap().clear();
        self.chats.lock().unwrap().clear();
        self.active.lock().unwrap().clear();
        self.targets.lock().unwrap().clear();
    }
    async fn connect(&self) -> Result<socket::Socket> {
        tokio::time::timeout(Duration::from_secs(20),async{let mut ws=socket::Socket::connect(&self.options.websocket_url,None).await?;let id=req_id("subscribe");ws.send(json!({"cmd":"aibot_subscribe","headers":{"req_id":id},"body":{"bot_id":self.options.bot_key.trim(),"secret":self.options.bot_secret.trim(),"device_id":self.device}})).await?;loop{let value=ws.next().await?.ok_or_else(||failure("WeCom socket closed during authentication"))?;if value["headers"]["req_id"]==id{response_error(&value)?;return Ok(ws);}}}).await.map_err(|_|failure("WeCom authentication timed out"))?
    }
    pub async fn start(self: &Arc<Self>) -> Result<WeComHandle> {
        if self.options.bot_key.trim().is_empty() || self.options.bot_secret.trim().is_empty() {
            return Err(failure("WeCom bot ID and secret are required"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("WeCom channel already started"));
        }
        let ws = match self.connect().await {
            Ok(ws) => ws,
            Err(e) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(e);
            }
        };
        self.connected.store(true, Ordering::SeqCst);
        let (tx, rx) = mpsc::unbounded_channel();
        *self.rpc.lock().unwrap() = Some(tx);
        let (shutdown, stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            owner.run(ws, rx, stopped).await;
        });
        Ok(WeComHandle {
            shutdown,
            owner: self.clone(),
            task: StdMutex::new(Some(task)),
        })
    }
    async fn run(
        self: Arc<Self>,
        mut ws: socket::Socket,
        mut requests: mpsc::UnboundedReceiver<Rpc>,
        mut stopped: watch::Receiver<bool>,
    ) {
        let mut pending: HashMap<String, RpcPending> = HashMap::new();
        let mut tasks = JoinSet::new();
        let mut heartbeat = tokio::time::interval_at(
            Instant::now() + Duration::from_secs(30),
            Duration::from_secs(30),
        );
        let mut tick = tokio::time::interval(Duration::from_millis(10));
        let mut attempt = 0usize;
        'owner: loop {
            let disconnected = loop {
                tokio::select! {biased;_=stopped.changed()=>break 'owner,
                request=requests.recv()=>{let Some(r)=request else{break 'owner;};pending.insert(r.id.clone(),RpcPending{answer:r.answer,deadline:Instant::now()+Duration::from_secs(15)});let mut value=pilotdeck_protocol::parse_json(&json!({"cmd":r.cmd,"headers":{"req_id":r.id},"body":r.body}).to_string()).unwrap();if let Some(text)=r.text{if let pilotdeck_protocol::JsValue::Object(root)=&mut value{if let Some(body)=root.get("body").and_then(pilotdeck_protocol::JsValue::as_object){let mut body=body.clone();if let Some(markdown)=body.get("markdown").and_then(pilotdeck_protocol::JsValue::as_object){let mut markdown=markdown.clone();markdown.insert("content".into(),pilotdeck_protocol::JsValue::String(text));body.insert("markdown".into(),pilotdeck_protocol::JsValue::Object(markdown));root.insert("body".into(),pilotdeck_protocol::JsValue::Object(body));}}}}let wire=pilotdeck_protocol::stringify_json(&value).unwrap().unwrap();if ws.send_text(&wire).await.is_err(){break true;}},
                value=ws.next()=>{match value{Ok(Some(v))=>{let cmd=v["cmd"].as_str().unwrap_or("");let id=js(&v["headers"]["req_id"]);if !matches!(cmd,"aibot_msg_callback"|"aibot_callback"|"aibot_event_callback")&&pending.contains_key(&id){let p=pending.remove(&id).unwrap();let _=p.answer.send(Ok(v));}else if matches!(cmd,"aibot_msg_callback"|"aibot_callback"){let owner=self.clone();tasks.spawn(async move{let _=owner.message(v).await;});}},_=>break true,}},
                _=heartbeat.tick()=>{if ws.send(json!({"cmd":"ping","headers":{"req_id":req_id("ping")},"body":{}})).await.is_err(){break true;}},
                _=tick.tick()=>{let now=Instant::now();let expired=pending.iter().filter(|(_,p)|p.deadline<=now).map(|(id,_)|id.clone()).collect::<Vec<_>>();for id in expired{if let Some(p)=pending.remove(&id){let _=p.answer.send(Err(failure("Timeout waiting for WeCom response")));}}let due={let mut batches=self.batches.lock().unwrap();let keys=batches.iter().filter(|(_,b)|b.due<=now).map(|(k,_)|k.clone()).collect::<Vec<_>>();keys.into_iter().filter_map(|k|batches.remove(&k).map(|b|b.input)).collect::<Vec<_>>()};for input in due{let owner=self.clone();tasks.spawn(async move{let _=owner.dispatch(input).await;});}},
                Some(_)=tasks.join_next()=>{}
                }
            };
            if disconnected {
                self.connected.store(false, Ordering::SeqCst);
                for (_, p) in pending.drain() {
                    let _ = p.answer.send(Err(failure("WeCom disconnected")));
                }
                let _ = ws.close().await;
                loop {
                    let delay = [2000, 5000, 10000, 30000, 60000][attempt.min(4)];
                    attempt += 1;
                    let reconnect = async {
                        tokio::time::sleep(Duration::from_millis(delay)).await;
                        self.connect().await
                    };
                    tokio::select! {_=stopped.changed()=>break 'owner,result=reconnect=>{match result{Ok(socket)=>{ws=socket;self.connected.store(true,Ordering::SeqCst);attempt=0;heartbeat.reset_at(Instant::now()+Duration::from_secs(30));break;},Err(_)=>continue,}}}
                }
            }
        }
        let _ = ws.close().await;
        for (_, p) in pending {
            let _ = p.answer.send(Err(failure("WeCom stopped")));
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        self.clear();
    }
    async fn request(&self, cmd: &str, body: Value, id: Option<String>) -> Result<Value> {
        self.request_text(cmd, body, id, None).await
    }
    async fn request_text(
        &self,
        cmd: &str,
        body: Value,
        id: Option<String>,
        text: Option<pilotdeck_protocol::JsString>,
    ) -> Result<Value> {
        if !self.running.load(Ordering::SeqCst) || !self.connected.load(Ordering::SeqCst) {
            return Err(failure("WeCom websocket is not connected"));
        }
        let (tx, rx) = oneshot::channel();
        let sender = self
            .rpc
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| failure("WeCom not connected"))?;
        sender
            .send(Rpc {
                id: id.unwrap_or_else(|| req_id(cmd)),
                cmd: cmd.into(),
                body,
                text,
                answer: tx,
            })
            .map_err(|_| failure("WeCom stopped"))?;
        let value = tokio::time::timeout(Duration::from_secs(15), rx)
            .await
            .map_err(|_| failure("Timeout waiting for WeCom response"))?
            .map_err(|_| failure("WeCom connection closed"))??;
        response_error(&value)?;
        Ok(value)
    }
    fn reply_id(&self, target: &Target) -> Option<String> {
        self.replies
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| id == &target.reply)
            .map(|(_, req)| req.clone())
            .or_else(|| {
                self.last_replies
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(id, _)| id == &target.chat)
                    .map(|(_, req)| req.clone())
            })
    }
    async fn send_target(&self, target: &Target, mut body: Value) -> bool {
        let id = self.reply_id(target);
        let cmd = if id.is_some() {
            "aibot_respond_msg"
        } else if target.group {
            return false;
        } else {
            body["chatid"] = target.chat.clone().into();
            "aibot_send_msg"
        };
        self.request(cmd, body, id).await.is_ok()
    }
    async fn text_target(&self, target: &Target, text: &str) -> bool {
        let units = text.encode_utf16().take(4000).collect::<Vec<_>>();
        let text = pilotdeck_protocol::JsString::from_utf16(units);
        let id = self.reply_id(target);
        let mut body =
            json!({"msgtype":"markdown","markdown":{"content":"__WECOM_UTF16_MARKDOWN__"}});
        let cmd = if id.is_some() {
            "aibot_respond_msg"
        } else if target.group {
            return false;
        } else {
            body["chatid"] = target.chat.clone().into();
            "aibot_send_msg"
        };
        self.request_text(cmd, body, id, Some(text)).await.is_ok()
    }
    async fn send_reply(&self, key: &str, text: &str) -> bool {
        let target = self.targets.lock().unwrap().get(key).cloned();
        match target {
            Some(t) => self.text_target(&t, text).await,
            None => false,
        }
    }
    pub async fn send_text(&self, chat: &str, text: &str, reply_to: Option<&str>) -> bool {
        self.text_target(
            &Target {
                chat: chat.into(),
                group: false,
                reply: reply_to.unwrap_or("").into(),
            },
            text,
        )
        .await
    }
    fn allowed(&self, group: bool, chat: &str, sender: &str) -> bool {
        if !group && sender.is_empty() {
            return false;
        }
        let policy = if group {
            &self.options.group_policy
        } else {
            &self.options.dm_policy
        };
        match policy.trim().to_lowercase().as_str() {
            "open" => {}
            "allowlist" => {
                if !matches_entry(
                    if group {
                        &self.options.group_allow_from
                    } else {
                        &self.options.allow_from
                    },
                    if group { chat } else { sender },
                ) {
                    return false;
                }
            }
            _ => return false,
        }
        if group {
            let cfg = self.options.groups.as_object().and_then(|groups| {
                groups
                    .get(chat)
                    .filter(|v| v.is_object())
                    .or_else(|| {
                        groups
                            .iter()
                            .find(|(k, v)| k.to_lowercase() == chat.to_lowercase() && v.is_object())
                            .map(|(_, v)| v)
                    })
                    .or_else(|| groups.get("*").filter(|v| v.is_object()))
            });
            if let Some(cfg) = cfg {
                let entries = coerce_list(
                    cfg.get("allow_from")
                        .or_else(|| cfg.get("allowFrom"))
                        .unwrap_or(&Value::Null),
                );
                if !entries.is_empty() && !matches_entry(&entries, sender) {
                    return false;
                }
            }
        }
        true
    }
    fn scope(&self, input: &Input) -> String {
        if input.target.group {
            if self.options.group_sessions_per_user && !input.sender.trim().is_empty() {
                format!("wecom:group={}:user={}", input.target.chat, input.sender)
            } else {
                format!("wecom:group={}", input.target.chat)
            }
        } else {
            format!(
                "wecom:dm={}",
                if input.sender.trim().is_empty() {
                    &input.target.chat
                } else {
                    &input.sender
                }
            )
        }
    }
    async fn message(self: &Arc<Self>, raw: Value) -> Result<()> {
        let Some(body) = raw.get("body").filter(|b| b.is_object()) else {
            return Ok(());
        };
        let rid = js(&raw["headers"]["req_id"]);
        let mid = body
            .get("msgid")
            .filter(|v| !v.is_null())
            .map(js)
            .unwrap_or_else(|| rid.clone())
            .trim()
            .to_owned();
        {
            let mut seen = self.seen.lock().unwrap();
            let now = Instant::now();
            seen.retain(|(_, t)| now.duration_since(*t) <= Duration::from_secs(300));
            if seen.iter().any(|(id, _)| id == &mid) {
                return Ok(());
            }
            seen.push_back((mid.clone(), now));
            while seen.len() > 1000 {
                seen.pop_front();
            }
        }
        let sender = js(&body["from"]["userid"]).trim().to_owned();
        let chat = body
            .get("chatid")
            .filter(|v| !v.is_null())
            .map(js)
            .unwrap_or_else(|| sender.clone())
            .trim()
            .to_owned();
        let group = js(&body["chattype"]).to_lowercase() == "group";
        if chat.is_empty() || !self.allowed(group, &chat, &sender) {
            return Ok(());
        }
        if !rid.is_empty() {
            remember(&mut self.replies.lock().unwrap(), mid.clone(), rid.clone());
            remember(&mut self.last_replies.lock().unwrap(), chat.clone(), rid);
        }
        let (mut text, attachments) = self.parts(body).await?;
        if group {
            text = strip_mention(&text);
        }
        if text.trim().is_empty() && attachments.is_empty() {
            return Ok(());
        }
        let key = if group && self.options.group_sessions_per_user && !sender.is_empty() {
            format!("{chat}:{sender}")
        } else {
            chat.clone()
        };
        let target = Target {
            chat,
            group,
            reply: mid,
        };
        self.targets
            .lock()
            .unwrap()
            .insert(key.clone(), target.clone());
        if self.answer(&key, &text).await? {
            return Ok(());
        }
        let input = Input {
            target,
            sender,
            key: key.clone(),
            text: if text.is_empty() && !attachments.is_empty() {
                "用户发送了企业微信附件。".into()
            } else {
                text
            },
            attachments,
        };
        if input.text.trim().starts_with('/')
            || !input.attachments.is_empty()
            || self.options.text_batch_delay_ms == 0
        {
            return self.dispatch(input).await;
        }
        let delay = if input.text.encode_utf16().count() >= 3900 {
            self.options.text_batch_split_delay_ms
        } else {
            self.options.text_batch_delay_ms
        };
        let mut batches = self.batches.lock().unwrap();
        let due = Instant::now() + Duration::from_millis(delay);
        if let Some(old) = batches.get_mut(&key) {
            old.input.text = format!("{}\n{}", old.input.text, input.text);
            old.input.target = input.target;
            old.due = due;
        } else {
            batches.insert(key, Batch { input, due });
        }
        Ok(())
    }
    async fn dispatch(self: &Arc<Self>, mut input: Input) -> Result<()> {
        input.text = input.text.trim().into();
        let scope = self.scope(&input);
        let new = input.text == "/new" || input.text.starts_with("/new ");
        let (message, session, project) = {
            let mut s = self.mapper.lock().unwrap();
            if new {
                s.active_by_chat_id
                    .insert(scope.clone(), format!("{scope}:s_{}", uuid::Uuid::new_v4()));
            }
            (
                if new {
                    input.text[4..].trim().into()
                } else {
                    input.text.clone()
                },
                s.active_by_chat_id
                    .get(&scope)
                    .cloned()
                    .unwrap_or_else(|| format!("{scope}:general")),
                s.project_by_chat_id.get(&scope).cloned(),
            )
        };
        if new {
            self.changed();
            if message.is_empty() {
                self.text_target(&input.target, "已创建新会话。").await;
                return Ok(());
            }
        }
        // Shared commands expect a chat-keyed state; translate the mapper scope at this boundary.
        let mut state = self.mapper.lock().unwrap().clone();
        let before = state.snapshot();
        let reply = Reply {
            owner: self.clone(),
            target: input.target.clone(),
        };
        let handled = execute_channel_command(
            &input.text,
            &mut CommandContext {
                gateway: self.gateway.as_ref(),
                reply: &reply,
                state: &mut state,
                channel: "wecom",
                chat: &scope,
                home: &self.options.pilot_home,
            },
        )
        .await?;
        if handled {
            if before != state.snapshot() {
                if let Some(key) = state.active_by_chat_id.get_mut(&scope) {
                    if let Some(suffix) = key.strip_prefix(&format!("wecom:chat={scope}:")) {
                        *key = format!("{scope}:{suffix}");
                    }
                }
                {
                    let mut mapper = self.mapper.lock().unwrap();
                    if let Some(key) = state.active_by_chat_id.get(&scope) {
                        mapper.active_by_chat_id.insert(scope.clone(), key.clone());
                    }
                    if let Some(key) = state.project_by_chat_id.get(&scope) {
                        mapper.project_by_chat_id.insert(scope.clone(), key.clone());
                    }
                }
                self.changed();
            }
            return Ok(());
        }
        if message.is_empty() {
            return Ok(());
        }
        if !self.active.lock().unwrap().insert(session.clone()) {
            return Ok(());
        }
        let _active = Active {
            owner: self.clone(),
            session: session.clone(),
        };
        let sink = Sink {
            owner: self.clone(),
            input: input.clone(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let mut params = json!({"sessionKey":session,"channelKey":"wecom","message":message,"attachments":input.attachments,"allowPlanModeTools":false,"syntheticMessages":[{"text":"[WeCom attachment hint: If the user explicitly asks you to send a generated file to this WeCom chat, save it to an absolute local path and include MEDIA:/absolute/path in your final answer. Do not use MEDIA for files that were only read or analyzed internally.]","purpose":"wecom_deliverable_hint"}]});
        if let Some(project) = project.filter(|p| !p.is_empty()) {
            params["projectKey"] = project.into();
        }
        let result = self.gateway.stream_turn(params, &sink).await;
        self.chat(&input.key).lock().await.finish_turn();
        let text = if result.is_err() {
            "处理消息时发生错误，请重试。".into()
        } else {
            sink.text.lock().unwrap().clone()
        };
        if !text.trim().is_empty() {
            self.deliver_final(&input.target, text.trim()).await;
        }
        Ok(())
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::{origin_channel_matches, parse_chat_id, session_field};
        if !origin_channel_matches(delivery, "wecom") {
            return Ok(false);
        }
        let Some(session) = session_field(delivery) else {
            return Ok(false);
        };
        let Some(chat) = parse_chat_id(session, "wecom") else {
            return Ok(false);
        };
        Ok(self
            .send_reply(chat, delivery["text"].as_str().unwrap_or(""))
            .await)
    }
}
struct Active {
    owner: Arc<WeComChannel>,
    session: String,
}
impl Drop for Active {
    fn drop(&mut self) {
        self.owner.active.lock().unwrap().remove(&self.session);
    }
}
struct Reply {
    owner: Arc<WeComChannel>,
    target: Target,
}
impl ChannelReply for Reply {
    fn reply<'a>(&'a self, text: &'a str) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            if self.owner.text_target(&self.target, text).await {
                Ok(())
            } else {
                Err(failure("WeCom reply failed"))
            }
        })
    }
}
struct Sink {
    owner: Arc<WeComChannel>,
    input: Input,
    session: String,
    text: StdMutex<String>,
}
impl ChannelEventSink for Sink {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            match event["type"].as_str().unwrap_or("") {
                "permission_request" => {
                    self.owner
                        .capture_permission(&self.input.key, &self.session, event)
                        .await
                }
                "elicitation_request" => {
                    self.owner.chat(&self.input.key).lock().await.elicitation =
                        Some((self.session.clone(), event.clone()));
                    self.owner
                        .text_target(&self.input.target, &interaction::elicitation_prompt(&event))
                        .await;
                }
                kind => {
                    self.owner.event_media(&self.input.target, &event).await;
                    let fragment = match kind {
                        "assistant_text_delta" => js(&event["text"]),
                        "tool_call_finished" if event["ok"] != true => format!(
                            "\n⚠️ {} failed\n",
                            event
                                .get("toolName")
                                .filter(|v| !v.is_null())
                                .map(js)
                                .unwrap_or_else(|| js(&event["toolCallId"]))
                        ),
                        "error" => format!("\n❌ {}\n", js(&event["message"])),
                        _ => String::new(),
                    };
                    self.text.lock().unwrap().push_str(&fragment);
                }
            }
            Ok(())
        })
    }
}
fn req_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4().simple())
}
fn failure(message: &str) -> RouteError {
    RouteError::new("WECOM_ERROR", message)
}
fn response_error(value: &Value) -> Result<()> {
    let code = value["body"]
        .get("errcode")
        .filter(|v| !v.is_null())
        .or_else(|| value.get("errcode").filter(|v| !v.is_null()));
    if let Some(code) = code {
        if code != 0 {
            return Err(failure(&format!(
                "WeCom errcode {}: {}",
                js(code),
                value["body"]
                    .get("errmsg")
                    .or_else(|| value.get("errmsg"))
                    .map(js)
                    .unwrap_or_else(|| "error".into())
            )));
        }
    }
    Ok(())
}
fn js(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => pilotdeck_protocol::parse_json(&value.to_string())
            .ok()
            .and_then(|v| pilotdeck_protocol::js_value::display(&v).ok())
            .unwrap_or_default(),
    }
}
fn remember(map: &mut VecDeque<(String, String)>, key: String, value: String) {
    let key = key.trim().to_owned();
    let value = value.trim().to_owned();
    if key.is_empty() || value.is_empty() {
        return;
    }
    if let Some((_, v)) = map.iter_mut().find(|(k, _)| k == &key) {
        *v = value;
    } else {
        map.push_back((key, value));
    }
    while map.len() > 1000 {
        map.pop_front();
    }
}
pub fn coerce_list(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .map(js)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
        Value::String(s) => s
            .split(',')
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect(),
        _ => vec![],
    }
}
fn matches_entry(entries: &[String], id: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.trim().to_lowercase();
        s.strip_prefix("wecom:user:")
            .or_else(|| s.strip_prefix("wecom:group:"))
            .unwrap_or(&s)
            .into()
    }
    let id = norm(id);
    entries.iter().any(|entry| {
        let e = norm(entry);
        e == "*" || e == id
    })
}
fn strip_mention(text: &str) -> String {
    let text = if text.starts_with('@') {
        text.find(char::is_whitespace)
            .map(|i| &text[i..])
            .unwrap_or("")
    } else {
        text
    };
    text.trim().into()
}
