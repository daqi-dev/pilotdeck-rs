//! BlueBubbles HTTP polling owner (`BlueBubblesChannel.ts:56-269`).
//! All configuration is explicit; this owner never reads user home or env files.
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
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
#[cfg(test)]
mod tests;
use http::Api;
use interaction::Chat;

#[derive(Clone, Debug, Default)]
pub struct BlueBubblesOptions {
    pub server_url: String,
    pub password: String,
}
#[derive(Default)]
struct Cursor {
    timestamp: f64,
    seen: HashSet<String>,
}
pub struct BlueBubblesChannel {
    options: BlueBubblesOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    cursor: StdMutex<Cursor>,
    running: AtomicBool,
}
pub struct BlueBubblesHandle {
    shutdown: watch::Sender<bool>,
    owner: Arc<BlueBubblesChannel>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl BlueBubblesHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}
impl Drop for BlueBubblesHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl BlueBubblesChannel {
    pub fn new(mut options: BlueBubblesOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        mut options: BlueBubblesOptions, gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        options.server_url = options.server_url.trim_end_matches('/').into();
        Arc::new(Self {

            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            cursor: Default::default(),
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
    pub async fn start(self: &Arc<Self>) -> Result<BlueBubblesHandle> {
        if self.options.server_url.is_empty() || self.options.password.is_empty() {
            return Err(failure("BlueBubbles serverUrl and password are required"));
        }
        self.api.validate(&self.options)?;
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("BlueBubbles channel already started"));
        }
        *self.cursor.lock().unwrap() = Cursor {
            timestamp: (now_seconds().floor() - 5.0),
            seen: HashSet::new(),
        };
        let (shutdown, mut stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(2500));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut polls = JoinSet::new();
            loop {
                tokio::select! {_ = stopped.changed()=>break,_=interval.tick()=>{let owner=owner.clone();polls.spawn(async move{let _=owner.poll().await;});},_=polls.join_next(),if !polls.is_empty()=>{}}
            }
            polls.abort_all();
            while polls.join_next().await.is_some() {}
        });
        Ok(BlueBubblesHandle {
            shutdown,
            owner: self.clone(),
            task: StdMutex::new(Some(task)),
        })
    }
    async fn poll(self: &Arc<Self>) -> Result<()> {
        if !self.running.load(Ordering::SeqCst) {
            return Ok(());
        }
        let start = self.cursor.lock().unwrap().timestamp;
        let data = self.api.poll(&self.options, start).await?;
        let rows = data
            .as_array()
            .or_else(|| data["data"].as_array())
            .or_else(|| data["messages"].as_array());
        let mut maximum = start;
        for row in rows.into_iter().flatten() {
            if !row.is_object() && !row.is_array() {
                continue;
            }
            let guid = js_string(nullish(row, &["guid", "id"]));
            if !guid.is_empty() {
                let mut cursor = self.cursor.lock().unwrap();
                if !cursor.seen.insert(guid) {
                    continue;
                }
            }
            if let Some(timestamp) = number(nullish(row, &["dateCreated", "timestamp", "time"])) {
                maximum = maximum.max(timestamp);
            }
            self.dispatch(row).await?;
        }
        let mut cursor = self.cursor.lock().unwrap();
        if maximum > cursor.timestamp {
            cursor.timestamp = maximum;
        }
        Ok(())
    }
    async fn dispatch(self: &Arc<Self>, row: &Value) -> Result<()> {
        if truthy(nullish(row, &["isFromMe", "is_from_me"])) {
            return Ok(());
        }
        let text = js_string(nullish(row, &["text", "body", "message"]));
        let text = crate::metadata::js_trim(&text);
        let chat = if let Some(chat) = ["chatGuid", "chat_guid"]
            .into_iter()
            .find_map(|key| row.get(key).filter(|value| !value.is_null()))
        {
            js_string(Some(chat))
        } else {
            js_string(row["chats"].as_array().and_then(|chats| chats.first()))
        };
        if chat.is_empty() || text.is_empty() {
            return Ok(());
        }
        if self.answer(&chat, text).await? {
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
                mapper.reset("bluebubbles", &chat);
            }
            (
                mapper.session("bluebubbles", &chat),
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
            self.send_reply(&chat, "已创建新会话。").await;
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
            chat: chat.clone(),
            session: session.clone(),
            text: StdMutex::new(String::new()),
        };
        let result = self
            .gateway
            .stream_turn(
                json!({"sessionKey":session,"channelKey":"bluebubbles","message":message}),
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
            self.send_reply(&chat, text).await;
        }
        Ok(())
    }
    async fn send_reply(&self, chat: &str, text: &str) -> bool {
        if !self.running.load(Ordering::SeqCst) {
            return false;
        }
        self.api.send(&self.options,json!({"chatGuid":chat,"message":text,"tempGuid":uuid::Uuid::new_v4().to_string(),"password":self.options.password})).await.is_ok()
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::{origin_channel_matches, parse_chat_id, session_field};
        if !origin_channel_matches(delivery, "bluebubbles") {
            return Ok(false);
        }
        let Some(session) = session_field(delivery) else {
            return Ok(false);
        };
        let Some(chat) = parse_chat_id(session, "bluebubbles") else {
            return Ok(false);
        };
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
    owner: Arc<BlueBubblesChannel>,
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
fn now_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}
fn failure(message: &str) -> RouteError {
    RouteError::new("BLUEBUBBLES_CHANNEL_FAILED", message)
}
fn nullish<'a>(row: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    keys.iter()
        .find_map(|key| row.get(*key).filter(|value| !value.is_null()))
}
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(v)) => *v,
        Some(Value::Number(v)) => v.as_f64() != Some(0.0),
        Some(Value::String(v)) => !v.is_empty(),
        _ => true,
    }
}
fn js_string(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(v)) => v.clone(),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| js_string(Some(value)))
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".into(),
        Some(value) => value.to_string(),
    }
}
fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(text) if !crate::metadata::js_trim(text).is_empty() => {
            let text = crate::metadata::js_trim(text);
            let radix = |digits: &str, base: u32| {
                if digits.is_empty() {
                    return None;
                }
                digits.chars().try_fold(0.0, |value, digit| {
                    Some(value * f64::from(base) + f64::from(digit.to_digit(base)?))
                })
            };
            let parsed = if let Some(hex) =
                text.strip_prefix("0x").or_else(|| text.strip_prefix("0X"))
            {
                radix(hex, 16)
            } else if let Some(binary) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B"))
            {
                radix(binary, 2)
            } else if let Some(octal) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O"))
            {
                radix(octal, 8)
            } else {
                text.parse().ok()
            };
            parsed.filter(|n| n.is_finite())
        }
        _ => None,
    }
}
