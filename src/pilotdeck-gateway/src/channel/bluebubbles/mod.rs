//! `BlueBubblesChannel.ts` (baseline cc7c4e6) on the #1327 channel core:
//! a 2.5 s poll of `/api/v1/message` for inbound rows, replies POSTed to
//! `/api/v1/message/text`. No SDK, no stub — TS speaks global `fetch`, so this
//! port speaks the shared hyper client directly and the golden fixture runs
//! the same loopback mock against both sides.
//!
//! The elicitation/permission helper protocol is the same shared
//! `ImElicitationHelper`/`ImPermissionHelper` state machine the sms lane
//! ported; this channel drives it through [`InteractionPort`] with
//! [`interaction::HelperInteraction`] as the real implementation.
//!
//! Not carried over from TS (registered diffs, see
//! `specs/gateway-parity/diffs/`): production assembly installs
//! [`no_interaction`] (A8.3-CH-BLUEBUBBLES-002), `stop` does not await
//! in-flight turns (A8.3-CH-BLUEBUBBLES-003, #1353 owns the drain entry), a
//! same-guid row arriving after a restart re-dispatches channel-side
//! (A8.3-CH-BLUEBUBBLES-004 — upstream admission owns no-re-execution), and
//! the #2285 poll-driver deviations — drop-without-stop and swallowed
//! poll-task panics — are registered as A8.3-CH-BLUEBUBBLES-007.
mod interaction;
mod mapper;
#[cfg(test)]
mod tests;

pub use interaction::{HelperInteraction, InteractionResponder, ResponderRef};
pub use mapper::{BlueBubblesSessionMapper, BlueBubblesSessionMapperState, MappedSession};

