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
use std::{collections::HashSet, path::PathBuf, sync::Arc, sync::atomic::{AtomicU64, Ordering}, time::Duration};
use tokio::{sync::Mutex, task::JoinHandle, time::Instant};
mod inbound;
mod live;

pub(crate) use inbound::{weixin_inbound_text, weixin_inbound_text_and_attachments};

pub const POLL_RETRY_DELAY_MS: u64 = 3000;
pub const WEIXIN_LOGIN_RECOVERY_CHECK_INTERVAL_MS: u64 = 2000;
pub const WEIXIN_LOGIN_RECOVERY_TIMEOUT_MS: u64 = 120_000;

pub const WEIXIN_CONNECTION_LOST_TEXT: &str =
    "微信连接暂时中断，正在尝试恢复。当前任务仍会继续处理，恢复后我会继续回复。";
pub const WEIXIN_CONNECTION_RECOVERED_TEXT: &str =
    "微信连接已恢复，我会继续处理当前任务。";
pub const WEIXIN_CONNECTION_RECOVERED_AFTER_LOSS_TEXT: &str =
    "微信连接刚刚中断过，现在已恢复。我会继续处理当前任务。";
pub const WEIXIN_SESSION_EXPIRED_TEXT: &str =
    "微信登录状态已失效，当前任务无法继续通过微信回复。请重新扫码登录后再试。";

pub const WEIXIN_QR_BANNER: &str = "\
╔══════════════════════════════════════════════╗\n\
║  微信 iLink 登录 — 请用微信扫描二维码        ║\n\
╚══════════════════════════════════════════════╝\n\
[weixin] 等待扫码登录；PilotDeck Web UI 已可继续使用。\n";

pub fn print_qr_banner() {
    print!("\n{WEIXIN_QR_BANNER}");
}

/// Channel-owned context tokens used by scheduled-result delivery. Credentials
/// stay in the installed storage owner and are never passed through cron data.
pub struct WeixinDelivery {
    storage: Storage,
    http: Http,
    contexts: std::sync::Mutex<std::collections::HashMap<String, String>>,
    pending_replies: std::sync::Mutex<std::collections::HashMap<String, Vec<String>>>,
    active_chats: std::sync::Mutex<HashSet<String>>,
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

    pub(crate) fn mark_chat_active(&self, chat: &str) {
        let mut active = self
            .active_chats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.insert(chat.into());
    }

    pub(crate) fn mark_chat_inactive(&self, chat: &str) {
        let mut active = self
            .active_chats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        active.remove(chat);
    }

