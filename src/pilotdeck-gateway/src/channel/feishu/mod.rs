//! `resources/PilotDeck/src/adapters/channel/feishu/FeishuChannel.ts`
//! (baseline cc7c4e6) on the #1327 channel core: webhook-mode inbound
//! (JSON → optional AES decrypt → `url_verification` → verify-token gate →
//! direct/v2/v1 shapes), the 2000-event dedupe ring, per-chat inbound
//! batching, the `ImChatSessionState` queue/generation, the A7 submit path
//! and the Lark Open API outbound surface.
//!
//! Out of scope for this unit, each registered in the gateway-parity diff
//! ledger: the Lark WS (`connectionMode: "stream"`) transport, attachment
//! download/upload, the centralized system-command registry, `abortTurn` (the
//! A7 port has no abort), and the streaming live-reply controller (a turn
//! emits its final text once instead of editing a live message).
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::{
    ActiveChat,
    im_cron::{CronResultDelivery, deliver_chat_cron_result},
    im_interaction::{InteractionPort, NoInteraction},
    protocol::{
        BoxFuture, ChannelAdapter, ChannelError, ChannelHandle, ChannelKey, ChannelLog,
        ChannelStartDeps, TurnGateway, TurnRequest,
    },
};
use crate::metadata::js_trim;
use futures_util::future::FutureExt;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

mod client;
mod crypto;
mod mapper;
mod parse;
mod render;
#[cfg(test)]
mod tests;

use client::{ApiClient, Log, SendResult};
use mapper::{FeishuCommand, FeishuSessionMapper};
use parse::{
    Clock, InboundMessage, ParsedEvent, Parser, group_inbound, mask_app_id,
    merge_text_and_diagnostics, should_batch,
};
use render::{
    LiveReplyBuffer, MAX_TEXT_MESSAGE_LENGTH, activity_kind_from_text, live_chunks, live_text_limit,
};

/// `FeishuChannel.channelKey`.
pub const CHANNEL_KEY: &str = "feishu";
/// `sdk.Domain.Feishu` / `sdk.Domain.Lark`.
pub const FEISHU_BASE_URL: &str = "https://open.feishu.cn";
pub const LARK_BASE_URL: &str = "https://open.larksuite.com";

const SEEN_EVENTS_MAX: usize = 2000;
/// `FEISHU_MULTI_ATTACHMENT_BATCH_MS`.
const MULTI_ATTACHMENT_BATCH_MS: u64 = 250;
/// `FEISHU_MAX_PENDING_TURNS_PER_CHAT`.
const MAX_PENDING_TURNS_PER_CHAT: usize = 20;

/// `FeishuChannelOptions` after the constructor's `?? ""` defaults. `base_url`
/// is the one Rust-only seam: it replaces the two fixed Lark origins so the
/// golden fixture can point the client at a loopback mock.
#[derive(Clone, Debug)]
pub struct FeishuOptions {
    pub app_id: String,
    pub app_secret: String,
    pub encrypt_key: Option<String>,
    pub verify_token: Option<String>,
    /// `connectionMode ?? "stream"`.
    pub connection_mode: String,
    /// `domainName ?? "feishu"`.
    pub domain_name: String,
    pub permission_mode: Option<String>,
    pub base_url: String,
}

impl Default for FeishuOptions {
    fn default() -> Self {
        Self {
            app_id: String::new(),
            app_secret: String::new(),
            encrypt_key: None,
            verify_token: None,
            connection_mode: "stream".to_owned(),
            domain_name: "feishu".to_owned(),
            permission_mode: None,
            base_url: FEISHU_BASE_URL.to_owned(),
        }
    }
}

fn option_string(options: &JsObject, name: &str) -> Option<String> {
    options
        .get(name)
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
}

impl FeishuOptions {
    /// `new FeishuChannel(options)` field extraction.
    pub fn from_object(options: &JsObject) -> Self {
        let domain_name =
            option_string(options, "domainName").unwrap_or_else(|| "feishu".to_owned());
        let base_url = if domain_name == "lark" {
            LARK_BASE_URL
        } else {
            FEISHU_BASE_URL
        };
        Self {
            app_id: option_string(options, "appId").unwrap_or_default(),
            app_secret: option_string(options, "appSecret").unwrap_or_default(),
            encrypt_key: option_string(options, "encryptKey"),
            verify_token: option_string(options, "verifyToken"),
            connection_mode: option_string(options, "connectionMode")
                .unwrap_or_else(|| "stream".to_owned()),
            domain_name,
            permission_mode: option_string(options, "permissionMode"),
            base_url: base_url.to_owned(),
        }
    }

    /// Test seam: point the Lark Open API at a loopback mock.
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }
}

/// `Date.now()` in the adapter's default clock.
pub struct SystemClock;
impl Clock for SystemClock {
    fn now_ms(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as f64)
            .unwrap_or(0.0)
    }
}

#[derive(Clone)]
struct Logger {
    sink: Arc<Mutex<Option<ChannelLog>>>,
}

impl Logger {
    fn new() -> Self {
        Self {
            sink: Arc::new(Mutex::new(None)),
        }
    }
    fn install(&self, log: Option<ChannelLog>) {
        *self.sink.lock().expect("#1329 logger mutex") = log;
    }
    fn log(&self, level: &str, message: &str) {
        let sink = self.sink.lock().expect("#1329 logger mutex").clone();
        if let Some(sink) = sink {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(level, message)));
        }
    }
    fn fn_handle(&self) -> Log {
        let logger = self.clone();
        Arc::new(move |level: &str, message: &str| logger.log(level, message))
    }
}

/// `this.seenEvents` (`Set` + `SEEN_EVENTS_MAX` eviction of the oldest id).
#[derive(Default)]
struct SeenEvents {
    order: VecDeque<String>,
    set: HashSet<String>,
}

