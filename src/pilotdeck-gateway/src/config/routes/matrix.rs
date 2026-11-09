//! Matrix channel owner. TS: adapters/channel/matrix/MatrixChannel.ts:48-264.
//! The application resolves credentials; this module never reads environment.
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use pilotdeck_protocol::JsString;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
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
#[cfg(test)]
mod tests;
use http::Api;
use interaction::Chat;

#[derive(Clone, Debug, Default)]
pub struct MatrixOptions {
    pub access_token: String,
    pub homeserver: String,
    pub user_id: Option<String>,
    /// TS SimpleFsStorageProvider file, normally pilotHome/matrix-bot-storage.json.
    pub storage_path: PathBuf,
}
pub struct MatrixChannel {
    options: MatrixOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    user_id: StdMutex<Option<String>>,
    running: AtomicBool,
    shutdown: StdMutex<Option<watch::Receiver<bool>>>,
}
pub struct MatrixHandle {
    shutdown: watch::Sender<bool>,
    owner: Arc<MatrixChannel>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl MatrixHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.owner.user_id.lock().unwrap().take();
    }
}
impl Drop for MatrixHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        self.owner.user_id.lock().unwrap().take();
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl MatrixChannel {
    pub fn new(mut options: MatrixOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        mut options: MatrixOptions,
        gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        if options.homeserver.ends_with('/') {
            options.homeserver.pop();
        }
        Arc::new(Self {
            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            user_id: Default::default(),
            running: AtomicBool::new(false),
            shutdown: Default::default(),
        })
    }
    fn persist_mapper(&self) {
        ChannelSessionState::persist_channel_mapper(
            &self.mapper.lock().unwrap(),
            &self.on_state_change,
        );
    }
    fn chat(&self, room: &str) -> Arc<Mutex<Chat>> {
        self.chats
            .lock()
            .unwrap()
            .entry(room.into())
            .or_default()
            .clone()
    }
    pub async fn start(self: &Arc<Self>) -> Result<MatrixHandle> {
        if self.options.homeserver.is_empty() {
            return Err(failure("homeserver not set (MATRIX_HOMESERVER)"));
        }
        if self.options.access_token.is_empty() {
            return Err(failure("access token not set (MATRIX_ACCESS_TOKEN)"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("Matrix channel already started"));
        }
        let stored = match self.load_storage() {
            Ok(value) => value,
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        // getUserId failure uses the configured identity, as TS:80-84 does.
        let user = self
            .api
            .json(&self.options, "GET", "/account/whoami", None)
            .await
            .ok()
            .and_then(|v| v["user_id"].as_str().map(str::to_owned))
            .or_else(|| self.options.user_id.clone());
        *self.user_id.lock().unwrap() = user;
        let (shutdown, mut stopped) = watch::channel(false);
        *self.shutdown.lock().unwrap() = Some(stopped.clone());
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let mut storage = stored;
            let mut since = storage["syncToken"].as_str().map(str::to_owned);
            let mut messages = JoinSet::new();
            loop {
                let received = tokio::select! {
                    _ = stopped.changed() => break,
                    response = owner.api.sync(&owner.options, since.as_deref()) => response,
                };
                let response = match received {
                    Ok(response) => response,
                    Err(_) => {
                        tokio::select! { _ = stopped.changed() => break, _ = tokio::time::sleep(Duration::from_secs(3)) => {} }
                        continue;
                    }
                };
                if let Some(invites) = response["rooms"]["invite"].as_object() {
                    for room in invites.keys() {
                        let room = room.clone();
                        let owner = owner.clone();
                        messages.spawn(async move {
                            let path = format!("/join/{}", http::encode_component(&room));
                            let _ = owner
                                .api
                                .json(&owner.options, "POST", &path, Some(json!({})))
                                .await;
                        });
                    }
                }
                if let Some(rooms) = response["rooms"]["join"].as_object() {
                    for (room, data) in rooms {
                        for event in data["timeline"]["events"].as_array().into_iter().flatten() {
                            if event["type"] != "m.room.message" {
                                continue;
                            }
                            let owner = owner.clone();
                            let room = room.clone();
                            let event = event.clone();
                            let mut message = Box::pin(async move {
                                let _ = owner.message(&room, event).await;
                            });
                            // JS invokes each callback through its first await
                            // before emitting the next event. Poll once here so
                            // the active-chat guard has the same ordering.
                            let completed = std::future::poll_fn(|cx| {
                                std::task::Poll::Ready(message.as_mut().poll(cx).is_ready())
                            })
                            .await;
                            if !completed {
                                messages.spawn(message);
                            }
                        }
                    }
                }
                if let Some(token) = response["next_batch"].as_str() {
                    since = Some(token.into());
                    storage["syncToken"] = token.into();
                    let _ = owner.save_storage(&storage);
                }
                while messages.try_join_next().is_some() {}
            }
            messages.abort_all();
            while messages.join_next().await.is_some() {}
        });
        Ok(MatrixHandle {
            shutdown,
            owner: self.clone(),
            task: StdMutex::new(Some(task)),
        })
    }
    fn load_storage(&self) -> Result<Value> {
        match std::fs::read(&self.options.storage_path) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
                .map_err(|_| failure("Invalid Matrix storage JSON"))
                .and_then(|value| {
                    if value.is_object() {
                        Ok(value)
                    } else {
                        Err(failure("Invalid Matrix storage object"))
                    }
                }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
            Err(_) => Err(failure("Cannot read Matrix sync storage")),
        }
    }
    fn save_storage(&self, value: &Value) -> Result<()> {
        use std::io::Write;
        let temporary = self
            .options
            .storage_path
            .with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temporary)?;
            file.write_all(value.to_string().as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temporary, &self.options.storage_path)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        }
        result.map_err(|_| failure("Cannot save Matrix sync token"))
    }
    async fn message(self: &Arc<Self>, room: &str, raw: Value) -> Result<()> {
        let Some(sender) = raw["sender"].as_str().filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        if self.user_id.lock().unwrap().as_deref() == Some(sender) {
            return Ok(());
        }
        let content = &raw["content"];
        if content["m.relates_to"]["rel_type"] == "m.replace" {
            return Ok(());
        }
        let kind = content["msgtype"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("m.text");
        if kind != "m.text" {
            return Ok(());
        }
        let text = match content.get("body").filter(|v| !v.is_null()) {
            Some(Value::String(text)) => text.clone(),
            Some(value) => pilotdeck_protocol::parse_json(&value.to_string())
                .ok()
                .and_then(|value| pilotdeck_protocol::js_value::display(&value).ok())
                .unwrap_or_default(),
            None => String::new(),
        };
        let text = crate::metadata::js_trim(&text);
        if text.is_empty() {
            return Ok(());
        }
        if self.answer(room, text).await? {
            return Ok(());
        }
        let shared = self.chat(room);
        let mut state = shared.lock().await;
        if state.is_active() {
            return Ok(());
        }
        let (session, message, new) = {
            let mut mapper = self.mapper.lock().unwrap();
            let new = text == "/new" || text.starts_with("/new ");
            if new {
                mapper.reset("matrix", room);
            }
            (
                mapper.session("matrix", room),
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
            self.send_reply(room, "已创建新会话。").await;
            return Ok(());
        }
        let flag = Arc::new(AtomicBool::new(true));
        state.active = Some(flag.clone());
        drop(state);
        let _active = Active(flag);
        let sink = Sink {
            owner: self.clone(),
            room: room.into(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let result = self
            .gateway
            .stream_turn(
                json!({"sessionKey":session,"channelKey":"matrix","message":message}),
                &sink,
            )
            .await;
        self.chat(room).lock().await.finish_turn();
        let text = if result.is_err() {
            "处理消息时发生错误，请重试。".into()
        } else {
            sink.text.lock().unwrap().clone()
        };
        let text = crate::metadata::js_trim(&text);
        if !text.is_empty() {
            self.send_reply(room, text).await;
        }
        Ok(())
    }
    async fn send_reply(&self, room: &str, text: &str) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        let Some(mut stopped) = self.shutdown.lock().unwrap().clone() else {
            return false;
        };
        if *stopped.borrow() {
            return false;
        }
        let mut delivered = true;
        for chunk in chunk_text(text, 4000) {
            let result = tokio::select! {
                _ = stopped.changed() => return false,
                result = self.api.send(&self.options, room, chunk) => result,
            };
            delivered &= result.is_ok();
        }
        delivered
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "matrix") {
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
        let Some((room, suffix)) = session
            .strip_prefix("matrix:chat=")
            .and_then(|s| s.rsplit_once(':'))
        else {
            return Ok(false);
        };
        if room.is_empty()
            || room.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
            || !(suffix == "general"
                || suffix.strip_prefix("s_").is_some_and(|s| {
                    s.len() == 36 && s.bytes().all(|c| c.is_ascii_hexdigit() || c == b'-')
                }))
        {
            return Ok(false);
        }
        Ok(self
            .send_reply(room, delivery["text"].as_str().unwrap_or(""))
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
    owner: Arc<MatrixChannel>,
    room: String,
    session: String,
    text: StdMutex<String>,
}
impl ChannelEventSink for Sink {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            match event["type"].as_str() {
                Some("permission_request") => {
                    self.owner
                        .capture_permission(&self.room, &self.session, event)
                        .await
                }
                Some("elicitation_request") => {
                    self.owner.chat(&self.room).lock().await.elicitation =
                        Some((self.session.clone(), event.clone()));
                    self.owner
                        .send_reply(&self.room, &interaction::elicitation_prompt(&event))
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
    RouteError::new("MATRIX_CHANNEL_FAILED", message)
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
