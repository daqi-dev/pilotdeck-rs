//! Email IMAP/SMTP consumer using the existing A7 and IM interaction ports.
pub mod mapper;
pub mod render;
#[cfg(test)]
mod tests;
pub mod transport;
use super::{
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    im_interaction::InteractionPort,
    protocol::*,
};
pub use mapper::{EmailSessionMapper, EmailSessionMapperState};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, watch};
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn shown(s: &JsString) -> String {
    String::from_utf16_lossy(s.units())
}
fn is(v: &JsValue, k: &str, s: &str) -> bool {
    v.get(k)
        .and_then(JsValue::as_string)
        .is_some_and(|v| v.equals_str(s))
}
#[derive(Default)]
pub struct EmailOptions {
    pub extra: JsObject,
    pub mapper: EmailSessionMapper,
    pub env: Option<super::sms::EnvLookup>,
}
impl EmailOptions {
    pub fn from_object(o: &JsObject) -> Self {
        Self {
            extra: o
                .get("extra")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default(),
            ..Default::default()
        }
    }
}
struct Shared {
    config: transport::EmailConfig,
    mapper: Mutex<EmailSessionMapper>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    imap: tokio::sync::Mutex<Option<transport::Imap>>,
    smtp: Mutex<Option<Arc<transport::Smtp>>>,
    seen: Mutex<HashSet<u32>>,
    closed: AtomicBool,
    active_chats: Arc<Mutex<HashSet<JsString>>>,
    active_tasks: AtomicUsize,
    idle: Notify,
}
impl Shared {
    fn log(&self, level: &str, text: String) {
        let l = lock(&self.deps).log.clone();
        if let Some(l) = l {
            l(level, &text)
        }
    }
    fn finish_task(&self) {
        if self.active_tasks.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.idle.notify_waiters();
        }
    }
    async fn send_reply(&self, chat: &JsString, text: JsString) -> bool {
        let Some(smtp) = lock(&self.smtp).clone() else {
            return false;
        };
        match transport::send(
            &smtp,
            &self.config.address,
            &shown(chat),
            &self.config.subject,
            &shown(&text),
        )
        .await
        {
            Ok(()) => true,
            Err(e) => {
                self.log("error", format!("email: sendMail failed: {e}"));
                false
            }
        }
    }
    async fn poll(self: &Arc<Self>) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let mut guard = self.imap.lock().await;
        let Some(imap) = guard.as_mut() else { return };
        match transport::unseen(imap).await {
            Ok(messages) => {
                for msg in messages {
                    if !lock(&self.seen).insert(msg.uid) {
                        continue;
                    }
                    let text = extract_plain_text(&msg.raw);
                    if text.is_empty() {
                        continue;
                    }
                    self.active_tasks.fetch_add(1, Ordering::SeqCst);
                    let done = TaskDone(self.clone());
                    let shared = self.clone();
                    tokio::spawn(async move {
                        let _done = done;
                        shared.incoming(msg.from.into(), text).await;
                    });
                }
            }
            Err(e) => self.log("error", format!("email: poll error: {e}")),
        }
    }
    async fn incoming(self: Arc<Self>, chat: JsString, text: JsString) {
        if chat.is_empty() || chat.equals_str("unknown") {
            return;
        }
        let id = shown(&chat);
        let text_scalar = shown(&text);
        let gateway = lock(&self.deps).turn_gateway.clone();
        if self.interaction.elicitation_pending(&id) && gateway.is_some() {
            match self.interaction.elicitation_answer(&id, &text_scalar).await {
                Ok(Some(reply)) => {
                    self.send_reply(&chat, reply.into()).await;
                }
                Ok(None) => {}
                Err(e) => self.log("error", format!("email: elicitation answer error: {e}")),
            }
            return;
        }
        if self.interaction.permission_pending(&id) && gateway.is_some() {
            match self.interaction.permission_answer(&id, &text_scalar).await {
                Ok(Some(answer)) => {
                    let mut release = PermissionRelease {
                        port: self.interaction.clone(),
                        chat: id.clone(),
                        token: answer.answer_token,
                        armed: true,
                    };
                    if let Some(text) = answer.text {
                        if !self.send_reply(&chat, text.into()).await {
                            return;
                        }
                        if !answer.can_advance && !answer.retry_prompt {
                            release.armed = false;
                            return;
                        }
                        if let Some(next) = self
                            .interaction
                            .permission_take_next_prompt(&id, answer.answer_token)
                        {
                            let expected = self
                                .interaction
                                .permission_prompt_request_id(&id, answer.answer_token);
                            let delivered = self.send_reply(&chat, next.into()).await;
                            self.interaction.permission_confirm_next(
                                &id,
                                delivered,
                                expected.as_deref(),
                                answer.answer_token,
                            );
                        }
                    }
                    release.armed = false;
                }
                Ok(None) => {}
                Err(e) => self.log("error", format!("email: permission answer error: {e}")),
            }
            return;
        }
        enum Admission {
            Busy,
            New,
            Empty,
            Turn(mapper::MappedSession),
        }
        let admitted = {
            let mut active = lock(&self.active_chats);
            if active.contains(&chat) {
                Admission::Busy
            } else {
                let mapped = lock(&self.mapper).resolve(&chat, &text);
                if mapped.new_session && mapped.message.is_empty() {
                    Admission::New
                } else if mapped.message.is_empty() {
                    Admission::Empty
                } else {
                    active.insert(chat.clone());
                    Admission::Turn(mapped)
                }
            }
        };
        let mapped = match admitted {
            Admission::Busy => {
                self.log("info", format!("email: chat {id} already active, skipping"));
                return;
            }
            Admission::New => {
                self.send_reply(&chat, "已创建新会话。".into()).await;
                return;
            }
            Admission::Empty => return,
            Admission::Turn(mapped) => mapped,
        };
        let _active = super::ActiveChat::new(self.active_chats.clone(), chat.clone());
        let Some(gateway) = gateway else { return };
        let mut reply = Vec::new();
        let stream = gateway
            .submit(TurnRequest {
                session_key: mapped.session_key.clone(),
                channel_key: ChannelKey::new("email"),
                message: mapped.message,
            })
            .await;
        let mut failure = stream.as_ref().err().map(ToString::to_string);
        if let Ok(mut stream) = stream {
            loop {
                let event = match stream.next().await {
                    Ok(Some(e)) => e,
                    Ok(None) => break,
                    Err(e) => {
                        failure = Some(e.to_string());
                        break;
                    }
                };
                if is(&event, "type", "elicitation_request") {
                    let prompt = self.interaction.elicitation_capture(
                        &id,
                        &shown(&mapped.session_key),
                        &event,
                    );
                    self.send_reply(&chat, prompt.into()).await;
                    continue;
                }
                if is(&event, "type", "permission_request") {
                    if let Some(prompt) = self.interaction.permission_capture(
                        &id,
                        &shown(&mapped.session_key),
                        &event,
                    ) {
                        let delivered = self.send_reply(&chat, prompt.into()).await;
                        let request = event
                            .get("requestId")
                            .and_then(JsValue::as_string)
                            .map(shown);
                        self.interaction.permission_confirm_initial(
                            &id,
                            delivered,
                            request.as_deref(),
                        );
                    }
                    continue;
                }
                if let Some(fragment) = render::render_email_event(&event) {
                    reply.extend_from_slice(fragment.units());
                }
            }
        }
        if let Some(error) = failure {
            self.log("error", format!("email: submitTurn error: {error}"));
            reply = "处理消息时发生错误，请重试。".encode_utf16().collect();
        }
        self.interaction.elicitation_clear(&id);
        self.interaction.permission_clear_after_turn(&id);
        let reply = super::sms::trim_units(&reply);
        if !reply.is_empty() {
            self.send_reply(&chat, JsString::from_utf16(reply.to_vec()))
                .await;
        }
    }
}

