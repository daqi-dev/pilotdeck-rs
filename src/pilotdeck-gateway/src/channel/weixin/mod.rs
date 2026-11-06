//! Weixin channel (`adapters/channel/weixin/WeixinChannel.ts` pr1 subset) on
//! the #1327 lifecycle core: text-message dispatch through the same gateway
//! submit port as every other inbound path, `/new` session rotation with old
//! turn abort, chunked replies, pending reply queue, cursor persistence, QR
//! background login with a bounded recovery watcher, and the three-branch
//! poll loop (`errcode -14` / `ret != 0` / exception).
//!
//! pr1 scope cuts (pr2 follows the plan): elicitation/permission helpers,
//! attachments (AES/CDN, image/file/voice items), the full live-reply
//! controller (the weixin transport subset with no `edit` and no
//! `maxMessageLength` is inlined here: deltas only accumulate, one final
//! `sendReply` at `flushFinal`), cron delivery, and the channel command
//! registry. The gateway surface is a weixin-owned port
//! ([`WeixinGatewayPort`]) constructor-injected in pr1; pr2 collects it into
//! `ChannelStartDeps` once the A7 inbound port lands. The adapter-state
//! reporter ([`WeixinRuntimeStatusReporter`]) is likewise injected in pr1:
//! the #1327 core reports only the four lifecycle states, while this adapter
//! also reports its own login states.
mod client;
mod live_reply;
mod mapper;
#[cfg(test)]
mod replay_tests;
mod state;
#[cfg(test)]
mod tests;

pub use client::{
    IlinkHttpClient, SavedCredentials, WeixinClientError, WeixinIlinkClient, WeixinLogin,
    WeixinLoginCallbacks, WeixinLoginResult, default_client_factory, default_login_base_url,
    is_recoverable_poll_error, login_with_qr,
};
use live_reply::{LiveReplyController, LiveReplyOptions, LiveReplyTransport, SendOutcome};
pub use mapper::{WeixinCommand, WeixinResolveResult, WeixinSessionMapper};
pub use state::ChannelStatePersistence;
pub(crate) use state::stringify_pretty;

use super::protocol::{
    BoxFuture, ChannelAdapter, ChannelError, ChannelHandle, ChannelKey, ChannelLog,
    ChannelStartDeps,
};
use crate::runtime::network::Cancellation;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle};

const CHANNEL_KEY: &str = "weixin";
/// `msg.message_type === 1` (text messages; `MessageItemType.TEXT` is the
/// item-list constant in client.rs).
const MESSAGE_KIND_TEXT: f64 = 1.0;
const POLL_RETRY_DELAY_MS: u64 = 3000;
const WEIXIN_MAX_PENDING_REPLIES_PER_CHAT: usize = 20;
const WEIXIN_MAX_PENDING_TURNS_PER_CHAT: usize = 20;
const WEIXIN_LOGIN_RECOVERY_CHECK_INTERVAL_MS: u64 = 2000;
const WEIXIN_LOGIN_RECOVERY_TIMEOUT_MS: u64 = 120_000;
const WEIXIN_SESSION_EXPIRED_TEXT: &str =
    "微信登录状态已失效，当前任务无法继续通过微信回复。请重新扫码登录后再试。";
const WEIXIN_CONNECTION_LOST_TEXT: &str =
    "微信连接暂时中断，正在尝试恢复。当前任务仍会继续处理，恢复后我会继续回复。";
const WEIXIN_CONNECTION_RECOVERED_TEXT: &str = "微信连接已恢复，我会继续处理当前任务。";
const WEIXIN_CONNECTION_RECOVERED_AFTER_LOSS_TEXT: &str =
    "微信连接刚刚中断过，现在已恢复。我会继续处理当前任务。";
/// `timeoutFinalText` override WeixinChannel passes to the reply controller.
const WEIXIN_TIMEOUT_FINAL_TEXT: &str =
    "处理时间已超过上限，任务已停止。你可以调整需求后重新发送。";
const WEIXIN_NEW_SESSION_TEXT: &str = "已创建新会话。";

/// TS `CREDENTIALS_PATH = join(homedir(), ".pilotdeck", "weixin-credentials.json")`;
/// `saveCredentials` always mkdirs `homedir()/.pilotdeck`.
pub fn default_credentials_path() -> PathBuf {
    home_pilotdeck_dir().join("weixin-credentials.json")
}
fn home_pilotdeck_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".pilotdeck")
}

/// The channel-specific runtime status shape (ChannelRuntimeStatus states
/// this adapter reports in addition to the lifecycle core's four).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WeixinChannelState {
    Starting,
    Connected,
    WaitingForLogin,
    Expired,
    Failed,
    Stopped,
}
impl WeixinChannelState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Connected => "connected",
            Self::WaitingForLogin => "waiting_for_login",
            Self::Expired => "expired",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
        }
    }
}
#[derive(Clone, Debug)]
pub struct WeixinChannelStatus {
    pub state: WeixinChannelState,
    pub message: String,
    pub account_id: Option<JsString>,
    pub error: Option<String>,
    pub qr_url: Option<JsString>,
}
pub type WeixinRuntimeStatusReporter = Arc<dyn Fn(WeixinChannelStatus) + Send + Sync>;

/// The gateway surface the channel needs: the same submit admission as any
/// other inbound path, plus the abort-and-wait contract of
/// `Gateway.abortTurn` (InProcessGateway.ts:2000-2026 — the default reason is
/// `aborted:{runId}` / `aborted`, and the call resolves only after the
/// in-flight submit has unwound its completion fence).
pub type WeixinAbortFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
pub trait WeixinGatewayPort: Send + Sync {
    fn submit_turn(&self, input: JsValue) -> WeixinTurnStream;
    /// `reason: Option<&str>` mirrors the TS `abortTurn` `reason` field:
    /// `Some("system:timeout")` from the live-reply watchdog, `None` keeps the
    /// default `aborted:{runId}` / `aborted` reason.
    fn abort_turn<'a>(
        &'a self,
        session_key: &'a JsString,
        run_id: Option<&'a JsString>,
        reason: Option<&'a str>,
    ) -> WeixinAbortFuture<'a>;
}

/// One `submitTurn` iteration: events until end, or the TS iteration throw.
pub struct WeixinTurnStream {
    receiver: tokio::sync::mpsc::UnboundedReceiver<WeixinStreamItem>,
    /// #1328 replay seam: invoked at every `next()` entry — the point the TS
    /// consumer asks its async generator for the next value, which is after it
    /// has processed the previous event. The oracle harness uses it to record
    /// its held-turn marker at exactly that rendezvous.
    #[cfg(test)]
    receipt: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
}
enum WeixinStreamItem {
    Event(JsValue),
    Failed(String),
}
impl WeixinTurnStream {
    pub async fn next(&mut self) -> Result<Option<JsValue>, String> {
        #[cfg(test)]
        if let Some(hook) = &self.receipt {
            hook();
        }
        match self.receiver.recv().await {
            Some(WeixinStreamItem::Event(event)) => Ok(Some(event)),
            Some(WeixinStreamItem::Failed(message)) => Err(message),
            None => Ok(None),
        }
    }
}

