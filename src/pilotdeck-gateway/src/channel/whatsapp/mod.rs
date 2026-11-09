//! Native first-party WhatsApp bridge consumer. External user bridge remains external.
pub mod mapper;
pub mod render;
#[cfg(test)]
mod tests;
use super::{
    http_client::HttpClient,
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    im_interaction::InteractionPort,
    protocol::*,
};
pub use mapper::{WhatsAppSessionMapper, WhatsAppSessionMapperState};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
    path::PathBuf,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    process::{Child, Command},
    sync::{Notify, watch},
};
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
fn obj(rows: impl IntoIterator<Item = (&'static str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in rows {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn field(o: &JsValue, names: &[&str]) -> JsString {
    let v = names.iter().find_map(|k| {
        o.get(k)
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
    });
    let value = v.map(super::sms::js_string_of).unwrap_or_else(|| "".into());
    JsString::from_utf16(super::sms::trim_units(value.units()).to_vec())
}
#[derive(Default)]
pub struct WhatsAppOptions {
    pub bridge_path: Option<JsString>,
    pub bridge_url: Option<JsString>,
    pub node_executable: Option<PathBuf>,
    pub mapper: WhatsAppSessionMapper,
    pub env: Option<super::sms::EnvLookup>,
}
impl WhatsAppOptions {
    pub fn from_object(o: &JsObject) -> Self {
        Self {
            bridge_path: o.get("bridgePath").and_then(JsValue::as_string).cloned(),
            bridge_url: o.get("bridgeUrl").and_then(JsValue::as_string).cloned(),
            ..Default::default()
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct InboundMessage {
    pub id: JsString,
    pub chat_id: JsString,
    pub text: JsString,
}
pub fn normalize_messages(data: &JsValue) -> Vec<InboundMessage> {
    let rows = match data {
        JsValue::Array(a) => Some(a),
        _ => ["messages", "data"]
            .into_iter()
            .find_map(|key| match data.get(key) {
                Some(JsValue::Array(a)) => Some(a),
                _ => None,
            }),
    };
    rows.into_iter()
        .flatten()
        .filter(|row| matches!(row, JsValue::Object(_) | JsValue::Array(_)))
        .filter_map(|row| {
            let id = field(row, &["id", "key", "messageId"]);
            let chat_id = field(row, &["chatId", "chat_id", "from"]);
            let text = field(row, &["text", "body", "content"]);
            if id.is_empty() || chat_id.is_empty() {
                None
            } else {
                Some(InboundMessage { id, chat_id, text })
            }
        })
        .collect()
}
pub fn extract_port(url: &str) -> Option<u16> {
    url::Url::parse(url).ok()?.port().filter(|p| *p > 0)
}
struct Shared {
    path: String,
    url: String,
    node: PathBuf,
    mapper: Mutex<WhatsAppSessionMapper>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    http: HttpClient,
    seen: Mutex<HashSet<JsString>>,
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
        if self.closed.load(Ordering::SeqCst) {
            return false;
        }
        let payload = obj([
            ("chatId", JsValue::String(chat.clone())),
            ("message", JsValue::String(text)),
        ]);
        let Ok(Some(body)) = stringify_json(&payload) else {
            return false;
        };
        match self
            .http
            .request(
                "POST",
                &format!("{}/send", self.url),
                Some("application/json"),
                &body,
                Duration::from_secs(60),
            )
            .await
        {
            Ok(r) if r.is_ok() => true,
            Ok(r) => {
                let body = parse_json(&String::from_utf8_lossy(&r.body)).ok();
                let error = body
                    .as_ref()
                    .and_then(|b| b.get("error"))
                    .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
                    .map(super::sms::js_string_of)
                    .map(|s| shown(&s))
                    .unwrap_or_else(|| r.status_text.into());
                self.log(
                    "error",
                    format!("whatsapp: send HTTP {}: {error}", r.status),
                );
                false
            }
            Err(e) => {
                self.log("error", format!("whatsapp: send failed: {e}"));
                false
            }
        }
    }
    async fn ready(&self) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while tokio::time::Instant::now() < deadline {
            if self
                .http
                .request(
                    "GET",
                    &format!("{}/messages", self.url),
                    None,
                    "",
                    Duration::from_secs(3),
                )
                .await
                .is_ok_and(|r| r.is_ok() || r.status == 404)
            {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        false
    }
    async fn poll(self: Arc<Self>) {
        if self.closed.load(Ordering::SeqCst) {
            return;
        }
        let response = self
            .http
            .request_unbounded("GET", &format!("{}/messages", self.url), None, "")
            .await;
        let response = match response {
            Ok(r) if r.is_ok() => r,
            Ok(r) => {
                self.log(
                    "warn",
                    format!("whatsapp: poll {} {}", r.status, r.status_text),
                );
                return;
            }
            Err(e) => {
                if !self.closed.load(Ordering::SeqCst) {
                    self.log("error", format!("whatsapp: poll error: {e}"));
                }
                return;
            }
        };
        let data = match parse_json(&String::from_utf8_lossy(&response.body)) {
            Ok(v) => v,
            Err(e) => {
                self.log("error", format!("whatsapp: poll error: {e}"));
                return;
            }
        };
        for message in normalize_messages(&data) {
            if !lock(&self.seen).insert(message.id) {
                continue;
            }
            if message.text.is_empty() {
                continue;
            }
            self.active_tasks.fetch_add(1, Ordering::SeqCst);
            let done = TaskDone(self.clone());
            let shared = self.clone();
            tokio::spawn(async move {
                let _done = done;
                shared.incoming(message.chat_id, message.text).await;
            });
        }
    }
    async fn incoming(self: Arc<Self>, chat: JsString, text: JsString) {
        let id = shown(&chat);
        let text_scalar = shown(&text);
        let gateway = lock(&self.deps).turn_gateway.clone();
        if self.interaction.elicitation_pending(&id) && gateway.is_some() {
            match self.interaction.elicitation_answer(&id, &text_scalar).await {
                Ok(Some(reply)) => {
                    self.send_reply(&chat, reply.into()).await;
                }
                Ok(None) => {}
                Err(e) => self.log("error", format!("whatsapp: elicitation answer error: {e}")),
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
                Err(e) => self.log("error", format!("whatsapp: permission answer error: {e}")),
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
                self.log(
                    "info",
                    format!("whatsapp: chat {id} already active, skipping"),
                );
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
                channel_key: ChannelKey::new("whatsapp"),
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
                if let Some(fragment) = render::render_whatsapp_event(&event) {
                    reply.extend_from_slice(fragment.units());
                }
            }
        }
        if let Some(error) = failure {
            self.log("error", format!("whatsapp: submitTurn error: {error}"));
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

/// Only terminate the Child created here. An unreaped owned Unix child retains its
/// PID even after exit, so there is no PID-reuse window before our wait reaps it.
async fn cleanup_child(child: &mut Child) {
    match child.try_wait() {
        Ok(Some(_)) => return,
        Err(_) => return,
        Ok(None) => {}
    }
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
    }
    #[cfg(not(unix))]
    let _ = child.start_kill();
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}
pub struct WhatsAppChannel {
    key: ChannelKey,
    shared: Arc<Shared>,
}
impl WhatsAppChannel {
    pub fn new(options: WhatsAppOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        let env = |key: &str| {
            options.env.as_ref().and_then(|e| e(key)).or_else(|| {
                if options.env.is_none() {
                    std::env::var(key).ok().map(Into::into)
                } else {
                    None
                }
            })
        };
        let path = options
            .bridge_path
            .clone()
            .or_else(|| env("WHATSAPP_BRIDGE_PATH"))
            .unwrap_or_else(|| "".into());
        let path = shown(&JsString::from_utf16(
            super::sms::trim_units(path.units()).to_vec(),
        ));
        let url = shown(
            &options
                .bridge_url
                .clone()
                .or_else(|| env("WHATSAPP_BRIDGE_URL"))
                .unwrap_or_else(|| "http://127.0.0.1:3100".into()),
        )
        .trim_end_matches('/')
        .to_owned();
        Self {
            key: ChannelKey::new("whatsapp"),
            shared: Arc::new(Shared {
                path,
                url,
                node: options
                    .node_executable
                    .unwrap_or_else(|| PathBuf::from("node")),
                mapper: Mutex::new(options.mapper),
                interaction,
                deps: Mutex::new(Default::default()),
                http: HttpClient::new(),
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
        let shared = self.shared.clone();
        let task = self.task.clone();
        let reason = reason.to_owned();
        Box::pin(async move {
            shared.log("info", format!("whatsapp: stopping ({reason})"));
            if let Some(task) = task.lock().await.take() {
                let _ = task.await;
            }
            Ok(())
        })
    }
}
impl ChannelAdapter for WhatsAppChannel {
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
            if shared.path.is_empty() {
                shared.log(
                    "error",
                    "whatsapp: bridgePath / WHATSAPP_BRIDGE_PATH is required".into(),
                );
                return Ok(Arc::new(Empty) as Arc<dyn ChannelHandle>);
            }
            lock(&shared.seen).clear();
            let mut cmd = Command::new(&shared.node);
            cmd.arg(&shared.path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if let Some(port) = extract_port(&shared.url) {
                cmd.env("BRIDGE_PORT", port.to_string());
            }
            #[cfg(unix)]
            pilotdeck_child_signals::install_child_signal_reset(cmd.as_std_mut());
            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    shared.log("error", format!("whatsapp: bridge spawn error: {e}"));
                    return Ok(Arc::new(Empty) as Arc<dyn ChannelHandle>);
                }
            };
            let stderr = child.stderr.take();
            let weak = Arc::downgrade(&shared);
            let stderr_task = tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                if let Some(mut stderr) = stderr {
                    let mut bytes = [0; 4096];
                    loop {
                        match stderr.read(&mut bytes).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if let Some(shared) = weak.upgrade() {
                                    shared.log(
                                        "warn",
                                        format!(
                                            "whatsapp[bridge]: {}",
                                            String::from_utf8_lossy(&bytes[..n]).trim_end()
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }
            });
            if !shared.ready().await {
                cleanup_child(&mut child).await;
                stderr_task.abort();
                shared.log("error", "whatsapp: bridge HTTP did not become ready".into());
                return Ok(Arc::new(Empty) as Arc<dyn ChannelHandle>);
            }
            shared.closed.store(false, Ordering::SeqCst);
            let (stop, mut stopped) = watch::channel(false);
            let weak = Arc::downgrade(&shared);
            let task = tokio::spawn(async move {
                let mut polls = tokio::task::JoinSet::new();
                if let Some(shared) = weak.upgrade() {
                    polls.spawn(shared.poll());
                }
                let mut interval = tokio::time::interval_at(
                    tokio::time::Instant::now() + Duration::from_secs(2),
                    Duration::from_secs(2),
                );
                let mut exited = false;
                loop {
                    tokio::select! {biased;_=async{drop(stopped.wait_for(|s|*s).await);}=>break,status=child.wait(),if !exited=>{exited=true;if let Some(shared)=weak.upgrade(){if !shared.closed.load(Ordering::SeqCst){let(code,signal)=match status{Ok(s)=>{#[cfg(unix)]{use std::os::unix::process::ExitStatusExt;(s.code().map(|n|n.to_string()).unwrap_or_else(||"null".into()),s.signal().map(|n|if n==libc::SIGTERM{"SIGTERM".into()}else if n==libc::SIGKILL{"SIGKILL".into()}else{n.to_string()}).unwrap_or_else(||"none".into()))}#[cfg(not(unix))]{(s.code().map(|n|n.to_string()).unwrap_or_else(||"null".into()),"none".into())}},Err(_)=>("null".into(),"none".into())};shared.log("error",format!("whatsapp: bridge exited (code={code}, signal={signal})"));}}},_=interval.tick()=>{if let Some(shared)=weak.upgrade(){polls.spawn(shared.poll());}else{break}},_=polls.join_next(),if !polls.is_empty()=>{}}
                }
                polls.abort_all();
                while polls.join_next().await.is_some() {}
                cleanup_child(&mut child).await;
                stderr_task.abort();
            });
            shared.log(
                "info",
                format!("whatsapp: connected, polling {}/messages", shared.url),
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