impl SeenEvents {
    /// Returns whether the id was new (`Set.add`'s boolean): the dedupe
    /// decision and the registration share the caller's critical section.
    fn remember(&mut self, id: String) -> bool {
        if !self.set.insert(id.clone()) {
            return false;
        }
        self.order.push_back(id);
        while self.order.len() > SEEN_EVENTS_MAX {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
        true
    }
}

#[derive(Default)]
struct Batch {
    /// Identity of this queue instance. `/new` removes the entry and the next
    /// message creates a fresh one; a drainer from the old instance must not
    /// consume or remove the new one's messages (it only ever touches the id
    /// it started with).
    id: u64,
    messages: Vec<InboundMessage>,
    draining: bool,
    /// Sequence of the live timer; every enqueue bumps it.
    ///
    /// `clearTimeout` cannot be expressed with `JoinHandle::abort`: abort only
    /// takes effect at the task's next yield point, and a timer that already
    /// woke up is blocked on the state lock *inside* `drain_inbound_batch` —
    /// the abort would land after it claimed the batch and took its messages,
    /// dropping them and leaving `draining` set with no drainer left to clear
    /// it (the chat would never drain again). Each timer instead carries the
    /// sequence it was armed with and returns untouched when a newer message
    /// superseded it.
    timer_seq: u64,
}

#[derive(Clone, Debug)]
struct QueuedTurn {
    session_key: String,
    message: String,
    /// Registered diff A8.3-CH-FEISHU-006: TS forwards `projectKey` with
    /// `submitTurn`, the A7 `TurnRequest` port has no such field, so the turn
    /// carries it until the port grows one.
    #[allow(dead_code)]
    project_key: Option<String>,
    message_id: Option<String>,
    generation: u64,
}

#[derive(Clone, Debug)]
struct ActiveRun {
    /// Registered diff A8.3-CH-FEISHU-004: `abortTurn({ sessionKey, runId })`
    /// is the only consumer in TS; the A7 port has no abort, so the pair is
    /// recorded for the day the port grows one.
    #[allow(dead_code)]
    session_key: String,
    #[allow(dead_code)]
    run_id: Option<String>,
    generation: u64,
}

#[derive(Default)]
struct ChatEntry {
    generation: u64,
    queue: VecDeque<QueuedTurn>,
    active_run: Option<ActiveRun>,
}

struct State {
    mapper: FeishuSessionMapper,
    seen: SeenEvents,
    batches: HashMap<String, Batch>,
    chats: HashMap<String, ChatEntry>,
    /// `permissionAnsweringChats` + `permissionAnsweringTokens`.
    permission_answering: HashMap<String, u64>,
    /// #2055: the elicitation answer task's occupancy, the mirror of
    /// `permission_answering` for the elicitation branch — seat + token, the
    /// same two halves. TS needs no such flag — `ImElicitationHelper.answer`
    /// consumes the pending entry synchronously before its first await — but
    /// the pending entry here lives behind the interaction port, so the check
    /// and the reservation have to share one critical section instead
    /// (registered divergence A8.3-CH-FEISHU-004-R2: a mid-answer event is
    /// swallowed for the one `respondElicitation` await, like the permission
    /// branch). #2775 fix-r4: the token is the stale-release guard — a `/new`
    /// between two answers clears the seat, so the old answer's release must
    /// not delete the new answer's (the permission branch's
    /// `get == Some(&token)` shape; TS cannot double-answer because `answer`
    /// consumes the pending entry before its first await).
    elicitation_answering: HashMap<String, u64>,
    next_answering_token: u64,
    /// Monotonic batch identity (see [`Batch::id`]).
    next_batch_id: u64,
}

impl State {
    fn new(uuid: mapper::Uuid, saved: Option<&JsValue>) -> Self {
        Self {
            mapper: match saved {
                Some(saved) => FeishuSessionMapper::from_saved_with_uuid(saved, uuid),
                None => FeishuSessionMapper::new(uuid),
            },
            seen: SeenEvents::default(),
            batches: HashMap::new(),
            chats: HashMap::new(),
            permission_answering: HashMap::new(),
            elicitation_answering: HashMap::new(),
            // TS starts the counter at 1.
            next_answering_token: 1,
            next_batch_id: 0,
        }
    }
}

/// A webhook body handler the native HTTP listener can call. `None` from
/// [`FeishuWebhookSlot::handler`] is the TS `activeFeishu === undefined`
/// case: the route falls through to the rest of the router.
pub(crate) type WebhookHandler = crate::native_http::WebhookHandler;

/// `options.onStateChange`: the bundle's debounced state write.
pub(crate) type StateChange = Arc<dyn Fn(JsValue) + Send + Sync>;

#[derive(Default)]
pub(crate) struct FeishuWebhookSlot {
    target: Mutex<Option<Arc<Runtime>>>,
}

impl FeishuWebhookSlot {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The currently active feishu channel, mirroring `activeFeishu`.
    pub(crate) fn handler(&self) -> Option<WebhookHandler> {
        let target = self
            .target
            .lock()
            .expect("#1329 webhook slot mutex")
            .clone()?;
        Some(Arc::new(move |body: String| {
            let target = target.clone();
            Box::pin(async move { target.handle_webhook(&body).await })
                as std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send>>
        }))
    }

    /// The `options.feishuWebhook` entry the native listener mounts.
    pub(crate) fn resolver(slot: &Arc<Self>) -> crate::native_http::WebhookResolver {
        let slot = slot.clone();
        Arc::new(move || slot.handler())
    }

    fn publish(&self, target: Arc<Runtime>) {
        *self.target.lock().expect("#1329 webhook slot mutex") = Some(target);
    }

    /// `if (reason !== "hot-reload" && activeFeishu === channel) activeFeishu = undefined`.
    fn clear(&self, target: &Arc<Runtime>) {
        let mut slot = self.target.lock().expect("#1329 webhook slot mutex");
        let same = slot
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, target));
        if same {
            *slot = None;
        }
    }
}

pub struct FeishuChannel {
    key: ChannelKey,
    runtime: Arc<Runtime>,
}

struct Runtime {
    key: ChannelKey,
    /// TS mutates `this.appId`/`encryptKey`/… from `deps.config` in `start()`;
    /// the merged view lives here so the webhook entry sees it.
    options: Mutex<FeishuOptions>,
    gateway: Mutex<Option<Arc<dyn TurnGateway>>>,
    interaction: Arc<dyn InteractionPort>,
    clock: Arc<dyn Clock>,
    logger: Logger,
    state: Mutex<State>,
    api: Mutex<Option<Arc<ApiClient>>>,
    /// `this.activeChats`, shared with the `ActiveChat` admission guard.
    active_chats: Arc<Mutex<HashSet<String>>>,
    slot: Option<Arc<FeishuWebhookSlot>>,
    /// `options.onStateChange` (ChannelAdapterBundle.createFeishu).
    on_state_change: Option<StateChange>,
}