pub type WeixinClientFactory =
    Arc<dyn Fn(&SavedCredentials) -> Arc<dyn WeixinIlinkClient> + Send + Sync>;
pub type WeixinStateChange = Arc<dyn Fn(&JsValue) + Send + Sync>;

#[derive(Clone)]
pub struct WeixinChannelOptions {
    pub credentials_path: PathBuf,
    /// The directory `saveCredentials` mkdirs before writing
    /// (`PilotDeckServerShutdownBundle`-era TS: `join(homedir(), ".pilotdeck")`,
    /// WeixinChannel.ts:990-998). Defaults to the process `HOME`; tests point it
    /// inside their scratch directory — the TS generator does the same by
    /// overriding `process.env.HOME` — so a replay never writes into the
    /// machine's real `~/.pilotdeck`.
    pub credentials_dir: Option<PathBuf>,
    pub mapper: Arc<WeixinSessionMapper>,
    pub client_factory: WeixinClientFactory,
    pub login: WeixinLogin,
    pub on_state_change: Option<WeixinStateChange>,
    pub gateway: Option<Arc<dyn WeixinGatewayPort>>,
    pub runtime_status: Option<WeixinRuntimeStatusReporter>,
    /// `WeixinChannel.ts:666-676`: the weixin overrides over the TS
    /// `ImLiveReplyController` defaults (watchdog off, 10-minute activity
    /// delay, no update cap, weixin timeout text and activity formatter).
    pub live_reply: LiveReplyOptions,
}
impl Default for WeixinChannelOptions {
    fn default() -> Self {
        Self::with_login_base_url(default_login_base_url())
    }
}
impl WeixinChannelOptions {
    /// Default seams with the QR login pointed at `base_url` (tests use the
    /// loopback mock; production keeps the auth.js default).
    pub fn with_login_base_url(base_url: String) -> Self {
        Self {
            credentials_path: default_credentials_path(),
            credentials_dir: None,
            mapper: Arc::new(WeixinSessionMapper::new()),
            client_factory: default_client_factory(),
            login: Arc::new(move |callbacks| {
                let base_url = base_url.clone();
                Box::pin(async move { login_with_qr(callbacks, &base_url).await })
            }),
            on_state_change: None,
            gateway: None,
            runtime_status: None,
            live_reply: weixin_live_reply_options(),
        }
    }
}

/// The weixin `ImLiveReplyController` option overrides
/// (WeixinChannel.ts:666-676, constants at :34-39): the turn watchdog is off
/// by default, activity pulses wait ten minutes, and the activity/timeout
/// texts are the weixin ones.
pub fn weixin_live_reply_options() -> LiveReplyOptions {
    LiveReplyOptions {
        turn_timeout_ms: 0,
        activity_delay_ms: 600_000,
        activity_update_throttle_ms: 600_000,
        activity_max_updates: u32::MAX,
        activity_ttl_ms: u64::MAX,
        timeout_final_text: WEIXIN_TIMEOUT_FINAL_TEXT.into(),
        format_activity: Arc::new(live_reply::format_weixin_activity),
        ..LiveReplyOptions::default()
    }
}

/// ImChatSessionState.ts port (per-chat turn serialization and `/new`
/// generation invalidation), pr1 turn payload (no attachments).
#[derive(Default)]
struct ChatState {
    pending_turns: HashMap<JsString, Vec<QueuedWeixinTurn>>,
    generations: HashMap<JsString, u64>,
    active_runs: HashMap<JsString, ActiveRun>,
}
#[derive(Clone, Debug)]
struct QueuedWeixinTurn {
    session_key: JsString,
    message: JsString,
    project_key: Option<JsString>,
}
#[derive(Clone, Debug)]
struct ActiveRun {
    session_key: JsString,
    run_id: Option<JsString>,
    /// Stored for the `ImActiveRun` shape; the channel never reads it.
    #[allow(dead_code)]
    generation: u64,
}
impl ChatState {
    fn generation(&self, chat_id: &JsString) -> u64 {
        self.generations.get(chat_id).copied().unwrap_or(0)
    }
    fn is_current(&self, chat_id: &JsString, generation: u64) -> bool {
        self.generation(chat_id) == generation
    }
    fn reset_for_new_session(&mut self, chat_id: &JsString) {
        let generation = self.generation(chat_id) + 1;
        self.generations.insert(chat_id.clone(), generation);
        self.pending_turns.remove(chat_id);
        self.active_runs.remove(chat_id);
    }
    fn queue_turn(&mut self, chat_id: &JsString, turn: QueuedWeixinTurn) {
        let pending = self.pending_turns.entry(chat_id.clone()).or_default();
        pending.push(turn);
        if pending.len() > WEIXIN_MAX_PENDING_TURNS_PER_CHAT {
            let excess = pending.len() - WEIXIN_MAX_PENDING_TURNS_PER_CHAT;
            pending.drain(..excess);
        }
    }
    fn shift_turn(&mut self, chat_id: &JsString) -> Option<QueuedWeixinTurn> {
        let pending = self.pending_turns.get_mut(chat_id)?;
        if pending.is_empty() {
            return None;
        }
        let next = pending.remove(0);
        if pending.is_empty() {
            self.pending_turns.remove(chat_id);
        }
        Some(next)
    }
    fn set_active_run(&mut self, chat_id: &JsString, run: ActiveRun) {
        self.active_runs.insert(chat_id.clone(), run);
    }
    fn active_run(&self, chat_id: &JsString) -> Option<ActiveRun> {
        self.active_runs.get(chat_id).cloned()
    }
    fn clear_active_run(&mut self, chat_id: &JsString) {
        self.active_runs.remove(chat_id);
    }
}

/// JS `Set` insertion order for the chat sets the notification paths iterate.
#[derive(Default)]
struct OrderedSet {
    items: Vec<JsString>,
}
impl OrderedSet {
    fn insert(&mut self, value: &JsString) {
        if !self.items.iter().any(|item| item == value) {
            self.items.push(value.clone());
        }
    }
    fn snapshot(&self) -> Vec<JsString> {
        self.items.clone()
    }
    fn remove(&mut self, value: &JsString) {
        self.items.retain(|item| item != value);
    }
    fn contains(&self, value: &JsString) -> bool {
        self.items.iter().any(|item| item == value)
    }
}

/// #1780: releases a chat's `active_chats` slot when its queued-turn task
/// ends, including on the panic path (`process_message` unwinds, the bare
/// `tokio::spawn` task dies, and nothing else would remove the marker).
/// The drop path only takes the same `Inner` lock every other accessor uses;
/// `lock_inner` recovers a poisoned lock, so the guard itself cannot panic
/// while the process unwinds (a `Drop` panic aborts the process).
struct ActiveChatSlot {
    channel: WeixinChannel,
    user: JsString,
}
impl Drop for ActiveChatSlot {
    fn drop(&mut self) {
        self.channel.lock_inner().active_chats.remove(&self.user);
    }
}