pub fn extract_plain_text(raw: &str) -> JsString {
    fn trim(s: &str) -> JsString {
        let u: Vec<u16> = s.encode_utf16().collect();
        JsString::from_utf16(super::sms::trim_units(&u).to_vec())
    }
    if !raw.contains("Content-Type:") {
        return trim(raw);
    }
    let start = regex::Regex::new(r"(?i)Content-Type:\s*text/plain[^\r\n]*[\r\n]+").unwrap();
    if let Some(m) = start.find(raw) {
        let rest = &raw[m.end()..];
        let stop = regex::Regex::new(r"(?i)--[a-f0-9]{8,}|Content-Type:").unwrap();
        let end = stop.find(rest).map(|m| m.start()).unwrap_or(rest.len());
        let body = &rest[..end];
        if !body.is_empty() {
            let body = body
                .strip_prefix("\r\n")
                .or_else(|| body.strip_prefix('\n'))
                .unwrap_or(body);
            let qp = regex::Regex::new(r"(?i)^Content-Transfer-Encoding:\s*quoted-printable\r?\n")
                .unwrap();
            if let Some(m) = qp.find(body) {
                let folded = body[m.end()..].replace("=\r\n", "").replace("=\n", "");
                let hex = regex::Regex::new(r"=([0-9A-F]{2})").unwrap();
                let decoded = hex.replace_all(&folded, |c: &regex::Captures<'_>| {
                    char::from(u8::from_str_radix(&c[1], 16).unwrap()).to_string()
                });
                return trim(&decoded);
            }
            return trim(body);
        }
    }
    let units: Vec<u16> = raw.encode_utf16().take(8000).collect();
    JsString::from_utf16(super::sms::trim_units(&units).to_vec())
}
struct PermissionRelease {
    port: Arc<dyn InteractionPort>,
    chat: String,
    token: Option<u64>,
    armed: bool,
}
impl Drop for PermissionRelease {
    fn drop(&mut self) {
        if self.armed {
            self.port.permission_release_answer(&self.chat, self.token);
        }
    }
}
struct TaskDone(Arc<Shared>);
impl Drop for TaskDone {
    fn drop(&mut self) {
        self.0.finish_task();
    }
}