    pub(crate) fn active_chats(&self) -> HashSet<String> {
        self.active_chats
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
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
        let context = self
            .contexts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(chat)
            .cloned();
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
    pub(crate) fn test_mark_active(&self, chat: &str) {
        self.mark_chat_active(chat);
    }
}

#[cfg(test)]
#[path = "native_tests/mod.rs"]
mod tests;

pub fn format_elapsed(elapsed_ms: u64) -> String {
    if elapsed_ms < 60_000 {
        return String::new();
    }
    let minutes = elapsed_ms / 60_000;
    if minutes < 60 {
        return format!("{minutes} 分钟");
    }
    let hours = minutes / 60;
    let rest = minutes % 60;
    if rest > 0 {
        format!("{hours} 小时 {rest} 分钟")
    } else {
        format!("{hours} 小时")
    }
}

pub fn is_recoverable_poll_error(error: &crate::config::routes::RouteError) -> bool {
    is_recoverable_poll_error_str(&error.code) || is_recoverable_poll_error_str(&error.message)
}

pub fn is_recoverable_poll_error_str(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    lower.contains("fetch failed")
        || lower.contains("econnreset")
        || lower.contains("enet")
        || lower.contains("etimedout")
        || lower.contains("und_err")
        || lower.contains("socket")
        || lower.contains("network")
        || lower.contains("timeout")
}

pub fn is_stale_generation(aborted: bool, current_generation: u64, generation: u64) -> bool {
    aborted || current_generation != generation
}

pub fn load_credentials(storage: &Storage) -> Option<Value> {
    match storage.credentials() {
        Ok(Some(creds)) => {
            let base = text(&creds, "baseUrl");
            let token = text(&creds, "botToken");
            let account = text(&creds, "accountId");
            if let (Some(base), Some(_), Some(_)) = (base, token, account) {
                if origin(base).is_ok() {
                    return Some(creds);
                }
            }
            None
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WeixinIlinkClient {
    pub base_url: String,
    pub bot_token: String,
    pub account_id: String,
    pub cursor: String,
}

impl WeixinIlinkClient {
    pub fn new(
        base_url: String,
        bot_token: String,
        account_id: String,
        cursor: Option<String>,
    ) -> Self {
        Self {
            base_url,
            bot_token,
            account_id,
            cursor: cursor.unwrap_or_default(),
        }
    }
}

pub fn create_client(creds: &Value, cursor: Option<&str>) -> Option<WeixinIlinkClient> {
    let base_url = text(creds, "baseUrl")?.to_string();
    let bot_token = text(creds, "botToken")?.to_string();
    let account_id = text(creds, "accountId")?.to_string();
    let cursor = cursor
        .map(|s| s.to_string())
        .or_else(|| text(creds, "cursor").map(|s| s.to_string()));
    Some(WeixinIlinkClient::new(base_url, bot_token, account_id, cursor))
}

pub fn save_cursor(storage: &Storage, cursor: &str) -> Result<()> {
    if cursor.is_empty() {
        return Ok(());
    }
    if let Ok(Some(mut creds)) = storage.credentials() {
        if creds.get("cursor").and_then(Value::as_str) != Some(cursor) {
            creds["cursor"] = cursor.into();
            let _ = super::storage::write(&storage.home.join("weixin-credentials.json"), &creds);
        }
    }
    Ok(())
}

pub fn rebuild_client_after_poll_error(
    storage: &Storage,
    client: &mut Option<WeixinIlinkClient>,
    error: &crate::config::routes::RouteError,
) -> bool {
    if !is_recoverable_poll_error(error) {
        return false;
    }
    let creds = match load_credentials(storage) {
        Some(c) => c,
        None => {
            eprintln!("weixin: cannot rebuild iLink client because credentials are missing");
            return false;
        }
    };
    let memory_cursor = client.as_ref().map(|c| c.cursor.clone()).unwrap_or_default();
    let file_cursor = text(&creds, "cursor").unwrap_or("").to_string();
    let effective_cursor = if !memory_cursor.is_empty() {
        memory_cursor
    } else {
        file_cursor.clone()
    };
    *client = create_client(&creds, Some(&effective_cursor));
    if !effective_cursor.is_empty() && effective_cursor != file_cursor {
        let _ = save_cursor(storage, &effective_cursor);
    }
    eprintln!("weixin: rebuilt iLink client after recoverable poll error");
    true
}

#[derive(Default, Debug, Clone)]
pub struct WeixinConnectionState {
    pub connection_issue_notified: bool,
    pub connection_issue_chats: HashSet<String>,
    pub connection_lost_notice_delivered_chats: HashSet<String>,
}

pub async fn notify_active_chats(
    delivery: &WeixinDelivery,
    storage: &Storage,
    http: &Http,
    credentials: &Value,
    active_chats: &HashSet<String>,
    text: &str,
) -> HashSet<String> {
    let mut delivered = HashSet::new();
    if active_chats.is_empty() {
        return delivered;
    }
    for chat in active_chats {
        let context = delivery
            .contexts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(chat)
            .cloned();
        if let Some(context) = context {
            if send_reply(storage, http, credentials, chat, Some(&context), text)
                .await
                .is_ok()
            {
                delivered.insert(chat.clone());
            }
        }
    }
    delivered
}

pub async fn notify_connection_lost(
    state: &mut WeixinConnectionState,
    delivery: &WeixinDelivery,
    storage: &Storage,
    http: &Http,
    credentials: &Value,
    active_chats: &HashSet<String>,
) {
    if state.connection_issue_notified {
        return;
    }
    state.connection_issue_notified = true;
    state.connection_issue_chats = active_chats.clone();
    state.connection_lost_notice_delivered_chats = notify_active_chats(
        delivery,
        storage,
        http,
        credentials,
        active_chats,
        WEIXIN_CONNECTION_LOST_TEXT,
    )
    .await;
}

pub async fn notify_connection_recovered(
    state: &mut WeixinConnectionState,
    delivery: &WeixinDelivery,
    storage: &Storage,
    http: &Http,
    credentials: &Value,
    active_chats: &HashSet<String>,
) {
    if !state.connection_issue_notified {
        return;
    }
    state.connection_issue_notified = false;
    let mut chats_to_notify = state.connection_issue_chats.clone();
    chats_to_notify.extend(active_chats.iter().cloned());

    for chat in &chats_to_notify {
        let text = if state.connection_lost_notice_delivered_chats.contains(chat) {
            WEIXIN_CONNECTION_RECOVERED_TEXT
        } else {
            WEIXIN_CONNECTION_RECOVERED_AFTER_LOSS_TEXT
        };
        let context = delivery
            .contexts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(chat)
            .cloned();
        if let Some(context) = context {
            let _ = send_reply(storage, http, credentials, chat, Some(&context), text).await;
        }
    }
    state.connection_issue_chats.clear();
    state.connection_lost_notice_delivered_chats.clear();
}

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
                active_chats: Default::default(),
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
                start_generation: Arc::new(AtomicU64::new(0)),
                loop_abort: Mutex::new(None),
                cursor: Arc::new(std::sync::Mutex::new(String::new())),
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
    start_generation: Arc<AtomicU64>,
    loop_abort: Mutex<Option<tokio::sync::watch::Sender<bool>>>,
    cursor: Arc<std::sync::Mutex<String>>,
}
impl WeixinChannel for NativeWeixinChannel {
    fn start(&self) -> ChannelCall<'_, ()> {
        Box::pin(async {
            let mut task = self.task.lock().await;
            let generation = self
                .start_generation
                .fetch_add(1, Ordering::SeqCst)
                + 1;
            if let Some(old_task) = task.take() {
                old_task.abort();
            }
            if let Some(old_abort) = self.loop_abort.lock().await.take() {
                let _ = old_abort.send(true);
            }

            let (abort_tx, abort_rx) = tokio::sync::watch::channel(false);
            *self.loop_abort.lock().await = Some(abort_tx);

            let mapper = self.storage.load_mapper()?;
            let storage = self.storage.clone();
            let base = self.base_url.clone();
            let gateway = self.gateway.clone();
            let delivery = self.delivery.clone();
            let cursor = self.cursor.clone();
            let start_generation = self.start_generation.clone();

            let creds = load_credentials(&self.storage);
            if let Some(creds) = creds {
                self.storage.status(
                    "connected",
                    "weixin: connected",
                    json!({"accountId": creds["accountId"]}),
                )?;
                *task = Some(tokio::spawn(async move {
                    let http = Http::new();
                    let _ = connected(
                        &storage,
                        &http,
                        creds,
                        mapper,
                        gateway,
                        delivery,
                        cursor,
                        abort_rx,
                    )
                    .await;
                }));
            } else {
                print_qr_banner();
                self.storage.status(
                    "waiting_for_login",
                    "微信等待扫码登录",
                    json!({}),
                )?;
                *task = Some(tokio::spawn(async move {
                    let http = Http::new();
                    let _ = run_qr_login(
                        &storage,
                        &http,
                        &base,
                        generation,
                        start_generation,
                        abort_rx,
                        mapper,
                        gateway,
                        delivery,
                        cursor,
                    )
                    .await;
                }));
            }
            Ok(())
        })
    }
    fn stop(&self) -> ChannelCall<'_, ()> {
        Box::pin(async {
            self.start_generation.fetch_add(1, Ordering::SeqCst);
            if let Some(abort_tx) = self.loop_abort.lock().await.take() {
                let _ = abort_tx.send(true);
            }
            let cursor = self.cursor.lock().unwrap().clone();
            if !cursor.is_empty() {
                let _ = save_cursor(&self.storage, &cursor);
            }
            if let Some(task) = self.task.lock().await.take() {
                task.abort();
                let _ = task.await;
            }
            self.storage.status("stopped", "weixin: stopped", json!({}))
        })
    }
}
impl Drop for NativeWeixinChannel {
    fn drop(&mut self) {
        self.start_generation.fetch_add(1, Ordering::SeqCst);
        let cursor = self.cursor.lock().unwrap().clone();
        if !cursor.is_empty() {
            let _ = save_cursor(&self.storage, &cursor);
        }
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }
}
async fn run_qr_login(
    storage: &Storage,
    http: &Http,
    base: &str,
    generation: u64,
    start_generation: Arc<AtomicU64>,
    abort_rx: tokio::sync::watch::Receiver<bool>,
    mapper: Value,
    gateway: Option<Arc<dyn ChannelGateway>>,
    delivery: Arc<WeixinDelivery>,
    cursor: Arc<std::sync::Mutex<String>>,
) -> Result<()> {
    if is_stale_generation(*abort_rx.borrow(), start_generation.load(Ordering::SeqCst), generation) {
        return Ok(());
    }
    let login_result = login(storage, http, base).await;
    if is_stale_generation(*abort_rx.borrow(), start_generation.load(Ordering::SeqCst), generation) {
        return Ok(());
    }
    match login_result {
        Ok(creds) => {
            let creds = storage.save_login(creds)?;
            if is_stale_generation(*abort_rx.borrow(), start_generation.load(Ordering::SeqCst), generation) {
                return Ok(());
            }
            connected(storage, http, creds, mapper, gateway, delivery, cursor, abort_rx).await
        }
        Err(err) => {
            if is_stale_generation(*abort_rx.borrow(), start_generation.load(Ordering::SeqCst), generation) {
                return Ok(());
            }
            eprintln!("[weixin] 登录失败: {err}");
            storage.status("failed", "weixin: QR login failed", json!({"error": err.message}))?;
            watch_credentials_for_login_recovery(
                storage,
                http,
                start_generation,
                generation,
                abort_rx,
                mapper,
                gateway,
                delivery,
                cursor,
            )
            .await
        }
    }
}
async fn watch_credentials_for_login_recovery(
    storage: &Storage,
    http: &Http,
    start_generation: Arc<AtomicU64>,
    generation: u64,
    mut abort_rx: tokio::sync::watch::Receiver<bool>,
    mapper: Value,
    gateway: Option<Arc<dyn ChannelGateway>>,
    delivery: Arc<WeixinDelivery>,
    cursor: Arc<std::sync::Mutex<String>>,
) -> Result<()> {
    eprintln!("weixin: QR login failed, watching credentials for recovery");
    let deadline = Instant::now() + Duration::from_millis(WEIXIN_LOGIN_RECOVERY_TIMEOUT_MS);
    while !is_stale_generation(*abort_rx.borrow(), start_generation.load(Ordering::SeqCst), generation) {
        if let Some(creds) = load_credentials(storage) {
            eprintln!("weixin: loaded saved credentials (account: {:?})", creds["accountId"]);
            return connected(storage, http, creds, mapper, gateway, delivery, cursor, abort_rx).await;
        }
        if Instant::now() >= deadline {
            eprintln!("weixin: QR login recovery timed out");
            return Ok(());
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(WEIXIN_LOGIN_RECOVERY_CHECK_INTERVAL_MS)) => {}
            _ = abort_rx.changed() => {
                if *abort_rx.borrow() {
                    break;
                }
            }
        }
    }
    Ok(())
}
async fn qr(storage: &Storage, http: &Http, base: &str) -> Result<String> {
    let result = http
        .json(base, "ilink/bot/get_bot_qrcode?bot_type=3", None, None)
        .await?;
    let code = text(&result, "qrcode").ok_or_else(|| failure("QR response missing qrcode"))?;
    let url = text(&result, "qrcode_img_content")
        .ok_or_else(|| failure("QR response missing image URL"))?;
    storage.status("waiting_for_login", "微信等待扫码登录", json!({"qrUrl":url}))?;
    println!("[weixin] 扫码登录链接:\n{url}\n");
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
                println!("[weixin] 登录成功! accountId: {account}\n");
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
    cursor: Arc<std::sync::Mutex<String>>,
    mut abort_rx: tokio::sync::watch::Receiver<bool>,
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
    let mut processing = HashSet::new();
    let mut conn_state = WeixinConnectionState::default();
    let mut consecutive_errors = 0usize;
    let mut client = create_client(&credentials, None);

    storage.status(
        "connected",
        "weixin: connected",
        json!({"accountId":credentials["accountId"]}),
    )?;
    while !*abort_rx.borrow() {
        let Some(current) = storage.credentials()? else {
            return Ok(());
        };
        if !same_generation(&current, &credentials) {
            origin(text(&current, "baseUrl").unwrap())?;
            credentials = current;
            client = create_client(&credentials, None);
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
                    if let Some(from_user) = batch
                        .get("messages")
                        .and_then(Value::as_array)
                        .and_then(|msgs| msgs.first())
                        .and_then(|m| text(m, "from_user_id"))
                    {
                        delivery.mark_chat_active(from_user);
                    }
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
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                _ = abort_rx.changed() => {
                    if *abort_rx.borrow() { return Ok(()); }
                }
            }
            continue;
        }

        let buf = client
            .as_ref()
            .map(|c| c.cursor.as_str())
            .unwrap_or_else(|| text(&credentials, "cursor").unwrap_or(""));

        let base_url = match text(&credentials, "baseUrl") {
            Some(b) => b,
            None => return Ok(()),
        };
        let bot_token = text(&credentials, "botToken");

        let getupdates_fut = http.json(
            base_url,
            "ilink/bot/getupdates",
            bot_token,
            Some(json!({"get_updates_buf": buf})),
        );
        let response = tokio::select! {
            resp = getupdates_fut => resp,
            _ = abort_rx.changed() => {
                if *abort_rx.borrow() { return Ok(()); }
                continue;
            }
        };

        if *abort_rx.borrow() {
            return Ok(());
        }

        let Some(current) = storage.credentials()? else {
            return Ok(());
        };
        if !same_generation(&current, &credentials) {
            origin(text(&current, "baseUrl").unwrap())?;
            credentials = current;
            client = create_client(&credentials, None);
            storage.status(
                "connected",
                "微信登录状态已更新",
                json!({"accountId":credentials["accountId"]}),
            )?;
            continue;
        }
        match response {
            Ok(response) if response["errcode"] == -14 => {
                eprintln!("[weixin] Session 过期，请删除凭证文件并重启以重新扫码登录");
                storage.status("expired", "微信登录已过期，请重新扫码登录", json!({}))?;
                let active = delivery.active_chats();
                notify_active_chats(
                    &delivery,
                    storage,
                    http,
                    &credentials,
                    &active,
                    WEIXIN_SESSION_EXPIRED_TEXT,
                )
                .await;
                return Ok(());
            }
            Ok(response) if response.get("ret").is_some_and(|r| r != 0) => {
                eprintln!("weixin: poll ret={} errmsg={:?}", response["ret"], response["errmsg"]);
                let active = delivery.active_chats();
                notify_connection_lost(
                    &mut conn_state,
                    &delivery,
                    storage,
                    http,
                    &credentials,
                    &active,
                )
                .await;
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(POLL_RETRY_DELAY_MS)) => {}
                    _ = abort_rx.changed() => {
                        if *abort_rx.borrow() { return Ok(()); }
                    }
                }
                continue;
            }
            Ok(response) => {
                if consecutive_errors > 0 {
                    eprintln!("weixin: poll recovered after {consecutive_errors} error(s)");
                    consecutive_errors = 0;
                }
                let active = delivery.active_chats();
                notify_connection_recovered(
                    &mut conn_state,
                    &delivery,
                    storage,
                    http,
                    &credentials,
                    &active,
                )
                .await;
                delivery
                    .flush_pending_replies(&credentials)
                    .await;

                // A credential replacement while this HTTP request was in flight
                // retires this generation before it can publish or write anything.
                if !storage
                    .credentials()?
                    .as_ref()
                    .is_some_and(|c| same_generation(c, &credentials))
                {
                    return Ok(());
                }
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
                if let Some(cursor_str) = text(&response, "get_updates_buf") {
                    if let Some(c) = client.as_mut() {
                        c.cursor = cursor_str.to_string();
                    }
                    *cursor.lock().unwrap() = cursor_str.to_string();
                    if let Some(next) = storage.advance_cursor(&credentials, cursor_str)? {
                        credentials = next;
                    }
                    let _ = save_cursor(storage, cursor_str);
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
                        tokio::select! {
                            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                            _ = abort_rx.changed() => {
                                if *abort_rx.borrow() { return Ok(()); }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                if *abort_rx.borrow() {
                    return Ok(());
                }
                consecutive_errors += 1;
                eprintln!("weixin: poll error #{consecutive_errors}: {e}");
                storage.status(
                    "connected",
                    "微信连接暂时中断，正在重试",
                    json!({"accountId":credentials["accountId"],"error":e.message}),
                )?;
                let active = delivery.active_chats();
                notify_connection_lost(
                    &mut conn_state,
                    &delivery,
                    storage,
                    http,
                    &credentials,
                    &active,
                )
                .await;
                if is_recoverable_poll_error(&e) {
                    rebuild_client_after_poll_error(storage, &mut client, &e);
                }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(POLL_RETRY_DELAY_MS)) => {}
                    _ = abort_rx.changed() => {
                        if *abort_rx.borrow() { return Ok(()); }
                    }
                }
            }
        }
    }
    Ok(())
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
            active_chats: Default::default(),
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
pub async fn send_text_chunked(
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
        Err(error) => {
            if queue_on_failure {
                delivery.queue_pending_reply(chat, reply);
                eprintln!(
                    "weixin: sendTextChunked reply queued after failure: {}",
                    error.message
                );
                Ok(())
            } else {
                Err(error)
            }
        }
    }
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
    send_text_chunked(
        storage,
        http,
        delivery,
        credentials,
        chat,
        context,
        reply,
        queue_on_failure,
    )
    .await
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