struct Inner {
    generation: u64,
    abort: Option<Cancellation>,
    client: Option<Arc<dyn WeixinIlinkClient>>,
    poll_task: Option<JoinHandle<()>>,
    login_task: Option<JoinHandle<()>>,
    recovery_task: Option<JoinHandle<()>>,
    recovery_timer: Option<JoinHandle<()>>,
    active_chats: OrderedSet,
    chat_state: ChatState,
    context_tokens: HashMap<JsString, JsString>,
    consecutive_poll_errors: u32,
    connection_issue_notified: bool,
    connection_issue_chats: OrderedSet,
    connection_lost_notice_delivered_chats: OrderedSet,
    pending_replies: HashMap<JsString, Vec<JsString>>,
    current_login_qr_url: Option<JsString>,
}

struct Shared {
    key: ChannelKey,
    credentials_path: PathBuf,
    credentials_dir: PathBuf,
    mapper: Arc<WeixinSessionMapper>,
    client_factory: WeixinClientFactory,
    login: WeixinLogin,
    on_state_change: Option<WeixinStateChange>,
    gateway: Option<Arc<dyn WeixinGatewayPort>>,
    runtime_status: Option<WeixinRuntimeStatusReporter>,
    live_reply: LiveReplyOptions,
    logger: Mutex<Option<ChannelLog>>,
    inner: Mutex<Inner>,
}

/// The adapter. Cheap to clone; all mutable state is behind one lock, as in
/// the single-threaded TS object.
#[derive(Clone)]
pub struct WeixinChannel(Arc<Shared>);

impl WeixinChannel {
    pub fn new(options: WeixinChannelOptions) -> Self {
        Self(Arc::new(Shared {
            key: ChannelKey::new(CHANNEL_KEY),
            credentials_path: options.credentials_path,
            credentials_dir: options.credentials_dir.unwrap_or_else(home_pilotdeck_dir),
            mapper: options.mapper,
            client_factory: options.client_factory,
            login: options.login,
            on_state_change: options.on_state_change,
            gateway: options.gateway,
            runtime_status: options.runtime_status,
            live_reply: options.live_reply,
            logger: Mutex::new(None),
            inner: Mutex::new(Inner {
                generation: 0,
                abort: None,
                client: None,
                poll_task: None,
                login_task: None,
                recovery_task: None,
                recovery_timer: None,
                active_chats: OrderedSet::default(),
                chat_state: ChatState::default(),
                context_tokens: HashMap::new(),
                consecutive_poll_errors: 0,
                connection_issue_notified: false,
                connection_issue_chats: OrderedSet::default(),
                connection_lost_notice_delivered_chats: OrderedSet::default(),
                pending_replies: HashMap::new(),
                current_login_qr_url: None,
            }),
        }))
    }

    fn lock_inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn log(&self, level: &str, text: String) {
        let logger = self
            .0
            .logger
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(log) = logger {
            log(level, &text);
        }
    }

    fn report_status(
        &self,
        state: WeixinChannelState,
        message: impl Into<String>,
        details: impl FnOnce(&mut WeixinChannelStatus),
    ) {
        if let Some(report) = &self.0.runtime_status {
            let mut status = WeixinChannelStatus {
                state,
                message: message.into(),
                account_id: None,
                error: None,
                qr_url: None,
            };
            details(&mut status);
            report(status);
        }
    }

    fn is_stale_generation(&self, generation: u64) -> bool {
        let inner = self.lock_inner();
        inner.abort.as_ref().is_some_and(|abort| abort.is_aborted())
            || inner.generation != generation
    }

    // ── credentials (synchronous fs, like the TS module) ─────────────────

    fn load_credentials(&self) -> Option<SavedCredentials> {
        let raw = std::fs::read_to_string(&self.0.credentials_path).ok()?;
        let value = parse_json(&raw).ok()?;
        let field = |name: &str| {
            value
                .get(name)
                .and_then(JsValue::as_string)
                .filter(|text| !text.units().is_empty())
                .map(|text| String::from_utf16_lossy(text.units()))
        };
        Some(SavedCredentials {
            base_url: field("baseUrl")?,
            bot_token: field("botToken")?,
            account_id: field("accountId")?,
            cursor: field("cursor"),
        })
    }

    fn save_credentials(&self, creds: &SavedCredentials) {
        let write = (|| -> std::io::Result<()> {
            // TS mkdirs `homedir()/.pilotdeck` regardless of credentialsPath.
            std::fs::create_dir_all(&self.0.credentials_dir)?;
            let mut object = JsObject::new();
            object.insert("baseUrl".into(), JsValue::from(creds.base_url.as_str()));
            object.insert("botToken".into(), JsValue::from(creds.bot_token.as_str()));
            object.insert("accountId".into(), JsValue::from(creds.account_id.as_str()));
            if let Some(cursor) = &creds.cursor {
                object.insert("cursor".into(), JsValue::from(cursor.as_str()));
            }
            std::fs::write(
                &self.0.credentials_path,
                state::stringify_pretty(&JsValue::Object(object)),
            )
        })();
        if let Err(error) = write {
            self.log(
                "error",
                format!("weixin: failed to save credentials: {error}"),
            );
        }
    }

    fn save_cursor(&self) {
        let cursor = self
            .lock_inner()
            .client
            .as_ref()
            .map(|client| client.cursor());
        let Some(cursor) = cursor else {
            return;
        };
        if let Some(mut creds) = self.load_credentials() {
            creds.cursor = Some(cursor);
            self.save_credentials(&creds);
        }
    }

    // ── start / stop ──────────────────────────────────────────────────────

    fn start_polling_with_credentials(&self, creds: SavedCredentials) {
        {
            let mut inner = self.lock_inner();
            if inner.poll_task.is_some() {
                self.log(
                    "warn",
                    "weixin: poll loop already running, skip duplicate start".into(),
                );
                return;
            }
            let client = (self.0.client_factory)(&creds);
            // TS createClient: an empty cursor is falsy and not restored.
            if let Some(cursor) = creds.cursor.as_ref().filter(|cursor| !cursor.is_empty()) {
                client.set_cursor(cursor);
            }
            // startPollingWithCredentials installs a fresh AbortController.
            let abort = Cancellation::new();
            inner.abort = Some(abort.clone());
            inner.client = Some(client);
            let task = tokio::spawn(self.clone().poll_loop(abort));
            inner.poll_task = Some(task);
        }
        self.log("info", "weixin: connected, poll loop started".into());
        self.report_status(
            WeixinChannelState::Connected,
            "weixin: connected",
            |status| status.account_id = Some(JsString::from(creds.account_id.as_str())),
        );
    }

    fn start_login_recovery_watcher(&self) {
        let abort = {
            let inner = self.lock_inner();
            if inner.recovery_task.is_some() {
                return;
            }
            inner.abort.clone().unwrap_or_default()
        };
        self.log(
            "warn",
            "weixin: QR login failed, watching credentials for recovery".into(),
        );
        let task = tokio::spawn(self.clone().watch_credentials_for_login_recovery(abort));
        self.lock_inner().recovery_task = Some(task);
    }

