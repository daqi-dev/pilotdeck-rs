//! CLI login preparation: QR work outlives the RPC and replacement cancels it.
use super::{
    ChannelCall, WeixinChannel, WeixinChannelFactory,
    http::{Http, failure, origin},
    storage::{Storage, same_generation, text},
};
use crate::config::channel_commands::{
    ChannelGateway, ChannelReply, ChannelSessionState, CommandContext, execute_channel_command,
};
use crate::config::routes::Result;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::Mutex, task::JoinHandle, time::Instant};
mod inbound;
mod live;

pub(crate) use inbound::{weixin_inbound_text, weixin_inbound_text_and_attachments};
/// Channel-owned context tokens used by scheduled-result delivery. Credentials
/// stay in the installed storage owner and are never passed through cron data.
pub struct WeixinDelivery {
    storage: Storage,
    http: Http,
    contexts: std::sync::Mutex<std::collections::HashMap<String, String>>,
    pending_replies: std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
}
const MAX_PENDING_REPLIES_PER_CHAT: usize = 20;

impl WeixinDelivery {
    pub(crate) fn queue_attachment_fallback(&self, chat: &str, attachment: &Value) {
        let text = super::attachments::format_attachment_fallback(attachment);
        self.queue_pending_reply(chat, &text);
    }

    pub(crate) fn clear_pending_replies(&self, chat: &str) {
        let mut pending = self
            .pending_replies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        pending.remove(chat);
    }

    pub(crate) fn queue_pending_reply(&self, chat: &str, text: &str) {
        let mut pending = self
            .pending_replies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let replies = pending.entry(chat.into()).or_default();
        replies.push(text.into());
        if replies.len() > MAX_PENDING_REPLIES_PER_CHAT {
            let drop = replies.len() - MAX_PENDING_REPLIES_PER_CHAT;
            replies.drain(0..drop);
        }
    }

    pub async fn deliver(&self, delivery: &Value) -> Result<bool> {
        use crate::config::routes::im_cron::{parse_chat_id, session_field};
        if delivery
            .get("originChannelKey")
            .and_then(|v| v.as_str())
            .is_some_and(|key| !key.is_empty() && key != "weixin")
        {
            return Ok(false);
        }
        let Some(session) = session_field(delivery) else {
            return Ok(false);
        };
        let Some(chat) = parse_chat_id(session, "weixin") else {
            return Ok(false);
        };
        let text = delivery["text"].as_str().unwrap_or("");
        let context = self.contexts.lock().unwrap().get(chat).cloned();
        let Some(context) = context else {
            self.queue_pending_reply(chat, text);
            return Ok(false);
        };
        let credentials = match self.storage.credentials() {
            Ok(Some(credentials)) => credentials,
            Ok(None) | Err(_) => {
                self.queue_pending_reply(chat, text);
                return Ok(false);
            }
        };
        match send_reply(
            &self.storage,
            &self.http,
            &credentials,
            chat,
            Some(&context),
            text,
        )
        .await
        {
            Ok(()) => Ok(true),
            Err(_) => {
                self.queue_pending_reply(chat, text);
                Ok(false)
            }
        }
    }