use std::{
    collections::HashSet,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use tokio::sync::watch;

use crate::channel::ActiveChat;
use crate::channel::http_client::HttpClient;
use crate::channel::im_interaction::{InteractionPort, NoInteraction};
use crate::channel::protocol::{
    BoxFuture, ChannelAdapter, ChannelError, ChannelHandle, ChannelKey, ChannelLog,
    ChannelStartDeps, TurnGateway, TurnRequest,
};
use crate::channel::sms::{self, render::render_sms_event};
use crate::js_value::{number_string, string_to_number};
use crate::metadata::js_trim;

/// `BlueBubblesChannel.channelKey`.
pub const CHANNEL_KEY: &str = "bluebubbles";
/// `POLL_MS`.
const POLL_MS: Duration = Duration::from_millis(2500);
/// `MESSAGE_LIMIT`.
const MESSAGE_LIMIT: u32 = 50;
/// `AbortSignal.timeout(120_000)` on the reply POST.
const SEND_TIMEOUT: Duration = Duration::from_millis(120_000);
/// Bounded reaping of the aborted interval task at `stop` (#2285 ③): the
/// task is aborted before the await, so this only bounds a stuck runtime —
/// the same 20 s shape the test suite's `DEADLINE` uses, never unbounded.
const DRIVER_STOP: Duration = Duration::from_secs(20);

/// `process.env` lookup at construction (`BLUEBUBBLES_SERVER_URL` /
/// `BLUEBUBBLES_PASSWORD`).
pub type Env = Arc<dyn Fn(&str) -> Option<JsString> + Send + Sync>;

/// `BlueBubblesChannelOptions` before the constructor's `?? env ?? ""` defaults.
#[derive(Default)]
pub struct BlueBubblesOptions {
    pub server_url: Option<JsString>,
    pub password: Option<JsString>,
    pub mapper: Option<BlueBubblesSessionMapper>,
    pub env: Option<Env>,
}

fn env_lookup(env: &Option<Env>, name: &str) -> Option<JsString> {
    env.as_ref()?(name)
}

/// A present, non-nullish option value folded to its string form. The TS
/// constructor's `password` limb is `options.password ?? env ?? ""` with no
/// `String()` (BlueBubblesChannel.ts:52-55 — only `serverUrl` goes through
/// `normalizeBaseUrl`, whose `url.replace` throws on a non-string; see the
/// #2433 guard in `register`), so a non-string password passes through the
/// TS constructor and is coerced where it is used; this fold mirrors that
/// accept-instead-of-throw shape rather than refusing the value.
fn option_js_string(object: &JsObject, name: &str) -> Option<JsString> {
    match object.get(name) {
        Some(value) if !sms::nullish(value) => Some(sms::js_string_of(value)),
        _ => None,
    }
}

impl BlueBubblesOptions {
    /// `new BlueBubblesChannel(options)` field extraction for the loader mount
    /// (`build_options` already put `serverUrl`/`password` on the object).
    pub fn from_object(options: &JsObject) -> Self {
        Self {
            server_url: option_js_string(options, "serverUrl"),
            password: option_js_string(options, "password"),
            mapper: None,
            env: None,
        }
    }
}

/// `normalizeBaseUrl`: `url.replace(/\/+$/, "")`.
fn normalize_base_url(url: &str) -> &str {
    url.trim_end_matches('/')
}

/// `new URL(path, base)` for the two fixed absolute paths: the reference
/// resolves against the base's origin, so a base path or query is replaced
/// from the root. The `url` crate is WHATWG-compliant, so the parse follows
/// the TS reference exactly: special schemes tolerate missing slashes
/// (`http:/h`, `http:h`), an empty port is dropped, the host is
/// IDNA-normalized and lowercased, an explicit default port disappears, and
/// a base that does not parse is the TS `TypeError: Invalid URL`. A userinfo
/// component survives `new URL` into the request URL and Node v22.22.0
/// `fetch` refuses it outright (`TypeError: Request cannot be constructed
/// from a URL that includes credentials`, Node 实测); a non-http(s) scheme is
/// `TypeError: fetch failed` with cause `unknown scheme` (same 实测) — both
/// carry the same `poll error` / `send failed` log shape the caller gives
/// every other parse failure.
fn url_with_path(server_url: &str, path: &str) -> Result<String, String> {
    let base = url::Url::parse(server_url).map_err(|_| "Invalid URL".to_owned())?;
    if !base.username().is_empty() || base.password().is_some() {
        return Err(
            "Request cannot be constructed from a URL that includes credentials".to_owned(),
        );
    }
    let scheme = base.scheme();
    if scheme != "http" && scheme != "https" {
        return Err("fetch failed".to_owned());
    }
    Ok(format!("{}{}", base.origin().ascii_serialization(), path))
}

/// The `URLSearchParams` serializer (`application/x-www-form-urlencoded`):
/// UTF-8 bytes, ASCII alphanumerics plus `*-._` kept, space → `+`, everything
/// else percent-encoded. Differs from `encodeURIComponent` (`!~'()` encode).
fn query_encode(value: &JsString) -> String {
    let mut out = String::with_capacity(value.len_utf16());
    for byte in String::from_utf16_lossy(value.units()).as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'*' | b'-' | b'.' | b'_') {
            out.push(*byte as char);
        } else if *byte == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// TS `num(v)`: a finite number, or a non-blank string that parses finite.
fn num(value: Option<&JsValue>) -> Option<f64> {
    match value {
        Some(JsValue::Number(number)) if number.is_finite() => Some(*number),
        Some(JsValue::String(text)) => {
            let raw = String::from_utf16_lossy(text.units());
            if js_trim(&raw).is_empty() {
                return None;
            }
            let parsed = string_to_number(&raw);
            parsed.is_finite().then_some(parsed)
        }
        _ => None,
    }
}

/// `a ?? b` over `JsObject::get` (missing and null both fall through).
fn non_nullish<'a>(object: &'a JsObject, name: &str) -> Option<&'a JsValue> {
    object.get(name).filter(|value| !sms::nullish(value))
}

/// `Date.now() / 1000`, seconds since the epoch.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

#[derive(Default)]
struct RuntimeState {
    mapper: BlueBubblesSessionMapper,
    seen_guids: HashSet<JsString>,
    last_timestamp: f64,
}

/// The `im_interaction` logger cell (feishu shape): installed by `start`,
/// optional afterwards.
#[derive(Clone, Default)]
struct Logger {
    sink: Arc<StdMutex<Option<ChannelLog>>>,
}