impl Runtime {
    fn log(&self, level: &str, message: &str) {
        self.logger.log(level, message);
    }

    fn gateway(&self) -> Option<Arc<dyn TurnGateway>> {
        self.gateway.lock().expect("#1329 gateway mutex").clone()
    }

    fn options(&self) -> FeishuOptions {
        self.options.lock().expect("#1329 options mutex").clone()
    }

    fn api(&self) -> Option<Arc<ApiClient>> {
        self.api.lock().expect("#1329 api mutex").clone()
    }

    /// The interaction-state lock, recovering from the poisoning a panic that
    /// unwound through a guard leaves behind (#2055): this is in-memory
    /// channel bookkeeping with no external invariant, so a poisoned lock must
    /// not take every later state user down with it (TS has no poisoning —
    /// its `catch`/`finally` blocks keep the channel alive; the weixin lane's
    /// `lock_inner` made the same call for #1780).
    fn lock_state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ----- lifecycle -------------------------------------------------------

    async fn start(
        self: &Arc<Self>,
        deps: ChannelStartDeps,
    ) -> Result<Arc<dyn ChannelHandle>, ChannelError> {
        // TS `start()`: gateway/logger first, then the config merge.
        *self.gateway.lock().expect("#1329 gateway mutex") = deps.turn_gateway;
        self.logger.install(deps.log);
        if let Some(config) = deps.config.as_ref() {
            self.merge_config(config);
        }
        let options = self.options();
        let api = Arc::new(ApiClient::new(
            options.app_id.clone(),
            options.app_secret.clone(),
            options.base_url.clone(),
            self.clock.clone(),
            self.logger.fn_handle(),
        ));
        *self.api.lock().expect("#1329 api mutex") = Some(api);

        // `pilotdeckServer.startAndTrack` publishes the active channel after
        // `start` resolves; registration lives here so a channel that never
        // started never answers on the route.
        if let Some(slot) = &self.slot {
            slot.publish(self.clone());
        }

        if options.app_id.is_empty() || options.app_secret.is_empty() {
            self.log(
                "warn",
                "feishu: appId/appSecret not configured; outbound replies will not be sent. \
                 Configure adapters.feishu.appId/appSecret in pilotdeck.yaml.",
            );
        } else if options.connection_mode == "stream" {
            // The Lark WS SDK is not ported; this is the TS fallback line for
            // a WS start that did not come up.
            self.log(
                "warn",
                "feishu: stream mode failed to start; falling back to webhook-only \
                 (set adapters.feishu.connectionMode: webhook in pilotdeck.yaml to silence this).",
            );
        } else {
            self.log(
                "info",
                &format!(
                    "feishu: ready in webhook mode (appId={}); waiting for POST /feishu/webhook",
                    mask_app_id(&options.app_id)
                ),
            );
        }
        Ok(Arc::new(FeishuHandle {
            runtime: self.clone(),
        }))
    }

    /// `if (cfg) { this.appId = this.appId || cfg.appId || ""; … }`.
    fn merge_config(&self, config: &JsValue) {
        let Some(cfg) = config.get("adapters").and_then(|value| value.get("feishu")) else {
            return;
        };
        let mut options = self.options.lock().expect("#1329 options mutex");
        if options.app_id.is_empty() {
            options.app_id = config_text(cfg.get("appId"));
        }
        if options.app_secret.is_empty() {
            options.app_secret = config_text(cfg.get("appSecret"));
        }
        if options.encrypt_key.is_none() {
            options.encrypt_key = config_string(cfg.get("encryptKey"));
        }
        if options.verify_token.is_none() {
            options.verify_token = config_string(cfg.get("verifyToken"));
        }
        if options.permission_mode.is_none() {
            options.permission_mode = config_string(cfg.get("permissionMode"));
        }
    }

    // ----- webhook entry ---------------------------------------------------

    /// `handleWebhook(request, response, body)` minus the HTTP write: the
    /// native listener owns the status line and the `application/json`
    /// content type, exactly what `respondJson` writes.
    async fn handle_webhook(self: &Arc<Self>, body: &str) -> (u16, String) {
        if self.gateway().is_none() {
            return (
                503,
                respond_json(vec![(
                    "error",
                    JsValue::String("feishu_not_started".into()),
                )]),
            );
        }
        let options = self.options();
        let log = self.logger.fn_handle();
        let log_ref = |level: &str, message: &str| log(level, message);
        let parser = Parser {
            encrypt_key: options.encrypt_key.as_deref(),
            verify_token: options.verify_token.as_deref(),
            clock: &*self.clock,
            log: &log_ref,
        };
        match parser.parse(body) {
            ParsedEvent::UrlVerification { challenge } => (
                200,
                respond_json(vec![("challenge", JsValue::String(challenge.into()))]),
            ),
            ParsedEvent::Ignore => (200, respond_json(vec![("ok", JsValue::Bool(true))])),
            ParsedEvent::Message(message) => {
                // Lookup and registration are one critical section: two
                // concurrent deliveries of the same `event_id` (Feishu retries
                // on its own schedule) must not both enqueue. TS does the
                // `has`/`add` pair synchronously on one event loop.
                let already_seen = {
                    let mut state = self.lock_state();
                    !state.seen.remember(message.event_id.clone())
                };
                if already_seen {
                    return (
                        200,
                        respond_json(vec![
                            ("ok", JsValue::Bool(true)),
                            ("deduped", JsValue::Bool(true)),
                        ]),
                    );
                }
                self.enqueue_inbound(message);
                (200, respond_json(vec![("ok", JsValue::Bool(true))]))
            }
        }
    }

    // ----- inbound pipeline ------------------------------------------------

