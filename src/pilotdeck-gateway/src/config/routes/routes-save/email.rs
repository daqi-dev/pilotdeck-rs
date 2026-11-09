//! IMAP/SMTP channel matching EmailChannel.ts:25-356. Native TLS, authenticated
//! mail protocols and owned polling replace optional imapflow/nodemailer.
use super::home_assistant::{
    interaction::{Chat, ImChannel, elicitation_prompt},
    render_event,
};
use super::{ChannelCall, Result, RouteError};
use crate::config::channel_commands::{ChannelEventSink, ChannelGateway, ChannelSessionState};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
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
mod imap;
mod smtp;
#[cfg(test)]
mod tests;
mod wire;
#[derive(Clone, Debug)]
pub struct EmailOptions {
    pub address: String,
    pub password: String,
    pub imap_host: String,
    pub smtp_host: String,
    pub imap_port: u16,
    pub smtp_port: u16,
    pub poll_interval_ms: u64,
    pub imap_tls: bool,
    pub smtp_tls: bool,
    pub default_subject: String,
}
impl Default for EmailOptions {
    fn default() -> Self {
        Self {
            address: String::new(),
            password: String::new(),
            imap_host: String::new(),
            smtp_host: String::new(),
            imap_port: 993,
            smtp_port: 587,
            poll_interval_ms: 45_000,
            imap_tls: true,
            smtp_tls: true,
            default_subject: "Message".into(),
        }
    }
}
pub struct EmailChannel {
    options: EmailOptions,
    gateway: Arc<dyn ChannelGateway>,
    mapper: StdMutex<ChannelSessionState>,
    on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    chats: StdMutex<HashMap<String, Arc<Mutex<Chat>>>>,
    seen: StdMutex<HashSet<u64>>,
    running: AtomicBool,
}
pub struct EmailHandle {
    owner: Arc<EmailChannel>,
    shutdown: watch::Sender<bool>,
    task: StdMutex<Option<JoinHandle<()>>>,
}
impl EmailHandle {
    pub async fn close(&self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let task = self.task.lock().unwrap().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        self.owner.chats.lock().unwrap().clear();
    }
}
impl Drop for EmailHandle {
    fn drop(&mut self) {
        self.owner.running.store(false, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        if let Some(task) = self.task.lock().unwrap().take() {
            task.abort();
        }
    }
}
impl EmailChannel {
    pub fn new(options: EmailOptions, gateway: Arc<dyn ChannelGateway>) -> Arc<Self> {
        Self::with_mapper(options, gateway, ChannelSessionState::default(), None)
    }
    pub fn with_mapper(
        options: EmailOptions, gateway: Arc<dyn ChannelGateway>,
        mapper: ChannelSessionState,
        on_state_change: Option<Arc<dyn Fn(Value) + Send + Sync>>,
    ) -> Arc<Self> {
        Arc::new(Self {

            options,
            gateway,
            mapper: StdMutex::new(mapper),
            on_state_change,
            chats: Default::default(),
            seen: Default::default(),
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
    pub async fn start(self: &Arc<Self>) -> Result<EmailHandle> {
        if self.options.address.is_empty()
            || self.options.password.is_empty()
            || self.options.imap_host.is_empty()
            || self.options.smtp_host.is_empty()
        {
            return Err(failure("Email address/password/IMAP/SMTP hosts required"));
        }
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(failure("Email channel already started"));
        }
        let prepare = async {
            let mut imap = imap::Imap::connect(&self.options).await?;
            if let Ok(seen) = imap.prime().await {
                self.seen.lock().unwrap().extend(seen);
            }
            let mut smtp = match smtp::Smtp::connect(&self.options).await {
                Ok(smtp) => smtp,
                Err(error) => {
                    imap.logout().await;
                    return Err(error);
                }
            };
            smtp.quit().await;
            Ok::<_, RouteError>(imap)
        }
        .await;
        let mut imap = match prepare {
            Ok(imap) => imap,
            Err(error) => {
                self.running.store(false, Ordering::SeqCst);
                return Err(error);
            }
        };
        let mut messages = JoinSet::new();
        self.poll(&mut imap, &mut messages).await;
        let (shutdown, mut stopped) = watch::channel(false);
        let owner = self.clone();
        let task = tokio::spawn(async move {
            let mut interval =
                tokio::time::interval(Duration::from_millis(owner.options.poll_interval_ms.max(1)));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            interval.tick().await;
            loop {
                tokio::select! {_=stopped.changed()=>break,_=interval.tick()=>{tokio::select!{_=stopped.changed()=>break,_=owner.poll(&mut imap,&mut messages)=>{}}}}
                while messages.try_join_next().is_some() {}
            }
            messages.abort_all();
            while messages.join_next().await.is_some() {}
            let _ = tokio::time::timeout(Duration::from_secs(1), imap.logout()).await;
        });
        Ok(EmailHandle {
            owner: self.clone(),
            shutdown,
            task: StdMutex::new(Some(task)),
        })
    }
    async fn poll(self: &Arc<Self>, imap: &mut imap::Imap, messages: &mut JoinSet<()>) {
        let Ok(incoming) = imap.unseen().await else {
            return;
        };
        for incoming in incoming {
            if !self.seen.lock().unwrap().insert(incoming.uid) {
                continue;
            }
            let text = extract_plain_text(&incoming.source);
            if crate::metadata::js_trim(&text).is_empty() {
                continue;
            }
            let owner = self.clone();
            messages.spawn(async move {
                let _ = owner.incoming(&incoming.from, &text).await;
            });
        }
    }
    async fn incoming(self: &Arc<Self>, chat: &str, text: &str) -> Result<()> {
        if chat.is_empty() || chat == "unknown" {
            return Ok(());
        }
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
                mapper.reset("email", chat);
            }
            (
                mapper.session("email", chat),
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
                json!({"sessionKey":session,"channelKey":"email","message":message}),
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
        self.running.load(Ordering::SeqCst)
            && smtp::Smtp::send(&self.options, chat, text).await.is_ok()
    }
    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use super::im_cron::origin_channel_matches;
        if !origin_channel_matches(delivery, "email") {
            return Ok(false);
        }
        let session = delivery
            .get("originSessionKey")
            .filter(|v| !v.is_null())
            .unwrap_or(&delivery["sessionKey"]);
        let Some(chat) = session
            .as_str()
            .and_then(|session| session.strip_prefix("email:chat="))
            .and_then(|suffix| suffix.rsplit_once(':'))
            .filter(|(chat, suffix)| {
                !chat.is_empty()
                    && !chat.contains(['\n', '\r', '\u{2028}', '\u{2029}'])
                    && (*suffix == "general"
                        || suffix.strip_prefix("s_").is_some_and(|id| {
                            id.len() == 36 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
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
impl ImChannel for EmailChannel {
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
    owner: Arc<EmailChannel>,
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
fn extract_plain_text(raw: &str) -> String {
    if !raw.contains("Content-Type:") {
        return crate::metadata::js_trim(raw).into();
    }
    fn fallback(raw: &str) -> String {
        let raw = String::from_utf16_lossy(&raw.encode_utf16().take(8000).collect::<Vec<_>>());
        crate::metadata::js_trim(&raw).into()
    }
    let re = regex::Regex::new(r"(?i)Content-Type:\s*text/plain[^\r\n]*[\r\n]+").unwrap();
    if let Some(found) = re.find(raw) {
        let rest = &raw[found.end()..];
        let stop = regex::Regex::new(r"(?i)--[a-f0-9]{8,}|Content-Type:")
            .unwrap()
            .find(rest)
            .map_or(rest.len(), |found| found.start());
        if stop == 0 {
            return fallback(raw);
        }
        let mut body = rest[..stop].to_owned();
        if body.starts_with("\r\n") {
            body = body[2..].into();
        } else if body.starts_with('\n') {
            body = body[1..].into();
        }
        if let Some(found) =
            regex::Regex::new(r"(?i)^Content-Transfer-Encoding:\s*quoted-printable\r?\n")
                .unwrap()
                .find(&body)
        {
            let body = body[found.end()..].replace("=\r\n", "").replace("=\n", "");
            let mut decoded = String::new();
            let mut chars = body.chars().peekable();
            while let Some(ch) = chars.next() {
                if ch == '=' {
                    let mut copy = chars.clone();
                    if let (Some(a), Some(b)) = (copy.next(), copy.next()) {
                        if a.is_ascii_digit() || ('A'..='F').contains(&a) {
                            if b.is_ascii_digit() || ('A'..='F').contains(&b) {
                                let mut pair = String::new();
                                pair.push(a);
                                pair.push(b);
                                decoded.push(char::from(u8::from_str_radix(&pair, 16).unwrap()));
                                chars = copy;
                                continue;
                            }
                        }
                    }
                }
                decoded.push(ch);
            }
            return crate::metadata::js_trim(&decoded).into();
        }
        return crate::metadata::js_trim(&body).into();
    }
    fallback(raw)
}
fn failure(message: &str) -> RouteError {
    RouteError::new("EMAIL_CHANNEL_FAILED", message)
}