impl Logger {
    fn install(&self, log: Option<ChannelLog>) {
        *self.sink.lock().expect("#1343 logger mutex") = log;
    }
    fn log(&self, level: &str, message: &str) {
        let sink = self.sink.lock().expect("#1343 logger mutex").clone();
        if let Some(sink) = sink {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(level, message)));
        }
    }
}

// Lock strategy: the helper state machines in `interaction.rs` recover a
// poisoned mutex (`PoisonError::into_inner`, the `sms/permission.rs`
// `lock_state` pattern, #1790) because a turn that panics mid-prompt must
// not take the channel down. These plain-state guards keep `.expect`
// instead: their sections only move scalars and set members (the payloads
// are converted to `JsString` before the guard is taken), so a panic under
// one of them means an invariant is already broken — fail loudly and let
// the driver's panic surface it (panic detection is #2285 ③).
struct Runtime {
    key: ChannelKey,
    /// Normalized `serverUrl` (scalar string; it only feeds the URL builder
    /// and the `polling` log line).
    server_url: String,
    /// Exact UTF-16: it rides the query string and the POST body.
    password: JsString,
    gateway: StdMutex<Option<Arc<dyn TurnGateway>>>,
    interaction: Arc<dyn InteractionPort>,
    logger: Logger,
    /// `mapper` + `seenGuids` + `lastTimestamp`, one synchronous section each
    /// exactly where TS runs run-to-completion code.
    state: StdMutex<RuntimeState>,
    active_chats: Arc<StdMutex<HashSet<JsString>>>,
    running: std::sync::atomic::AtomicBool,
    /// `pollAbort`: recreated by every `start`, aborted by `stop`.
    poll_abort: StdMutex<watch::Sender<bool>>,
    /// The interval task's `JoinHandle` of the current epoch: taken and
    /// reaped by `stop` (#2285 ③, the api_server `Listener.task` form) so
    /// the driver cannot outlive the channel silently.
    driver: StdMutex<Option<tokio::task::JoinHandle<()>>>,
    client: HttpClient,
}

impl Runtime {
    fn log(&self, level: &str, message: &str) {
        self.logger.log(level, message);
    }

    fn gateway(&self) -> Option<Arc<dyn TurnGateway>> {
        self.gateway.lock().expect("#1343 gateway mutex").clone()
    }