    fn enqueue_inbound(self: &Arc<Self>, input: InboundMessage) {
        if self.try_handle_pending_interaction(&input) {
            return;
        }
        let chat_id = input.chat_id.clone();
        let delay = if should_batch(&input) {
            MULTI_ATTACHMENT_BATCH_MS
        } else {
            0
        };
        let mut state = self.lock_state();
        if !state.batches.contains_key(&chat_id) {
            state.next_batch_id += 1;
            let id = state.next_batch_id;
            state.batches.insert(
                chat_id.clone(),
                Batch {
                    id,
                    ..Batch::default()
                },
            );
        }
        let batch = state.batches.get_mut(&chat_id).expect("#1329 batch entry");
        // `clearTimeout(batch.timer)`: a timer whose drain already started
        // must not be stopped — the running drainer picks up the new message
        // in its re-read loop. `JoinHandle::abort` cannot draw that line (see
        // `Batch::timer_seq`), so the superseded timer is retired by bumping
        // the sequence instead: when it wakes it finds a newer sequence and
        // returns before touching the batch or claiming `draining`.
        batch.timer_seq += 1;
        let timer_seq = batch.timer_seq;
        batch.messages.push(input);
        // The timer is spawned only after the message is visible in `batches`,
        // while the state lock is still held: a `delay == 0` task stolen by
        // another worker between `spawn` and the push would find no entry in
        // `drain_inbound_batch` and leave the message stranded behind an
        // already-finished timer (TS `setTimeout` can never fire before the
        // synchronous enqueue returns). `tokio::spawn` never awaits, so the
        // lock is not held across a suspension point.
        let runtime = self.clone();
        let timer_chat = chat_id.clone();
        let timer_batch = batch.id;
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            let mut timer_seq = timer_seq;
            loop {
                // TS `drainInboundBatch` is a try/finally: a panic mid-drain
                // still resets `draining` in the `finally` and re-arms the
                // drain while messages remain, instead of leaving the chat's
                // batch claimed forever (#2055). The catch below is that
                // `finally`; the retry is its re-entry — an always-panicking
                // drain keeps retrying there too.
                let drain = runtime.drain_inbound_batch(&timer_chat, timer_batch, timer_seq);
                if let Err(panic) =
                    FutureExt::catch_unwind(std::panic::AssertUnwindSafe(drain)).await
                {
                    runtime.log(
                        "error",
                        &format!("feishu: drainInboundBatch panic: {panic:?}"),
                    );
                    // The `finally` reset: un-claim this drainer's batch. A
                    // message that re-armed the window during the panic owns
                    // the newer sequence, so the retry drains with the
                    // entry's current one; a `/new` that replaced the entry
                    // leaves nothing to recover.
                    let retry = {
                        let mut state = runtime.lock_state();
                        match state.batches.get_mut(&timer_chat) {
                            Some(batch) if batch.id == timer_batch => {
                                batch.draining = false;
                                Some(batch.timer_seq)
                            }
                            _ => None,
                        }
                    };
                    match retry {
                        Some(seq) => {
                            timer_seq = seq;
                            continue;
                        }
                        None => return,
                    }
                }
                return;
            }
        });
    }

    /// `tryHandlePendingInteraction`: synchronous decision, detached answer.
    fn try_handle_pending_interaction(self: &Arc<Self>, input: &InboundMessage) -> bool {
        if self.gateway().is_none() {
            return false;
        }
        let chat_id = input.chat_id.clone();
        if self.interaction.elicitation_pending(&chat_id) {
            // #2055: the lookup and the reservation share one critical
            // section, the same occupancy the permission branch below
            // established. TS needs no flag — `answer` consumes the pending
            // entry synchronously before its first await — but the entry here
            // lives behind the interaction port, so two webhook deliveries
            // racing across threads could both observe it pending (one
            // answer, one swallowed event; see `elicitation_answering`).
            // #2775 fix-r4: the reservation carries a token from the same
            // counter the permission branch uses, so the release below can
            // tell a stale answer (its seat already cleared by a `/new`)
            // from the current one.
            let token = {
                let mut state = self.lock_state();
                if state.elicitation_answering.contains_key(&chat_id) {
                    return true;
                }
                let token = state.next_answering_token;
                state.next_answering_token += 1;
                state.elicitation_answering.insert(chat_id.clone(), token);
                token
            };
            let runtime = self.clone();
            let text = input.text.clone();
            tokio::spawn(async move {
                let answer = runtime.answer_pending_elicitation(&chat_id, &text);
                // TS attaches a `.catch` to the answer task; a Rust panic
                // unwinds it instead, so the occupancy reset below is the
                // `.finally` (#2055).
                match FutureExt::catch_unwind(std::panic::AssertUnwindSafe(answer)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        runtime.log(
                            "error",
                            &format!("feishu: elicitation answer error: {error}"),
                        );
                    }
                    Err(panic) => {
                        runtime.log(
                            "error",
                            &format!("feishu: elicitation answer panic: {panic:?}"),
                        );
                    }
                }
                // #2775 fix-r4: only the seat this task reserved may be
                // released — a `/new` in between cleared it and a newer
                // answer re-took it, and removing that one would let the
                // next event start a second answer task for the same
                // pending entry (TS cannot: `answer` consumes it
                // synchronously). The permission branch's `.finally` guards
                // the same window with its token; this is the elicitation
                // mirror of that shape.
                let mut state = runtime.lock_state();
                if state.elicitation_answering.get(&chat_id) == Some(&token) {
                    state.elicitation_answering.remove(&chat_id);
                }
            });
            return true;
        }
        let pending = self.interaction.permission_pending(&chat_id);
        // The lookup and the reservation share one critical section: two
        // concurrent webhook deliveries for the same chat must not both start
        // an answer task (TS does this synchronously on one event loop).
        let token = {
            let mut state = self.lock_state();
            if state.permission_answering.contains_key(&chat_id) {
                return true;
            }
            if !pending {
                return false;
            }
            let token = state.next_answering_token;
            state.next_answering_token += 1;
            state.permission_answering.insert(chat_id.clone(), token);
            token
        };
        let runtime = self.clone();
        let text = input.text.clone();
        tokio::spawn(async move {
            let answer = runtime.answer_pending_permission(&chat_id, &text);
            // TS chains the release onto the answer with `.finally`, so it
            // runs for an error and a throw alike; the catch keeps a Rust
            // panic on the same path (#2055).
            match FutureExt::catch_unwind(std::panic::AssertUnwindSafe(answer)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    runtime.log(
                        "error",
                        &format!("feishu: permission answer error: {error}"),
                    );
                }
                Err(panic) => {
                    runtime.log(
                        "error",
                        &format!("feishu: permission answer panic: {panic:?}"),
                    );
                }
            }
            let mut state = runtime.lock_state();
            if state.permission_answering.get(&chat_id) == Some(&token) {
                state.permission_answering.remove(&chat_id);
            }
        });
        true
    }

    async fn answer_pending_elicitation(
        self: &Arc<Self>,
        chat_id: &str,
        text: &str,
    ) -> Result<(), ChannelError> {
        if self.gateway().is_none() {
            return Ok(());
        }
        let confirmation = self.interaction.elicitation_answer(chat_id, text).await?;
        if let Some(confirmation) = confirmation.filter(|value| !value.is_empty()) {
            self.send(chat_id, &confirmation).await;
        }
        Ok(())
    }

    async fn answer_pending_permission(
        self: &Arc<Self>,
        chat_id: &str,
        text: &str,
    ) -> Result<(), ChannelError> {
        if self.gateway().is_none() {
            return Ok(());
        }
        let answer = self.interaction.permission_answer(chat_id, text).await?;
        let Some(answer) = answer else {
            return Ok(());
        };
        let answer_token = answer.answer_token;
        if let Some(reply) = answer.text.as_ref().filter(|value| !value.is_empty()) {
            let delivered = self.send(chat_id, reply).await;
            if !delivered {
                self.interaction
                    .permission_release_answer(chat_id, answer_token);
                return Ok(());
            }
            if !answer.can_advance && !answer.retry_prompt {
                return Ok(());
            }
            if let Some(next_prompt) = self
                .interaction
                .permission_take_next_prompt(chat_id, answer_token)
            {
                let request_id = self
                    .interaction
                    .permission_prompt_request_id(chat_id, answer_token);
                let delivered = self.send(chat_id, &next_prompt).await;
                self.interaction.permission_confirm_next(
                    chat_id,
                    delivered,
                    request_id.as_deref(),
                    answer_token,
                );
            }
        }
        Ok(())
    }

    async fn drain_inbound_batch(self: &Arc<Self>, chat_id: &str, batch_id: u64, timer_seq: u64) {
        let batch_id = {
            let mut state = self.lock_state();
            let Some(batch) = state.batches.get_mut(chat_id) else {
                return;
            };
            // A superseded timer (a newer message re-armed the window, or a
            // `/new` replaced the entry) returns without touching anything:
            // the live timer drains every message that is still queued, and a
            // stale one must not claim `draining` on its way out.
            if batch.id != batch_id || batch.timer_seq != timer_seq {
                return;
            }
            if batch.draining {
                return;
            }
            batch.draining = true;
            batch.id
        };
        loop {
            loop {
                let messages = {
                    let mut state = self.lock_state();
                    match state.batches.get_mut(chat_id) {
                        // A batch replaced under us (`/new` removed the entry,
                        // then a new message created a fresh one) belongs to
                        // another drainer: stop, never take its messages.
                        Some(batch) if batch.id == batch_id && !batch.messages.is_empty() => {
                            std::mem::take(&mut batch.messages)
                        }
                        _ => break,
                    }
                };
                for group in group_inbound(&messages) {
                    self.process_inbound_messages(&group).await;
                }
            }
            // Nothing left. Give the batch up, then re-read under the same
            // lock: a message that arrived while the last group was being
            // processed keeps this drainer (the TS `retry` branch re-entered
            // `drainInboundBatch` from a fresh microtask; looping in place
            // keeps the same single-drainer invariant without an async
            // recursion that the `Send` check cannot prove).
            let mut state = self.lock_state();
            let done = state
                .batches
                .get(chat_id)
                .is_none_or(|batch| batch.id != batch_id || batch.messages.is_empty());
            if done {
                // Only the instance this drainer started on may be removed;
                // a replacement keeps its own `draining` flag for its drainer.
                if state
                    .batches
                    .get(chat_id)
                    .is_some_and(|batch| batch.id == batch_id)
                {
                    state.batches.remove(chat_id);
                }
                return;
            }
        }
    }

    async fn process_inbound_messages(self: &Arc<Self>, inputs: &[InboundMessage]) {
        if self.gateway().is_none() {
            return;
        }
        let Some(first) = inputs.first() else {
            return;
        };
        let chat_id = first.chat_id.clone();
        // Attachments are out of scope for this unit, so the diagnostics list
        // is always empty and an attachment-only group has no message text.
        let joined = inputs
            .iter()
            .map(|input| input.text.as_str())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        let message_text = merge_text_and_diagnostics(&joined, &[]);
        if js_trim(&message_text).is_empty() {
            return;
        }

        if self.interaction.elicitation_pending(&chat_id) {
            if let Err(error) = self
                .answer_pending_elicitation(&chat_id, &message_text)
                .await
            {
                self.log(
                    "error",
                    &format!("feishu: elicitation answer error: {error}"),
                );
            }
            return;
        }
        if self.interaction.permission_pending(&chat_id) {
            if let Err(error) = self
                .answer_pending_permission(&chat_id, &message_text)
                .await
            {
                self.log(
                    "error",
                    &format!("feishu: permission answer error: {error}"),
                );
            }
            return;
        }

        // #2055: TS resolves the session and reads `chatState.generation` in
        // one synchronous block (no await between `mapper.resolve` and the
        // queue-time read), so a concurrent `/new` can never rotate the
        // session under a turn that then carries the new generation into the
        // old session. Reading the generation under the same guard keeps that
        // invariant on a multi-threaded runtime.
        let (mapped, mut generation) = {
            let mut state = self.lock_state();
            let mapped = state.mapper.resolve(&chat_id, &message_text);
            let generation = state
                .chats
                .get(&chat_id)
                .map(|entry| entry.generation)
                .unwrap_or(0);
            (mapped, generation)
        };

        if mapped.command == Some(FeishuCommand::New) {
            // `this.onStateChange?.(this.mapper.snapshot())` runs before the
            // reset, so the persisted state already carries the new session.
            if let Some(on_state_change) = self.on_state_change.as_ref() {
                let snapshot = self.lock_state().mapper.snapshot_js();
                on_state_change(snapshot);
            }
            // No `abortTurn` on the A7 port: the previous run keeps going
            // (registered diff). The generation bump below makes the event loop
            // and the flush skip this chat's output once the generation has
            // moved; a segment whose check already passed — in flight or not
            // yet sent — is outside this gate.
            //
            // The reset hands the new generation back from inside its own
            // critical section (#2055): the follow-up turn must observe the
            // pair {new session, new generation} that this `/new` produced,
            // not a generation a racing `/new` bumped after the reset.
            generation = self.reset_chat_interaction_state(&chat_id);
            if mapped.message.is_empty() {
                self.send(&chat_id, "已创建新会话。").await;
                return;
            }
            // `/new <text>` keeps going with the *new* session key: TS only
            // returns when `!mapped.message`.
        } else if mapped.command.is_some() {
            // The system command registry is out of scope: `/projects` and
            // `/switch-project` are reported as handled (and dropped) rather
            // than executed, while an unknown slash message still reaches the
            // gateway. `/new` is the one mapper command with a submit path
            // (registered diff A8.3-CH-FEISHU-003).
            return;
        }
        if mapped.message.is_empty() {
            return;
        }

        let turn = QueuedTurn {
            session_key: mapped.session_key.clone(),
            message: mapped.message.clone(),
            project_key: mapped.project_key.clone(),
            message_id: first.message_id.clone(),
            generation,
        };

        // No separate "is the chat active?" pre-check: the admission and the
        // queue push have to be one decision (`run_queued_turns` takes the
        // slot or queues under the state lock). A check made here, followed by
        // a push made later, could observe a running drain that releases in
        // between and leave the turn queued with nobody to drain it.
        self.run_queued_turns(&chat_id, turn).await;
    }

    /// `resetChatInteractionState`, returning the post-reset generation so
    /// the `/new` follow-up turn can carry it out of the same critical
    /// section (#2055; TS reads it in the same synchronous block).
    fn reset_chat_interaction_state(self: &Arc<Self>, chat_id: &str) -> u64 {
        let generation = {
            let mut state = self.lock_state();
            let entry = state.chats.entry(chat_id.to_owned()).or_default();
            entry.generation += 1;
            entry.queue.clear();
            entry.active_run = None;
            let generation = entry.generation;
            state.permission_answering.remove(chat_id);
            state.elicitation_answering.remove(chat_id);
            // `clearTimeout(batch?.timer)`: once the timer fired and the
            // drain is running, clearTimeout is a no-op in JS — the drain
            // (including this /new's own send) runs to completion. Removing
            // the entry has the same effect here: a timer that has not
            // claimed the batch yet finds no entry (or a fresh one with a
            // different id/sequence) and returns, while a running drainer
            // keeps going and only drops its claim when it re-reads.
            state.batches.remove(chat_id);
            generation
        };
        self.interaction.elicitation_clear(chat_id);
        self.interaction.permission_clear_after_turn(chat_id);
        generation
    }

    fn generation(&self, chat_id: &str) -> u64 {
        self.lock_state()
            .chats
            .get(chat_id)
            .map(|entry| entry.generation)
            .unwrap_or(0)
    }

    fn is_current(&self, chat_id: &str, generation: u64) -> bool {
        self.generation(chat_id) == generation
    }

    fn set_active_run(&self, chat_id: &str, run: ActiveRun) {
        self.lock_state()
            .chats
            .entry(chat_id.to_owned())
            .or_default()
            .active_run = Some(run);
    }

    fn active_run(&self, chat_id: &str) -> Option<ActiveRun> {
        self.lock_state()
            .chats
            .get(chat_id)
            .and_then(|entry| entry.active_run.clone())
    }

    fn clear_active_run(&self, chat_id: &str) {
        if let Some(entry) = self.lock_state().chats.get_mut(chat_id) {
            entry.active_run = None;
        }
    }

    async fn run_queued_turns(self: &Arc<Self>, chat_id: &str, first: QueuedTurn) {
        let mut pending = first;
        // True when `pending` came off the front of this chat's queue (the
        // lost-admit hand-off below): re-queueing it at the back would let a
        // newer turn overtake it, so it goes back to the front.
        let mut pending_from_queue = false;
        loop {
            let mut slot = match ActiveChat::admit(self.active_chats.clone(), chat_id.to_owned()) {
                Some(slot) => slot,
                None => {
                    // Lost the race with the running drain: the turn must still
                    // be queued, exactly like the admission-failure path that
                    // `process_inbound_messages` reaches through this same
                    // function. The push, the "is someone still holding
                    // the slot" check and the take-back all happen under the one
                    // state lock the holder takes to decide "queue empty and
                    // release", so the two sides cannot both conclude "the other
                    // one has it".
                    let mut state = self.lock_state();
                    let entry = state.chats.entry(chat_id.to_owned()).or_default();
                    if pending_from_queue {
                        entry.queue.push_front(pending);
                    } else {
                        entry.queue.push_back(pending);
                    }
                    while entry.queue.len() > MAX_PENDING_TURNS_PER_CHAT {
                        entry.queue.pop_front();
                    }
                    let holder_active = self
                        .active_chats
                        .lock()
                        .expect("#1329 active chats mutex")
                        .contains(chat_id);
                    // Take the turn back inside that same critical section: a
                    // racer that admits in the gap between this lock and the
                    // next one must never see the queue it will later drain.
                    let taken = if holder_active {
                        None
                    } else {
                        entry.queue.pop_front()
                    };
                    drop(state);
                    if holder_active {
                        self.log(
                            "info",
                            &format!("feishu: chat {chat_id} already active, queued message"),
                        );
                        return;
                    }
                    // The holder released between our `admit` and our push:
                    // become the drainer ourselves. If a third racer already
                    // drained the queue, stop here.
                    match taken {
                        Some(turn) => {
                            pending = turn;
                            pending_from_queue = true;
                            continue;
                        }
                        None => return,
                    }
                }
            };
            let mut next = Some(pending);
            while let Some(turn) = next {
                self.process_turn(chat_id, &turn).await;
                next = self.take_next_or_release(chat_id, &mut slot);
            }
            return;
        }
    }

    /// Take the next queued turn, or — with the queue lock still held — give
    /// the admission back. Releasing inside that critical section is what
    /// makes the hand-off atomic: a racer that lost `admit` and pushed while
    /// we were finishing is either seen by the `pop_front` here or wakes up to
    /// a free slot in its own `admit`.
    fn take_next_or_release(
        &self,
        chat_id: &str,
        slot: &mut ActiveChat<String>,
    ) -> Option<QueuedTurn> {
        let mut state = self.lock_state();
        let next = state
            .chats
            .get_mut(chat_id)
            .and_then(|entry| entry.queue.pop_front());
        if next.is_none() {
            slot.release();
        }
        next
    }

    async fn process_turn(self: &Arc<Self>, chat_id: &str, turn: &QueuedTurn) {
        let Some(gateway) = self.gateway() else {
            return;
        };
        if !self.is_current(chat_id, turn.generation) {
            return;
        }
        let api = self.api();
        let reaction_id = match (&turn.message_id, &api) {
            (Some(message_id), Some(api)) => api.add_reaction(message_id).await,
            _ => None,
        };
        let mut live = LiveReplyBuffer::new();
        match gateway
            .submit(TurnRequest {
                session_key: JsString::from(turn.session_key.as_str()),
                channel_key: self.key.clone(),
                message: JsString::from(turn.message.as_str()),
            })
            .await
        {
            Err(error) => {
                self.log("error", &format!("feishu: submitTurn error: {error}"));
                live.handle_submit_failure("Failed to process this message. Please retry.");
            }
            Ok(mut events) => loop {
                match events.next().await {
                    Ok(None) => break,
                    Err(error) => {
                        self.log("error", &format!("feishu: submitTurn error: {error}"));
                        live.handle_submit_failure("Failed to process this message. Please retry.");
                        break;
                    }
                    Ok(Some(event)) => {
                        if !self.is_current(chat_id, turn.generation) {
                            break;
                        }
                        match event_kind(&event).as_str() {
                            "turn_started" => {
                                let run_id = event
                                    .get("runId")
                                    .and_then(JsValue::as_string)
                                    .map(|value| String::from_utf16_lossy(value.units()));
                                self.set_active_run(
                                    chat_id,
                                    ActiveRun {
                                        session_key: turn.session_key.clone(),
                                        run_id,
                                        generation: turn.generation,
                                    },
                                );
                                live.handle_event(&event);
                            }
                            "elicitation_request" => {
                                let question = self.interaction.elicitation_capture(
                                    chat_id,
                                    &turn.session_key,
                                    &event,
                                );
                                live.handle_event(&event);
                                self.send(chat_id, &question).await;
                            }
                            "permission_request" => {
                                let question = self.interaction.permission_capture(
                                    chat_id,
                                    &turn.session_key,
                                    &event,
                                );
                                live.handle_event(&event);
                                if let Some(question) = question {
                                    let delivered = self.send(chat_id, &question).await;
                                    self.interaction.permission_confirm_initial(
                                        chat_id,
                                        delivered,
                                        request_id(&event).as_deref(),
                                    );
                                }
                            }
                            "assistant_attachment" => {
                                // The attachment itself is out of scope; the
                                // controller still flushes the text it has.
                                if !js_trim(&live.text).is_empty() {
                                    self.send_final(chat_id, turn.generation, &live.text).await;
                                }
                                live.text.clear();
                            }
                            _ => live.handle_event(&event),
                        }
                    }
                }
            },
        }
        // A turn that a `/new` (or a later turn) superseded while it was still
        // streaming: the buffered text — including the synthesized
        // "Please retry." fragment — belongs to a session that no longer
        // exists, so this side skips the flush when generation has moved.
        // TS asks the gateway to abort the run there (`abortTurn`, best effort)
        // but still calls `flushFinal()` unconditionally.
        if self.is_current(chat_id, turn.generation) {
            self.interaction.elicitation_clear(chat_id);
            self.interaction.permission_clear_after_turn(chat_id);
            if !js_trim(&live.text).is_empty() {
                self.send_final(chat_id, turn.generation, &live.text).await;
            }
        }
        // The reaction is not part of the flush: TS removes it in a `finally`,
        // so an aborted (superseded) turn still clears the reaction it put on
        // the user's message. Gating this on `current` would strand it.
        if let (Some(message_id), Some(reaction_id), Some(api)) =
            (&turn.message_id, reaction_id, api)
        {
            api.remove_reaction(message_id, &reaction_id).await;
        }
        if self
            .active_run(chat_id)
            .is_some_and(|run| run.generation == turn.generation)
        {
            self.clear_active_run(chat_id);
        }
    }

    // ----- outbound --------------------------------------------------------

    /// `sendLiveMessage` for a final segment: the `post` payload first, the
    /// raw `text` message as its fallback, chunked like `splitForTransport`.
    /// The final flush of one turn: the controller's `append`/`flushSegment`
    /// pair, which splits at `liveTextLimit()` and sends each segment as a
    /// final `post`, falling back to a plain text message per segment when
    /// the post call reports `false`.
    async fn send_final(self: &Arc<Self>, chat_id: &str, generation: u64, text: &str) {
        let Some(api) = self.api() else {
            return;
        };
        for chunk in live_chunks(text, live_text_limit(MAX_TEXT_MESSAGE_LENGTH)) {
            // Re-check before every segment: if the generation has already
            // changed by the time we look, this segment does not start. This is
            // a narrowing, not a guarantee — a bump landing between the check
            // and the send, or while a segment is already inside `send_post`
            // (token fetch + POST) or its `send_raw` fallback, still lets that
            // segment land, exactly like TS's best-effort `abortTurn` for a
            // send already dispatched.
            if !self.is_current(chat_id, generation) {
                return;
            }
            let is_final = !chunk.ends_with(render::DEFAULT_LIVE_REPLY_CURSOR);
            let kind = activity_kind_from_text(&chunk);
            if api
                .send_post(chat_id, &chunk, is_final, Some(kind))
                .await
                .is_failed()
                && api
                    .send_raw(chat_id, "text", text_content(&chunk))
                    .await
                    .is_failed()
            {
                // `flushCurrent` returns without touching the following
                // segments; the append loop still sends them.
                continue;
            }
        }
    }

    /// `sendTextMessage` (the TS `explicitSend` seam has no production
    /// caller, so the raw text send is the whole path here).
    async fn send_text_message(self: &Arc<Self>, chat_id: &str, text: &str) -> SendResult {
        let Some(api) = self.api() else {
            return SendResult::Failed;
        };
        api.send_raw(chat_id, "text", text_content(text)).await
    }

    /// `send(message)`: `(await sendTextMessage(message)) !== false`.
    async fn send(self: &Arc<Self>, chat_id: &str, text: &str) -> bool {
        !self.send_text_message(chat_id, text).await.is_failed()
    }
}

