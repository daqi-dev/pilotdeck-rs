//! Signal REST/SSE channel (`SignalChannel.ts:86-325`). Options are supplied by
//! the embedding application; no user directory or environment is read here.
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use http_body_util::BodyExt;
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
pub struct SignalOptions {
    pub rest_url: String,
    pub account: String,
}
impl Default for SignalOptions {
    fn default() -> Self {
        Self {
            rest_url: "http://127.0.0.1:8080".into(),
            account: String::new(),
        }
    }
}
pub struct SignalChannel {
    options: SignalOptions,
    api: Api,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    recipients: StdMutex<HashMap<String, String>>,
    running: AtomicBool,
}
pub struct SignalHandle {
    shutdown: watch::Sender<bool>,
    owner: Arc<SignalChannel>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl SignalHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}
impl Drop for SignalHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl SignalChannel {
    pub fn new(mut options: SignalOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        mut options: SignalOptions, gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        options.rest_url = options.rest_url.trim_end_matches('/').into();
        Arc::new(Self {

            options,
            api: Api::new(),
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            recipients: Default::default(),
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
    pub async fn start(self: &Arc<Self>) -> Result<SignalHandle> {
        if self.options.account.is_empty() {
            return Err(failure("SIGNAL_ACCOUNT / account not set"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("Signal channel already started"));
        }
        let (shutdown, mut stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let mut carry = Vec::<u8>::new();
            let mut messages = JoinSet::new();
            loop {
                let received = tokio::select! {_ = stopped.changed()=>break,response=owner.api.receive(&owner.options)=>response};
                let mut body = match received {
                    Ok(body) => body,
                    Err(_) => {
                        tokio::select! {_ = stopped.changed()=>break,_=tokio::time::sleep(Duration::from_secs(3))=>{}}
                        continue;
                    }
                };
                let mut failed = false;
                loop {
                    let frame =
                        tokio::select! {_ = stopped.changed()=>break,frame=body.frame()=>frame};
                    let Some(frame) = frame else { break };
                    let Ok(frame) = frame else {
                        failed = true;
                        break;
                    };
                    if let Some(bytes) = frame.data_ref() {
                        carry.extend_from_slice(bytes);
                        while let Some(newline) = carry.iter().position(|byte| *byte == b'\n') {
                            let mut line = carry.drain(..=newline).collect::<Vec<_>>();
                            line.pop();
                            if line.last() == Some(&b'\r') {
                                line.pop();
                            }
                            let line = String::from_utf8_lossy(&line).into_owned();
                            let owner = owner.clone();
                            messages.spawn(async move {
                                let _ = owner.parse_line(&line).await;
                            });
                            tokio::task::yield_now().await;
                        }
                    }
                    while messages.try_join_next().is_some() {}
                }
                if *stopped.borrow() {
                    break;
                }
                if failed {
                    tokio::select! {_ = stopped.changed()=>break,_=tokio::time::sleep(Duration::from_secs(3))=>{}}
                }
            }
            messages.abort_all();
            while messages.join_next().await.is_some() {}
        });
        Ok(SignalHandle {
            shutdown,
            owner: self.clone(),
            task: StdMutex::new(Some(task)),
        })
    }
    async fn parse_line(self: &Arc<Self>, line: &str) -> Result<()> {
        let line = crate::metadata::js_trim(line);
        let payload = crate::metadata::js_trim(line.strip_prefix("data:").unwrap_or(line));
        if payload.is_empty() || matches!(payload, "[DONE]" | ":ok") {
            return Ok(());
        }
        let Ok(raw) = serde_json::from_str::<Value>(payload) else {
            return Ok(());
        };
        let Some(envelope) = raw.get("envelope").filter(|v| !v.is_null()) else {
            return Ok(());
        };
        let sent = &envelope["syncMessage"]["sentMessage"];
        let data = &envelope["dataMessage"];
        let text = sent["message"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| data["message"].as_str().filter(|s| !s.is_empty()))
            .unwrap_or("");
        if crate::metadata::js_trim(text).is_empty() {
            return Ok(());
        }
        let source = envelope["source"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| envelope["sourceNumber"].as_str().filter(|s| !s.is_empty()));
        let group = data["groupInfo"]
            .get("groupId")
            .filter(|v| !v.is_null())
            .or_else(|| sent["groupInfo"].get("groupId"));
        let chat = if let Some(group) = group.and_then(Value::as_str) {
            format!("group:{group}")
        } else if let Some(source) = source {
            format!("dm:{source}")
        } else {
            self.options.account.clone()
        };
        let recipient = source
            .map(str::to_owned)
            .unwrap_or_else(|| strip_chat_prefix(&chat).to_owned());
        if !recipient.is_empty() {
            self.recipients
                .lock()
                .unwrap()
                .insert(chat.clone(), recipient);
        }
        if self.answer(&chat, text).await? {
            return Ok(());
        }
        let shared = self.chat(&chat);
        let mut state = shared.lock().await;
        if state.is_active() {
            return Ok(());
        }
        let input = crate::metadata::js_trim(text);
        let (session, message, new) = {
            let mut mapper = self.mapper.lock().unwrap();
            let new = input == "/new" || input.starts_with("/new ");
            if new {
                mapper.reset("signal", &chat);
            }
            (
                mapper.session("signal", &chat),
                if new {
                    crate::metadata::js_trim(&input[4..]).to_owned()
                } else {
                    input.into()
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
                json!({"sessionKey":session,"channelKey":"signal","message":message}),
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
        let recipient = self
            .recipients
            .lock()
            .unwrap()
            .get(chat)
            .cloned()
            .unwrap_or_else(|| strip_chat_prefix(chat).into());
        if recipient.is_empty() {
            return false;
        }
        let mut delivered = true;
        for chunk in chunk_text(text, 2000) {
            let mut body = pilotdeck_protocol::JsObject::new();
            body.insert("message".into(), pilotdeck_protocol::JsValue::String(chunk));
            body.insert("number".into(), self.options.account.as_str().into());
            body.insert(
                "recipients".into(),
                pilotdeck_protocol::JsValue::Array(vec![recipient.as_str().into()]),
            );
            let body =
                pilotdeck_protocol::stringify_json(&pilotdeck_protocol::JsValue::Object(body))
                    .unwrap()
                    .unwrap();
            if self.api.send(&self.options, body).await.is_err() {
                delivered = false;
            }
        }
        delivered
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "signal") {
            return Ok(false);
        }
        let Some(session) = delivery["originSessionKey"]
            .as_str()
            .or_else(|| delivery["sessionKey"].as_str())
        else {
            return Ok(false);
        };
        let Some((chat, suffix)) = session
            .strip_prefix("signal:chat=")
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
    owner: Arc<SignalChannel>,
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
    RouteError::new("SIGNAL_CHANNEL_FAILED", message)
}
fn strip_chat_prefix(chat: &str) -> &str {
    chat.strip_prefix("dm:")
        .or_else(|| chat.strip_prefix("group:"))
        .unwrap_or(chat)
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
