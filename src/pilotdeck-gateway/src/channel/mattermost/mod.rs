//! Native Mattermost REST + WebSocket transport, fixed cc7c4e6 channel policy.
mod mapper;
pub mod render;
#[cfg(test)]
mod tests;
use crate::channel::{
    ActiveChat,
    http_client::HttpClient,
    im_interaction::{InteractionPort, NoInteraction},
    protocol::*,
    sms,
};
use futures_util::{SinkExt, StreamExt};
pub use mapper::{MappedSession, MattermostSessionMapper, MattermostSessionMapperState};
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
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
#[derive(Default)]
pub struct MattermostOptions {
    pub token: Option<JsString>,
    pub server_url: Option<String>,
    pub team_id: Option<JsString>,
    pub mapper: Option<MattermostSessionMapper>,
}
struct SendContext {
    channel_id: JsValue,
    root_id: Option<JsValue>,
}
struct Inner {
    token: JsString,
    server_url: String,
    team_id: Option<JsString>,
    http: HttpClient,
    bot_user_id: Mutex<Option<JsValue>>,
    mapper: Mutex<MattermostSessionMapper>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    active: Arc<Mutex<HashSet<JsString>>>,
    running: AtomicBool,
    inbound: Mutex<Option<mpsc::Sender<JsValue>>>,
}
pub struct MattermostChannel {
    key: ChannelKey,
    inner: Arc<Inner>,
}
impl MattermostChannel {
    pub fn new(options: MattermostOptions) -> Self {
        Self::with_interaction(options, Arc::new(NoInteraction))
    }
    pub fn with_interaction(
        options: MattermostOptions,
        interaction: Arc<dyn InteractionPort>,
    ) -> Self {
        Self {
            key: ChannelKey::new("mattermost"),
            inner: Arc::new(Inner {
                token: options.token.unwrap_or_else(|| {
                    std::env::var("MATTERMOST_TOKEN").unwrap_or_default().into()
                }),
                server_url: options
                    .server_url
                    .unwrap_or_else(|| std::env::var("MATTERMOST_URL").unwrap_or_default()),
                team_id: options
                    .team_id
                    .or_else(|| std::env::var("MATTERMOST_TEAM_ID").ok().map(Into::into)),
                http: HttpClient::new(),
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
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'~' | b'!' | b'\'' | b'(' | b')' | b'*'
            )
        {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
fn ws_endpoint(server: &str, token: &JsString) -> Result<(String, String), ChannelError> {
    let source = if server.starts_with("http") {
        server.to_owned()
    } else {
        format!("https://{server}")
    };
    let url = url::Url::parse(&source).map_err(|e| ChannelError::error(e.to_string()))?;
    let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
    let origin_scheme = if url.scheme() == "https" {
        "https"
    } else {
        "http"
    };
    let authority = url[url::Position::BeforeHost..url::Position::AfterPort].to_owned();
    let path = url.path().strip_suffix('/').unwrap_or(url.path());
    Ok((
        format!(
            "{scheme}://{authority}{path}/api/v4/websocket?token={}",
            encode_component(token)
        ),
        format!("{origin_scheme}://{authority}"),
    ))
}
impl Inner {
    fn log(&self, level: &str, text: &str) {
        let log = self.deps.lock().expect("mattermost deps").log.clone();
        if let Some(log) = log {
            log(level, text);
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
            .map_err(|_| ChannelError::error("mattermost: invalid token"))?;
        let json = match body {
            Some(body) => stringify_json(&body)
                .map_err(|e| ChannelError::error(e.to_string()))?
                .unwrap_or_default(),
            None => String::new(),
        };
        let base = self
            .server_url
            .strip_suffix('/')
            .unwrap_or(&self.server_url);
        let response = self
            .http
            .request_bearer(
                method,
                &format!("{base}{path}"),
                Some("application/json"),
                &token,
                &json,
                crate::channel::http_client::UNDICI_DEFAULT_TIMEOUT,
            )
            .await
            .map_err(ChannelError::error)?;
        let text = String::from_utf8_lossy(&response.body);
        if !response.is_ok() {
            let units = JsString::from(text.as_ref());
            return Err(ChannelError::error(format!(
                "HTTP {}: {}",
                response.status,
                String::from_utf16_lossy(&units.units()[..units.units().len().min(400)])
            )));
        }
        if text.is_empty() {
            return Ok(object([]));
        }
        Ok(parse_json(&text).unwrap_or_else(|_| JsValue::from(text.as_ref())))
    }
    async fn websocket(&self, tx: mpsc::Sender<JsValue>, mut stop: watch::Receiver<bool>) {
        let (endpoint, origin) = match ws_endpoint(&self.server_url, &self.token) {
            Ok(v) => v,
            Err(e) => {
                self.log("error", &format!("mattermost: ws connect failed: {e}"));
                return;
            }
        };
        loop {
            let mut request = match endpoint.as_str().into_client_request() {
                Ok(r) => r,
                Err(e) => {
                    self.log("error", &format!("mattermost: ws connect failed: {e}"));
                    return;
                }
            };
            match origin.parse() {
                Ok(value) => {
                    request.headers_mut().insert("Origin", value);
                }
                Err(e) => {
                    self.log("error", &format!("mattermost: ws connect failed: {e}"));
                    return;
                }
            }
            let connection = tokio::select! {biased;_=stop.changed()=>return,result=connect_async(request)=>result};
            match connection {
                Ok((mut socket, _)) => loop {
                    tokio::select! {biased;
                     _=stop.changed()=>{let _=tokio::time::timeout(Duration::from_secs(2),socket.close(None)).await;return;},
                     frame=socket.next()=>match frame{
                      Some(Ok(Message::Text(text)))=>{if let Ok(value)=parse_json(text.as_str()){tokio::select!{biased;_=stop.changed()=>{let _=socket.close(None).await;return;},sent=tx.send(value)=>if sent.is_err(){return;}}}},
                      Some(Ok(Message::Binary(bytes)))=>{if let Ok(value)=parse_json(&String::from_utf8_lossy(&bytes)){tokio::select!{biased;_=stop.changed()=>{let _=socket.close(None).await;return;},sent=tx.send(value)=>if sent.is_err(){return;}}}},
                      Some(Ok(Message::Ping(bytes)))=>{if let Err(e)=socket.send(Message::Pong(bytes)).await{self.log("error",&format!("mattermost: ws error: {e}"));break;}},
                      Some(Ok(Message::Close(_)))|None=>break,
                      Some(Err(e))=>{self.log("error",&format!("mattermost: ws error: {e}"));break;},_=>(),
                     }
                    }
                },
                Err(e) => self.log("error", &format!("mattermost: ws error: {e}")),
            }
            tokio::select! {biased;_=stop.changed()=>return,_=tokio::time::sleep(Duration::from_secs(4))=>{}}
        }
    }
    async fn send_reply(&self, ctx: &SendContext, text: &JsString) -> bool {
        let mut delivered = true;
        for chunk in render::chunk_text(text, 16383) {
            let mut body = JsObject::new();
            body.insert("channel_id".into(), ctx.channel_id.clone());
            body.insert("message".into(), JsValue::String(chunk));
            if let Some(root) = &ctx.root_id {
                body.insert("root_id".into(), root.clone());
            }
            if let Err(e) = self
                .rest("POST", "/posts", Some(JsValue::Object(body)))
                .await
            {
                self.log("error", &format!("mattermost: post failed: {e}"));
                delivered = false;
            }
        }
        delivered
    }
    async fn handle(self: Arc<Self>, update: JsValue) {
        if update.get("event") != Some(&JsValue::from("posted")) {
            return;
        }
        let Some(raw) = update
            .get("data")
            .and_then(|d| d.get("post"))
            .and_then(JsValue::as_string)
        else {
            return;
        };
        let Ok(post) = parse_json(&String::from_utf16_lossy(raw.units())) else {
            return;
        };
        let user_id = post.get("user_id").filter(|v| sms::truthy(v));
        if user_id.is_some_and(|id| {
            self.bot_user_id.lock().expect("mattermost bot id").as_ref() == Some(id)
        }) {
            return;
        }
        let props = post.get("props");
        if ["from_webhook", "from_bot"]
            .iter()
            .any(|key| props.and_then(|p| p.get(key)) == Some(&JsValue::from("true")))
        {
            return;
        }
        let Some(channel_id) = post.get("channel_id").filter(|v| sms::truthy(v)).cloned() else {
            return;
        };
        let root_id = post.get("root_id").filter(|v| sms::truthy(v)).cloned();
        let raw_text = post
            .get("message")
            .filter(|v| !sms::nullish(v))
            .map(sms::js_string_of)
            .unwrap_or_default();
        let units = raw_text.units();
        let mut normalized = Vec::new();
        let mut i = 0;
        while i < units.len() {
            if units[i] == 13 && units.get(i + 1) == Some(&10) {
                i += 1;
            }
            normalized.push(units[i]);
            i += 1;
        }
        let text = JsString::from_utf16(sms::trim_units(&normalized).to_vec());
        if text.is_empty() {
            return;
        }
        let channel = sms::js_string_of(&channel_id);
        let chat = if let Some(root) = &root_id {
            sms::concat_units(&[channel.units(), &[58], sms::js_string_of(root).units()])
        } else {
            channel
        };
        let send_ctx = SendContext {
            channel_id,
            root_id,
        };
        let chat_id = String::from_utf16_lossy(chat.units());
        let input = String::from_utf16_lossy(text.units());
        let gateway = self
            .deps
            .lock()
            .expect("mattermost deps")
            .turn_gateway
            .clone();
        if self.interaction.elicitation_pending(&chat_id) && gateway.is_some() {
            match self.interaction.elicitation_answer(&chat_id, &input).await {
                Ok(Some(reply)) => {
                    self.send_reply(&send_ctx, &reply.into()).await;
                }
                Ok(None) => (),
                Err(e) => self.log(
                    "error",
                    &format!("mattermost: elicitation answer error: {e}"),
                ),
            }
            return;
        }
        if self.interaction.permission_pending(&chat_id) && gateway.is_some() {
            match self.interaction.permission_answer(&chat_id, &input).await {
                Ok(Some(answer)) => {
                    if let Some(reply) = answer.text.filter(|s| !s.is_empty()) {
                        if !self.send_reply(&send_ctx, &reply.into()).await {
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
                            let delivered = self.send_reply(&send_ctx, &next.into()).await;
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
                Err(e) => self.log(
                    "error",
                    &format!("mattermost: permission answer error: {e}"),
                ),
            }
            return;
        }
        // Match the synchronous JS active-check/map/admit stretch. /new-only
        // replies never occupy an active-turn slot.
        let selected = {
            let mut mapper = self.mapper.lock().expect("mattermost mapper");
            if self
                .active
                .lock()
                .expect("mattermost active")
                .contains(&chat)
            {
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
                &format!("mattermost: chat {chat_id} already active, skipping"),
            );
            return;
        };
        if mapped.command_new && mapped.message.is_empty() {
            self.send_reply(&send_ctx, &"已创建新会话。".into()).await;
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
                    channel_key: ChannelKey::new("mattermost"),
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
                    self.send_reply(&send_ctx, &prompt.into()).await;
                    continue;
                }
                if kind.is_some_and(|s| s.equals_str("permission_request")) {
                    if let Some(prompt) = self.interaction.permission_capture(
                        &chat_id,
                        &String::from_utf16_lossy(mapped.session_key.units()),
                        &event,
                    ) {
                        let delivered = self.send_reply(&send_ctx, &prompt.into()).await;
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
                if let Some(fragment) = render::render_mattermost_event(&event) {
                    result.extend_from_slice(fragment.units());
                }
            }
            Ok::<(), ChannelError>(())
        }
        .await;
        if let Err(error) = processed {
            self.log("error", &format!("mattermost: submitTurn error: {error}"));
            result = "处理消息时发生错误，请重试。".encode_utf16().collect();
        }
        drop(_interaction_cleanup);
        let final_text = JsString::from_utf16(sms::trim_units(&result).to_vec());
        if !final_text.is_empty() {
            self.send_reply(&send_ctx, &final_text).await;
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
        self.0.bot_user_id.lock().expect("mattermost bot id").take();
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
            .log("info", &format!("mattermost: stopping ({reason})"));
        {
            let mut current = self.inner.inbound.lock().expect("mattermost inbox");
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
                result
                    .map_err(|e| ChannelError::error(format!("mattermost: worker failed: {e}")))?;
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
impl ChannelAdapter for MattermostChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let inner = self.inner.clone();
        *inner.deps.lock().expect("mattermost deps") = deps;
        Box::pin(async move {
            if inner.server_url.is_empty() || inner.token.is_empty() {
                inner.log("error","mattermost: serverUrl and token are required (MATTERMOST_URL, MATTERMOST_TOKEN)");
                return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
            }
            if inner.running.swap(true, Ordering::AcqRel) {
                return Err(ChannelError::error("mattermost: already started"));
            }
            let mut startup = StartupGuard {
                inner: inner.clone(),
                armed: true,
            };
            match inner.rest("GET", "/users/me", None).await {
                Ok(me) => {
                    *inner.bot_user_id.lock().expect("mattermost bot id") =
                        me.get("id").filter(|v| !sms::nullish(v)).cloned();
                }
                Err(e) => {
                    inner.log("error", &format!("mattermost: token check failed: {e}"));
                    return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
                }
            }
            let (stop, stopped) = watch::channel(false);
            let (tx, mut updates) = mpsc::channel(64);
            *inner.inbound.lock().expect("mattermost inbox") = Some(tx.clone());
            let owner = inner.clone();
            let receive_tx = tx.clone();
            let base = inner
                .server_url
                .strip_suffix('/')
                .unwrap_or(&inner.server_url);
            let team = inner
                .team_id
                .as_ref()
                .filter(|s| !s.is_empty())
                .map(|s| format!(", team={}", String::from_utf16_lossy(s.units())))
                .unwrap_or_default();
            inner.log(
                "info",
                &format!("mattermost: connected (REST={base}{team})"),
            );
            let task = tokio::spawn(async move {
                let _running = RunningGuard(owner.clone());
                let receive = owner.websocket(receive_tx, stopped);
                tokio::pin!(receive);
                let mut tasks = JoinSet::new();
                loop {
                    tokio::select! {biased;
                     Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
                     Some(update)=updates.recv(),if tasks.len()<64=>{let worker=owner.clone();tasks.spawn(async move{worker.handle(update).await;});},
                     _=&mut receive=>break,
                    }
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
        "mattermost",
        Arc::new(move |options: &JsObject| {
            Ok(Arc::new(MattermostChannel::with_interaction(
                MattermostOptions {
                    token: options
                        .get("token")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of),
                    server_url: options
                        .get("serverUrl")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of)
                        .map(|s| String::from_utf16_lossy(s.units())),
                    team_id: options
                        .get("teamId")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of),
                    ..Default::default()
                },
                interaction.clone(),
            )) as Arc<dyn ChannelAdapter>)
        }),
    );
}