fn text_content(text: &str) -> JsValue {
    let mut content = JsObject::new();
    content.insert("text".into(), JsValue::String(JsString::from(text)));
    JsValue::Object(content)
}

fn event_kind(event: &JsValue) -> String {
    event
        .get("type")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default()
}

fn config_text(value: Option<&JsValue>) -> String {
    config_string(value).unwrap_or_default()
}

fn config_string(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
}

fn request_id(event: &JsValue) -> Option<String> {
    event
        .get("requestId")
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
}

/// `JSON.stringify` of the `respondJson` payload, in insertion order.
fn respond_json(pairs: Vec<(&str, JsValue)>) -> String {
    let mut object = JsObject::new();
    for (key, value) in pairs {
        object.insert(key.into(), value);
    }
    stringify_json(&JsValue::Object(object))
        .ok()
        .flatten()
        .unwrap_or_default()
}

// ----- adapters ------------------------------------------------------------

impl FeishuChannel {
    pub fn new(options: FeishuOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        Self::build(
            options,
            interaction,
            None,
            Arc::new(SystemClock),
            FeishuSessionMapper::random_uuid(),
            None,
            None,
        )
    }

    /// The bundle construction path (`ChannelAdapterBundle.createFeishu`): the
    /// persisted mapper state and `options.onStateChange`. TS assigns
    /// `activeFeishu = options.feishu` at construction, before `start()`, so
    /// the slot is published here rather than in `start`.
    pub(crate) fn for_assembly(
        options: FeishuOptions,
        interaction: Arc<dyn InteractionPort>,
        slot: Arc<FeishuWebhookSlot>,
        saved: Option<&JsValue>,
        on_state_change: Option<StateChange>,
    ) -> Self {
        Self::build(
            options,
            interaction,
            Some(slot),
            Arc::new(SystemClock),
            FeishuSessionMapper::random_uuid(),
            saved,
            on_state_change,
        )
    }