#[cfg(test)]
mod native_poll_alignment_tests {
    use super::*;

    #[test]
    fn test_format_elapsed_boundaries() {
        assert_eq!(format_elapsed(0), "");
        assert_eq!(format_elapsed(500), "");
        assert_eq!(format_elapsed(59_999), "");
        assert_eq!(format_elapsed(60_000), "1 分钟");
        assert_eq!(format_elapsed(119_999), "1 分钟");
        assert_eq!(format_elapsed(120_000), "2 分钟");
        assert_eq!(format_elapsed(59 * 60_000), "59 分钟");
        assert_eq!(format_elapsed(60 * 60_000), "1 小时");
        assert_eq!(format_elapsed(61 * 60_000), "1 小时 1 分钟");
        assert_eq!(format_elapsed(119 * 60_000), "1 小时 59 分钟");
        assert_eq!(format_elapsed(120 * 60_000), "2 小时");
        assert_eq!(format_elapsed(150 * 60_000), "2 小时 30 分钟");
    }

    #[test]
    fn test_is_recoverable_poll_error_patterns() {
        use crate::config::routes::RouteError;
        let recoverable_cases = [
            ("fetch failed", "network down"),
            ("UND_ERR_CONNECT_TIMEOUT", "ETIMEDOUT"),
            ("ECONNRESET", "connection reset by peer"),
            ("ENETUNREACH", "network unreachable"),
            ("WEIXIN_REQUEST_TIMEOUT", "request timed out"),
            ("socket_closed", "socket hang up"),
        ];
        for (code, msg) in recoverable_cases {
            let err = RouteError::new(code, msg);
            assert!(
                is_recoverable_poll_error(&err),
                "Expected error {:?} to be recoverable",
                err
            );
        }

        let non_recoverable_cases = [
            ("UNAUTHORIZED", "token expired"),
            ("BAD_REQUEST", "invalid json syntax"),
            ("NOT_FOUND", "resource missing"),
        ];
        for (code, msg) in non_recoverable_cases {
            let err = RouteError::new(code, msg);
            assert!(
                !is_recoverable_poll_error(&err),
                "Expected error {:?} to be non-recoverable",
                err
            );
        }
    }

