//! Home Assistant WS adapter (HomeAssistantChannel.ts:20-400). All options are
//! supplied by the application, including environment fallbacks.
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    sync::{Mutex, mpsc, oneshot, watch},
    task::{JoinHandle, JoinSet},
};
pub(crate) mod interaction;
pub(crate) mod socket;
#[cfg(test)]
pub(crate) mod tests;
use interaction::{Chat, ImChannel};
use socket::Socket;

#[derive(Clone, Debug)]
pub struct HomeAssistantOptions {
    pub url: String,
    pub token: String,
    pub watch_prefixes: Vec<String>,
    pub notification_title: Option<String>,
}
impl Default for HomeAssistantOptions {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:8123".into(),
            token: String::new(),
            watch_prefixes: vec!["conversation.".into()],
            notification_title: None,
        }
    }
}
pub struct HomeAssistantChannel {
    options: HomeAssistantOptions,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    running: AtomicBool,
    ready: AtomicBool,
    next_id: AtomicU64,
    outbound: StdMutex<Option<mpsc::UnboundedSender<Value>>>,
    pending: StdMutex<HashMap<u64, oneshot::Sender<bool>>>,
}
pub struct HomeAssistantHandle {
    owner: Arc<HomeAssistantChannel>,
    shutdown: watch::Sender<bool>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl HomeAssistantHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.owner.ready.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        self.owner.fail_pending();
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.owner.chats.lock().unwrap().clear();
    }
}
impl Drop for HomeAssistantHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.owner.ready.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        self.owner.fail_pending();
        self.owner.outbound.lock().unwrap().take();
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl HomeAssistantChannel {
    pub fn new(mut options: HomeAssistantOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        mut options: HomeAssistantOptions,
        gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        if options.watch_prefixes.is_empty() {
            options.watch_prefixes.push("conversation.".into());
        }
        Arc::new(Self {
            options,
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            running: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            outbound: Default::default(),
            pending: Default::default(),
        })
    }
    fn persist_mapper(&self) {
        ChannelSessionState::persist_channel_mapper(
            &self.mapper.lock().unwrap(),
            &self.on_state_change,
        );
    }
    fn chat(&self, chat: &str) -> Arc<Mutex<Chat>> {
        self.chats
            .lock()
            .unwrap()
            .entry(chat.into())
            .or_default()
            .clone()
    }
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }
    async fn authenticate(&self) -> Result<Socket> {
        let authenticate = async {
            let endpoint = websocket_url(&self.options.url)?;
            let mut socket = Socket::connect(&endpoint, None).await?;
            loop {
                match socket.next().await? {
                    Some(msg) => match msg["type"].as_str() {
                        Some("auth_required") => {
                            socket
                                .send(json!({"type":"auth","access_token":self.options.token}))
                                .await?
                        }
                        Some("auth_ok") => {
                            socket.send(json!({"id":self.next_id(),"type":"subscribe_events","event_type":"state_changed"})).await?;
                            return Ok(socket);
                        }
                        Some("auth_invalid") => {
                            return Err(failure("Home Assistant rejected token"));
                        }
                        _ => {}
                    },
                    None => return Err(failure("Home Assistant closed during authentication")),
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(20), authenticate)
            .await
            .map_err(|_| failure("Home Assistant authentication timed out"))?
    }
    pub async fn start(self: &Arc<Self>) -> Result<HomeAssistantHandle> {
        if self.options.token.is_empty() {
            return Err(failure("HASS_TOKEN / token not set"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("Home Assistant already started"));
        }
        let socket = match self.authenticate().await {
            Ok(socket) => socket,
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        let (outbound, receiver) = mpsc::unbounded_channel();
        *self.outbound.lock().unwrap() = Some(outbound);
        self.ready.store(true, Ordering::SeqCst);
        let (shutdown, stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            owner.run(socket, receiver, stopped).await;
        });
        Ok(HomeAssistantHandle {
            owner: self.clone(),
            shutdown,
            task: StdMutex::new(Some(task)),
        })
    }
    async fn run(
        self: Arc<Self>,
        mut socket: Socket,
        mut outbound: mpsc::UnboundedReceiver<Value>,
        mut stopped: watch::Receiver<bool>,
    ) {
        let mut messages = JoinSet::new();
        'connections: loop {
            loop {
                tokio::select! {
                    _ = stopped.changed() => break 'connections,
                    value = outbound.recv() => {
                        let Some(value) = value else { break 'connections };
                        let sent = tokio::select! { _ = stopped.changed() => break 'connections, sent = socket.send(value) => sent };
                        if sent.is_err() { break; }
                    }
                    message = socket.next() => {
                        match message {
                            Ok(Some(message)) if message["type"] == "result" => {
                                if let Some(id) = message["id"].as_u64() {
                                    if let Some(pending) = self.pending.lock().unwrap().remove(&id) { let _ = pending.send(message["success"] == true); }
                                }
                            }
                            Ok(Some(message)) => {
                                let owner = self.clone();
                                messages.spawn(async move { let _ = owner.receive(message).await; });
                            }
                            _ => break,
                        }
                    }
                }
                while messages.try_join_next().is_some() {}
            }
            self.ready.store(false, Ordering::SeqCst);
            self.fail_pending();
            // Queued service calls belong to the disconnected socket and cannot
            // be replayed as successful delivery after reconnecting.
            while outbound.try_recv().is_ok() {}
            loop {
                tokio::select! { _ = stopped.changed() => break 'connections, _ = tokio::time::sleep(Duration::from_secs(5)) => {} }
                let connected = tokio::select! { _ = stopped.changed() => break 'connections, connected = self.authenticate() => connected };
                if let Ok(connected) = connected {
                    socket = connected;
                    self.ready.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
        self.ready.store(false, Ordering::SeqCst);
        self.fail_pending();
        self.outbound.lock().unwrap().take();
        messages.abort_all();
        while messages.join_next().await.is_some() {}
        socket.close().await;
    }
    fn fail_pending(&self) {
        for (_, sender) in self.pending.lock().unwrap().drain() {
            let _ = sender.send(false);
        }
    }
    async fn receive(self: &Arc<Self>, message: Value) -> Result<()> {
        if message["type"] != "event" || message["event"]["event_type"] != "state_changed" {
            return Ok(());
        }
        let data = &message["event"]["data"];
        let Some(chat) = data["entity_id"].as_str().filter(|id| {
            self.options
                .watch_prefixes
                .iter()
                .any(|prefix| id.starts_with(prefix))
        }) else {
            return Ok(());
        };
        let new = data["new_state"].get("state");
        if new
            .zip(data["old_state"].get("state"))
            .is_some_and(|(new, old)| !new.is_array() && !new.is_object() && new == old)
        {
            return Ok(());
        }
        let Some(new) = new else {
            return Ok(());
        };
        let text = new
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| new.to_string());
        if crate::metadata::js_trim(&text).is_empty() {
            return Ok(());
        }
        self.incoming(chat, &text).await
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
                mapper.reset("homeassistant", chat);
            }
            (
                mapper.session("homeassistant", chat),
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
                json!({"sessionKey":session,"channelKey":"homeassistant","message":message}),
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
        if !self.running.load(Ordering::SeqCst) || !self.ready.load(Ordering::SeqCst) {
            return false;
        }
        let id = self.next_id();
        let title = self
            .options
            .notification_title
            .clone()
            .unwrap_or_else(|| format!("Gateway · {chat}"));
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let value = json!({"id":id,"type":"call_service","domain":"persistent_notification","service":"create","service_data":{"title":title,"message":text,"notification_id":format!("gw_{millis}")}});
        let (sender, receiver) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, sender);
        let sent = self
            .outbound
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|sender| sender.send(value).is_ok());
        if !sent {
            self.pending.lock().unwrap().remove(&id);
            return false;
        }
        let result = tokio::time::timeout(Duration::from_secs(10), receiver).await;
        self.pending.lock().unwrap().remove(&id);
        matches!(result, Ok(Ok(true)))
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "homeassistant") {
            return Ok(false);
        }
        let session = delivery
            .get("originSessionKey")
            .filter(|value| !value.is_null())
            .unwrap_or(&delivery["sessionKey"]);
        let Some(chat) = session
            .as_str()
            .and_then(|session| session.strip_prefix("homeassistant:chat="))
            .and_then(|suffix| suffix.rsplit_once(':'))
            .filter(|(chat, suffix)| {
                !chat.is_empty()
                    && !chat.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
                    && (*suffix == "general"
                        || suffix.strip_prefix("s_").is_some_and(|id| {
                            id.len() == 36
                                && id
                                    .bytes()
                                    .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
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
impl ImChannel for HomeAssistantChannel {
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
    owner: Arc<HomeAssistantChannel>,
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
                        .send_reply(&self.chat, &interaction::elicitation_prompt(&event))
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
pub(crate) fn render_event(event: &Value) -> Option<String> {
    match event["type"].as_str() {
        Some("assistant_text_delta") => event["text"].as_str().map(str::to_owned),
        Some("tool_call_finished") if event["ok"] != true => Some(format!(
            "\n⚠️ {} failed\n",
            event["toolName"]
                .as_str()
                .or_else(|| event["toolCallId"].as_str())
                .unwrap_or("")
        )),
        Some("error") => Some(format!(
            "\n❌ {}\n",
            event["message"].as_str().unwrap_or("")
        )),
        _ => None,
    }
}
fn websocket_url(base: &str) -> Result<String> {
    let base = if base.starts_with("http") {
        base.to_owned()
    } else {
        format!("http://{base}")
    };
    let parsed = url::Url::parse(&base).map_err(|_| failure("Invalid Home Assistant URL"))?;
    let host = parsed[url::Position::BeforeHost..url::Position::AfterPort].to_owned();
    let scheme = if parsed.scheme() == "https" {
        "wss"
    } else {
        "ws"
    };
    Ok(format!(
        "{scheme}://{host}{}/api/websocket",
        parsed.path().trim_end_matches('/')
    ))
}
fn failure(message: &str) -> RouteError {
    RouteError::new("HOMEASSISTANT_CHANNEL_FAILED", message)
}
