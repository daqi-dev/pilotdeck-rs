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

use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use tokio::task::JoinHandle;

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

mod client;
mod crypto;
mod mapper;
mod parse;
mod render;
#[cfg(test)]
mod tests;

use client::{ApiClient, SendResult};
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
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                sink(level, message)
            }));
        }
    }
    fn fn_handle(&self) -> Arc<dyn Fn(&str, &str) + Send + Sync> {
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
    fn contains(&self, id: &str) -> bool {
        self.set.contains(id)
    }
    fn remember(&mut self, id: String) {
        if !self.set.insert(id.clone()) {
            return;
        }
        self.order.push_back(id);
        while self.order.len() > SEEN_EVENTS_MAX {
            if let Some(oldest) = self.order.pop_front() {
                self.set.remove(&oldest);
            }
        }
    }
}

#[derive(Default)]
struct Batch {
    messages: Vec<InboundMessage>,
    draining: bool,
    timer: Option<JoinHandle<()>>,
}

#[derive(Clone, Debug)]
struct QueuedTurn {
    session_key: String,
    message: String,
    project_key: Option<String>,
    message_id: Option<String>,
    generation: u64,
}

#[derive(Clone, Debug)]
struct ActiveRun {
    session_key: String,
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
    next_answering_token: u64,
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
            // TS starts the counter at 1.
            next_answering_token: 1,
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
            return Ok(Arc::new(NoopHandle));
        }
        if options.connection_mode == "stream" {
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
                respond_json(vec![("error", JsValue::String("feishu_not_started".into()))]),
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
                if self
                    .state
                    .lock()
                    .expect("#1329 state mutex")
                    .seen
                    .contains(&message.event_id)
                {
                    return (
                        200,
                        respond_json(vec![
                            ("ok", JsValue::Bool(true)),
                            ("deduped", JsValue::Bool(true)),
                        ]),
                    );
                }
                self.state
                    .lock()
                    .expect("#1329 state mutex")
                    .seen
                    .remember(message.event_id.clone());
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
        let runtime = self.clone();
        let timer_chat = chat_id.clone();
        let timer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(delay)).await;
            runtime.drain_inbound_batch(&timer_chat).await;
        });
        let mut state = self.state.lock().expect("#1329 state mutex");
        let batch = state.batches.entry(chat_id).or_default();
        if let Some(previous) = batch.timer.take() {
            previous.abort();
        }
        batch.messages.push(input);
        batch.timer = Some(timer);
    }

    /// `tryHandlePendingInteraction`: synchronous decision, detached answer.
    fn try_handle_pending_interaction(self: &Arc<Self>, input: &InboundMessage) -> bool {
        if self.gateway().is_none() {
            return false;
        }
        let chat_id = input.chat_id.clone();
        if self.interaction.elicitation_pending(&chat_id) {
            let runtime = self.clone();
            let text = input.text.clone();
            tokio::spawn(async move {
                if let Err(error) = runtime.answer_pending_elicitation(&chat_id, &text).await {
                    runtime.log("error", &format!("feishu: elicitation answer error: {error}"));
                }
            });
            return true;
        }
        let pending = self.interaction.permission_pending(&chat_id);
        let answering = self
            .state
            .lock()
            .expect("#1329 state mutex")
            .permission_answering
            .contains_key(&chat_id);
        if !pending && !answering {
            return false;
        }
        if answering {
            return true;
        }
        let token = {
            let mut state = self.state.lock().expect("#1329 state mutex");
            let token = state.next_answering_token;
            state.next_answering_token += 1;
            state.permission_answering.insert(chat_id.clone(), token);
            token
        };
        let runtime = self.clone();
        let text = input.text.clone();
        tokio::spawn(async move {
            if let Err(error) = runtime.answer_pending_permission(&chat_id, &text).await {
                runtime.log("error", &format!("feishu: permission answer error: {error}"));
            }
            let mut state = runtime.state.lock().expect("#1329 state mutex");
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

    async fn drain_inbound_batch(self: &Arc<Self>, chat_id: &str) {
        {
            let mut state = self.state.lock().expect("#1329 state mutex");
            let Some(batch) = state.batches.get_mut(chat_id) else {
                return;
            };
            if batch.draining {
                return;
            }
            batch.draining = true;
        }
        loop {
            loop {
                let messages = {
                    let mut state = self.state.lock().expect("#1329 state mutex");
                    match state.batches.get_mut(chat_id) {
                        Some(batch) if !batch.messages.is_empty() => {
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
            let mut state = self.state.lock().expect("#1329 state mutex");
            let done = state
                .batches
                .get(chat_id)
                .is_none_or(|batch| batch.messages.is_empty());
            if done {
                state.batches.remove(chat_id);
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
                self.log("error", &format!("feishu: elicitation answer error: {error}"));
            }
            return;
        }
        if self.interaction.permission_pending(&chat_id) {
            if let Err(error) = self.answer_pending_permission(&chat_id, &message_text).await {
                self.log("error", &format!("feishu: permission answer error: {error}"));
            }
            return;
        }

        let mapped = {
            let mut state = self.state.lock().expect("#1329 state mutex");
            state.mapper.resolve(&chat_id, &message_text)
        };

        if mapped.command == Some(FeishuCommand::New) {
            // `this.onStateChange?.(this.mapper.snapshot())` runs before the
            // reset, so the persisted state already carries the new session.
            if let Some(on_state_change) = self.on_state_change.as_ref() {
                let snapshot = self
                    .state
                    .lock()
                    .expect("#1329 state mutex")
                    .mapper
                    .snapshot_js();
                on_state_change(snapshot);
            }
            // No `abortTurn` on the A7 port: the previous run keeps going
            // (registered diff), but the generation bump below already stops
            // its events from reaching this chat.
            self.reset_chat_interaction_state(&chat_id);
            if mapped.message.is_empty() {
                self.send(&chat_id, "已创建新会话。").await;
                return;
            }
        }

        // The system command registry is out of scope: the two commands the
        // mapper recognizes are reported as handled (and dropped) rather than
        // executed, while an unknown slash message still reaches the gateway.
        if mapped.command.is_some() {
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
            generation: self.generation(&chat_id),
        };

        let active = self
            .active_chats
            .lock()
            .expect("#1329 active chats mutex")
            .contains(&chat_id);
        if active {
            let mut state = self.state.lock().expect("#1329 state mutex");
            let entry = state.chats.entry(chat_id.clone()).or_default();
            entry.queue.push_back(turn);
            while entry.queue.len() > MAX_PENDING_TURNS_PER_CHAT {
                entry.queue.pop_front();
            }
            self.log(
                "info",
                &format!("feishu: chat {chat_id} already active, queued message"),
            );
            return;
        }
        self.run_queued_turns(&chat_id, turn).await;
    }

    fn reset_chat_interaction_state(self: &Arc<Self>, chat_id: &str) {
        {
            let mut state = self.state.lock().expect("#1329 state mutex");
            let entry = state.chats.entry(chat_id.to_owned()).or_default();
            entry.generation += 1;
            entry.queue.clear();
            entry.active_run = None;
            state.permission_answering.remove(chat_id);
            if let Some(mut batch) = state.batches.remove(chat_id)
                && let Some(timer) = batch.timer.take()
            {
                timer.abort();
            }
        }
        self.interaction.elicitation_clear(chat_id);
        self.interaction.permission_clear_after_turn(chat_id);
    }

    fn generation(&self, chat_id: &str) -> u64 {
        self.state
            .lock()
            .expect("#1329 state mutex")
            .chats
            .get(chat_id)
            .map(|entry| entry.generation)
            .unwrap_or(0)
    }

    fn is_current(&self, chat_id: &str, generation: u64) -> bool {
        self.generation(chat_id) == generation
    }

    fn shift_turn(&self, chat_id: &str) -> Option<QueuedTurn> {
        let mut state = self.state.lock().expect("#1329 state mutex");
        let entry = state.chats.get_mut(chat_id)?;
        entry.queue.pop_front()
    }

    fn set_active_run(&self, chat_id: &str, run: ActiveRun) {
        self.state
            .lock()
            .expect("#1329 state mutex")
            .chats
            .entry(chat_id.to_owned())
            .or_default()
            .active_run = Some(run);
    }

    fn active_run(&self, chat_id: &str) -> Option<ActiveRun> {
        self.state
            .lock()
            .expect("#1329 state mutex")
            .chats
            .get(chat_id)
            .and_then(|entry| entry.active_run.clone())
    }

    fn clear_active_run(&self, chat_id: &str) {
        if let Some(entry) = self
            .state
            .lock()
            .expect("#1329 state mutex")
            .chats
            .get_mut(chat_id)
        {
            entry.active_run = None;
        }
    }

    async fn run_queued_turns(self: &Arc<Self>, chat_id: &str, first: QueuedTurn) {
        let _slot = match ActiveChat::admit(self.active_chats.clone(), chat_id.to_owned()) {
            Some(slot) => slot,
            None => return,
        };
        let mut next = Some(first);
        while let Some(turn) = next {
            self.process_turn(chat_id, &turn).await;
            next = self.shift_turn(chat_id);
        }
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
                                    self.send_final(chat_id, &live.text).await;
                                }
                                live.text.clear();
                            }
                            _ => live.handle_event(&event),
                        }
                    }
                }
            },
        }
        if self.is_current(chat_id, turn.generation) {
            self.interaction.elicitation_clear(chat_id);
            self.interaction.permission_clear_after_turn(chat_id);
        }
        if !js_trim(&live.text).is_empty() {
            self.send_final(chat_id, &live.text).await;
        }
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
    async fn send_final(self: &Arc<Self>, chat_id: &str, text: &str) {
        let Some(api) = self.api() else {
            return;
        };
        for chunk in live_chunks(text, live_text_limit(MAX_TEXT_MESSAGE_LENGTH)) {
            let is_final = !chunk.ends_with(render::DEFAULT_LIVE_REPLY_CURSOR);
            let kind = activity_kind_from_text(&chunk);
            if api.send_post(chat_id, &chunk, is_final, Some(kind)).await.is_failed()
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

    pub(crate) fn with_webhook_slot(
        options: FeishuOptions,
        interaction: Arc<dyn InteractionPort>,
        slot: Arc<FeishuWebhookSlot>,
    ) -> Self {
        Self::build(
            options,
            interaction,
            Some(slot),
            Arc::new(SystemClock),
            FeishuSessionMapper::random_uuid(),
            None,
            None,
        )
    }

    /// The parity seams: a deterministic clock, session-key uuid and slot.
    pub(crate) fn with_seams(
        options: FeishuOptions,
        interaction: Arc<dyn InteractionPort>,
        slot: Option<Arc<FeishuWebhookSlot>>,
        clock: Arc<dyn Clock>,
        uuid: mapper::Uuid,
    ) -> Self {
        Self::build(options, interaction, slot, clock, uuid, None, None)
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

    /// The shared webhook slot this channel registers itself into.
    pub(crate) fn webhook_slot(&self) -> Option<Arc<FeishuWebhookSlot>> {
        self.runtime.slot.clone()
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

struct NoopHandle;

impl ChannelHandle for NoopHandle {
    fn stop(&self, _reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
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