    async fn watch_credentials_for_login_recovery(self, abort: Cancellation) {
        let deadline =
            tokio::time::Instant::now() + Duration::from_millis(WEIXIN_LOGIN_RECOVERY_TIMEOUT_MS);
        loop {
            if abort.is_aborted() || self.lock_inner().poll_task.is_some() {
                return;
            }
            if let Some(creds) = self.load_credentials() {
                self.log(
                    "info",
                    format!(
                        "weixin: loaded saved credentials (account: {})",
                        creds.account_id
                    ),
                );
                self.clear_login_recovery_timer();
                self.start_polling_with_credentials(creds);
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                self.log("warn", "weixin: QR login recovery timed out".into());
                return;
            }
            self.sleep_login_recovery(&abort).await;
        }
    }

    async fn sleep_login_recovery(&self, abort: &Cancellation) {
        let (fired, mut fired_rx) = tokio::sync::oneshot::channel::<()>();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(
                WEIXIN_LOGIN_RECOVERY_CHECK_INTERVAL_MS,
            ))
            .await;
            let _ = fired.send(());
        });
        self.lock_inner().recovery_timer = Some(timer);
        tokio::select! {
            _ = &mut fired_rx => {}
            _ = abort.cancelled() => {}
        }
        self.clear_login_recovery_timer();
    }

    fn clear_login_recovery_timer(&self) {
        if let Some(timer) = self.lock_inner().recovery_timer.take() {
            timer.abort();
        }
    }

    fn start_qr_login_in_background(&self, generation: u64) {
        {
            let mut inner = self.lock_inner();
            // TS guards on the login promise; a finished task has already
            // run its `.finally`.
            if inner
                .login_task
                .as_ref()
                .is_some_and(|task| !task.is_finished())
            {
                self.log("warn", "weixin: QR login already running".into());
                return;
            }
            inner.current_login_qr_url = None;
        }
        self.log(
            "info",
            "weixin: no credentials found, starting QR login...".into(),
        );
        self.report_status(
            WeixinChannelState::WaitingForLogin,
            "微信等待扫码登录",
            |_| {},
        );
        // The TS channel prints its QR banner through console.log
        // (WeixinChannel.ts:277-280); the adapter logger and the console
        // share the log sink in the assembly, so both reach the harness.
        self.log(
            "info",
            "\n╔══════════════════════════════════════════════╗".into(),
        );
        self.log(
            "info",
            "║  微信 iLink 登录 — 请用微信扫描二维码        ║".into(),
        );
        self.log(
            "info",
            "╚══════════════════════════════════════════════╝\n".into(),
        );
        self.log(
            "info",
            "[weixin] 等待扫码登录；PilotDeck Web UI 已可继续使用。\n".into(),
        );
        let task = tokio::spawn(self.clone().run_qr_login(generation));
        self.lock_inner().login_task = Some(task);
    }

    async fn run_qr_login(self, generation: u64) {
        let channel = self.clone();
        let on_qr_code = Box::new(move |url: &JsString| {
            if channel.is_stale_generation(generation) {
                return;
            }
            channel.lock_inner().current_login_qr_url = Some(url.clone());
            channel.report_status(
                WeixinChannelState::WaitingForLogin,
                "微信等待扫码登录",
                |status| status.qr_url = Some(url.clone()),
            );
            // WeixinChannel.ts:296 — the QR link banner is a console.log.
            channel.log(
                "info",
                format!(
                    "[weixin] 扫码登录链接:\n{}\n",
                    String::from_utf16_lossy(url.units())
                ),
            );
        });
        let channel = self.clone();
        let on_status_change = Box::new(move |status: &str| {
            if channel.is_stale_generation(generation) {
                return;
            }
            let labels = match status {
                "waiting" => "等待扫码...",
                "scanned" => "已扫码，等待确认...",
                "expired" => "二维码已过期，正在刷新...",
                "refreshing" => "刷新中...",
                other => other,
            };
            channel.log("info", format!("[weixin] {labels}"));
            if matches!(status, "waiting" | "scanned" | "refreshing") {
                let qr_url = channel.lock_inner().current_login_qr_url.clone();
                channel.report_status(
                    WeixinChannelState::WaitingForLogin,
                    format!("weixin: {labels}"),
                    |report| report.qr_url = qr_url,
                );
            }
        });
        let result = (self.0.login)(WeixinLoginCallbacks {
            on_qr_code,
            on_status_change,
        })
        .await;
        match result {
            Ok(result) => {
                if !self.is_stale_generation(generation) {
                    let creds = SavedCredentials {
                        base_url: result.base_url,
                        bot_token: result.bot_token,
                        account_id: result.account_id,
                        cursor: None,
                    };
                    self.save_credentials(&creds);
                    self.lock_inner().current_login_qr_url = None;
                    // WeixinChannel.ts:323 — the success banner is a console.log.
                    self.log(
                        "info",
                        format!("[weixin] 登录成功! accountId: {}\n", creds.account_id),
                    );
                    self.log(
                        "info",
                        format!("weixin: login successful, accountId={}", creds.account_id),
                    );
                    self.start_polling_with_credentials(creds);
                }
            }
            Err(error) => {
                if !self.is_stale_generation(generation) {
                    // TS formatWeixinError also appends `code=`/`cause=`/`at=`
                    // pieces; those are host-specific and never appear in the
                    // replayed (mock-server) failures.
                    let message = error.formatted();
                    self.lock_inner().current_login_qr_url = None;
                    self.log("error", format!("weixin: QR login failed: {error}"));
                    // WeixinChannel.ts:331 — the failure line is a console.error
                    // printing the error itself (`${e}`).
                    self.log("error", format!("[weixin] 登录失败: {error}"));
                    self.report_status(
                        WeixinChannelState::Failed,
                        "weixin: QR login failed",
                        |status| status.error = Some(message),
                    );
                    self.start_login_recovery_watcher();
                }
            }
        }
        self.finish_qr_login(generation);
    }

    /// The `.finally` on `runQrLogin`: clear the running marker only when the
    /// generation is still current.
    fn finish_qr_login(&self, generation: u64) {
        let mut inner = self.lock_inner();
        if inner.generation == generation {
            inner.login_task = None;
        }
    }

    async fn stop(self, reason: &str) -> Result<(), ChannelError> {
        self.log("info", format!("weixin: stopping ({reason})"));
        let (poll_task, recovery_task) = {
            let mut inner = self.lock_inner();
            inner.generation += 1;
            if let Some(abort) = &inner.abort {
                abort.abort();
            }
            if let Some(timer) = inner.recovery_timer.take() {
                timer.abort();
            }
            (inner.poll_task.take(), inner.recovery_task.take())
        };
        self.save_cursor();
        // TS awaits the poll and recovery promises; the QR login task only
        // gets its marker dropped (stale-generation checks end it).
        self.lock_inner().login_task = None;
        if let Some(task) = poll_task {
            let _ = task.await;
        }
        if let Some(task) = recovery_task {
            let _ = task.await;
        }
        self.report_status(WeixinChannelState::Stopped, "weixin: stopped", |_| {});
        Ok(())
    }

    // ── poll loop ─────────────────────────────────────────────────────────

    async fn poll_loop(self, abort: Cancellation) {
        loop {
            if abort.is_aborted() {
                break;
            }
            let client = self.lock_inner().client.clone();
            // TS reads `this.client` each iteration; the loop exits when it
            // is missing.
            let Some(client) = client else {
                break;
            };
            let response = match client.poll().await {
                Ok(response) => response,
                Err(error) => {
                    if abort.is_aborted() {
                        break;
                    }
                    let count = {
                        let mut inner = self.lock_inner();
                        inner.consecutive_poll_errors += 1;
                        inner.consecutive_poll_errors
                    };
                    self.log(
                        "error",
                        format!("weixin: poll error #{count}: {}", error.formatted()),
                    );
                    self.notify_connection_lost().await;
                    self.rebuild_client_after_poll_error(&error);
                    self.sleep(&abort, POLL_RETRY_DELAY_MS).await;
                    continue;
                }
            };
            if response.get("errcode") == Some(&JsValue::Number(-14.0)) {
                self.log(
                    "error",
                    "weixin: session expired (errcode -14), need re-login".into(),
                );
                // WeixinChannel.ts:357-358 — the expiry notice is a
                // console.error pair carrying the credentials path.
                self.log(
                    "error",
                    "[weixin] Session 过期，请删除凭证文件并重启以重新扫码登录:".into(),
                );
                self.log(
                    "error",
                    format!("[weixin]   rm {}", self.0.credentials_path.display()),
                );
                self.report_status(
                    WeixinChannelState::Expired,
                    "微信登录已过期，请重新扫码登录",
                    |_| {},
                );
                self.notify_active_chats(&JsString::from(WEIXIN_SESSION_EXPIRED_TEXT))
                    .await;
                break;
            }
            let ret = response.get("ret").cloned().unwrap_or(JsValue::Undefined);
            if !ret.is_undefined() && ret != JsValue::Number(0.0) {
                self.log(
                    "warn",
                    format!(
                        "weixin: poll ret={} errmsg={}",
                        js_template_text(&ret),
                        js_template_text(
                            &response
                                .get("errmsg")
                                .cloned()
                                .unwrap_or(JsValue::Undefined)
                        ),
                    ),
                );
                self.notify_connection_lost().await;
                self.sleep(&abort, POLL_RETRY_DELAY_MS).await;
                continue;
            }
            let recovered = {
                let mut inner = self.lock_inner();
                if inner.consecutive_poll_errors > 0 {
                    let count = inner.consecutive_poll_errors;
                    inner.consecutive_poll_errors = 0;
                    Some(count)
                } else {
                    None
                }
            };
            if let Some(count) = recovered {
                self.log(
                    "info",
                    format!("weixin: poll recovered after {count} error(s)"),
                );
            }
            self.notify_connection_recovered().await;
            self.flush_pending_replies().await;
            let messages: Vec<JsValue> = match response.get("msgs") {
                Some(JsValue::Array(items)) => items.clone(),
                _ => Vec::new(),
            };
            if !messages.is_empty() {
                self.log(
                    "info",
                    format!("weixin: polled {} message(s)", messages.len()),
                );
            }
            for message in messages {
                let is_text = message
                    .get("message_type")
                    .map(|value| value == &JsValue::Number(MESSAGE_KIND_TEXT))
                    .unwrap_or(false);
                if is_text {
                    // The synchronous prefix of `void dispatchMessage(msg)`
                    // runs inline, as it does before the TS first await.
                    if let Some(start) = self.dispatch_sync_prefix(&message) {
                        self.spawn_dispatch(start);
                    }
                }
            }
            self.save_cursor();
        }
    }

    /// Everything `dispatchMessage` does before its first await: context
    /// token capture, text extraction, mapper resolution, and the `/new`
    /// state rotation. The remainder runs in the spawned task.
    fn dispatch_sync_prefix(&self, message: &JsValue) -> Option<DispatchStart> {
        let from_user = message
            .get("from_user_id")
            .and_then(JsValue::as_string)
            .cloned()?;
        if from_user.units().is_empty() {
            return None;
        }
        if let Some(token) = message
            .get("context_token")
            .and_then(JsValue::as_string)
            .filter(|token| !token.units().is_empty())
        {
            self.lock_inner()
                .context_tokens
                .insert(from_user.clone(), token.clone());
        }
        // pr1 extraction: text items only (image/file/voice are pr2).
        let mut text_parts: Vec<JsString> = Vec::new();
        if let Some(JsValue::Array(items)) = message.get("item_list") {
            for item in items {
                if item.get("type") != Some(&JsValue::Number(client::MESSAGE_ITEM_TYPE_TEXT)) {
                    continue;
                }
                if let Some(text) = item
                    .get("text_item")
                    .and_then(|value| value.get("text"))
                    .and_then(JsValue::as_string)
                    .filter(|text| !text.units().is_empty())
                {
                    text_parts.push(text.clone());
                }
            }
        }
        let joined = String::from_utf16_lossy(join_units(&text_parts).units());
        let text = crate::metadata::js_trim(&joined);
        // pr1: attachments are always empty, so the attachment fallback text
        // is unreachable and `messageText` is the trimmed text.
        if text.is_empty() {
            return None;
        }
        self.log(
            "info",
            format!(
                "weixin: received message from {} attachments=0",
                String::from_utf16_lossy(from_user.units())
            ),
        );
        let previous_session_key = self.0.mapper.get_session(&from_user);
        let mapped = self.0.mapper.resolve(&from_user, &JsString::from(text));
        if mapped.command == Some(WeixinCommand::New) {
            if let Some(on_state_change) = &self.0.on_state_change {
                on_state_change(&self.0.mapper.snapshot());
            }
            let active_run = {
                let mut inner = self.lock_inner();
                let active_run = inner.chat_state.active_run(&from_user);
                inner.chat_state.reset_for_new_session(&from_user);
                inner.pending_replies.remove(&from_user);
                active_run
            };
            let follow_up = if mapped.message.units().is_empty() {
                None
            } else {
                Some(QueuedWeixinTurn {
                    session_key: mapped.session_key,
                    message: mapped.message,
                    project_key: mapped.project_key,
                })
            };
            return Some(DispatchStart::NewSession {
                user: from_user,
                active_run,
                previous_session_key,
                follow_up,
            });
        }
        if mapped.message.units().is_empty() {
            return None;
        }
        Some(DispatchStart::Turn {
            user: from_user,
            turn: QueuedWeixinTurn {
                session_key: mapped.session_key,
                message: mapped.message,
                project_key: mapped.project_key,
            },
        })
    }

    fn spawn_dispatch(&self, start: DispatchStart) {
        match start {
            DispatchStart::NewSession {
                user,
                active_run,
                previous_session_key,
                follow_up,
            } => {
                let channel = self.clone();
                tokio::spawn(async move {
                    if let Some(gateway) = channel.0.gateway.clone() {
                        let session_key = active_run
                            .as_ref()
                            .map(|run| run.session_key.clone())
                            .unwrap_or(previous_session_key);
                        let run_id = active_run.as_ref().and_then(|run| run.run_id.clone());
                        if let Err(error) = gateway
                            .abort_turn(&session_key, run_id.as_ref(), None)
                            .await
                        {
                            channel.log(
                                "warn",
                                format!("weixin: abort previous session on /new failed: {error}"),
                            );
                        }
                    }
                    match follow_up {
                        None => {
                            channel
                                .send_reply(&user, &JsString::from(WEIXIN_NEW_SESSION_TEXT), false)
                                .await;
                        }
                        Some(turn) => {
                            let _ = channel.enqueue_or_run(user, turn);
                        }
                    }
                });
            }
            DispatchStart::Turn { user, turn } => {
                // Inline, so a later message in the same poll batch sees the
                // chat as active exactly when TS's microtask would.
                let _ = self.enqueue_or_run(user, turn);
            }
        }
    }

    /// Returns the spawned turn task's handle so tests can observe the exact
    /// cancellation window (#1780); production callers drop it.
    fn enqueue_or_run(
        &self,
        user: JsString,
        turn: QueuedWeixinTurn,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let run_now = {
            let mut inner = self.lock_inner();
            if inner.active_chats.contains(&user) {
                inner.chat_state.queue_turn(&user, turn.clone());
                false
            } else {
                inner.active_chats.insert(&user);
                true
            }
        };
        if !run_now {
            self.log(
                "info",
                format!(
                    "weixin: chat {} already active, queued message",
                    String::from_utf16_lossy(user.units())
                ),
            );
            return None;
        }
        // #1780: TS clears the marker in `finally`, even on a rejected turn —
        // and on a panicking one. This task is a bare `tokio::spawn`, so a
        // panic in `process_message` only kills the task; without the guard
        // the chat would stay in `active_chats` and every later message of
        // that user would queue behind a dead consumer forever.
        //
        // The guard is taken *here*, after the admission insert and before the
        // spawn, so the marker is owned from spawn rather than from the task's
        // first poll: a task cancelled while still queued (runtime shutdown
        // drops un-polled tasks) would otherwise never build the guard and
        // leak the marker. Moving it into the future makes `Drop` run on the
        // normal return, on unwind, and on cancellation before the first poll.
        //
        // Same `Drop`-guard shape as `channel::ActiveChat` (#1738); that type
        // owns a standalone `Arc<Mutex<HashSet<_>>>`, while this set lives in
        // the channel's single `Inner` mutex and must keep JS `Set` insertion
        // order for the notification paths, so release through `lock_inner`.
        let active = ActiveChatSlot {
            channel: self.clone(),
            user: user.clone(),
        };
        Some(tokio::spawn(
            self.clone().run_queued_turns(user, turn, active),
        ))
    }

    async fn run_queued_turns(
        self,
        user: JsString,
        first_turn: QueuedWeixinTurn,
        active: ActiveChatSlot,
    ) {
        // Built by `enqueue_or_run` before the spawn and bound for the whole
        // body, so `Drop` releases the marker on every exit path.
        let _active = active;
        let mut next = Some(first_turn);
        while let Some(turn) = next.take() {
            self.process_message(&user, &turn).await;
            next = self.lock_inner().chat_state.shift_turn(&user);
        }
        // #1725#4: the `shift_turn` above and the marker release in `_active`'s
        // drop are two separate lock sections; a message arriving in between
        // queues with nobody left to drain it. Not changed here — this fix is
        // panic safety only, the double-lock window stays under #1725#4.
    }

    async fn process_message(&self, user: &JsString, turn: &QueuedWeixinTurn) {
        let Some(gateway) = self.0.gateway.clone() else {
            return;
        };
        let generation = self.lock_inner().chat_state.generation(user);
        let is_current = || self.lock_inner().chat_state.is_current(user, generation);
        // `void this.sendTypingIfPossible(userId)` — unawaited.
        let typing_channel = self.clone();
        let typing_user = user.clone();
        tokio::spawn(async move {
            typing_channel.send_typing_if_possible(&typing_user).await;
        });
        let mut input = JsObject::new();
        input.insert(
            "sessionKey".into(),
            JsValue::String(turn.session_key.clone()),
        );
        input.insert("channelKey".into(), JsValue::from(CHANNEL_KEY));
        input.insert("message".into(), JsValue::String(turn.message.clone()));
        input.insert("allowPlanModeTools".into(), JsValue::Bool(false));
        if let Some(project_key) = &turn.project_key {
            input.insert("projectKey".into(), JsValue::String(project_key.clone()));
        }
        let turn_timeout_ms = self.0.live_reply.turn_timeout_ms;
        if turn_timeout_ms > 0 {
            input.insert("timeoutMs".into(), JsValue::Number(turn_timeout_ms as f64));
        }
        let mut stream = gateway.submit_turn(JsValue::Object(input));
        let mut live_options = self.0.live_reply.clone();
        let error_channel = self.clone();
        live_options.on_transport_error = Some(Arc::new(move |error: &str, phase: &str| {
            error_channel.log(
                "warn",
                format!("weixin: live reply {phase} failed: {error}"),
            );
        }));
        let mut reply =
            LiveReplyController::new(self.weixin_live_reply_transport(user), live_options);
        let mut active_run_id: Option<JsString> = None;
        let mut failed = false;
        loop {
            // The `deadline` is `Copy` and the timer future borrows nothing,
            // so `reply` stays usable in both select arms.
            let deadline = reply.next_deadline();
            let timer = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };
            let next = tokio::select! {
                biased;
                item = stream.next() => Some(item),
                _ = timer => None,
            };
            let Some(item) = next else {
                let fired = reply.fire_due_timers().await;
                if fired.turn_timed_out {
                    self.log(
                        "warn",
                        format!(
                            "weixin: live reply timed out for user {}",
                            String::from_utf16_lossy(user.units())
                        ),
                    );
                    if let Err(message) = gateway
                        .abort_turn(
                            &turn.session_key,
                            active_run_id.as_ref(),
                            Some("system:timeout"),
                        )
                        .await
                    {
                        self.log(
                            "warn",
                            format!("weixin: abortTurn after timeout failed: {message}"),
                        );
                    }
                }
                continue;
            };
            let event = match item {
                Ok(Some(event)) => event,
                Ok(None) => break,
                Err(message) => {
                    self.log("error", format!("weixin: submitTurn error: {message}"));
                    failed = true;
                    break;
                }
            };
            if !is_current() {
                break;
            }
            let event_type = js_string_text(event.get("type"));
            match event_type.as_str() {
                "turn_started" => {
                    let run_id = event
                        .get("runId")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap_or_else(|| JsString::from(""));
                    active_run_id = Some(run_id.clone());
                    self.lock_inner().chat_state.set_active_run(
                        user,
                        ActiveRun {
                            session_key: turn.session_key.clone(),
                            run_id: Some(run_id),
                            generation,
                        },
                    );
                    reply.handle_event(&event).await;
                }
                // After a turn_timeout, the cached timeout notice wins over
                // the later agent_aborted terminal text (WeixinChannel.ts:741-747).
                "error"
                    if !reply.is_timed_out()
                        && js_string_text(event.get("code")) == "agent_aborted" =>
                {
                    reply.mark_aborted().await;
                }
                "error" if js_string_text(event.get("code")) == "turn_timeout" => {
                    reply.mark_timed_out().await;
                }
                // Prompt delivery lands with pr2c; both paths pause the
                // activity pulses first (WeixinChannel.ts:723-730).
                "elicitation_request" | "permission_request" => {
                    reply.pause_activity().await;
                }
                _ => reply.handle_event(&event).await,
            }
        }
        if failed {
            // The TS catch path feeds the visible `channel_submit_failed`
            // agent status into the live reply controller.
            reply.channel_submit_failed().await;
        }
        if is_current() {
            self.lock_inner().chat_state.clear_active_run(user);
            reply.flush_final().await;
        }
    }

    /// `WeixinChannel.ts:786-800 createLiveReplyTransport`: only `send`
    /// (fire-and-forget, queue-on-failure), `pulseActivity` (reply + typing
    /// refresh) and `stopActivity` (no-op) — no `edit`, no `maxMessageLength`.
    fn weixin_live_reply_transport(&self, user: &JsString) -> LiveReplyTransport<()> {
        let send_channel = self.clone();
        let send_user = user.clone();
        let send: Arc<
            dyn Fn(String) -> live_reply::TransportFuture<SendOutcome<()>> + Send + Sync,
        > = Arc::new(move |text: String| {
            let channel = send_channel.clone();
            let user = send_user.clone();
            Box::pin(async move {
                let text = JsString::from(text);
                channel.send_reply(&user, &text, true).await;
                // TS `send` resolves `undefined`; a failed weixin send never
                // enters the edit fallback.
                Ok(SendOutcome::NoHandle)
            })
        });
        let pulse_channel = self.clone();
        let pulse_user = user.clone();
        let pulse_activity: Option<
            Arc<
                dyn Fn(live_reply::LiveReplyActivity) -> live_reply::TransportFuture<bool>
                    + Send
                    + Sync,
            >,
        > = Some(Arc::new(move |activity: live_reply::LiveReplyActivity| {
            let channel = pulse_channel.clone();
            let user = pulse_user.clone();
            Box::pin(async move {
                let text = JsString::from(activity.text);
                channel.send_reply(&user, &text, false).await;
                channel.send_typing_if_possible(&user).await;
                Ok(true)
            })
        }));
        let stop_activity: Option<
            Arc<dyn Fn() -> live_reply::TransportFuture<bool> + Send + Sync>,
        > = Some(Arc::new(|| Box::pin(async { Ok(true) })));
        LiveReplyTransport {
            send,
            edit: None,
            delete: None,
            clear: None,
            pulse_activity,
            stop_activity,
            max_message_length: None,
            format_text: None,
        }
    }

    async fn sleep(&self, abort: &Cancellation, ms: u64) {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(ms)) => {}
            _ = abort.cancelled() => {}
        }
    }

    fn rebuild_client_after_poll_error(&self, error: &WeixinClientError) {
        if !is_recoverable_poll_error(&error.formatted()) {
            return;
        }
        let Some(mut creds) = self.load_credentials() else {
            self.log(
                "warn",
                "weixin: cannot rebuild iLink client because credentials are missing".into(),
            );
            return;
        };
        // `this.client?.cursor || creds.cursor`.
        let cursor = self
            .lock_inner()
            .client
            .as_ref()
            .map(|client| client.cursor())
            .filter(|cursor| !cursor.is_empty())
            .or_else(|| creds.cursor.clone());
        let client = (self.0.client_factory)(&creds);
        if let Some(cursor) = cursor.as_ref().filter(|cursor| !cursor.is_empty()) {
            client.set_cursor(cursor);
            if creds.cursor.as_deref() != Some(cursor) {
                creds.cursor = Some(cursor.clone());
                self.save_credentials(&creds);
            }
        }
        self.lock_inner().client = Some(client);
        self.log(
            "warn",
            "weixin: rebuilt iLink client after recoverable poll error".into(),
        );
    }

    // ── connection notifications ──────────────────────────────────────────

    async fn notify_connection_lost(&self) {
        {
            let mut inner = self.lock_inner();
            if inner.connection_issue_notified {
                return;
            }
            inner.connection_issue_notified = true;
            let active = inner.active_chats.snapshot();
            inner.connection_issue_chats = OrderedSet::default();
            for chat in active {
                inner.connection_issue_chats.insert(&chat);
            }
        }
        let delivered = self
            .notify_active_chats(&JsString::from(WEIXIN_CONNECTION_LOST_TEXT))
            .await;
        self.lock_inner().connection_lost_notice_delivered_chats = delivered;
    }

    async fn notify_connection_recovered(&self) {
        let chats_to_notify = {
            let mut inner = self.lock_inner();
            if !inner.connection_issue_notified {
                return;
            }
            inner.connection_issue_notified = false;
            let mut chats = OrderedSet::default();
            for chat in inner.connection_issue_chats.snapshot() {
                chats.insert(&chat);
            }
            for chat in inner.active_chats.snapshot() {
                chats.insert(&chat);
            }
            chats
        };
        let lost = self
            .lock_inner()
            .connection_lost_notice_delivered_chats
            .snapshot();
        for chat in chats_to_notify.snapshot() {
            let text = if lost.contains(&chat) {
                WEIXIN_CONNECTION_RECOVERED_TEXT
            } else {
                WEIXIN_CONNECTION_RECOVERED_AFTER_LOSS_TEXT
            };
            self.send_reply(&chat, &JsString::from(text), false).await;
        }
        let mut inner = self.lock_inner();
        inner.connection_issue_chats = OrderedSet::default();
        inner.connection_lost_notice_delivered_chats = OrderedSet::default();
    }

    /// Sends to every active chat concurrently in TS (`Promise.all`); the
    /// replayed scenarios use one chat, where the orders coincide.
    async fn notify_active_chats(&self, text: &JsString) -> OrderedSet {
        let mut delivered = OrderedSet::default();
        let chats = self.lock_inner().active_chats.snapshot();
        for chat in chats {
            if self.send_reply(&chat, text, false).await {
                delivered.insert(&chat);
            }
        }
        delivered
    }

    // ── replies ───────────────────────────────────────────────────────────

    async fn send_reply(
        &self,
        user_id: &JsString,
        text: &JsString,
        queue_on_failure: bool,
    ) -> bool {
        let client = self.lock_inner().client.clone();
        let Some(client) = client else {
            if queue_on_failure {
                self.queue_pending_reply(user_id, text);
            }
            return false;
        };
        let Some(context_token) = self.lock_inner().context_tokens.get(user_id).cloned() else {
            self.log(
                "warn",
                format!(
                    "weixin: no context_token for {}, cannot send",
                    String::from_utf16_lossy(user_id.units())
                ),
            );
            if queue_on_failure {
                self.queue_pending_reply(user_id, text);
            }
            return false;
        };
        match client
            .send_text_chunked(user_id, text, &context_token, 2000)
            .await
        {
            Ok(_) => {
                self.log(
                    "info",
                    format!(
                        "weixin: sent reply to {}",
                        String::from_utf16_lossy(user_id.units())
                    ),
                );
                true
            }
            Err(error) => {
                self.log(
                    "error",
                    format!("weixin: sendText failed: {}", error.formatted()),
                );
                if queue_on_failure {
                    self.queue_pending_reply(user_id, text);
                }
                false
            }
        }
    }

    fn queue_pending_reply(&self, user_id: &JsString, text: &JsString) {
        let mut inner = self.lock_inner();
        let pending = inner.pending_replies.entry(user_id.clone()).or_default();
        pending.push(text.clone());
        if pending.len() > WEIXIN_MAX_PENDING_REPLIES_PER_CHAT {
            let excess = pending.len() - WEIXIN_MAX_PENDING_REPLIES_PER_CHAT;
            pending.drain(..excess);
        }
    }

    async fn flush_pending_replies(&self) {
        let users: Vec<JsString> = {
            let inner = self.lock_inner();
            if inner.pending_replies.is_empty() {
                return;
            }
            inner.pending_replies.keys().cloned().collect()
        };
        for user in users {
            loop {
                let text = self
                    .lock_inner()
                    .pending_replies
                    .get(&user)
                    .and_then(|pending| pending.first().cloned());
                let Some(text) = text else {
                    break;
                };
                if !self.send_reply(&user, &text, false).await {
                    break;
                }
                let mut inner = self.lock_inner();
                let drained = inner.pending_replies.get_mut(&user).is_some_and(|pending| {
                    if !pending.is_empty() && pending[0] == text {
                        pending.remove(0);
                        true
                    } else {
                        false
                    }
                });
                if drained
                    && inner
                        .pending_replies
                        .get(&user)
                        .is_none_or(|pending| pending.is_empty())
                {
                    inner.pending_replies.remove(&user);
                }
            }
        }
    }

    async fn send_typing_if_possible(&self, user_id: &JsString) {
        let client = self.lock_inner().client.clone();
        let Some(client) = client else {
            return;
        };
        let Some(context_token) = self.lock_inner().context_tokens.get(user_id).cloned() else {
            return;
        };
        if let Err(error) = client.send_typing(user_id, &context_token).await {
            self.log(
                "warn",
                format!("weixin: sendTyping failed: {}", error.formatted()),
            );
        }
    }
}