    /// `start()` — TS: gateway/logger first, then the config check.
    async fn start(
        self: &Arc<Self>,
        deps: ChannelStartDeps,
    ) -> Result<Arc<dyn ChannelHandle>, ChannelError> {
        *self.gateway.lock().expect("#1343 gateway mutex") = deps.turn_gateway;
        self.logger.install(deps.log);

        if self.server_url.is_empty() || self.password.is_empty() {
            self.log(
                "error",
                "bluebubbles: serverUrl and password (or BLUEBUBBLES_SERVER_URL / \
                 BLUEBUBBLES_PASSWORD) are required",
            );
            return Ok(Arc::new(EmptyHandle));
        }

        // A fresh abort controller: the previous epoch's polls (already
        // aborted by its stop, or orphaned by this replacement) lose their
        // signal — the dropped sender breaks the old driver instead of TS's
        // leaked interval, which only matters for the unsupported
        // start-without-stop flow.
        let (abort, _) = watch::channel(false);
        *self.poll_abort.lock().expect("#1343 abort mutex") = abort;
        {
            let mut state = self.state.lock().expect("#1343 state mutex");
            state.seen_guids.clear();
            state.last_timestamp = now_secs().floor() - 5.0;
        }
        self.running
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // TS logs before any poll can complete (the block is synchronous);
        // logging first keeps the golden `logs` order the same.
        self.log(
            "info",
            &format!("bluebubbles: polling {}/api/v1/message", self.server_url),
        );

        // `void this.pollMessages()` — the immediate poll, separate from the
        // interval's first tick (tokio's `interval` fires at t=0, `setInterval` does
        // not). #2285 ②: TS leaks its interval on the drop-without-stop flow; the task
        // holds only a `Weak`, so the upgrade failure ends it instead of keeping the
        // `Runtime` alive forever — but only while no poll is in flight (in-flight
        // polls hold the strong count up: the TS-leak shape, see A8.3-CH-BLUEBUBBLES-007).
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let Some(runtime) = weak.upgrade() else {
                return;
            };
            runtime.poll_messages().await;
        });
        let weak = Arc::downgrade(self);
        let mut abort = self
            .poll_abort
            .lock()
            .expect("#1343 abort mutex")
            .subscribe();
        let driver = tokio::spawn(async move {
            let mut timer = tokio::time::interval(POLL_MS);
            timer.tick().await;
            loop {
                tokio::select! {
                    changed = abort.changed() => {
                        // Aborted (true), or the sender replaced by a restart.
                        if changed.is_err() || *abort.borrow_and_update() {
                            break;
                        }
                    }
                    _ = timer.tick() => {
                        // #2285 ②: same `Weak` rule as the immediate poll — but only while
                        // no poll is in flight; a dropped channel ends the loop, not a forever poll.
                        let Some(runtime) = weak.upgrade() else { break };
                        tokio::spawn(async move {
                            runtime.poll_messages().await;
                        });
                    }
                }
            }
        });
        // #2285 ③: keep the handle so `stop` can abort and reap the task; a
        // restart replaces it (the previous epoch's task already exited via
        // its replaced sender).
        *self.driver.lock().expect("#1343 driver mutex") = Some(driver);

        Ok(Arc::new(BlueBubblesHandle {
            runtime: self.clone(),
        }))
    }

    /// `pollMessages()` — the abort check, then the GET, then row dispatch.
    async fn poll_messages(self: &Arc<Self>) {
        if !self.running.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let after = number_string(self.state.lock().expect("#1343 state mutex").last_timestamp);
        let query = format!(
            "password={}&after={}&limit={}",
            query_encode(&self.password),
            after,
            MESSAGE_LIMIT
        );
        // `new URL(...)` runs before the try in TS; a malformed base here
        // logs instead of surfacing as the TS unhandled rejection.
        let url = match url_with_path(&self.server_url, "/api/v1/message") {
            Ok(origin) => format!("{origin}?{query}"),
            Err(error) => {
                self.log(
                    "error",
                    &format!("bluebubbles: poll error: TypeError: {error}"),
                );
                return;
            }
        };

        let mut signal = self
            .poll_abort
            .lock()
            .expect("#1343 abort mutex")
            .subscribe();
        // `fetch(url, { signal })` with an already-aborted signal rejects at
        // once; `subscribe` marks the current value seen, so check it first.
        if *signal.borrow() {
            return;
        }
        let result = tokio::select! {
            _ = signal.changed() => return,
            result = self.client.request_unbounded("GET", &url, None, "") => result,
        };
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.log("error", &format!("bluebubbles: poll error: {error}"));
                return;
            }
        };
        if !response.is_ok() {
            self.log("warn", &format!("bluebubbles: poll {}", response.status));
            return;
        }
        let data = match parse_json(&String::from_utf8_lossy(&response.body)) {
            Ok(data) => data,
            Err(error) => {
                self.log(
                    "error",
                    &format!("bluebubbles: poll error: SyntaxError: {error}"),
                );
                return;
            }
        };
        let rows = match &data {
            JsValue::Array(items) => items.as_slice(),
            JsValue::Object(object) => {
                if let Some(JsValue::Array(items)) = object.get("data") {
                    items.as_slice()
                } else if let Some(JsValue::Array(items)) = object.get("messages") {
                    items.as_slice()
                } else {
                    &[]
                }
            }
            _ => &[],
        };

        let mut max_ts = self.state.lock().expect("#1343 state mutex").last_timestamp;
        for row in rows {
            // `!row || typeof row !== "object"` — null/scalars skip; arrays
            // pass (their `guid`/`text` lookups are undefined, like TS).
            if !matches!(row, JsValue::Object(_) | JsValue::Array(_)) {
                continue;
            }
            let object = row.as_object();
            let guid = object
                .and_then(|object| {
                    non_nullish(object, "guid").or_else(|| non_nullish(object, "id"))
                })
                .map(sms::js_string_of)
                .unwrap_or_default();
            let skip = {
                let mut state = self.state.lock().expect("#1343 state mutex");
                !guid.is_empty() && !state.seen_guids.insert(guid.clone())
            };
            if skip {
                continue;
            }
            let timestamp = object.and_then(|object| {
                non_nullish(object, "dateCreated")
                    .or_else(|| non_nullish(object, "timestamp"))
                    .or_else(|| non_nullish(object, "time"))
            });
            if let Some(ts) = num(timestamp)
                && ts > max_ts
            {
                max_ts = ts;
            }
            self.dispatch_payload(row).await;
        }
        let mut state = self.state.lock().expect("#1343 state mutex");
        if max_ts > state.last_timestamp {
            state.last_timestamp = max_ts;
        }
    }

    /// `dispatchPayload(o)`. An array row passes TS's `typeof === "object"`
    /// gate; every lookup below then reads `undefined` (an array has no
    /// `guid`/`text` own properties), so it falls out at the empty
    /// `chatGuid`/`text` gate — `object` being `None` models that.
    async fn dispatch_payload(self: &Arc<Self>, row: &JsValue) {
        let object = row.as_object();
        let from_me = object
            .and_then(|object| {
                non_nullish(object, "isFromMe").or_else(|| non_nullish(object, "is_from_me"))
            })
            .map(sms::truthy)
            .unwrap_or(false);
        if from_me {
            return;
        }
        let text = sms::js_string_of(
            &object
                .and_then(|object| {
                    non_nullish(object, "text")
                        .or_else(|| non_nullish(object, "body"))
                        .or_else(|| non_nullish(object, "message"))
                })
                .cloned()
                .unwrap_or(JsValue::from("")),
        );
        let text = JsString::from_utf16(sms::trim_units(text.units()).to_vec());
        let chat_value = object
            .and_then(|object| {
                non_nullish(object, "chatGuid").or_else(|| non_nullish(object, "chat_guid"))
            })
            .or_else(|| {
                // `Array.isArray(chatsField) ? chatsField[0] : ""` — a
                // nullish first element falls to the outer `?? ""`.
                match object.and_then(|object| object.get("chats")) {
                    Some(JsValue::Array(chats)) => {
                        chats.first().filter(|value| !sms::nullish(value))
                    }
                    _ => None,
                }
            });
        let chat_guid = sms::js_string_of(chat_value.unwrap_or(&JsValue::from("")));
        if chat_guid.is_empty() || text.is_empty() {
            return;
        }
        let chat_id = String::from_utf16_lossy(chat_guid.units());
        let text_scalar = String::from_utf16_lossy(text.units());

        if self.interaction.elicitation_pending(&chat_id) && self.gateway().is_some() {
            match self
                .interaction
                .elicitation_answer(&chat_id, &text_scalar)
                .await
            {
                Ok(Some(confirmation)) => {
                    self.send_reply(&chat_guid, &JsString::from(confirmation))
                        .await;
                }
                Ok(None) => {}
                Err(error) => self.log(
                    "error",
                    &format!(
                        "bluebubbles: elicitation answer error: {}",
                        error.to_js_string()
                    ),
                ),
            }
            return;
        }

        if self.interaction.permission_pending(&chat_id) && self.gateway().is_some() {
            // TS arms `answerToken` only after a successful `answerWithState`;
            // on its rejection the token is still undefined, so the catch
            // releases nothing (the helper restored its own state).
            match self
                .interaction
                .permission_answer(&chat_id, &text_scalar)
                .await
            {
                Err(error) => self.log(
                    "error",
                    &format!(
                        "bluebubbles: permission answer error: {}",
                        error.to_js_string()
                    ),
                ),
                Ok(None) => {}
                Ok(Some(answer)) => {
                    if let Some(confirmation) = answer.text.filter(|text| !text.is_empty()) {
                        let delivered = self
                            .send_reply(&chat_guid, &JsString::from(confirmation))
                            .await;
                        if !delivered {
                            self.interaction
                                .permission_release_answer(&chat_id, answer.answer_token);
                            return;
                        }
                        if !answer.can_advance && !answer.retry_prompt {
                            return;
                        }
                        if let Some(next_prompt) = self
                            .interaction
                            .permission_take_next_prompt(&chat_id, answer.answer_token)
                        {
                            let request_id = self
                                .interaction
                                .permission_prompt_request_id(&chat_id, answer.answer_token);
                            let delivered = self
                                .send_reply(&chat_guid, &JsString::from(next_prompt))
                                .await;
                            self.interaction.permission_confirm_next(
                                &chat_id,
                                delivered,
                                request_id.as_deref(),
                                answer.answer_token,
                            );
                        }
                    }
                }
            }
            return;
        }

        // The already-active check, `mapper.resolve` and the admission run in
        // one synchronous stretch in TS; one lock section here keeps the
        // mapper untouched on the skip path.
        enum Outcome {
            Skip,
            NewChat,
            EmptyMessage,
            Dispatch(MappedSession, ActiveChat<JsString>),
        }
        let outcome = {
            let mut state = self.state.lock().expect("#1343 state mutex");
            if self
                .active_chats
                .lock()
                .expect("#1343 active chats mutex")
                .contains(&chat_guid)
            {
                Outcome::Skip
            } else {
                let mapped = state.mapper.resolve(&chat_guid, &text);
                if mapped.command_new && mapped.message.is_empty() {
                    Outcome::NewChat
                } else if mapped.message.is_empty() {
                    Outcome::EmptyMessage
                } else {
                    self.active_chats
                        .lock()
                        .expect("#1343 active chats mutex")
                        .insert(chat_guid.clone());
                    let guard = ActiveChat::new(self.active_chats.clone(), chat_guid.clone());
                    Outcome::Dispatch(mapped, guard)
                }
            }
        };
        match outcome {
            Outcome::Skip => self.log(
                "info",
                &format!("bluebubbles: chat {chat_id} already active, skipping"),
            ),
            Outcome::NewChat => {
                self.send_reply(&chat_guid, &JsString::from("已创建新会话。"))
                    .await;
            }
            Outcome::EmptyMessage => {}
            Outcome::Dispatch(mapped, _guard) => {
                // The guard's `Drop` is the TS `finally { activeChats.delete }`.
                self.process_message(&chat_guid, &mapped.session_key, &mapped.message)
                    .await;
            }
        }
    }

    /// `processMessage(chatGuid, sessionKey, message)`.
    async fn process_message(
        self: &Arc<Self>,
        chat_guid: &JsString,
        session_key: &JsString,
        message: &JsString,
    ) {
        let Some(gateway) = self.gateway() else {
            return;
        };
        let chat_id = String::from_utf16_lossy(chat_guid.units());
        let session_id = String::from_utf16_lossy(session_key.units());

        let mut reply_units: Vec<u16> = Vec::new();
        let mut error_text: Option<String> = None;
        let mut stream = match gateway
            .submit(TurnRequest {
                session_key: session_key.clone(),
                channel_key: self.key.clone(),
                message: message.clone(),
            })
            .await
        {
            Err(error) => {
                error_text = Some(error.to_js_string());
                None
            }
            Ok(stream) => Some(stream),
        };
        if let Some(stream) = stream.as_mut() {
            loop {
                let event = match stream.next().await {
                    Ok(Some(event)) => event,
                    Ok(None) => break,
                    Err(error) => {
                        error_text = Some(error.to_js_string());
                        break;
                    }
                };
                let kind = event.get("type").and_then(JsValue::as_string);
                if kind.is_some_and(|kind| kind.equals_str("elicitation_request")) {
                    let question =
                        self.interaction
                            .elicitation_capture(&chat_id, &session_id, &event);
                    self.send_reply(chat_guid, &JsString::from(question)).await;
                    continue;
                }
                if kind.is_some_and(|kind| kind.equals_str("permission_request")) {
                    if let Some(question) =
                        self.interaction
                            .permission_capture(&chat_id, &session_id, &event)
                    {
                        let delivered = self.send_reply(chat_guid, &JsString::from(question)).await;
                        let request_id = event
                            .get("requestId")
                            .and_then(sms::permission::optional_string)
                            .map(|id| String::from_utf16_lossy(id.units()));
                        self.interaction.permission_confirm_initial(
                            &chat_id,
                            delivered,
                            request_id.as_deref(),
                        );
                    }
                    continue;
                }
                if let Some(fragment) = render_sms_event(&event) {
                    reply_units.extend_from_slice(fragment.units());
                }
            }
        }
        if let Some(error) = error_text {
            self.log("error", &format!("bluebubbles: submitTurn error: {error}"));
            reply_units = "处理消息时发生错误，请重试。".encode_utf16().collect();
        }

        self.interaction.elicitation_clear(&chat_id);
        self.interaction.permission_clear_after_turn(&chat_id);

        let final_text = sms::trim_units(&reply_units);
        if !final_text.is_empty() {
            self.send_reply(chat_guid, &JsString::from_utf16(final_text.to_vec()))
                .await;
        }
    }

    /// `sendReply(chatGuid, text)` — the POST, with its `!running` gate.
    async fn send_reply(self: &Arc<Self>, chat_guid: &JsString, text: &JsString) -> bool {
        if !self.running.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        let url = match url_with_path(&self.server_url, "/api/v1/message/text") {
            Ok(origin) => origin,
            Err(error) => {
                // TS builds the URL outside its try; a malformed base throws
                // out of `sendReply` into the caller's catch. That base was
                // already logged by the poll path.
                self.log(
                    "error",
                    &format!("bluebubbles: send failed: TypeError: {error}"),
                );
                return false;
            }
        };
        let temp_guid = uuid::Uuid::new_v4().to_string();
        let mut body = JsObject::new();
        body.insert("chatGuid".into(), JsValue::String(chat_guid.clone()));
        body.insert("message".into(), JsValue::String(text.clone()));
        body.insert("tempGuid".into(), JsValue::from(temp_guid.as_str()));
        body.insert("password".into(), JsValue::String(self.password.clone()));
        let payload = stringify_json(&JsValue::Object(body))
            .ok()
            .flatten()
            .unwrap_or_default();
        let bearer = String::from_utf16_lossy(self.password.units());
        match self
            .client
            .request_bearer(
                "POST",
                &url,
                Some("application/json"),
                &bearer,
                &payload,
                SEND_TIMEOUT,
            )
            .await
        {
            Err(error) => {
                self.log("error", &format!("bluebubbles: send failed: {error}"));
                false
            }
            Ok(response) if !response.is_ok() => {
                let raw = parse_json(&String::from_utf8_lossy(&response.body)).ok();
                let error_text = raw
                    .as_ref()
                    .and_then(|raw| raw.get("message").filter(|value| !sms::nullish(value)))
                    .or_else(|| {
                        raw.as_ref()
                            .and_then(|raw| raw.get("error").filter(|value| !sms::nullish(value)))
                    })
                    .map(sms::js_string_of)
                    .unwrap_or_else(|| JsString::from(response.status_text));
                self.log(
                    "error",
                    &format!(
                        "bluebubbles: send HTTP {}: {}",
                        response.status,
                        String::from_utf16_lossy(error_text.units())
                    ),
                );
                false
            }
            Ok(_) => true,
        }
    }
}

