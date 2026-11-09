//! Discord native REST + Gateway JSON transport. Fixed cc7c4e6 channel policy;
//! SDK-backed adapter contracts are captured with the approved minimal facade.
mod mapper;
pub mod render;
#[cfg(test)]
mod tests;
use crate::channel::{
    ActiveChat,
    http_client::{OutboundClient, UNDICI_DEFAULT_TIMEOUT},
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    im_interaction::{InteractionPort, NoInteraction},
    protocol::*,
    sms,
};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
pub use mapper::{DiscordSessionMapper, DiscordSessionMapperState, MappedSession};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
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
use tokio_tungstenite::{connect_async, tungstenite::Message};
#[derive(Default)]
pub struct DiscordOptions {
    pub token: Option<JsString>,
    pub mapper: Option<DiscordSessionMapper>,
    pub api_root: Option<String>,
}
struct Inner {
    token: JsString,
    api_root: String,
    http: OutboundClient,
    bot_user_id: Mutex<Option<JsValue>>,
    mapper: Mutex<DiscordSessionMapper>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    active: Arc<Mutex<HashSet<JsString>>>,
    running: AtomicBool,
    inbound: Mutex<Option<mpsc::Sender<JsValue>>>,
}
pub struct DiscordChannel {
    key: ChannelKey,
    inner: Arc<Inner>,
}
impl DiscordChannel {
    pub fn new(options: DiscordOptions) -> Self {
        Self::with_interaction(options, Arc::new(NoInteraction))
    }
    pub fn with_interaction(
        options: DiscordOptions,
        interaction: Arc<dyn InteractionPort>,
    ) -> Self {
        Self {
            key: ChannelKey::new("discord"),
            inner: Arc::new(Inner {
                token: options.token.unwrap_or_else(|| {
                    std::env::var("DISCORD_BOT_TOKEN")
                        .unwrap_or_default()
                        .into()
                }),
                api_root: options
                    .api_root
                    .unwrap_or_else(|| "https://discord.com/api/v10".into())
                    .trim_end_matches('/')
                    .into(),
                http: Client::builder(TokioExecutor::new()).build(
                    crate::runtime::model::http::https_connector(
                        crate::runtime::model::http::webpki_client_config(),
                    ),
                ),
                bot_user_id: Mutex::new(None),
                mapper: Mutex::new(options.mapper.unwrap_or_default()),
                interaction,
                deps: Mutex::new(ChannelStartDeps::default()),
                active: Arc::default(),
                running: AtomicBool::new(false),
                inbound: Mutex::new(None),
            }),
        }
    }
}
fn encode_component(value: &JsString) -> String {
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
fn packet(op: f64, data: JsValue) -> JsValue {
    object([("op", JsValue::Number(op)), ("d", data)])
}
impl Inner {
    fn log(&self, level: &str, message: &str) {
        let log = self.deps.lock().expect("discord deps").log.clone();
        if let Some(log) = log {
            log(level, message);
        }
    }
    async fn rest(
        &self,
        method: &str,
        path: &str,
        body: Option<JsValue>,
    ) -> Result<JsValue, ChannelError> {
        let token = self
            .token
            .to_utf8()
            .map_err(|_| ChannelError::error("discord: invalid token"))?;
        let body = match body {
            Some(value) => stringify_json(&value)
                .map_err(|e| ChannelError::error(e.to_string()))?
                .unwrap_or_default(),
            None => String::new(),
        };
        let mut attempt = 0;
        loop {
            let request = Request::builder()
                .method(method)
                .uri(format!("{}{path}", self.api_root))
                .header("Authorization", format!("Bot {token}"))
                .header("Content-Type", "application/json")
                .body(Full::new(Bytes::from(body.clone())))
                .map_err(|e| ChannelError::error(e.to_string()))?;
            let (status, text) = tokio::time::timeout(UNDICI_DEFAULT_TIMEOUT, async {
                let response = self
                    .http
                    .request(request)
                    .await
                    .map_err(|e| ChannelError::error(e.to_string()))?;
                let status = response.status().as_u16();
                let bytes = response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|e| ChannelError::error(e.to_string()))?
                    .to_bytes();
                Ok::<_, ChannelError>((status, String::from_utf8_lossy(&bytes).into_owned()))
            })
            .await
            .map_err(|_| ChannelError::error("Discord REST request timed out"))??;
            let value = if text.is_empty() {
                object([])
            } else {
                parse_json(&text).unwrap_or_else(|_| JsValue::from(text.as_str()))
            };
            if (200..300).contains(&status) {
                return Ok(value);
            }
            if attempt < 3 && (status == 429 || status >= 500) {
                attempt += 1;
                let delay = if status == 429 {
                    match value.get("retry_after") {
                        Some(JsValue::Number(n)) if n.is_finite() && *n >= 0.0 && *n <= 300.0 => *n,
                        _ => {
                            return Err(ChannelError::error(
                                "Discord rate limit response has no valid retry delay",
                            ));
                        }
                    }
                } else {
                    0.5
                };
                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                continue;
            }
            return Err(ChannelError::error(format!("HTTP {status}: {text}")));
        }
    }
    async fn channel_sendable(&self, chat: &JsString) -> Result<bool, ChannelError> {
        let channel = self
            .rest(
                "GET",
                &format!("/channels/{}", encode_component(chat)),
                None,
            )
            .await?;
        if channel == JsValue::Null {
            return Ok(false);
        }
        // Categories/forums/media containers have no direct send method.
        Ok(
            !matches!(channel.get("type"),Some(JsValue::Number(kind)) if matches!(*kind as i32,4|15|16)),
        )
    }
    async fn send_typing(&self, chat: &JsString) {
        if self.channel_sendable(chat).await == Ok(true) {
            let _ = self
                .rest(
                    "POST",
                    &format!("/channels/{}/typing", encode_component(chat)),
                    None,
                )
                .await;
        }
    }
    async fn send_reply(&self, chat: &JsString, text: &JsString) -> bool {
        if !self.running.load(Ordering::Acquire) {
            return false;
        }
        match self.channel_sendable(chat).await {
            Ok(true) => (),
            Ok(false) => {
                self.log(
                    "warn",
                    &format!(
                        "discord: channel {} not sendable",
                        String::from_utf16_lossy(chat.units())
                    ),
                );
                return false;
            }
            Err(e) => {
                self.log("error", &format!("discord: fetch channel failed: {e}"));
                return false;
            }
        }
        let mut ok = true;
        for chunk in render::chunk_text(text, 2000) {
            if let Err(e) = self
                .rest(
                    "POST",
                    &format!("/channels/{}/messages", encode_component(chat)),
                    Some(object([("content", JsValue::String(chunk))])),
                )
                .await
            {
                self.log("error", &format!("discord: send failed: {e}"));
                ok = false;
            }
        }
        ok
    }
    async fn gateway(
        &self,
        tx: mpsc::Sender<JsValue>,
        mut stop: watch::Receiver<bool>,
        initial: String,
    ) {
        let mut endpoint = initial;
        let mut session: Option<JsString> = None;
        let mut sequence: Option<f64> = None;
        loop {
            let mut url = match url::Url::parse(&endpoint) {
                Ok(u) => u,
                Err(e) => {
                    self.log("error", &format!("discord: client error: {e}"));
                    return;
                }
            };
            url.query_pairs_mut()
                .append_pair("v", "10")
                .append_pair("encoding", "json");
            let connection = tokio::select! {biased;_=stop.changed()=>return,result=connect_async(url.as_str())=>result};
            match connection {
                Ok((mut socket, _)) => {
                    let mut heartbeat: Option<tokio::time::Interval> = None;
                    let mut waiting_ack = false;
                    loop {
                        tokio::select! {biased;
                         _=stop.changed()=>{let _=tokio::time::timeout(Duration::from_secs(2),socket.close(None)).await;return;},
                         _=async{if let Some(timer)=heartbeat.as_mut(){timer.tick().await;}else{std::future::pending::<()>().await;}}=>{
                          if waiting_ack{self.log("error","discord: client error: heartbeat acknowledgement missing");break;}
                          let value=packet(1.0,sequence.map_or(JsValue::Null,JsValue::Number));let text=stringify_json(&value).ok().flatten().expect("gateway packet");if socket.send(Message::Text(text.into())).await.is_err(){break;}waiting_ack=true;
                         },
                         frame=socket.next()=>{
                          let raw=match frame{Some(Ok(Message::Text(t)))=>t.to_string(),Some(Ok(Message::Binary(b)))=>String::from_utf8_lossy(&b).into_owned(),Some(Ok(Message::Ping(b)))=>{if socket.send(Message::Pong(b)).await.is_err(){break;}continue;},Some(Ok(Message::Close(close)))=>{
                           if close.as_ref().is_some_and(|c|matches!(u16::from(c.code),4004|4010|4011|4012|4013|4014)){self.log("error","discord: client error: gateway rejected authentication or intents");return;}break;
                          },Some(Err(e))=>{self.log("error",&format!("discord: client error: {e}"));break;},None=>break,_=>continue};
                          let Ok(value)=parse_json(&raw)else{continue};if let Some(JsValue::Number(seq))=value.get("s"){sequence=Some(*seq);}let data=value.get("d").cloned().unwrap_or(JsValue::Null);
                          match value.get("op"){
                           Some(JsValue::Number(op)) if *op==10.0=>{
                            let ms=match data.get("heartbeat_interval"){Some(JsValue::Number(ms)) if ms.is_finite()&&*ms>=1.0&&*ms<=86_400_000.0=>*ms,_=>{self.log("error","discord: client error: invalid heartbeat interval");return;}};
                            let interval=Duration::from_secs_f64(ms/1000.0);let jitter=f64::from(uuid::Uuid::new_v4().as_bytes()[0])/256.0;heartbeat=Some(tokio::time::interval_at(tokio::time::Instant::now()+interval.mul_f64(jitter),interval));
                            let identify=if let Some(session)=&session{packet(6.0,object([("token",JsValue::String(self.token.clone())),("session_id",JsValue::String(session.clone())),("seq",sequence.map_or(JsValue::Null,JsValue::Number))]))}else{packet(2.0,object([("token",JsValue::String(self.token.clone())),("intents",JsValue::Number(37377.0)),("properties",object([("os",std::env::consts::OS.into()),("browser","pilotdeck-rs".into()),("device","pilotdeck-rs".into())]))]))};
                            let text=stringify_json(&identify).ok().flatten().expect("gateway packet");if socket.send(Message::Text(text.into())).await.is_err(){break;}
                           },
                           Some(JsValue::Number(op)) if *op==11.0=>waiting_ack=false,
                           Some(JsValue::Number(op)) if *op==1.0=>{let text=stringify_json(&packet(1.0,sequence.map_or(JsValue::Null,JsValue::Number))).ok().flatten().expect("heartbeat");if socket.send(Message::Text(text.into())).await.is_err(){break;}waiting_ack=true;},
                           Some(JsValue::Number(op)) if *op==7.0=>break,
                           Some(JsValue::Number(op)) if *op==9.0=>{if data!=JsValue::Bool(true){session=None;sequence=None;}break;},
                           Some(JsValue::Number(op)) if *op==0.0=>{
                            match value.get("t").and_then(JsValue::as_string){
                             Some(kind) if kind.equals_str("READY")=>{session=data.get("session_id").and_then(JsValue::as_string).cloned();if let Some(url)=data.get("resume_gateway_url").and_then(JsValue::as_string).and_then(|s|s.to_utf8().ok()){endpoint=url;}
                              *self.bot_user_id.lock().expect("discord bot id")=data.get("user").and_then(|u|u.get("id")).cloned();let name=data.get("user").and_then(|u|u.get("tag")).or_else(||data.get("user").and_then(|u|u.get("id"))).map(sms::js_string_of).unwrap_or_default();self.log("info",&format!("discord: logged in as {}",String::from_utf16_lossy(name.units())));
                             },
                             Some(kind) if kind.equals_str("MESSAGE_CREATE")=>{tokio::select!{biased;_=stop.changed()=>{let _=tokio::time::timeout(Duration::from_secs(2),socket.close(None)).await;return;},sent=tx.send(data)=>if sent.is_err(){return;}}},_=>(),
                            }
                           },_=>(),
                          }
                         }
                        }
                    }
                }
                Err(e) => self.log("error", &format!("discord: client error: {e}")),
            }
            tokio::select! {biased;_=stop.changed()=>return,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
        }
    }
    async fn handle(self: Arc<Self>, update: JsValue) {
        let Some(author) = update.get("author") else {
            return;
        };
        if sms::truthy(author.get("bot").unwrap_or(&JsValue::Undefined)) {
            return;
        }
        if update.get("system").is_some_and(sms::truthy) {
            return;
        }
        // Discord SDK marks non-user system message types. Replies retain type19.
        if let Some(JsValue::Number(kind)) = update.get("type") {
            if *kind != 0.0 && *kind != 19.0 && *kind != 20.0 && *kind != 23.0 {
                return;
            }
        }
        if author
            .get("id")
            .is_some_and(|id| self.bot_user_id.lock().expect("discord bot id").as_ref() == Some(id))
        {
            return;
        }
        let raw = update
            .get("content")
            .filter(|v| !sms::nullish(v))
            .map(sms::js_string_of)
            .unwrap_or_default();
        let text = JsString::from_utf16(sms::trim_units(raw.units()).to_vec());
        if text.is_empty() {
            return;
        }
        let chat = update
            .get("channel")
            .and_then(|c| c.get("id"))
            .or_else(|| update.get("channel_id"))
            .filter(|v| !sms::nullish(v))
            .map(sms::js_string_of)
            .unwrap_or_default();
        if chat.is_empty() {
            return;
        }
        let chat_id = String::from_utf16_lossy(chat.units());
        let input = String::from_utf16_lossy(text.units());
        let gateway = self.deps.lock().expect("discord deps").turn_gateway.clone();
        if self.interaction.elicitation_pending(&chat_id) && gateway.is_some() {
            match self.interaction.elicitation_answer(&chat_id, &input).await {
                Ok(Some(reply)) => {
                    self.send_reply(&chat, &reply.into()).await;
                }
                Ok(None) => (),
                Err(e) => self.log("error", &format!("discord: elicitation answer error: {e}")),
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
                Err(e) => self.log("error", &format!("discord: permission answer error: {e}")),
            }
            return;
        }
        // Match the synchronous JS active-check/map/admit stretch. /new-only
        // replies never occupy an active-turn slot.
        let selected = {
            let mut mapper = self.mapper.lock().expect("discord mapper");
            if self.active.lock().expect("discord active").contains(&chat) {
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
                &format!("discord: chat {chat_id} already active, skipping"),
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
        let processed = {
            let processed = async {
                let mut stream = gateway
                    .submit(TurnRequest {
                        session_key: mapped.session_key.clone(),
                        channel_key: ChannelKey::new("discord"),
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
                    if let Some(fragment) = render::render_discord_event(&event) {
                        result.extend_from_slice(fragment.units());
                    }
                }
                Ok::<(), ChannelError>(())
            };
            let typing = self.send_typing(&chat);
            tokio::pin!(processed, typing);
            let mut typing_done = false;
            loop {
                tokio::select! {biased;_=&mut typing,if !typing_done=>{typing_done=true;},result=&mut processed=>break result,}
            }
        };
        if let Err(error) = processed {
            self.log("error", &format!("discord: submitTurn error: {error}"));
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
        self.0.bot_user_id.lock().expect("discord bot id").take();
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
            .log("info", &format!("discord: stopping ({reason})"));
        {
            let mut current = self.inner.inbound.lock().expect("discord inbox");
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
                result.map_err(|e| ChannelError::error(format!("discord: worker failed: {e}")))?;
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
struct StartupGuard {
    inner: Arc<Inner>,
    armed: bool,
}
impl Drop for StartupGuard {
    fn drop(&mut self) {
        if self.armed {
            self.inner.running.store(false, Ordering::Release);
        }
    }
}
impl ChannelAdapter for DiscordChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let inner = self.inner.clone();
        *inner.deps.lock().expect("discord deps") = deps;
        Box::pin(async move {
            if inner.token.is_empty() {
                inner.log("error", "discord: DISCORD_BOT_TOKEN not set");
                return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
            }
            if inner.running.swap(true, Ordering::AcqRel) {
                return Err(ChannelError::error("discord: already started"));
            }
            let mut startup = StartupGuard {
                inner: inner.clone(),
                armed: true,
            };
            let gateway = match inner.rest("GET", "/gateway/bot", None).await {
                Ok(value) => value,
                Err(e) => {
                    inner.log("error", &format!("discord: start failed: {e}"));
                    return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
                }
            };
            let Some(url) = gateway
                .get("url")
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok())
            else {
                inner.log("error", "discord: start failed: missing gateway URL");
                return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
            };
            let (stop, stopped) = watch::channel(false);
            let (tx, mut updates) = mpsc::channel(64);
            *inner.inbound.lock().expect("discord inbox") = Some(tx.clone());
            let owner = inner.clone();
            let receive_tx = tx.clone();
            let task = tokio::spawn(async move {
                let _running = RunningGuard(owner.clone());
                let receive = owner.gateway(receive_tx, stopped, url);
                tokio::pin!(receive);
                let mut tasks = JoinSet::new();
                loop {
                    tokio::select! {biased;Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},Some(update)=updates.recv(),if tasks.len()<64=>{let worker=owner.clone();tasks.spawn(async move{worker.handle(update).await;});},_=&mut receive=>break,}
                }
                tasks.abort_all();
                while tasks.join_next().await.is_some() {}
            });
            startup.armed = false;
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
pub fn register_with_interaction(
    mounts: &mut crate::channel::loader::ChannelLoaderMounts,
    interaction: Arc<dyn InteractionPort>,
) {
    mounts.register(
        "discord",
        Arc::new(move |options: &JsObject| {
            Ok(Arc::new(DiscordChannel::with_interaction(
                DiscordOptions {
                    token: options
                        .get("token")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of),
                    ..Default::default()
                },
                interaction.clone(),
            )) as Arc<dyn ChannelAdapter>)
        }),
    );
}