pub struct EmailChannel {
    key: ChannelKey,
    shared: Arc<Shared>,
}
impl EmailChannel {
    pub fn new(options: EmailOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        let config = transport::EmailConfig::from_options(&options.extra, options.env.as_ref());
        Self {
            key: ChannelKey::new("email"),
            shared: Arc::new(Shared {
                config,
                mapper: Mutex::new(options.mapper),
                interaction,
                deps: Mutex::new(Default::default()),
                imap: tokio::sync::Mutex::new(None),
                smtp: Mutex::new(None),
                seen: Mutex::new(HashSet::new()),
                closed: AtomicBool::new(true),
                active_chats: Arc::new(Mutex::new(HashSet::new())),
                active_tasks: AtomicUsize::new(0),
                idle: Notify::new(),
            }),
        }
    }
    pub fn active_tasks(&self) -> usize {
        self.shared.active_tasks.load(Ordering::SeqCst)
    }
    /// Separate from TS stop: call after the shared A7 owner has drained turns.
    pub fn drain(&self) -> BoxFuture<()> {
        let shared = self.shared.clone();
        Box::pin(async move {
            loop {
                let notified = shared.idle.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if shared.active_tasks.load(Ordering::SeqCst) == 0 {
                    return;
                }
                notified.await;
            }
        })
    }
}

struct Empty;
impl ChannelHandle for Empty {
    fn stop(&self, _: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}
struct Handle {
    shared: Arc<Shared>,
    stop: watch::Sender<bool>,
    task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
}
impl ChannelHandle for Handle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        self.shared.closed.store(true, Ordering::SeqCst);
        self.stop.send_replace(true);
        *lock(&self.shared.smtp) = None;
        let shared = self.shared.clone();
        let task = self.task.clone();
        let reason = reason.to_owned();
        Box::pin(async move {
            shared.log("info", format!("email: stopping ({reason})"));
            if let Some(task) = task.lock().await.take() {
                let _ = task.await;
            }
            if let Some(mut imap) = shared.imap.lock().await.take() {
                let _ = imap.logout().await;
            }
            Ok(())
        })
    }
}
impl ChannelAdapter for EmailChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        *lock(&self.shared.deps) = deps;
        let shared = self.shared.clone();
        Box::pin(async move {
            shared.closed.store(false, Ordering::SeqCst);
            if !shared.config.complete() {
                shared.log("error","email: missing config; need extra.address, password, imapHost, smtpHost (or env equivalents)".into());
                return Ok(Arc::new(Empty) as Arc<dyn ChannelHandle>);
            }
            let started = async {
                let mut imap = transport::connect_imap(&shared.config).await?;
                match transport::prime(&mut imap).await {
                    Ok(ids) => lock(&shared.seen).extend(ids),
                    Err(e) => shared.log("warn", format!("email: UID priming skipped: {e}")),
                }
                *shared.imap.lock().await = Some(imap);
                let smtp = transport::connect_smtp(&shared.config).await?;
                *lock(&shared.smtp) = Some(Arc::new(smtp));
                shared.poll().await;
                Ok::<(), String>(())
            }
            .await;
            if let Err(e) = started {
                shared.log("error", format!("email: connect failed: {e}"));
                if let Some(mut imap) = shared.imap.lock().await.take() {
                    let _ = imap.logout().await;
                }
                *lock(&shared.smtp) = None;
                return Ok(Arc::new(Empty) as Arc<dyn ChannelHandle>);
            }
            let (stop, mut stopped) = watch::channel(false);
            let weak = Arc::downgrade(&shared);
            let interval = shared.config.poll_ms;
            let task = tokio::spawn(async move {
                let mut ticks = tokio::time::interval_at(
                    tokio::time::Instant::now() + Duration::from_millis(interval),
                    Duration::from_millis(interval),
                );
                loop {
                    tokio::select! {biased;_=async{drop(stopped.wait_for(|s|*s).await);}=>break,_=ticks.tick()=>{if let Some(shared)=weak.upgrade(){shared.poll().await;}else{break}}}
                }
            });
            shared.log(
                "info",
                format!(
                    "email: IMAP+SMTP connected ({} / {})",
                    shared.config.imap_host, shared.config.smtp_host
                ),
            );
            Ok(Arc::new(Handle {
                shared,
                stop,
                task: Arc::new(tokio::sync::Mutex::new(Some(task))),
            }) as Arc<dyn ChannelHandle>)
        })
    }
    fn deliver_cron_result(&self, d: &CronResultDelivery) -> BoxFuture<bool> {
        let shared = self.shared.clone();
        deliver_chat_cron_result(d, &self.key, move |chat, text| {
            Box::pin(async move { shared.send_reply(&chat, text).await })
        })
    }
}