pub struct BlueBubblesChannel {
    key: ChannelKey,
    runtime: Arc<Runtime>,
}

impl BlueBubblesChannel {
    pub fn new(options: BlueBubblesOptions, interaction: Arc<dyn InteractionPort>) -> Self {
        let server_url = options
            .server_url
            .clone()
            .or_else(|| env_lookup(&options.env, "BLUEBUBBLES_SERVER_URL"))
            .unwrap_or_default();
        let password = options
            .password
            .clone()
            .or_else(|| env_lookup(&options.env, "BLUEBUBBLES_PASSWORD"))
            .unwrap_or_default();
        let server_url =
            normalize_base_url(&String::from_utf16_lossy(server_url.units())).to_owned();
        let runtime = Arc::new(Runtime {
            key: ChannelKey::new(CHANNEL_KEY),
            server_url,
            password,
            gateway: StdMutex::new(None),
            interaction,
            logger: Logger::default(),
            state: StdMutex::new(RuntimeState {
                mapper: options.mapper.unwrap_or_default(),
                seen_guids: HashSet::new(),
                last_timestamp: 0.0,
            }),
            active_chats: Arc::new(StdMutex::new(HashSet::new())),
            running: std::sync::atomic::AtomicBool::new(false),
            poll_abort: StdMutex::new(watch::channel(false).0),
            driver: StdMutex::new(None),
            client: HttpClient::new(),
        });
        Self {
            key: ChannelKey::new(CHANNEL_KEY),
            runtime,
        }
    }
}

