//! Native WhatsApp bridge owner. TS WhatsAppChannel.ts:49-339.
//! This adapter owns the configured bridge subprocess and uses its HTTP API.
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex as StdMutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::Child,
    sync::{Mutex, watch},
    task::{JoinHandle, JoinSet},
};
mod http;
mod interaction;
#[cfg(test)]
mod tests;
use http::Api;
use interaction::Chat;

#[derive(Clone, Debug)]
pub struct WhatsAppOptions {
    pub bridge_path: PathBuf,
    pub bridge_url: String,
    /// The runtime executing the configured JS bridge. Normally `node`.
    pub node_executable: PathBuf,
}
impl Default for WhatsAppOptions {
    fn default() -> Self {
        Self {
            bridge_path: PathBuf::new(),
            bridge_url: "http://127.0.0.1:3100".into(),
            node_executable: "node".into(),
        }
    }
}
pub struct WhatsAppChannel {
    options: WhatsAppOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    seen: StdMutex<HashSet<String>>,
    running: AtomicBool,
    shutdown: StdMutex<Option<watch::Receiver<bool>>>,
}
pub struct WhatsAppHandle {
    shutdown: watch::Sender<bool>,
    owner: Arc<WhatsAppChannel>,
    task: StdMutex<Option<JoinHandle<()>>>,
    child: StdMutex<Option<Child>>,
}
impl WhatsAppHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        let child = self.child.lock().unwrap().take();
        if let Some(mut child) = child {
            terminate(&mut child).await;
        }
    }
}
impl Drop for WhatsAppHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.start_kill();
        }
    }
}
async fn terminate(child: &mut Child) {
    #[cfg(unix)]
    if let Some(pid) = child
        .id()
        .and_then(|pid| rustix::process::Pid::from_raw(pid as i32))
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    #[cfg(not(unix))]
    {
        let _ = child.start_kill();
    }
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}
impl WhatsAppChannel {
    pub fn new(mut options: WhatsAppOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        mut options: WhatsAppOptions, gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        options.bridge_url = options.bridge_url.trim_end_matches('/').into();
        options.bridge_path = PathBuf::from(crate::metadata::js_trim(
            &options.bridge_path.to_string_lossy(),
        ));
        Arc::new(Self {

            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            seen: Default::default(),
            running: AtomicBool::new(false),
            shutdown: Default::default(),        })
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
    pub async fn start(self: &Arc<Self>) -> Result<WhatsAppHandle> {
        if self.options.bridge_path.as_os_str().is_empty() {
            return Err(failure("bridgePath / WHATSAPP_BRIDGE_PATH is required"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("WhatsApp channel already started"));
        }
        self.seen.lock().unwrap().clear();
        let mut command = tokio::process::Command::new(&self.options.node_executable);
        command
            .arg(&self.options.bridge_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Node URL.port omits the default port; only explicit non-default ports
        // override BRIDGE_PORT. Other inherited variables stay with the child.
        if let Some(port) = url::Url::parse(&self.options.bridge_url)
            .ok()
            .and_then(|url| url.port())
        {
            command.env("BRIDGE_PORT", port.to_string());
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(_) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(failure("WhatsApp bridge spawn failed"));
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let mut output = JoinSet::new();
        if let Some(mut stdout) = stdout {
            output.spawn(async move {
                let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
            });
        }
        if let Some(mut stderr) = stderr {
            output.spawn(async move {
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            });
        }
        let started = tokio::time::Instant::now();
        let mut ready = false;
        while started.elapsed() < Duration::from_secs(15) {
            if let Ok(Ok((status, _))) = tokio::time::timeout(
                Duration::from_secs(3),
                self.api.request(&self.options, "GET", "/messages", None),
            )
            .await
                && ((200..300).contains(&status) || status == 404)
            {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        if !ready {
            self.running.store(false, Ordering::SeqCst);
            terminate(&mut child).await;
            return Err(failure("WhatsApp bridge HTTP did not become ready"));
        }
        let (shutdown, mut stopped) = watch::channel(false);
        *self.shutdown.lock().unwrap() = Some(stopped.clone());
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let _output = output;
            let mut interval = tokio::time::interval(Duration::from_secs(2));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut polls = JoinSet::new();
            let mut messages = JoinSet::new();
            loop {
                tokio::select! {
                    _=stopped.changed()=>break,
                    _=interval.tick()=>{let owner=owner.clone();polls.spawn(async move{owner.poll().await});},
                    received=polls.join_next(),if !polls.is_empty()=>{
                        if let Some(Ok(list))=received {
                            for msg in list {
                                if !owner.seen.lock().unwrap().insert(msg.id){continue;}
                                let owner=owner.clone();
                                let mut message=Box::pin(async move{let _=owner.message(&msg.chat,&msg.text).await;});
                                let completed=std::future::poll_fn(|cx|std::task::Poll::Ready(message.as_mut().poll(cx).is_ready())).await;
                                if !completed {messages.spawn(message);}
                            }
                        }
                    },
                }
                while messages.try_join_next().is_some() {}
            }
            polls.abort_all();
            messages.abort_all();
            while polls.join_next().await.is_some() {}
            while messages.join_next().await.is_some() {}
        });
        Ok(WhatsAppHandle {
            shutdown,
            owner: self.clone(),
            task: StdMutex::new(Some(task)),
            child: StdMutex::new(Some(child)),
        })
    }
    async fn poll(&self) -> Vec<Inbound> {
        let Ok((status, bytes)) = self
            .api
            .request(&self.options, "GET", "/messages", None)
            .await
        else {
            return Vec::new();
        };
        if !(200..300).contains(&status) {
            return Vec::new();
        }
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .map(normalize)
            .unwrap_or_default()
    }
    async fn message(self: &Arc<Self>, chat: &str, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
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
                mapper.reset("whatsapp", chat);
            }
            (
                mapper.session("whatsapp", chat),
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
        let sink = Sink {
            owner: self.clone(),
            chat: chat.into(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let result = self
            .gateway
            .stream_turn(
                json!({"sessionKey":session,"channelKey":"whatsapp","message":message}),
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
    async fn send_reply(&self, chat: &str, text: &str) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        let Some(mut stopped) = self.shutdown.lock().unwrap().clone() else {
            return false;
        };
        if *stopped.borrow() {
            return false;
        }
        let sent = tokio::select! {_=stopped.changed()=>return false,result=tokio::time::timeout(Duration::from_secs(60),self.api.request(&self.options,"POST","/send",Some(json!({"chatId":chat,"message":text}))))=>result};
        matches!(sent, Ok(Ok((200..=299, _))))
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "whatsapp") {
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
            .strip_prefix("whatsapp:chat=")
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
struct Inbound {
    id: String,
    chat: String,
    text: String,
}
fn normalize(data: Value) -> Vec<Inbound> {
    let raw = data
        .as_array()
        .or_else(|| data["messages"].as_array())
        .or_else(|| data["data"].as_array());
    raw.into_iter()
        .flatten()
        .filter(|v| v.is_object() || v.is_array())
        .filter_map(|v| {
            let string = |keys: &[&str]| {
                keys.iter()
                    .find_map(|key| v.get(*key).filter(|v| !v.is_null()))
                    .map(|v| {
                        let value = pilotdeck_protocol::parse_json(&v.to_string()).unwrap();
                        pilotdeck_protocol::js_value::display(&value).unwrap_or_default()
                    })
                    .map(|s| crate::metadata::js_trim(&s).to_owned())
                    .unwrap_or_default()
            };
            let id = string(&["id", "key", "messageId"]);
            let chat = string(&["chatId", "chat_id", "from"]);
            let text = string(&["text", "body", "content"]);
            if id.is_empty() || chat.is_empty() {
                None
            } else {
                Some(Inbound { id, chat, text })
            }
        })
        .collect()
}
struct Active(Arc<AtomicBool>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}
struct Sink {
    owner: Arc<WhatsAppChannel>,
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
    RouteError::new("WHATSAPP_CHANNEL_FAILED", message)
}