    /// The parity seams: a deterministic clock, session-key uuid and slot.
    #[cfg(test)]
    pub(crate) fn with_seams(
        options: FeishuOptions,
        interaction: Arc<dyn InteractionPort>,
        slot: Option<Arc<FeishuWebhookSlot>>,
        clock: Arc<dyn Clock>,
        uuid: mapper::Uuid,
    ) -> Self {
        Self::build(options, interaction, slot, clock, uuid, None, None)
    }

    /// The parity seams plus `options.onStateChange` — the #2055 F5 pin
    /// plants a racing reset in the callback's window.
    #[cfg(test)]
    pub(crate) fn with_seams_and_state_change(
        options: FeishuOptions,
        interaction: Arc<dyn InteractionPort>,
        slot: Option<Arc<FeishuWebhookSlot>>,
        clock: Arc<dyn Clock>,
        uuid: mapper::Uuid,
        on_state_change: Option<StateChange>,
    ) -> Self {
        Self::build(
            options,
            interaction,
            slot,
            clock,
            uuid,
            None,
            on_state_change,
        )
    }

    fn build(
        options: FeishuOptions,
        interaction: Arc<dyn InteractionPort>,
        slot: Option<Arc<FeishuWebhookSlot>>,
        clock: Arc<dyn Clock>,
        uuid: mapper::Uuid,
        saved: Option<&JsValue>,
        on_state_change: Option<StateChange>,
    ) -> Self {
        let runtime = Arc::new(Runtime {
            key: ChannelKey::new(CHANNEL_KEY),
            options: Mutex::new(options),
            gateway: Mutex::new(None),
            interaction,
            clock,
            logger: Logger::new(),
            state: Mutex::new(State::new(uuid, saved)),
            api: Mutex::new(None),
            active_chats: Arc::new(Mutex::new(HashSet::new())),
            slot,
            on_state_change,
        });
        // `activeFeishu = options.feishu` at GatewayServer construction: the
        // route answers on this runtime before `start()` runs.
        if let Some(slot) = runtime.slot.as_ref() {
            slot.publish(runtime.clone());
        }
        Self {
            key: ChannelKey::new(CHANNEL_KEY),
            runtime,
        }
    }
}