/// Default interaction port for the assembly mount (A8.3-CH-BLUEBUBBLES-002).
pub fn no_interaction() -> Arc<dyn InteractionPort> {
    Arc::new(NoInteraction)
}

impl ChannelAdapter for BlueBubblesChannel {
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
}

/// The missing-config handle: `{ stop: async () => undefined }`.
struct EmptyHandle;

impl ChannelHandle for EmptyHandle {
    fn stop(&self, _reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        Box::pin(async { Ok(()) })
    }
}

struct BlueBubblesHandle {
    runtime: Arc<Runtime>,
}

impl ChannelHandle for BlueBubblesHandle {
    fn stop(&self, reason: &str) -> BoxFuture<Result<(), ChannelError>> {
        let shown = if reason.is_empty() {
            "no reason"
        } else {
            reason
        };
        let runtime = self.runtime.clone();
        let message = format!("bluebubbles: stopping ({shown})");
        Box::pin(async move {
            runtime.log("info", &message);
            runtime
                .running
                .store(false, std::sync::atomic::Ordering::SeqCst);
            let _ = runtime
                .poll_abort
                .lock()
                .expect("#1343 abort mutex")
                .send(true);
            // #2285 ③: TS `clearInterval`s the timer; here the interval
            // task is aborted and reaped with a bounded wait (a task that
            // panicked before `stop` surfaces as a log instead of stalling
            // silently with `running === true`).
            let driver = runtime.driver.lock().expect("#1343 driver mutex").take();
            if let Some(driver) = driver {
                driver.abort();
                match tokio::time::timeout(DRIVER_STOP, driver).await {
                    Ok(Err(error)) if error.is_panic() => runtime.log(
                        "error",
                        &format!("bluebubbles: poll driver task failed: {error}"),
                    ),
                    Ok(_) => {}
                    Err(_) => runtime.log(
                        "error",
                        "bluebubbles: poll driver task did not exit after stop",
                    ),
                }
            }
            // TS does not await in-flight turns here (#1353 owns the drain
            // entry): a turn already dispatched keeps running, and its
            // `sendReply` sees `running === false` and posts nothing.
            Ok(())
        })
    }
}