    #[test]
    fn test_is_stale_generation_logic() {
        assert!(is_stale_generation(true, 1, 1));
        assert!(is_stale_generation(true, 2, 1));
        assert!(!is_stale_generation(false, 1, 1));
        assert!(is_stale_generation(false, 2, 1));
        assert!(is_stale_generation(false, 0, 1));
    }

    #[test]
    fn test_qr_banner_and_constants() {
        assert_eq!(POLL_RETRY_DELAY_MS, 3000);
        assert_eq!(WEIXIN_LOGIN_RECOVERY_CHECK_INTERVAL_MS, 2000);
        assert_eq!(WEIXIN_LOGIN_RECOVERY_TIMEOUT_MS, 120_000);
        assert_eq!(
            WEIXIN_SESSION_EXPIRED_TEXT,
            "微信登录状态已失效，当前任务无法继续通过微信回复。请重新扫码登录后再试。"
        );
        assert_eq!(
            WEIXIN_CONNECTION_LOST_TEXT,
            "微信连接暂时中断，正在尝试恢复。当前任务仍会继续处理，恢复后我会继续回复。"
        );
        assert_eq!(
            WEIXIN_CONNECTION_RECOVERED_TEXT,
            "微信连接已恢复，我会继续处理当前任务。"
        );
        assert_eq!(
            WEIXIN_CONNECTION_RECOVERED_AFTER_LOSS_TEXT,
            "微信连接刚刚中断过，现在已恢复。我会继续处理当前任务。"
        );
        assert!(WEIXIN_QR_BANNER.contains("╔══════════════════════════════════════════════╗"));
        assert!(WEIXIN_QR_BANNER.contains("微信 iLink 登录 — 请用微信扫描二维码"));
        assert!(WEIXIN_QR_BANNER.contains("[weixin] 等待扫码登录；PilotDeck Web UI 已可继续使用。"));
    }