enum DispatchStart {
    NewSession {
        user: JsString,
        active_run: Option<ActiveRun>,
        previous_session_key: JsString,
        follow_up: Option<QueuedWeixinTurn>,
    },
    Turn {
        user: JsString,
        turn: QueuedWeixinTurn,
    },
}

fn join_units(parts: &[JsString]) -> JsString {
    let mut units = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        units.extend_from_slice(part.units());
    }
    JsString::from_utf16(units)
}

/// JS template-literal text of a value (`${value}`): strings unquoted,
/// `undefined` prints `undefined`.
fn js_template_text(value: &JsValue) -> String {
    crate::js_value::display(value).unwrap_or_else(|_| "undefined".into())
}

fn js_string_text(value: Option<&JsValue>) -> String {
    value
        .and_then(JsValue::as_string)
        .map(|text| String::from_utf16_lossy(text.units()))
        .unwrap_or_default()
}

impl ChannelAdapter for WeixinChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.0.key
    }

    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let channel = self.clone();
        Box::pin(async move {
            *channel.0.logger.lock().unwrap_or_else(|e| e.into_inner()) = deps.log.clone();
            let generation = {
                let mut inner = channel.lock_inner();
                inner.generation += 1;
                let abort = Cancellation::new();
                inner.abort = Some(abort);
                inner.generation
            };
            if let Some(creds) = channel.load_credentials() {
                channel.log(
                    "info",
                    format!(
                        "weixin: loaded saved credentials (account: {})",
                        creds.account_id
                    ),
                );
                channel.start_polling_with_credentials(creds);
            } else {
                channel.start_qr_login_in_background(generation);
            }
            Ok(Arc::new(WeixinHandle(channel)) as Arc<dyn ChannelHandle>)
        })
    }
}