/// Loader mount: builds the adapter from the `bluebubbles` loader option
/// object (`build_options` already resolved `extra.serverUrl` /
/// `token ?? extra.password`).
///
/// #2433: the TS constructor's
/// `normalizeBaseUrl(options.serverUrl ?? …)` calls `url.replace`, so a
/// present non-nullish non-string throws `TypeError: url.replace is not a
/// function` (Node v22.22.0 probes, `_tmp/20261026-issue-p2batch-f/s5-probe/`)
/// — the loader catch records the key and continues, and the channel never
/// enters the startup array. The Rust mount must not paper over that with
/// `String()` coercion: it applies the same check at construction and
/// surfaces the same TypeError through the same loader catch.
pub fn register(mounts: &mut crate::channel::loader::ChannelLoaderMounts) {
    register_with_interaction(mounts, no_interaction());
}
pub fn register_with_interaction(
    mounts: &mut crate::channel::loader::ChannelLoaderMounts,
    interaction: Arc<dyn InteractionPort>,
) {
    mounts.register(
        "bluebubbles",
        Arc::new(move |options: &JsObject| {
            if let Some(value) = options
                .get("serverUrl")
                .filter(|value| !sms::nullish(value))
                && !matches!(value, JsValue::String(_))
            {
                return Err(ChannelError::Plain(
                    "TypeError: url.replace is not a function".to_owned(),
                ));
            }
            Ok(Arc::new(BlueBubblesChannel::new(
                BlueBubblesOptions {
                    env: Some(Arc::new(|name| {
                        std::env::var(name).ok().map(JsString::from)
                    })),
                    ..BlueBubblesOptions::from_object(options)
                },
                interaction.clone(),
            )) as Arc<dyn ChannelAdapter>)
        }),
    );
}