    /// `WeixinChannel.ts:936-947` — after a successful poll, resend queued cron replies.
    pub(crate) async fn flush_pending_replies(&self, credentials: &Value) {
        let chats: Vec<String> = self
            .pending_replies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .keys()
            .cloned()
            .collect();
        for chat in chats {
            loop {
                let text = {
                    let pending = self
                        .pending_replies
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    pending
                        .get(&chat)
                        .and_then(|replies| replies.first())
                        .cloned()
                };
                let Some(text) = text else {
                    self.pending_replies
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .remove(&chat);
                    break;
                };
                let Some(context) = self
                    .contexts
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .get(&chat)
                    .cloned()
                else {
                    break;
                };
                let Some(current) = self
                    .storage
                    .credentials()
                    .ok()
                    .flatten()
                    .filter(|current| same_generation(current, credentials))
                else {
                    break;
                };
                match send_reply(
                    &self.storage,
                    &self.http,
                    &current,
                    &chat,
                    Some(&context),
                    &text,
                )
                .await
                {
                    Ok(()) => {
                        let mut pending = self
                            .pending_replies
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if let Some(replies) = pending.get_mut(&chat) {
                            if replies.first().is_some_and(|head| head == &text) {
                                replies.remove(0);
                            }
                            if replies.is_empty() {
                                pending.remove(&chat);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

#[cfg(test)]
impl WeixinDelivery {
    pub(crate) fn test_seed_context(&self, chat: &str, context: &str) {
        self.contexts
            .lock()
            .unwrap()
            .insert(chat.into(), context.into());
    }
    pub(crate) fn test_pending_len(&self, chat: &str) -> usize {
        self.pending_replies
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(chat)
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;

pub struct NativeWeixinFactory {
    storage: Storage,
    base_url: String,
    gateway: Option<Arc<dyn ChannelGateway>>,
    delivery: Arc<WeixinDelivery>,
}
impl NativeWeixinFactory {
    /// This is host configuration, never a field accepted in a WS request.
    pub fn new(home: PathBuf, base_url: Option<String>) -> Result<Self> {
        let base_url = base_url.unwrap_or_else(|| "https://ilinkai.weixin.qq.com".into());
        origin(&base_url)?;
        let storage = Storage::new(home);
        Ok(Self {
            delivery: Arc::new(WeixinDelivery {
                storage: storage.clone(),
                http: Http::new(),
                contexts: Default::default(),
                pending_replies: Default::default(),
            }),
            storage,
            base_url,
            gateway: None,
        })
    }
    pub fn with_gateway(mut self, gateway: Arc<dyn ChannelGateway>) -> Self {
        self.gateway = Some(gateway);
        self
    }
    pub fn delivery(&self) -> Arc<WeixinDelivery> {
        self.delivery.clone()
    }
}
impl WeixinChannelFactory for NativeWeixinFactory {
    fn attach_mapper_persistence(&self, saver: Arc<dyn Fn(Value) + Send + Sync>) {
        self.storage.set_debounced_mapper_save(saver);
    }
    fn load_and_create(&self) -> ChannelCall<'_, Arc<dyn WeixinChannel>> {
        Box::pin(async {
            // Validate before the host stops the previous instance. start also
            // reloads so rollback never overwrites later mapper updates.
            self.storage.load_mapper()?;
            Ok(Arc::new(NativeWeixinChannel {
                storage: self.storage.clone(),
                base_url: self.base_url.clone(),
                task: Mutex::new(None),
                gateway: self.gateway.clone(),
                delivery: self.delivery.clone(),
            }) as Arc<dyn WeixinChannel>)
        })
    }
}
struct NativeWeixinChannel {
    storage: Storage,
    base_url: String,
    task: Mutex<Option<JoinHandle<()>>>,
    gateway: Option<Arc<dyn ChannelGateway>>,
    delivery: Arc<WeixinDelivery>,
}
impl WeixinChannel for NativeWeixinChannel {
    fn start(&self) -> ChannelCall<'_, ()> {
        Box::pin(async {
            let mut task = self.task.lock().await;
            if task.as_ref().is_some_and(|t| !t.is_finished()) {
                return Ok(());
            }
            let credentials = self.storage.credentials()?;
            if let Some(credentials) = &credentials {
                origin(text(credentials, "baseUrl").unwrap())?;
            }
            let mapper = self.storage.load_mapper()?;
            self.storage.status(
                if credentials.is_some() {
                    "connected"
                } else {
                    "waiting_for_login"
                },
                if credentials.is_some() {
                    "微信已连接"
                } else {
                    "微信渠道已启动，等待扫码登录"
                },
                credentials
                    .as_ref()
                    .map(|c| json!({"accountId":c["accountId"]}))
                    .unwrap_or_else(|| json!({})),
            )?;
            let storage = self.storage.clone();
            let base = self.base_url.clone();
            let gateway = self.gateway.clone();
            let delivery = self.delivery.clone();
            *task = Some(tokio::spawn(async move {
                let http = Http::new();
                let result = run(
                    &storage,
                    &http,
                    &base,
                    credentials,
                    mapper.clone(),
                    gateway.clone(),
                    delivery.clone(),
                )
                .await;
                if let Err(error) = result {
                    let _ = storage.status(
                        "failed",
                        "微信扫码登录失败",
                        json!({"error":error.message}),
                    );
                    // TS permits a CLI login to replace the file after QR failure.
                    let deadline = Instant::now() + Duration::from_secs(120);
                    while Instant::now() < deadline {
                        tokio::time::sleep(Duration::from_secs(2)).await;
                        if let Ok(Some(credentials)) = storage.credentials() {
                            let _ = connected(
                                &storage,
                                &http,
                                credentials,
                                mapper.clone(),
                                gateway.clone(),
                                delivery.clone(),
                            )
                            .await;
                            break;
                        }
                    }
                }
            }));
            Ok(())
        })
    }
    fn stop(&self) -> ChannelCall<'_, ()> {
        Box::pin(async {
            if let Some(task) = self.task.lock().await.take() {
                task.abort();
                let _ = task.await;
            }
            self.storage.status("stopped", "微信渠道已停止", json!({}))
        })
    }
}
impl Drop for NativeWeixinChannel {
    fn drop(&mut self) {
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}
async fn run(
    storage: &Storage,
    http: &Http,
    base: &str,
    credentials: Option<Value>,
    mapper: Value,
    gateway: Option<Arc<dyn ChannelGateway>>,
    delivery: Arc<WeixinDelivery>,
) -> Result<()> {
    let credentials = match credentials {
        Some(credentials) => credentials,
        None => storage.save_login(login(storage, http, base).await?)?,
    };
    connected(storage, http, credentials, mapper, gateway, delivery).await
}
async fn qr(storage: &Storage, http: &Http, base: &str) -> Result<String> {
    let result = http
        .json(base, "ilink/bot/get_bot_qrcode?bot_type=3", None, None)
        .await?;
    let code = text(&result, "qrcode").ok_or_else(|| failure("QR response missing qrcode"))?;
    let url = text(&result, "qrcode_img_content")
        .ok_or_else(|| failure("QR response missing image URL"))?;
    storage.status("waiting_for_login", "请扫码登录微信", json!({"qrUrl":url}))?;
    Ok(code.into())
}
async fn login(storage: &Storage, http: &Http, base: &str) -> Result<Value> {
    let mut code = qr(storage, http, base).await?;
    let mut refreshes = 1;
    let deadline = Instant::now() + Duration::from_secs(480);
    while Instant::now() < deadline {
        let encoded: String = url::form_urlencoded::byte_serialize(code.as_bytes()).collect();
        let response = http
            .json(
                base,
                &format!("ilink/bot/get_qrcode_status?qrcode={encoded}"),
                None,
                None,
            )
            .await;
        let response = match response {
            Err(e) if e.code == "WEIXIN_REQUEST_TIMEOUT" => json!({"status":"wait"}),
            other => other?,
        };
        match text(&response, "status") {
            Some("scaned") => {
                // Preserve the displayed QR across progress updates, as TS does.
                let status = std::fs::read(storage.home.join("channels/runtime-status.json"))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                let details = status
                    .and_then(|s| {
                        s["channels"]["weixin"]["qrUrl"]
                            .as_str()
                            .map(|q| json!({"qrUrl":q}))
                    })
                    .unwrap_or_else(|| json!({}));
                storage.status("waiting_for_login", "已扫码，请在微信中确认登录", details)?;
            }
            Some("expired") => {
                refreshes += 1;
                if refreshes > 3 {
                    return Err(failure("QR code expired too many times"));
                }
                storage.status("waiting_for_login", "二维码已过期，正在刷新", json!({}))?;
                code = qr(storage, http, base).await?;
            }
            Some("confirmed") => {
                let token = text(&response, "bot_token")
                    .ok_or_else(|| failure("Login response missing credentials"))?;
                let account = text(&response, "ilink_bot_id")
                    .ok_or_else(|| failure("Login response missing credentials"))?;
                let base = text(&response, "baseurl").unwrap_or(base);
                origin(base)?;
                return Ok(json!({"baseUrl":base,"botToken":token,"accountId":account}));
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(failure("Login timed out"))
}
async fn connected(
    storage: &Storage,
    http: &Http,
    mut credentials: Value,
    mapper: Value,
    gateway: Option<Arc<dyn ChannelGateway>>,
    delivery: Arc<WeixinDelivery>,
) -> Result<()> {
    let mapper = ChannelSessionState::from_value(&mapper);
    let live = gateway.clone().map(|gateway| {
        Arc::new(live::LiveOwner::new(
            storage.clone(),
            http.clone(),
            gateway,
            mapper.clone(),
            delivery.clone(),
        ))
    });
    let mut workers: tokio::task::JoinSet<(String, Result<()>)> = tokio::task::JoinSet::new();
    let mut processing = std::collections::HashSet::new();
    storage.status(
        "connected",
        "微信已连接",
        json!({"accountId":credentials["accountId"]}),
    )?;
    loop {
        let Some(current) = storage.credentials()? else {
            return Ok(());
        };
        if !same_generation(&current, &credentials) {
            origin(text(&current, "baseUrl").unwrap())?;
            credentials = current;
            storage.status(
                "connected",
                "微信登录状态已更新",
                json!({"accountId":credentials["accountId"]}),
            )?;
        }
        let pending = super::inbox::WeixinInbox::new(storage.home.clone());
        while let Some(result) = workers.try_join_next() {
            if let Ok((id, result)) = result {
                processing.remove(&id);
                if let Err(error) = result {
                    storage.status(
                        "connected",
                        "微信消息处理失败，正在重试",
                        json!({"error":error.message}),
                    )?;
                }
            }
        }
        if let Some(live) = &live {
            for batch in pending
                .pending()?
                .iter()
                .filter(|batch| batch.get("accountId") == credentials.get("accountId"))
            {
                let id = text(batch, "id")
                    .ok_or_else(|| failure("Invalid Weixin inbox batch"))?
                    .to_owned();
                if processing.insert(id.clone()) {
                    let live = live.clone();
                    let batch = batch.clone();
                    let credentials = credentials.clone();
                    workers.spawn(async move {
                        let result = live.process_batch(&credentials, batch).await;
                        (id, result)
                    });
                }
            }
        }
        if live.is_none()
            && pending
                .pending()?
                .iter()
                .any(|b| b.get("accountId") == credentials.get("accountId"))
        {
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        let response = http
            .json(
                text(&credentials, "baseUrl").unwrap(),
                "ilink/bot/getupdates",
                text(&credentials, "botToken"),
                Some(json!({"get_updates_buf":text(&credentials,"cursor").unwrap_or("")})),
            )
            .await;
        let Some(current) = storage.credentials()? else {
            return Ok(());
        };
        if !same_generation(&current, &credentials) {
            origin(text(&current, "baseUrl").unwrap())?;
            credentials = current;
            storage.status(
                "connected",
                "微信登录状态已更新",
                json!({"accountId":credentials["accountId"]}),
            )?;
            continue;
        }
        match response {
            Ok(response) if response["errcode"] == -14 => {
                storage.status("expired", "微信登录已过期，请重新扫码登录", json!({}))?;
                return Ok(());
            }
            Ok(response) if response.get("ret").is_none_or(|r| r == 0) => {
                // A credential replacement while this HTTP request was in flight
                // retires this generation before it can publish or write anything.
                if !storage
                    .credentials()?
                    .as_ref()
                    .is_some_and(|c| same_generation(c, &credentials))
                {
                    return Ok(());
                }
                delivery
                    .flush_pending_replies(&credentials)
                    .await;
                let pending = super::inbox::WeixinInbox::new(storage.home.clone());
                let batch = if let Some(messages) = response
                    .get("msgs")
                    .and_then(Value::as_array)
                    .filter(|m| !m.is_empty())
                {
                    Some(pending.enqueue(&credentials, &response, messages, &mapper.snapshot())?)
                } else {
                    None
                };
                if let Some(cursor) = text(&response, "get_updates_buf") {
                    let Some(next) = storage.advance_cursor(&credentials, cursor)? else {
                        return Ok(());
                    };
                    credentials = next;
                }
                // The owning channel drains the durable batch. Apply backpressure
                // until acknowledgement; never repeatedly fetch an unhandled batch.
                if let Some(batch) = batch {
                    if let Some(live) = &live {
                        let saved = pending
                            .pending()?
                            .into_iter()
                            .find(|value| text(value, "id") == Some(&batch))
                            .ok_or_else(|| failure("Weixin inbox batch disappeared"))?;
                        if processing.insert(batch.clone()) {
                            let live = live.clone();
                            let credentials = credentials.clone();
                            let id = batch.clone();
                            workers.spawn(async move {
                                let result = live.process_batch(&credentials, saved).await;
                                (id, result)
                            });
                        }
                    }
                    while live.is_none() && pending.contains(&batch) {
                        if !storage
                            .credentials()?
                            .as_ref()
                            .is_some_and(|c| same_generation(c, &credentials))
                        {
                            return Ok(());
                        }
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
            Ok(_) => {}
            Err(e) if e.code == "WEIXIN_REQUEST_TIMEOUT" => {}
            Err(e) => {
                storage.status(
                    "connected",
                    "微信连接暂时中断，正在重试",
                    json!({"accountId":credentials["accountId"],"error":e.message}),
                )?;
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// Drain the durable handoff through the same gateway used by other clients.
/// Stage the completed turn's reply before delivery so a transport retry cannot
/// submit the same agent input again. A stopped/replaced channel drops this
/// future and retains the batch for the next owner.
#[cfg(test)]
async fn drain_batch(
    storage: &Storage,
    http: &Http,
    credentials: &Value,
    batch: &Value,
    mapper: &mut ChannelSessionState,
    gateway: &dyn ChannelGateway,
) -> Result<()> {
    let id = text(batch, "id").ok_or_else(|| failure("Invalid Weixin inbox batch"))?;
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(failure("Invalid Weixin inbox id"));
    }
    let path = storage
        .home
        .join("channels/weixin-inbox")
        .join(format!("{id}.json"));
    let mut batch = batch.clone();
    let messages = batch["messages"].as_array().cloned().unwrap_or_default();
    let mut index = batch["nextIndex"].as_u64().unwrap_or(0) as usize;
    while let Some(message) = messages.get(index) {
        let Some(chat) = text(message, "from_user_id") else {
            index += 1;
            continue;
        };
        if batch.get("mapper").is_some() {
            *mapper = ChannelSessionState::from_value(&batch["mapper"]);
        }
        if batch.get("outbox").is_none() {
            let reply = CapturedReply::default();
            let input = message["item_list"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| {
                            (item["type"] == 1)
                                .then(|| item["text_item"]["text"].as_str())
                                .flatten()
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            let input = crate::metadata::js_trim(&input);
            if input == "/new" || input.starts_with("/new ") {
                if batch["resolvedNew"] != json!(index) {
                    let previous = mapper.session("weixin", chat);
                    mapper.reset("weixin", chat);
                    storage.save_mapper(&mapper.snapshot())?;
                    batch["mapper"] = mapper.snapshot();
                    batch["resolvedNew"] = json!(index);
                    super::storage::write(&path, &batch)?;
                    let _ = gateway
                        .request("abort_turn", json!({"sessionKey":previous}))
                        .await;
                }
                if input == "/new" {
                    reply.reply("已创建新会话。").await?;
                }
            }
            let input = if input == "/new" || input.starts_with("/new ") {
                input[4..].trim()
            } else {
                input
            };
            if !input.is_empty() {
                let mut ctx = CommandContext {
                    gateway,
                    reply: &reply,
                    state: mapper,
                    channel: "weixin",
                    chat,
                    home: &storage.home,
                };
                if !execute_channel_command(input, &mut ctx).await? {
                    let mut params = json!({"sessionKey":mapper.session("weixin",chat),"channelKey":"weixin","runId":format!("weixin:{id}:{index}"),"message":input,"allowPlanModeTools":false});
                    if let Some(project) = mapper.project(chat) {
                        params["projectKey"] = project.into();
                    }
                    let events = gateway.turn(params).await?;
                    let text = collect_reply(&events);
                    if !text.is_empty() {
                        reply.reply(&text).await?;
                    }
                }
            }
            storage.save_mapper(&mapper.snapshot())?;
            batch["mapper"] = mapper.snapshot();
            batch["outbox"] = json!(reply.0.lock().unwrap().clone());
            batch["nextIndex"] = json!(index);
            super::storage::write(&path, &batch)?;
        }
        for reply in batch["outbox"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            send_reply(
                storage,
                http,
                credentials,
                chat,
                text(message, "context_token"),
                reply,
            )
            .await?;
        }
        index += 1;
        batch["nextIndex"] = json!(index);
        batch.as_object_mut().unwrap().remove("outbox");
        super::storage::write(&path, &batch)?;
    }
    Ok(())
}
#[derive(Default)]
struct CapturedReply(std::sync::Mutex<Vec<String>>);
impl ChannelReply for CapturedReply {
    fn reply<'a>(&'a self, text: &'a str) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().push(text.into());
            Ok(())
        })
    }
}
const WEIXIN_TEXT_CHUNK: usize = 2000;

pub(crate) fn weixin_text_chunks(body: &str) -> Vec<String> {
    let units: Vec<u16> = body.encode_utf16().collect();
    if units.is_empty() {
        return vec![String::new()];
    }
    let mut chunks = Vec::new();
    let mut start = 0usize;
    while start < units.len() {
        let mut end = (start + WEIXIN_TEXT_CHUNK).min(units.len());
        if end < units.len() && is_high_surrogate(units[end - 1]) {
            end -= 1;
        }
        if end <= start {
            end = (start + 1).min(units.len());
        }
        chunks.push(
            String::from_utf16(&units[start..end])
                .expect("weixin chunk must preserve valid UTF-16 scalar boundaries"),
        );
        start = end;
    }
    chunks
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xD800..=0xDBFF).contains(&unit)
}

#[cfg(test)]
mod delivery_tests {
    use super::*;

    #[test]
    fn clear_pending_replies_matches_ts_reset() {
        let delivery = WeixinDelivery {
            storage: crate::config::routes::weixin::storage::Storage::new(std::env::temp_dir()),
            http: crate::config::routes::weixin::http::Http::new(),
            contexts: Default::default(),
            pending_replies: Default::default(),
        };
        delivery.queue_pending_reply("chat", "keep-me");
        assert_eq!(delivery.test_pending_len("chat"), 1);
        delivery.clear_pending_replies("chat");
        assert_eq!(delivery.test_pending_len("chat"), 0);
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::weixin_text_chunks;

    #[test]
    fn chunks_do_not_split_utf16_surrogate_pairs() {
        let emoji = "😀";
        let body = "y".repeat(1999) + emoji;
        let chunks = weixin_text_chunks(&body);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks.join(""), body);
        assert!(chunks.iter().all(|chunk| !chunk.contains('\u{FFFD}')));
    }
}

#[cfg(test)]
fn collect_reply(events: &[Value]) -> String {
    let mut text = String::new();
    for event in events {
        match event["type"].as_str() {
            Some("assistant_text_delta") => {
                if let Some(delta) = event["text"].as_str() {
                    text.push_str(delta);
                }
            }
            Some("assistant_block") if event["kind"] == "text" && text.is_empty() => {
                if let Some(block) = event["text"].as_str() {
                    text.push_str(block);
                }
            }
            Some("error") => {
                if let Some(message) = event["message"].as_str() {
                    text.push_str(message);
                }
            }
            _ => {}
        }
    }
    text
}
pub(crate) async fn send_reply_with_queue(
    storage: &Storage,
    http: &Http,
    delivery: &WeixinDelivery,
    credentials: &Value,
    chat: &str,
    context: Option<&str>,
    reply: &str,
    queue_on_failure: bool,
) -> Result<()> {
    match send_reply(storage, http, credentials, chat, context, reply).await {
        Ok(()) => Ok(()),
        Err(error) if queue_on_failure => {
            delivery.queue_pending_reply(chat, reply);
            eprintln!("weixin: reply queued after failure: {}", error.message);
            Ok(())
        }
        Err(error) => Err(error),
    }
}

async fn send_reply(
    storage: &Storage,
    http: &Http,
    credentials: &Value,
    chat: &str,
    context: Option<&str>,
    reply: &str,
) -> Result<()> {
    let context = context.ok_or_else(|| failure("Weixin reply has no context token"))?;
    if !storage
        .credentials()?
        .as_ref()
        .is_some_and(|current| super::storage::same_login(current, credentials))
    {
        return Err(failure("Weixin channel was replaced"));
    }
    // Preserve the SDK's 2000 UTF-16 unit chunk limit without splitting a Rust
    // scalar; each outbound call still carries the original context token.
    for chunk in weixin_text_chunks(reply) {
        let response = http.json(text(credentials,"baseUrl").unwrap(),"ilink/bot/sendmessage",text(credentials,"botToken"),Some(json!({"msg":{"from_user_id":"","to_user_id":chat,"client_id":uuid::Uuid::new_v4().to_string(),"message_type":2,"message_state":2,"context_token":context,"item_list":[{"type":1,"text_item":{"text":chunk}}]}}))).await?;
        if response.get("ret").is_some_and(|ret| ret != 0)
            || response.get("errcode").is_some_and(|code| code != 0)
        {
            return Err(failure("Weixin reply rejected"));
        }
    }
    Ok(())
}
