//! Telegram channel owner (`TelegramChannel.ts:45-230`).
//! Options are supplied by the application; this module never opens user home
//! files or reads environment variables. `base_url` permits loopback fixtures.
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
#[cfg(test)]
mod tests;
use http::Api;
use interaction::Chat;

#[derive(Clone, Debug)]
pub struct TelegramOptions {
    /// TS: options.token ?? TELEGRAM_BOT_TOKEN. Resolve the environment in the
    /// application, preserving an explicitly empty token.
    pub token: String,
    pub webhook_url: Option<String>,
    pub base_url: String,
}
impl Default for TelegramOptions {
    fn default() -> Self {
        Self {
            token: String::new(),
            webhook_url: None,
            base_url: "https://api.telegram.org/".into(),
        }
    }
}
pub struct TelegramChannel {
    options: TelegramOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    running: AtomicBool,
    shutdown: StdMutex<Option<watch::Receiver<bool>>>,
    typing: StdMutex<JoinSet<()>>,
}
pub struct TelegramHandle {
    shutdown: watch::Sender<bool>,
    owner: Arc<TelegramChannel>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl TelegramHandle {
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
    }
}
impl Drop for TelegramHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        self.owner.typing.lock().unwrap().abort_all();
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl TelegramChannel {
    pub fn new(options: TelegramOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        options: TelegramOptions,
        gateway: Arc<dyn ChannelGateway>,
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
            running: AtomicBool::new(false),
            typing: Default::default(),
            shutdown: Default::default(),
        })
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
    async fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.api.call(&self.options, method, params).await
    }
    pub async fn start(self: &Arc<Self>) -> Result<TelegramHandle> {
        if self.options.token.is_empty() {
            return Err(failure("TELEGRAM_BOT_TOKEN not set"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("Telegram channel already started"));
        }
        let initialized = async {
            if let Some(url) = self
                .options
                .webhook_url
                .as_deref()
                .filter(|s| !s.is_empty())
            {
                self.call("setWebhook", json!({"url":url})).await?;
            } else {
                self.call("deleteWebhook", json!({})).await?;
            }
            self.call("getMe", json!({})).await?;
            Ok::<_, RouteError>(())
        }
        .await;
        if let Err(error) = initialized {
            self.running.store(false, Ordering::SeqCst);
            return Err(error);
        }
        let (shutdown, mut stopped) = watch::channel(false);
        *self.shutdown.lock().unwrap() = Some(stopped.clone());
        let owner = self.clone();
        let webhook = self
            .options
            .webhook_url
            .as_deref()
            .is_some_and(|s| !s.is_empty());
        let task = tokio::spawn(async move {
            if webhook {
                let _ = stopped.changed().await;
                return;
            }
            let mut offset = None;
            let mut messages = JoinSet::new();
            loop {
                let mut params = json!({"timeout":30});
                if let Some(offset) = offset {
                    params["offset"] = json!(offset);
                }
                let response = tokio::select! {_ = stopped.changed()=>break, result=owner.call("getUpdates",params)=>result};
                match response {
                    Ok(response) => {
                        for update in response.as_array().into_iter().flatten() {
                            if let Some(id) = update["update_id"].as_i64() {
                                offset = Some(offset.map_or(id + 1, |old: i64| old.max(id + 1)));
                            }
                            let owner = owner.clone();
                            let update = update.clone();
                            messages.spawn(async move {
                                let _ = owner.message(update).await;
                            });
                            tokio::task::yield_now().await;
                        }
                    }
                    Err(_) => {
                        tokio::select! {_ = stopped.changed()=>break, _=tokio::time::sleep(Duration::from_secs(1))=>{}}
                    }
                }
                while messages.try_join_next().is_some() {}
            }
            messages.abort_all();
            while messages.join_next().await.is_some() {}
        });
        Ok(TelegramHandle {
            shutdown,
            owner: self.clone(),
            task: StdMutex::new(Some(task)),
        })
    }
    /// Webhook hosting belongs to the embedding application, as in TS. A
    /// Telegram webhook receiver can pass its already parsed update here.
    pub async fn handle_update(self: &Arc<Self>, update: Value) -> Result<()> {
        if !self.running.load(Ordering::SeqCst) {
            return Err(failure("Telegram channel stopped"));
        }
        let mut stopped = self
            .shutdown
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| failure("Telegram channel stopped"))?;
        if *stopped.borrow() {
            return Err(failure("Telegram channel stopped"));
        }
        tokio::select! {_ = stopped.changed()=>Ok(()), result=self.message(update)=>result}
    }
    async fn message(self: &Arc<Self>, update: Value) -> Result<()> {
        let Some(message) = update.get("message") else {
            return Ok(());
        };
        let Some(text) = message["text"].as_str().filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        let chat_id = match &message["chat"]["id"] {
            Value::String(value) => value.clone(),
            Value::Number(value) => value.to_string(),
            _ => return Ok(()),
        };
        let chat = self.chat(&chat_id);
        if self.answer(&chat_id, text).await? {
            return Ok(());
        }
        let mut state = chat.lock().await;
        if state.is_active() {
            return Ok(());
        }
        let input = crate::metadata::js_trim(text);
        let (session, message, new) = {
            let mut mapper = self.mapper.lock().unwrap();
            let new = input == "/new" || input.starts_with("/new ");
            if new {
                mapper.reset("telegram", &chat_id);
            }
            (
                mapper.session("telegram", &chat_id),
                if new {
                    crate::metadata::js_trim(&input[4..]).to_owned()
                } else {
                    input.to_owned()
                },
                new,
            )
        };
        if new {
            self.persist_mapper();
        }
        if new && message.is_empty() {
            drop(state);
            self.send_reply(&chat_id, "已创建新会话。").await;
            return Ok(());
        }
        if message.is_empty() {
            return Ok(());
        }
        let active = Arc::new(AtomicBool::new(true));
        state.active_flag = Some(active.clone());
        drop(state);
        let _active = Active { flag: active };
        {
            let owner = self.clone();
            let chat = chat_id.clone();
            let mut stopped = self
                .shutdown
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| failure("Telegram channel stopped"))?;
            let mut typing = self.typing.lock().unwrap();
            while typing.try_join_next().is_some() {}
            typing.spawn(async move {
                if *stopped.borrow() { return; }
                tokio::select! { _ = stopped.changed() => {}, _ = owner.call("sendChatAction", json!({"chat_id":chat,"action":"typing"})) => {} }
            });
        }
        let sink = Sink {
            owner: self.clone(),
            chat: chat_id.clone(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let turn = self.gateway.stream_turn(
            json!({"sessionKey":session,"channelKey":"telegram","message":message}),
            &sink,
        );
        let result = turn.await;
        chat.lock().await.finish_turn();
        let text = if result.is_err() {
            "处理消息时发生错误，请重试。".into()
        } else {
            sink.text.lock().unwrap().clone()
        };
        let text = crate::metadata::js_trim(&text);
        if !text.is_empty() {
            self.send_reply(&chat_id, text).await;
        }
        Ok(())
    }
    async fn send_reply(&self, chat: &str, text: &str) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        let mut success = true;
        for chunk in chunk_text(text, 4096) {
            if self
                .api
                .send_message(&self.options, chat, chunk)
                .await
                .is_err()
            {
                success = false;
            }
        }
        success
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "telegram") {
            return Ok(false);
        }
        let Some(session) = delivery["originSessionKey"]
            .as_str()
            .or_else(|| delivery["sessionKey"].as_str())
        else {
            return Ok(false);
        };
        let Some((chat, suffix)) = session
            .strip_prefix("telegram:chat=")
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
struct Active {
    flag: Arc<AtomicBool>,
}
impl Drop for Active {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}
struct Sink {
    owner: Arc<TelegramChannel>,
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
    RouteError::new("TELEGRAM_CHANNEL_FAILED", message)
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
