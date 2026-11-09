//! Fixed cc7c4e6 Signal REST/SSE adapter; no SDK, account or model ownership.
mod mapper;
pub mod render;
#[cfg(test)]
mod tests;
use crate::channel::{
    ActiveChat,
    http_client::{HttpClient, OutboundClient, UNDICI_DEFAULT_TIMEOUT},
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    im_interaction::{InteractionPort, NoInteraction},
    protocol::*,
    sms,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
pub use mapper::{MappedSession, SignalSessionMapper, SignalSessionMapperState};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinHandle, JoinSet},
};
#[derive(Default)]
pub struct SignalOptions {
    pub rest_url: Option<String>,
    pub account: Option<JsString>,
    pub mapper: Option<SignalSessionMapper>,
}
struct Inner {
    rest_url: String,
    account: JsString,
    http: HttpClient,
    receive: OutboundClient,
    mapper: Mutex<SignalSessionMapper>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    active: Arc<Mutex<HashSet<JsString>>>,
    recipients: Mutex<HashMap<JsString, JsString>>,
    running: AtomicBool,
    inbound: Mutex<Option<mpsc::Sender<JsValue>>>,
}
pub struct SignalChannel {
    key: ChannelKey,
    inner: Arc<Inner>,
}
impl SignalChannel {
    pub fn new(options: SignalOptions) -> Self {
        Self::with_interaction(options, Arc::new(NoInteraction))
    }
    pub fn with_interaction(options: SignalOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        Self {
            key: ChannelKey::new("signal"),
            inner: Arc::new(Inner {
                rest_url: options
                    .rest_url
                    .unwrap_or_else(|| {
                        std::env::var("SIGNAL_HTTP_URL")
                            .unwrap_or_else(|_| "http://127.0.0.1:8080".into())
                    })
                    .trim_end_matches('/')
                    .into(),
                account: options
                    .account
                    .unwrap_or_else(|| std::env::var("SIGNAL_ACCOUNT").unwrap_or_default().into()),
                http: HttpClient::new(),
                receive: Client::builder(TokioExecutor::new()).build(
                    crate::runtime::model::http::https_connector(
                        crate::runtime::model::http::webpki_client_config(),
                    ),
                ),
                mapper: Mutex::new(options.mapper.unwrap_or_default()),
                interaction,
                deps: Mutex::new(ChannelStartDeps::default()),
                active: Arc::default(),
                recipients: Mutex::new(HashMap::new()),
                running: AtomicBool::new(false),
                inbound: Mutex::new(None),
            }),
        }
    }
}
fn strip_chat_prefix(chat: &JsString) -> JsString {
    let units = chat.units();
    let prefix = if units.starts_with(&[100, 109, 58]) {
        3
    } else if units.starts_with(&[103, 114, 111, 117, 112, 58]) {
        6
    } else {
        0
    };
    JsString::from_utf16(units[prefix..].to_vec())
}
fn encode_path(value: &JsString) -> String {
    let mut out = String::new();
    for byte in String::from_utf16_lossy(value.units()).bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
/// SSE framing retains raw bytes until a complete line, so split UTF-8 code
/// points and CRLF packet boundaries decode identically to TextDecoder(stream).
fn parse_line(line: &[u8]) -> Option<JsValue> {
    let line = String::from_utf8_lossy(line);
    let mut text = crate::metadata::js_trim(&line);
    if let Some(data) = text.strip_prefix("data:") {
        text = crate::metadata::js_trim(data);
    }
    if text.is_empty() || text == "[DONE]" || text == ":ok" {
        return None;
    }
    parse_json(text).ok()
}
impl Inner {
    fn log(&self, level: &str, message: &str) {
        let log = self.deps.lock().expect("signal deps").log.clone();
        if let Some(log) = log {
            log(level, message);
        }
    }
    async fn receive_loop(&self, tx: mpsc::Sender<JsValue>) {
        let url = format!(
            "{}/v1/receive/{}",
            self.rest_url,
            encode_path(&self.account)
        );
        let mut carry = Vec::new();
        loop {
            let exchange = async {
                let request = Request::builder()
                    .uri(&url)
                    .header("Accept", "text/event-stream, application/json, */*")
                    .body(Full::new(Bytes::new()))
                    .map_err(|e| e.to_string())?;
                let response =
                    tokio::time::timeout(UNDICI_DEFAULT_TIMEOUT, self.receive.request(request))
                        .await
                        .map_err(|_| "TypeError: fetch failed".to_owned())?
                        .map_err(|e| e.to_string())?;
                if !response.status().is_success() {
                    let status = response.status().as_u16();
                    let body = response
                        .into_body()
                        .collect()
                        .await
                        .map_err(|e| e.to_string())?
                        .to_bytes();
                    self.log(
                        "error",
                        &format!(
                            "signal: receive HTTP {status}: {}",
                            String::from_utf8_lossy(&body)
                        ),
                    );
                    return Err(String::new());
                }
                let mut body = response.into_body();
                while let Some(frame) = body.frame().await {
                    let frame = frame.map_err(|e| e.to_string())?;
                    if let Some(bytes) = frame.data_ref() {
                        carry.extend_from_slice(bytes);
                        while let Some(end) = carry.iter().position(|b| *b == 10) {
                            let line: Vec<_> = carry.drain(..=end).collect();
                            if let Some(value) = parse_line(&line) {
                                if tx.send(value).await.is_err() {
                                    return Ok(());
                                }
                            }
                        }
                    }
                }
                Ok::<(), String>(())
            }
            .await;
            if let Err(e) = exchange {
                if !e.is_empty() {
                    self.log("error", &format!("signal: receive stream error: {e}"));
                }
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
        }
    }
    async fn send_reply(&self, chat: &JsString, text: &JsString) -> bool {
        if !self.running.load(Ordering::Acquire) {
            return false;
        }
        let recipient = self
            .recipients
            .lock()
            .expect("signal recipients")
            .get(chat)
            .cloned()
            .unwrap_or_else(|| strip_chat_prefix(chat));
        if recipient.is_empty() {
            self.log(
                "warn",
                &format!(
                    "signal: no recipient for {}, cannot send",
                    String::from_utf16_lossy(chat.units())
                ),
            );
            return false;
        }
        let mut ok = true;
        for chunk in render::chunk_text(text, 2000) {
            let value = object([
                ("message", JsValue::String(chunk)),
                ("number", JsValue::String(self.account.clone())),
                (
                    "recipients",
                    JsValue::Array(vec![JsValue::String(recipient.clone())]),
                ),
            ]);
            let body = match stringify_json(&value) {
                Ok(Some(body)) => body,
                _ => return false,
            };
            match self
                .http
                .request_unbounded(
                    "POST",
                    &format!("{}/v2/send", self.rest_url),
                    Some("application/json"),
                    &body,
                )
                .await
            {
                Ok(response) if response.is_ok() => (),
                Ok(response) => {
                    let raw = JsString::from(String::from_utf8_lossy(&response.body).as_ref());
                    let text = String::from_utf16_lossy(&raw.units()[..raw.units().len().min(500)]);
                    self.log(
                        "error",
                        &format!("signal: send HTTP {}: {text}", response.status),
                    );
                    ok = false;
                }
                Err(e) => {
                    self.log("error", &format!("signal: send failed: {e}"));
                    ok = false;
                }
            }
        }
        ok
    }
    async fn handle(self: Arc<Self>, update: JsValue) {
        let Some(envelope) = update.get("envelope") else {
            return;
        };
        let dm = envelope.get("dataMessage");
        let sent = envelope
            .get("syncMessage")
            .and_then(|s| s.get("sentMessage"));
        let string = |value: Option<&JsValue>| {
            value
                .and_then(JsValue::as_string)
                .filter(|s| !s.is_empty())
                .cloned()
        };
        let Some(text) = string(sent.and_then(|s| s.get("message")))
            .or_else(|| string(dm.and_then(|d| d.get("message"))))
        else {
            return;
        };
        if sms::trim_units(text.units()).is_empty() {
            return;
        }
        let source =
            string(envelope.get("source")).or_else(|| string(envelope.get("sourceNumber")));
        let group = dm
            .and_then(|d| d.get("groupInfo"))
            .and_then(|g| g.get("groupId"))
            .filter(|v| !sms::nullish(v))
            .or_else(|| {
                sent.and_then(|s| s.get("groupInfo"))
                    .and_then(|g| g.get("groupId"))
            });
        let chat = if let Some(group) = group.and_then(JsValue::as_string) {
            sms::concat_units(&[&"group:".encode_utf16().collect::<Vec<_>>(), group.units()])
        } else if let Some(source) = &source {
            sms::concat_units(&[&"dm:".encode_utf16().collect::<Vec<_>>(), source.units()])
        } else {
            self.account.clone()
        };
        let recipient = source.unwrap_or_else(|| strip_chat_prefix(&chat));
        if !recipient.is_empty() {
            self.recipients
                .lock()
                .expect("signal recipients")
                .insert(chat.clone(), recipient);
        }
        let chat_id = String::from_utf16_lossy(chat.units());
        let input = String::from_utf16_lossy(text.units());
        let gateway = self.deps.lock().expect("signal deps").turn_gateway.clone();
        if self.interaction.elicitation_pending(&chat_id) && gateway.is_some() {
            match self.interaction.elicitation_answer(&chat_id, &input).await {
                Ok(Some(reply)) => {
                    self.send_reply(&chat, &reply.into()).await;
                }
                Ok(None) => (),
                Err(e) => self.log("error", &format!("signal: elicitation answer error: {e}")),
            }
            return;
        }
        if self.interaction.permission_pending(&chat_id) && gateway.is_some() {
            match self.interaction.permission_answer(&chat_id, &input).await {
                Ok(Some(answer)) => {
                    if let Some(reply) = answer.text.filter(|s| !s.is_empty()) {
                        if !self.send_reply(&chat, &reply.into()).await {
                            self.interaction
                                .permission_release_answer(&chat_id, answer.answer_token);
                            return;
                        }
                        if !answer.can_advance && !answer.retry_prompt {
                            return;
                        }
                        if let Some(next) = self
                            .interaction
                            .permission_take_next_prompt(&chat_id, answer.answer_token)
                        {
                            let request = self
                                .interaction
                                .permission_prompt_request_id(&chat_id, answer.answer_token);
                            let delivered = self.send_reply(&chat, &next.into()).await;
                            self.interaction.permission_confirm_next(
                                &chat_id,
                                delivered,
                                request.as_deref(),
                                answer.answer_token,
                            );
                        }
                    }
                }
                Ok(None) => (),
                Err(e) => self.log("error", &format!("signal: permission answer error: {e}")),
            }
            return;
        }
        // Match the synchronous JS active-check/map/admit stretch. /new-only
        // replies never occupy an active-turn slot.
        let selected = {
            let mut mapper = self.mapper.lock().expect("signal mapper");
            if self.active.lock().expect("signal active").contains(&chat) {
                None
            } else {
                let mapped = mapper.resolve(&chat, &text);
                let guard = if mapped.message.is_empty() {
                    None
                } else {
                    Some(
                        ActiveChat::admit(self.active.clone(), chat.clone())
                            .expect("serialized admission"),
                    )
                };
                Some((mapped, guard))
            }
        };
        let Some((mapped, _active)) = selected else {
            self.log(
                "info",
                &format!("signal: chat {chat_id} already active, skipping"),
            );
            return;
        };
        if mapped.command_new && mapped.message.is_empty() {
            self.send_reply(&chat, &"已创建新会话。".into()).await;
            return;
        }
        if mapped.message.is_empty() {
            return;
        }
        let Some(gateway) = gateway else { return };
        let _interaction_cleanup = InteractionCleanup {
            inner: self.clone(),
            chat: chat_id.clone(),
        };
        let mut result = Vec::new();
        let processed = async {
            let mut stream = gateway
                .submit(TurnRequest {
                    session_key: mapped.session_key.clone(),
                    channel_key: ChannelKey::new("signal"),
                    message: mapped.message,
                })
                .await?;
            while let Some(event) = stream.next().await? {
                let kind = event.get("type").and_then(JsValue::as_string);
                if kind.is_some_and(|s| s.equals_str("elicitation_request")) {
                    let prompt = self.interaction.elicitation_capture(
                        &chat_id,
                        &String::from_utf16_lossy(mapped.session_key.units()),
                        &event,
                    );
                    self.send_reply(&chat, &prompt.into()).await;
                    continue;
                }
                if kind.is_some_and(|s| s.equals_str("permission_request")) {
                    if let Some(prompt) = self.interaction.permission_capture(
                        &chat_id,
                        &String::from_utf16_lossy(mapped.session_key.units()),
                        &event,
                    ) {
                        let delivered = self.send_reply(&chat, &prompt.into()).await;
                        let request = event
                            .get("requestId")
                            .and_then(JsValue::as_string)
                            .map(|s| String::from_utf16_lossy(s.units()));
                        self.interaction.permission_confirm_initial(
                            &chat_id,
                            delivered,
                            request.as_deref(),
                        );
                    }
                    continue;
                }
                if let Some(fragment) = render::render_signal_event(&event) {
                    result.extend_from_slice(fragment.units());
                }
            }
            Ok::<(), ChannelError>(())
        }
        .await;
        if let Err(error) = processed {
            self.log("error", &format!("signal: submitTurn error: {error}"));
            result = "处理消息时发生错误，请重试。".encode_utf16().collect();
        }
        drop(_interaction_cleanup);
        let final_text = JsString::from_utf16(sms::trim_units(&result).to_vec());
        if !final_text.is_empty() {
            self.send_reply(&chat, &final_text).await;
        }
    }
}
struct InteractionCleanup {
    inner: Arc<Inner>,
    chat: String,
}
impl Drop for InteractionCleanup {
    fn drop(&mut self) {
        self.inner.interaction.elicitation_clear(&self.chat);
        self.inner
            .interaction
            .permission_clear_after_turn(&self.chat);
    }
}
struct RunningGuard(Arc<Inner>);
impl Drop for RunningGuard {
    fn drop(&mut self) {
        self.0.running.store(false, Ordering::Release);
    }
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut value = JsObject::new();
    for (k, v) in fields {
        value.insert(k.into(), v);
    }
    JsValue::Object(value)
}
struct Handle {
    inner: Arc<Inner>,
    stop: watch::Sender<bool>,
    inbox: mpsc::Sender<JsValue>,
    task: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
}
impl ChannelHandle for Handle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        self.inner
            .log("info", &format!("signal: stopping ({reason})"));
        {
            let mut current = self.inner.inbound.lock().expect("signal inbox");
            if current
                .as_ref()
                .is_some_and(|sender| sender.same_channel(&self.inbox))
            {
                current.take();
            }
        }
        let _ = self.stop.send(true);
        let task = self.task.clone();
        Box::pin(async move {
            let mut task = task.lock().await;
            if let Some(worker) = task.as_mut() {
                let result = worker.await;
                task.take();
                result.map_err(|e| ChannelError::error(format!("signal: worker failed: {e}")))?;
            }
            Ok(())
        })
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}
impl ChannelAdapter for SignalChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let inner = self.inner.clone();
        *inner.deps.lock().expect("signal deps") = deps;
        Box::pin(async move {
            if inner.account.is_empty() {
                inner.log("error", "signal: SIGNAL_ACCOUNT / account not set");
                return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
            }
            if inner.running.swap(true, Ordering::AcqRel) {
                return Err(ChannelError::error("signal: already started"));
            }
            let (stop, mut stopped) = watch::channel(false);
            let (tx, mut updates) = mpsc::channel(64);
            *inner.inbound.lock().expect("signal inbox") = Some(tx.clone());
            let owner = inner.clone();
            let receive_tx = tx.clone();
            inner.log(
                "info",
                &format!(
                    "signal: SSE receive at {}/v1/receive/{}",
                    inner.rest_url,
                    encode_path(&inner.account)
                ),
            );
            let task = tokio::spawn(async move {
                let _running = RunningGuard(owner.clone());
                let receive = owner.receive_loop(receive_tx);
                tokio::pin!(receive);
                let mut tasks = JoinSet::new();
                loop {
                    tokio::select! {biased;
                     _=stopped.changed()=>break,
                     Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
                     Some(update)=updates.recv(),if tasks.len()<64=>{let worker=owner.clone();tasks.spawn(async move{worker.handle(update).await;});},
                     _=&mut receive=>break,
                    }
                }
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            });
            Ok(Arc::new(Handle {
                inner,
                stop,
                inbox: tx,
                task: Arc::new(tokio::sync::Mutex::new(Some(task))),
            }) as Arc<dyn ChannelHandle>)
        })
    }
    fn deliver_cron_result(&self, delivery: &CronResultDelivery) -> BoxFuture<bool> {
        let inner = self.inner.clone();
        deliver_chat_cron_result(delivery, &self.key, move |chat, text| {
            Box::pin(async move { inner.send_reply(&chat, &text).await })
        })
    }
}
struct Noop;
impl ChannelHandle for Noop {
    fn stop(&self, _: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}
pub fn register(mounts: &mut crate::channel::loader::ChannelLoaderMounts) {
    register_with_interaction(mounts, Arc::new(NoInteraction));
}
/// Composition supplies a fresh helper per adapter/reload, backed by the
/// common gateway coordinator rather than a second hidden session owner.
pub fn register_with_interaction(
    mounts: &mut crate::channel::loader::ChannelLoaderMounts,
    interaction: Arc<dyn InteractionPort>,
) {
    mounts.register(
        "signal",
        Arc::new(move |options: &JsObject| {
            Ok(Arc::new(SignalChannel::with_interaction(
                SignalOptions {
                    rest_url: options
                        .get("restUrl")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of)
                        .map(|s| String::from_utf16_lossy(s.units())),
                    account: options
                        .get("account")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of),
                    ..Default::default()
                },
                interaction.clone(),
            )) as Arc<dyn ChannelAdapter>)
        }),
    );
}