struct WeixinHandle(WeixinChannel);
impl ChannelHandle for WeixinHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        let channel = self.0.clone();
        let reason = if reason.is_empty() {
            "no reason".into()
        } else {
            reason.to_string()
        };
        Box::pin(async move { channel.stop(&reason).await })
    }
}

/// The production gateway port over the native turn submitter: every weixin
/// turn goes through the same `TurnSubmitter` admission as a WS submit, and
/// aborts follow the TS `abortTurn` contract (default reason, then wait for
/// the in-flight submission to unwind its completion fence).
pub struct NativeWeixinGatewayPort {
    submitter: Arc<crate::runtime::turn::submit::TurnSubmitter>,
    in_flight: Mutex<HashMap<JsString, watch::Receiver<bool>>>,
}

impl NativeWeixinGatewayPort {
    pub fn new(submitter: Arc<crate::runtime::turn::submit::TurnSubmitter>) -> Self {
        Self {
            submitter,
            in_flight: Mutex::new(HashMap::new()),
        }
    }
}

impl WeixinGatewayPort for NativeWeixinGatewayPort {
    fn submit_turn(&self, input: JsValue) -> WeixinTurnStream {
        let key = input
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .cloned();
        let (done, done_rx) = watch::channel(false);
        if let Some(key) = &key {
            self.in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key.clone(), done_rx);
        }
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let submitter = self.submitter.clone();
        tokio::spawn(async move {
            let mut submission = submitter.submit(input);
            loop {
                match submission.next().await {
                    Ok(Some(event)) => {
                        if sender.send(WeixinStreamItem::Event(event)).is_err() {
                            // Consumer dropped the stream: the TS iterator's
                            // `return()` releases the same completion fence.
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        // The TS `for await` throw formats the error value;
                        // SubmitError maps onto the native error shape.
                        let _ = sender.send(WeixinStreamItem::Failed(format!(
                            "Error: {}",
                            error.message
                        )));
                        break;
                    }
                }
            }
            done.send_replace(true);
        });
        WeixinTurnStream {
            receiver,
            #[cfg(test)]
            receipt: None,
        }
    }

    fn abort_turn<'a>(
        &'a self,
        session_key: &'a JsString,
        run_id: Option<&'a JsString>,
        reason: Option<&'a str>,
    ) -> WeixinAbortFuture<'a> {
        Box::pin(async move {
            // TS default reason: `aborted:{runId}` or `aborted`; the watchdog
            // overrides it with `system:timeout`.
            let reason = match reason {
                Some(reason) => JsString::from(reason),
                None => run_id
                    .map(|run| {
                        JsString::from(format!("aborted:{}", String::from_utf16_lossy(run.units())))
                    })
                    .unwrap_or_else(|| JsString::from("aborted")),
            };
            self.submitter
                .owner
                .start_gateway_abort(session_key, run_id, Some(&reason))
                .map_err(|error| error.to_string())?;
            // Model the TS async boundary before observing the fence, so an
            // abort racing the submit registration still observes it.
            tokio::task::yield_now().await;
            let waiter = self
                .in_flight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(session_key)
                .cloned();
            if let Some(mut waiter) = waiter {
                let _ = waiter.wait_for(|done| *done).await;
            }
            Ok(())
        })
    }
}