/// Default interaction port for the assembly mount.
pub fn no_interaction() -> Arc<dyn InteractionPort> {
    Arc::new(NoInteraction)
}

impl ChannelAdapter for FeishuChannel {
    fn channel_key(&self) -> &ChannelKey {
        &self.key
    }

    fn start(
        &self,
        deps: ChannelStartDeps,
    ) -> BoxFuture<Result<Arc<dyn ChannelHandle>, ChannelError>> {
        let runtime = self.runtime.clone();
        Box::pin(async move { runtime.start(deps).await })
    }

    /// `deliverCronResult`:
    /// `deliverChatCronResult(delivery, channelKey, (chatId, text) => sendTextMessage(...))`.
    fn deliver_cron_result(&self, delivery: &CronResultDelivery) -> BoxFuture<bool> {
        let runtime = self.runtime.clone();
        let key = self.key.clone();
        let delivery = delivery.clone();
        Box::pin(async move {
            let send = {
                let runtime = runtime.clone();
                move |chat_id: JsString, text: JsString| {
                    let runtime = runtime.clone();
                    let chat_id = String::from_utf16_lossy(chat_id.units());
                    let text = String::from_utf16_lossy(text.units());
                    Box::pin(async move {
                        !runtime.send_text_message(&chat_id, &text).await.is_failed()
                    }) as BoxFuture<bool>
                }
            };
            deliver_chat_cron_result(&delivery, &key, send).await
        })
    }
}

struct FeishuHandle {
    runtime: Arc<Runtime>,
}

impl ChannelHandle for FeishuHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        let reason = reason.to_owned();
        let runtime = self.runtime.clone();
        Box::pin(async move {
            runtime.log("info", &format!("feishu: stopping ({reason})"));
            // `pilotdeckServer.stopRunningChannel`: the active channel is
            // cleared for every stop but a hot reload, whose replacement is
            // already publishing into the same slot.
            if reason != "hot-reload"
                && let Some(slot) = &runtime.slot
            {
                slot.clear(&runtime);
            }
            Ok(())
        })
    }
}
