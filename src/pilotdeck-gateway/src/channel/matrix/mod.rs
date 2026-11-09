//! Native Matrix sync/room messages through the shared A7 channel port.
mod mapper;
pub mod render;
#[cfg(test)]
mod tests;
use crate::channel::{
    ActiveChat,
    http_client::HttpClient,
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    im_interaction::{InteractionPort, NoInteraction},
    protocol::*,
    sms,
};
pub use mapper::{MappedSession, MatrixSessionMapper, MatrixSessionMapperState};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::HashSet,
    path::PathBuf,
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
pub struct MatrixOptions {
    pub access_token: Option<JsString>,
    pub homeserver: Option<String>,
    pub user_id: Option<JsString>,
    pub storage_path: Option<PathBuf>,
    pub pilot_home: Option<PathBuf>,
    pub mapper: Option<MatrixSessionMapper>,
}
struct Inner {
    token: JsString,
    homeserver: String,
    user_id: Mutex<Option<JsString>>,
    configured_user_id: Option<JsString>,
    storage_path: PathBuf,
    storage_state: Mutex<JsObject>,
    http: HttpClient,
    mapper: Mutex<MatrixSessionMapper>,
    interaction: Arc<dyn InteractionPort>,
    deps: Mutex<ChannelStartDeps>,
    active: Arc<Mutex<HashSet<JsString>>>,
    running: AtomicBool,
    inbound: Mutex<Option<mpsc::Sender<JsValue>>>,
}
pub struct MatrixChannel {
    key: ChannelKey,
    inner: Arc<Inner>,
}
impl MatrixChannel {
    pub fn new(options: MatrixOptions) -> Self {
        Self::with_interaction(options, Arc::new(NoInteraction))
    }
    pub fn with_interaction(options: MatrixOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        let home = options.pilot_home.unwrap_or_else(|| {
            std::env::var_os("PILOT_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".pilotdeck")
                })
        });
        let user_id = options
            .user_id
            .or_else(|| std::env::var("MATRIX_USER_ID").ok().map(Into::into));
        Self {
            key: ChannelKey::new("matrix"),
            inner: Arc::new(Inner {
                token: options.access_token.unwrap_or_else(|| {
                    std::env::var("MATRIX_ACCESS_TOKEN")
                        .unwrap_or_default()
                        .into()
                }),
                homeserver: options
                    .homeserver
                    .unwrap_or_else(|| std::env::var("MATRIX_HOMESERVER").unwrap_or_default())
                    .trim_end_matches('/')
                    .into(),
                user_id: Mutex::new(user_id.clone()),
                configured_user_id: user_id,
                storage_path: options
                    .storage_path
                    .unwrap_or_else(|| home.join("matrix-bot-storage.json")),
                storage_state: Mutex::new(JsObject::new()),
                http: HttpClient::new(),
                mapper: Mutex::new(options.mapper.unwrap_or_default()),
                interaction,
                deps: Mutex::new(ChannelStartDeps::default()),
                active: Arc::default(),
                running: AtomicBool::new(false),
                inbound: Mutex::new(None),
            }),
        }
    }
    pub async fn accept_room_event(
        &self,
        room: JsString,
        event: JsValue,
    ) -> Result<(), ChannelError> {
        let tx = self
            .inner
            .inbound
            .lock()
            .expect("matrix inbox")
            .clone()
            .ok_or_else(|| ChannelError::error("matrix: not started"))?;
        tx.send(object([
            ("roomId", JsValue::String(room)),
            ("event", event),
        ]))
        .await
        .map_err(|_| ChannelError::error("matrix: stopped"))
    }
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
#[cfg(unix)]
fn read_storage(path: &std::path::Path) -> Result<JsObject, ChannelError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(JsObject::new()),
        Err(e) => return Err(ChannelError::error(e.to_string())),
        Ok(_) => (),
    }
    let bytes = crate::atomic_file::Target::prepare(path)
        .and_then(|target| target.read_existing_bounded(8 * 1024 * 1024))
        .map_err(|e| ChannelError::error(e.to_string()))?;
    crate::user_dialog::native_journal::check_json_bounds(&bytes)
        .map_err(|e| ChannelError::error(e.to_string()))?;
    let value = parse_json(&String::from_utf8_lossy(&bytes))
        .map_err(|e| ChannelError::error(e.to_string()))?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| ChannelError::error("matrix: storage is not an object"))
}
#[cfg(unix)]
fn write_storage(path: &std::path::Path, body: &[u8]) -> Result<(), ChannelError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ChannelError::error(e.to_string()))?;
    }
    crate::atomic_file::Target::prepare(path)
        .and_then(|target| target.publish(body))
        .map_err(|e| ChannelError::error(e.to_string()))
}
#[cfg(not(unix))]
fn read_storage(_path: &std::path::Path) -> Result<JsObject, ChannelError> {
    Err(ChannelError::error(
        "matrix: native storage is unavailable on this platform",
    ))
}
#[cfg(not(unix))]
fn write_storage(_path: &std::path::Path, _body: &[u8]) -> Result<(), ChannelError> {
    Err(ChannelError::error(
        "matrix: native storage is unavailable on this platform",
    ))
}
impl Inner {
    fn log(&self, level: &str, message: &str) {
        let log = self.deps.lock().expect("matrix deps").log.clone();
        if let Some(log) = log {
            log(level, message);
        }
    }
    async fn api(&self, method: &str, path: &str, body: JsValue) -> Result<JsValue, ChannelError> {
        let token = self
            .token
            .to_utf8()
            .map_err(|_| ChannelError::error("matrix: invalid token"))?;
        let body = stringify_json(&body)
            .map_err(|e| ChannelError::error(e.to_string()))?
            .unwrap_or_else(|| "{}".into());
        let response = self
            .http
            .request_bearer(
                method,
                &format!("{}/_matrix/client/v3{path}", self.homeserver),
                Some("application/json"),
                &token,
                &body,
                Duration::from_secs(60),
            )
            .await
            .map_err(ChannelError::error)?;
        let value = parse_json(&String::from_utf8_lossy(&response.body))
            .map_err(|e| ChannelError::error(e.to_string()))?;
        if !response.is_ok() {
            return Err(ChannelError::error(
                value
                    .get("error")
                    .map(sms::js_string_of)
                    .map(|s| String::from_utf16_lossy(s.units()))
                    .unwrap_or_else(|| "Matrix API request failed".into()),
            ));
        }
        Ok(value)
    }
    async fn sync(&self, since: Option<JsString>) -> Result<JsValue, ChannelError> {
        let mut url = url::Url::parse("http://fixture.invalid/sync").expect("literal");
        url.query_pairs_mut().append_pair("timeout", "30000");
        if let Some(since) = since {
            url.query_pairs_mut()
                .append_pair("since", &String::from_utf16_lossy(since.units()));
        }
        self.api(
            "GET",
            &format!("/sync?{}", url.query().expect("query")),
            object([]),
        )
        .await
    }
    async fn stored_token(&self) -> Result<Option<JsString>, ChannelError> {
        let path = self.storage_path.clone();
        let state = tokio::task::spawn_blocking(move || read_storage(&path))
            .await
            .map_err(|e| ChannelError::error(e.to_string()))??;
        let token = state.get("syncToken").and_then(JsValue::as_string).cloned();
        *self.storage_state.lock().expect("matrix storage") = state;
        Ok(token)
    }
    async fn store_token(&self, token: &JsString) -> Result<(), ChannelError> {
        let mut state = self.storage_state.lock().expect("matrix storage").clone();
        state.insert("syncToken".into(), JsValue::String(token.clone()));
        let body = stringify_json(&JsValue::Object(state.clone()))
            .map_err(|e| ChannelError::error(e.to_string()))?
            .expect("object");
        let path = self.storage_path.clone();
        tokio::task::spawn_blocking(move || write_storage(&path, body.as_bytes()))
            .await
            .map_err(|e| ChannelError::error(e.to_string()))??;
        *self.storage_state.lock().expect("matrix storage") = state;
        Ok(())
    }
    async fn send_reply(&self, chat: &JsString, text: &JsString) -> bool {
        if !self.running.load(Ordering::Acquire) {
            return false;
        }
        let mut ok = true;
        for chunk in render::chunk_text(text, 4000) {
            let path = format!(
                "/rooms/{}/send/m.room.message/{}",
                encode_path(chat),
                uuid::Uuid::new_v4()
            );
            if let Err(e) = self
                .api(
                    "PUT",
                    &path,
                    object([
                        ("msgtype", "m.text".into()),
                        ("body", JsValue::String(chunk)),
                    ]),
                )
                .await
            {
                self.log("error", &format!("matrix: sendMessage failed: {e}"));
                ok = false;
            }
        }
        ok
    }
    async fn handle(self: Arc<Self>, update: JsValue) {
        let Some(chat) = update.get("roomId").and_then(JsValue::as_string) else {
            return;
        };
        let Some(event) = update.get("event") else {
            return;
        };
        let Some(sender) = event
            .get("sender")
            .and_then(JsValue::as_string)
            .filter(|s| !s.is_empty())
        else {
            return;
        };
        if self.user_id.lock().expect("matrix user id").as_ref() == Some(sender) {
            return;
        }
        let Some(content) = event.get("content") else {
            return;
        };
        if content.get("m.relates_to").and_then(|v| v.get("rel_type"))
            == Some(&JsValue::from("m.replace"))
        {
            return;
        }
        let kind = content.get("msgtype").filter(|v| sms::truthy(v));
        if kind.is_some_and(|v| v != &JsValue::from("m.text")) {
            return;
        }
        let text = content
            .get("body")
            .filter(|v| !sms::nullish(v))
            .map(sms::js_string_of)
            .unwrap_or_default();
        let text = JsString::from_utf16(sms::trim_units(text.units()).to_vec());
        if text.is_empty() {
            return;
        }
        let chat = chat.clone();
        let chat_id = String::from_utf16_lossy(chat.units());
        let input = String::from_utf16_lossy(text.units());
        let gateway = self.deps.lock().expect("matrix deps").turn_gateway.clone();
        if self.interaction.elicitation_pending(&chat_id) && gateway.is_some() {
            match self.interaction.elicitation_answer(&chat_id, &input).await {
                Ok(Some(reply)) => {
                    self.send_reply(&chat, &reply.into()).await;
                }
                Ok(None) => (),
                Err(e) => self.log("error", &format!("matrix: elicitation answer error: {e}")),
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
                Err(e) => self.log("error", &format!("matrix: permission answer error: {e}")),
            }
            return;
        }
        // Match the synchronous JS active-check/map/admit stretch. /new-only
        // replies never occupy an active-turn slot.
        let selected = {
            let mut mapper = self.mapper.lock().expect("matrix mapper");
            if self.active.lock().expect("matrix active").contains(&chat) {
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
                &format!("matrix: chat {chat_id} already active, skipping"),
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
                    channel_key: ChannelKey::new("matrix"),
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
                if let Some(fragment) = render::render_matrix_event(&event) {
                    result.extend_from_slice(fragment.units());
                }
            }
            Ok::<(), ChannelError>(())
        }
        .await;
        if let Err(error) = processed {
            self.log("error", &format!("matrix: submitTurn error: {error}"));
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
        self.0.user_id.lock().expect("matrix user id").take();
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
            .log("info", &format!("matrix: stopping ({reason})"));
        {
            let mut current = self.inner.inbound.lock().expect("matrix inbox");
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
                result.map_err(|e| ChannelError::error(format!("matrix: worker failed: {e}")))?;
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
impl ChannelAdapter for MatrixChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }
    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let inner = self.inner.clone();
        *inner.deps.lock().expect("matrix deps") = deps;
        Box::pin(async move {
            if inner.homeserver.is_empty() {
                inner.log("error", "matrix: homeserver not set (MATRIX_HOMESERVER)");
                return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
            }
            if inner.token.is_empty() {
                inner.log(
                    "error",
                    "matrix: access token not set (MATRIX_ACCESS_TOKEN)",
                );
                return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
            }
            if inner.running.swap(true, Ordering::AcqRel) {
                return Err(ChannelError::error("matrix: already started"));
            }
            let since = match inner.stored_token().await {
                Ok(s) => s,
                Err(e) => {
                    inner.running.store(false, Ordering::Release);
                    inner.log("error", &format!("matrix: start failed: {e}"));
                    return Ok(Arc::new(Noop) as Arc<dyn ChannelHandle>);
                }
            };
            *inner.user_id.lock().expect("matrix user id") = inner.configured_user_id.clone();
            if let Ok(me) = inner.api("GET", "/account/whoami", object([])).await {
                if let Some(id) = me.get("user_id").and_then(JsValue::as_string) {
                    *inner.user_id.lock().expect("matrix user id") = Some(id.clone());
                }
            }
            let name = inner
                .user_id
                .lock()
                .expect("matrix user id")
                .as_ref()
                .map(|s| String::from_utf16_lossy(s.units()))
                .unwrap_or_else(|| "(unknown user)".into());
            inner.log("info", &format!("matrix: syncing as {name}"));
            let (stop, mut stopped) = watch::channel(false);
            let (tx, mut updates) = mpsc::channel(64);
            *inner.inbound.lock().expect("matrix inbox") = Some(tx.clone());
            let owner = inner.clone();
            let task = tokio::spawn(async move {
                let _running = RunningGuard(owner.clone());
                let mut tasks = JoinSet::new();
                let mut since = since;
                let mut poll = Box::pin(owner.sync(since.clone()));
                loop {
                    tokio::select! { biased;
                        _=stopped.changed()=>break,
                        Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
                        Some(update)=updates.recv()=>{let worker=owner.clone();tasks.spawn(async move{worker.handle(update).await;});},
                        result=&mut poll=>{
                            match result {
                                Ok(value)=>{
                                    if let Some(invites)=value.get("rooms").and_then(|r|r.get("invite")).and_then(JsValue::as_object) {for(room,_)in invites.entries() {if let Err(e)=owner.api("POST",&format!("/join/{}",encode_path(room)),object([])).await {owner.log("warn",&format!("matrix: joinRoom failed: {e}"));}}}
                                    if let Some(rooms)=value.get("rooms").and_then(|r|r.get("join")).and_then(JsValue::as_object) {for(room,data)in rooms.entries() {if let Some(JsValue::Array(events))=data.get("timeline").and_then(|t|t.get("events")) {for event in events {if event.get("type")!=Some(&JsValue::from("m.room.message")) {continue;}let worker=owner.clone();let update=object([("roomId",JsValue::String(room.clone())),("event",event.clone())]);tasks.spawn(async move{worker.handle(update).await;});}}}}
                                    if let Some(next)=value.get("next_batch").and_then(JsValue::as_string) {match owner.store_token(next).await {Ok(())=>since=Some(next.clone()),Err(e)=>{owner.log("error",&format!("matrix: storage failed: {e}"));break;}}}
                                },
                                Err(e)=>{owner.log("error",&format!("matrix: sync error: {e}"));tokio::select!{_=stopped.changed()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}}},
                            }
                            poll=Box::pin(owner.sync(since.clone()));
                        }
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
        "matrix",
        Arc::new(move |options: &JsObject| {
            Ok(Arc::new(MatrixChannel::with_interaction(
                MatrixOptions {
                    access_token: options
                        .get("accessToken")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of),
                    homeserver: options
                        .get("homeserver")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of)
                        .map(|s| String::from_utf16_lossy(s.units())),
                    user_id: options
                        .get("userId")
                        .filter(|v| !sms::nullish(v))
                        .map(sms::js_string_of),
                    pilot_home: options
                        .get("pilotHome")
                        .and_then(JsValue::as_string)
                        .map(|s| PathBuf::from(String::from_utf16_lossy(s.units()))),
                    ..Default::default()
                },
                interaction.clone(),
            )) as Arc<dyn ChannelAdapter>)
        }),
    );
}