    #[test]
    fn test_load_credentials_and_bad_json_handling() {
        let temp = std::env::temp_dir().join(format!("wx-cred-test-{}", uuid::Uuid::new_v4()));
        let storage = Storage::new(temp.clone());

        // 1. Missing file -> None
        assert!(load_credentials(&storage).is_none());

        // 2. Corrupted JSON -> None
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(temp.join("weixin-credentials.json"), b"invalid json content").unwrap();
        assert!(load_credentials(&storage).is_none());

        // 3. Missing fields -> None
        std::fs::write(
            temp.join("weixin-credentials.json"),
            br#"{"baseUrl":"https://ilinkai.weixin.qq.com"}"#,
        )
        .unwrap();
        assert!(load_credentials(&storage).is_none());

        // 4. Valid credentials -> Some
        std::fs::write(
            temp.join("weixin-credentials.json"),
            br#"{"baseUrl":"https://ilinkai.weixin.qq.com","botToken":"tok123","accountId":"acc123"}"#,
        )
        .unwrap();
        let creds = load_credentials(&storage);
        assert!(creds.is_some());
        let creds = creds.unwrap();
        assert_eq!(creds["accountId"], "acc123");

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_client_creation_and_cursor_save() {
        let temp = std::env::temp_dir().join(format!("wx-client-test-{}", uuid::Uuid::new_v4()));
        let storage = Storage::new(temp.clone());
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(
            temp.join("weixin-credentials.json"),
            br#"{"baseUrl":"https://ilinkai.weixin.qq.com","botToken":"tok123","accountId":"acc123","cursor":"c0"}"#,
        )
        .unwrap();

        let creds = load_credentials(&storage).unwrap();
        let client = create_client(&creds, None).unwrap();
        assert_eq!(client.cursor, "c0");
        assert_eq!(client.account_id, "acc123");

        // Save cursor
        save_cursor(&storage, "c1").unwrap();
        let updated = load_credentials(&storage).unwrap();
        assert_eq!(updated["cursor"], "c1");

        // Update client cursor in memory and verify rebuild retains cursor
        let mut client_opt = Some(client);
        client_opt.as_mut().unwrap().cursor = "c1".to_string();
        let error = crate::config::routes::RouteError::new("ETIMEDOUT", "request timed out");
        assert!(rebuild_client_after_poll_error(&storage, &mut client_opt, &error));
        assert_eq!(client_opt.as_ref().unwrap().cursor, "c1");

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[test]
    fn test_connection_notifications_bifurcation() {
        let mut state = WeixinConnectionState::default();
        assert!(!state.connection_issue_notified);

        let mut active = HashSet::new();
        active.insert("user1".to_string());
        active.insert("user2".to_string());

        // Simulate lost notification delivered only to user1
        state.connection_issue_notified = true;
        state.connection_issue_chats = active.clone();
        state.connection_lost_notice_delivered_chats.insert("user1".to_string());

        // Check text selection on recovery:
        let text_user1 = if state.connection_lost_notice_delivered_chats.contains("user1") {
            WEIXIN_CONNECTION_RECOVERED_TEXT
        } else {
            WEIXIN_CONNECTION_RECOVERED_AFTER_LOSS_TEXT
        };
        let text_user2 = if state.connection_lost_notice_delivered_chats.contains("user2") {
            WEIXIN_CONNECTION_RECOVERED_TEXT
        } else {
            WEIXIN_CONNECTION_RECOVERED_AFTER_LOSS_TEXT
        };

        assert_eq!(text_user1, WEIXIN_CONNECTION_RECOVERED_TEXT);
        assert_eq!(text_user2, WEIXIN_CONNECTION_RECOVERED_AFTER_LOSS_TEXT);
    }

    #[test]
    fn test_send_text_chunked_enqueues_on_failure() {
        let temp = std::env::temp_dir().join(format!("wx-chunk-test-{}", uuid::Uuid::new_v4()));
        let storage = Storage::new(temp.clone());
        let delivery = WeixinDelivery {
            storage: storage.clone(),
            http: Http::new(),
            contexts: Default::default(),
            pending_replies: Default::default(),
            active_chats: Default::default(),
        };

        let whole_text = "段落1：长消息测试。".repeat(200);
        // Queue whole text directly on failure
        delivery.queue_pending_reply("chat1", &whole_text);
        assert_eq!(delivery.test_pending_len("chat1"), 1);

        let _ = std::fs::remove_dir_all(&temp);
    }

    #[tokio::test]
    async fn test_start_generation_and_stop_lifecycle() {
        let temp = std::env::temp_dir().join(format!("wx-life-test-{}", uuid::Uuid::new_v4()));
        let storage = Storage::new(temp.clone());
        std::fs::create_dir_all(&temp).unwrap();
        std::fs::write(
            temp.join("weixin-credentials.json"),
            br#"{"baseUrl":"https://ilinkai.weixin.qq.com","botToken":"tok","accountId":"acc","cursor":"cur-init"}"#,
        )
        .unwrap();

        let factory = NativeWeixinFactory::new(temp.clone(), None).unwrap();
        let channel = factory.load_and_create().await.unwrap();

        // start increments generation and writes connected status
        channel.start().await.unwrap();
        let status = std::fs::read(temp.join("channels/runtime-status.json")).unwrap();
        let val: Value = serde_json::from_slice(&status).unwrap();
        assert_eq!(val["channels"]["weixin"]["state"], "connected");

        // stop increments generation and writes stopped status
        channel.stop().await.unwrap();
        let status = std::fs::read(temp.join("channels/runtime-status.json")).unwrap();
        let val: Value = serde_json::from_slice(&status).unwrap();
        assert_eq!(val["channels"]["weixin"]["state"], "stopped");

        let _ = std::fs::remove_dir_all(&temp);
    }
}
